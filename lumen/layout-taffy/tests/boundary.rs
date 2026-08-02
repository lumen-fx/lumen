//! D1 / D2 / D6 / spec-section 17.3 regression tests.
//!
//! * D1 - a `RelayoutBoundary` subtree recompute must keep the
//!   boundary's own absolute position and size (solved against the box
//!   the parent last gave it, not the viewport), while descendants get
//!   fresh geometry. A style change on the boundary *itself* must still
//!   reach the parent (the pierce in `react_to_style_changes`).
//! * D2 - `Style.display = None` releases the entity's space and the
//!   siblings reflow on the next tick, purely via `Changed<Style>`.
//! * D6 - an `ImageComponent.natural_size` write drives intrinsic image
//!   sizing through the measure path, and a change re-layouts.
//! * section 17.3 - at most one taffy solve per dirty root per tick, no matter
//!   how many mutations landed in between.

use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};

fn app_with_viewport(w: f32, h: f32) -> App {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    {
        let mut layout = app
            .world
            .get_non_send_resource_mut::<LayoutResource>()
            .unwrap();
        layout.set_viewport(w, h);
    }
    app.world.insert_resource(Viewport {
        size: glam::Vec2::new(w, h),
        ..Viewport::default()
    });
    app
}

fn px_style(w: f32, h: f32) -> Style {
    Style {
        width: Length::Px(w),
        height: Length::Px(h),
        ..Style::default()
    }
}

/// Probe scenario 1: `root(col) -> [toolbar 800x50, boundary 200x200 ->
/// leaf]`. Leaf-only dirt must recompute inside the boundary without
/// teleporting the boundary to its parent's origin.
#[test]
fn boundary_keeps_position_after_leaf_only_dirt() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app
        .world
        .spawn((
            Style {
                flex_direction: FlexDirection::Column,
                ..px_style(800.0, 600.0)
            },
            DirtyLayout,
        ))
        .id();
    let _toolbar = app
        .world
        .spawn((px_style(800.0, 50.0), ChildOf(root), DirtyLayout))
        .id();
    let boundary = app
        .world
        .spawn((
            Style {
                flex_direction: FlexDirection::Column,
                ..px_style(200.0, 200.0)
            },
            RelayoutBoundary,
            ChildOf(root),
            DirtyLayout,
        ))
        .id();
    let leaf = app
        .world
        .spawn((px_style(50.0, 20.0), ChildOf(boundary), DirtyLayout))
        .id();

    app.tick();
    let t0 = *app.world.get::<Transform>(boundary).unwrap();
    assert_eq!(t0.absolute.y, 50.0, "boundary sits below the toolbar");

    // Mutate only the leaf; propagation stops at the boundary.
    app.world.get_mut::<Style>(leaf).unwrap().width = Length::Px(80.0);
    app.tick();

    let t1 = *app.world.get::<Transform>(boundary).unwrap();
    let l1 = *app.world.get::<Transform>(leaf).unwrap();
    assert_eq!(
        t1.absolute, t0.absolute,
        "boundary must not move on a subtree-only recompute"
    );
    assert_eq!(t1.size, t0.size, "boundary size unchanged");
    assert_eq!(l1.size.x, 80.0, "leaf got its new width");
    assert_eq!(
        l1.absolute.y, t1.absolute.y,
        "leaf stays anchored to the boundary's origin"
    );
}

/// Probe scenario 2: a percent-sized boundary (explicit
/// `layout-boundary` attr case). The subtree recompute must not
/// re-resolve `50%` against the viewport - the boundary keeps the
/// 200 px its parent gave it.
#[test]
fn percent_boundary_keeps_size_after_leaf_only_dirt() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app
        .world
        .spawn((
            Style {
                flex_direction: FlexDirection::Column,
                ..px_style(400.0, 300.0)
            },
            DirtyLayout,
        ))
        .id();
    let boundary = app
        .world
        .spawn((
            Style {
                width: Length::Percent(50.0), // 200 within the 400px root
                height: Length::Px(100.0),
                ..Style::default()
            },
            RelayoutBoundary,
            ChildOf(root),
            DirtyLayout,
        ))
        .id();
    let leaf = app
        .world
        .spawn((px_style(10.0, 10.0), ChildOf(boundary), DirtyLayout))
        .id();

    app.tick();
    let p0 = *app.world.get::<Transform>(boundary).unwrap();
    assert_eq!(p0.size.x, 200.0, "50% of the 400px parent");

    app.world.get_mut::<Style>(leaf).unwrap().width = Length::Px(20.0);
    app.tick();

    let p1 = *app.world.get::<Transform>(boundary).unwrap();
    assert_eq!(
        p1.size.x, 200.0,
        "percent boundary must keep the parent-resolved 200px, not re-resolve against the viewport"
    );
    assert_eq!(p1.absolute, p0.absolute);
    assert_eq!(app.world.get::<Transform>(leaf).unwrap().size.x, 20.0);
}

/// D1 pierce: a Style change on the boundary *itself* can resize it, so
/// the parent must observe it - later siblings reflow.
#[test]
fn boundary_own_style_change_reaches_parent() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app.world.spawn((px_style(800.0, 100.0), DirtyLayout)).id();
    let boundary = app
        .world
        .spawn((
            px_style(200.0, 100.0),
            RelayoutBoundary,
            ChildOf(root),
            DirtyLayout,
        ))
        .id();
    let sibling = app
        .world
        .spawn((px_style(100.0, 100.0), ChildOf(root), DirtyLayout))
        .id();

    app.tick();
    assert_eq!(
        app.world.get::<Transform>(sibling).unwrap().absolute.x,
        200.0
    );

    app.world.get_mut::<Style>(boundary).unwrap().width = Length::Px(300.0);
    app.tick();

    assert_eq!(
        app.world.get::<Transform>(boundary).unwrap().size.x,
        300.0,
        "boundary got its new width"
    );
    assert_eq!(
        app.world.get::<Transform>(sibling).unwrap().absolute.x,
        300.0,
        "sibling moved - the boundary's own resize pierced to the parent"
    );
}

/// D2 / spec section 17.4: hiding the middle child of a 3-child row via
/// `Display::None` releases its space; siblings reflow next tick.
#[test]
fn display_none_releases_space_and_siblings_reflow() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app.world.spawn((px_style(600.0, 100.0), DirtyLayout)).id();
    let a = app
        .world
        .spawn((px_style(100.0, 100.0), ChildOf(root), DirtyLayout))
        .id();
    let b = app
        .world
        .spawn((px_style(100.0, 100.0), ChildOf(root), DirtyLayout))
        .id();
    let c = app
        .world
        .spawn((px_style(100.0, 100.0), ChildOf(root), DirtyLayout))
        .id();

    app.tick();
    assert_eq!(app.world.get::<Transform>(a).unwrap().absolute.x, 0.0);
    assert_eq!(app.world.get::<Transform>(c).unwrap().absolute.x, 200.0);

    // Hide the middle child - the pure Changed<Style> path must reflow.
    app.world.get_mut::<Style>(b).unwrap().display = lumen_core::components::Display::None;
    app.tick();

    assert_eq!(
        app.world.get::<Transform>(c).unwrap().absolute.x,
        100.0,
        "third child slid into the hidden sibling's space"
    );

    // Show it again: exactly the prior layout comes back (section 17.4 test
    // matrix c).
    app.world.get_mut::<Style>(b).unwrap().display = lumen_core::components::Display::Flex;
    app.tick();
    assert_eq!(app.world.get::<Transform>(b).unwrap().absolute.x, 100.0);
    assert_eq!(app.world.get::<Transform>(c).unwrap().absolute.x, 200.0);
}

/// Spec section 17.3: N mutations between ticks collapse into one taffy solve
/// per dirty root.
#[test]
fn at_most_one_solve_per_dirty_root_per_tick() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app.world.spawn((px_style(800.0, 600.0), DirtyLayout)).id();
    let leaf = app
        .world
        .spawn((px_style(50.0, 20.0), ChildOf(root), DirtyLayout))
        .id();
    app.tick();

    // Three mutations in one "handler" - one dirty root, one solve.
    for w in [60.0, 70.0, 80.0] {
        app.world.get_mut::<Style>(leaf).unwrap().width = Length::Px(w);
    }
    app.tick();

    let layout = app.world.non_send_resource::<LayoutResource>();
    assert_eq!(
        layout.solves_last_sync(),
        1,
        "3 style writes on one subtree must cost exactly 1 taffy solve"
    );
    assert_eq!(app.world.get::<Transform>(leaf).unwrap().size.x, 80.0);
}

/// D6: an image leaf with no explicit size lays out at its bitmap size
/// once `natural_size` is stamped, and a later stamp re-layouts via the
/// `Changed<ImageComponent>` hook.
#[test]
fn image_lays_out_at_natural_size_and_relayouts_on_change() {
    let mut app = app_with_viewport(800.0, 600.0);
    let img = app
        .world
        .spawn((
            Style::default(),
            ImageComponent {
                source: "probe.png".into(),
                natural_size: Some(glam::Vec2::new(40.0, 30.0)),
            },
            DirtyLayout,
        ))
        .id();
    app.tick();
    let t0 = *app.world.get::<Transform>(img).unwrap();
    assert_eq!(
        t0.size,
        glam::Vec2::new(40.0, 30.0),
        "intrinsic = bitmap logical size (spec section 13)"
    );

    // Decode swap (e.g. hot-reload): natural size changes -> relayout
    // without any other dirt.
    app.world
        .get_mut::<ImageComponent>(img)
        .unwrap()
        .natural_size = Some(glam::Vec2::new(64.0, 16.0));
    app.tick();
    let t1 = *app.world.get::<Transform>(img).unwrap();
    assert_eq!(t1.size, glam::Vec2::new(64.0, 16.0));
}

/// Spec section 0 / section 13: an explicit size beats the intrinsic bitmap size.
#[test]
fn explicit_size_beats_image_intrinsic() {
    let mut app = app_with_viewport(800.0, 600.0);
    let img = app
        .world
        .spawn((
            px_style(120.0, 90.0),
            ImageComponent {
                source: "probe.png".into(),
                natural_size: Some(glam::Vec2::new(40.0, 30.0)),
            },
            DirtyLayout,
        ))
        .id();
    app.tick();
    assert_eq!(
        app.world.get::<Transform>(img).unwrap().size,
        glam::Vec2::new(120.0, 90.0)
    );
}

/// Wave-6 T2 regression: `width: 100%` + explicit px height + margin
/// (the counter app's `.tile`) must not collapse to a zero-size rect.
/// Repro shape: column root -> scroll container -> tile children.
#[test]
fn percent_width_with_fixed_height_and_margin_is_not_zero() {
    let mut app = app_with_viewport(800.0, 600.0);
    let root = app
        .world
        .spawn((
            Style {
                flex_direction: FlexDirection::Column,
                ..px_style(800.0, 600.0)
            },
            DirtyLayout,
        ))
        .id();
    let tile = app
        .world
        .spawn((
            Style {
                width: Length::Percent(100.0),
                height: Length::Px(80.0),
                margin: Edges {
                    top: 8.0,
                    right: 8.0,
                    bottom: 4.0,
                    left: 4.0,
                    ..Edges::default()
                },
                padding: Edges {
                    top: 16.0,
                    bottom: 30.0,
                    ..Edges::default()
                },
                ..Style::default()
            },
            ChildOf(root),
            DirtyLayout,
        ))
        .id();

    app.tick();
    let t = *app.world.get::<Transform>(tile).unwrap();
    assert_eq!(t.size.y, 80.0, "explicit height wins");
    assert!(
        t.size.x > 0.0,
        "width:100% + margin must not collapse to zero (got {:?})",
        t.size
    );
    assert_eq!(
        t.absolute,
        glam::Vec2::new(4.0, 8.0),
        "margin offsets origin"
    );
}
