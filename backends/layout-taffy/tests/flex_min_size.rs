//! Flex shrinking and the automatic minimum size (CSS flexbox section 4.5).
//!
//! An item on an overflowing line shrinks until it reaches its automatic
//! minimum size, which is the smaller of its content's min-content size and
//! its own authored size. An explicit `min-width: 0` / `min-height: 0`
//! removes that floor, and `flex-shrink: 0` opts the item out of shrinking
//! altogether.

use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};

fn app_with_viewport(w: f32, h: f32) -> App {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    {
        let mut layout = app.world.get_non_send_mut::<LayoutResource>().unwrap();
        layout.set_viewport(w, h);
    }
    app.world.insert_resource(Viewport {
        size: glam::Vec2::new(w, h),
        ..Viewport::default()
    });
    app
}

/// Spawn `container` with `items` under it, tick once, and return the
/// items' laid-out sizes.
fn solve(container: Style, items: Vec<(Style, Option<Style>)>) -> Vec<glam::Vec2> {
    let mut app = app_with_viewport(1000.0, 1000.0);
    let root = app.world.spawn((container, DirtyLayout)).id();
    let ids: Vec<Entity> = items
        .into_iter()
        .map(|(item, child)| {
            let id = app.world.spawn((item, ChildOf(root), DirtyLayout)).id();
            if let Some(c) = child {
                app.world.spawn((c, ChildOf(id), DirtyLayout));
            }
            id
        })
        .collect();
    app.tick();
    ids.into_iter()
        .map(|e| app.world.get::<Transform>(e).expect("laid out").size)
        .collect()
}

fn row(w: f32, h: f32) -> Style {
    Style {
        width: Length::Px(w),
        height: Length::Px(h),
        flex_direction: FlexDirection::Row,
        ..Style::default()
    }
}

fn column(w: f32, h: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        ..row(w, h)
    }
}

fn box_of(w: f32, h: f32) -> Style {
    Style {
        width: Length::Px(w),
        height: Length::Px(h),
        ..Style::default()
    }
}

/// Two 300px items on a 400px row have nothing inside them, so their
/// automatic minimum size is zero and both shrink to 200. The authored
/// width is a preferred size, not a floor.
#[test]
fn fixed_width_items_shrink_on_a_crowded_row() {
    let sizes = solve(
        row(400.0, 100.0),
        vec![(box_of(300.0, 40.0), None), (box_of(300.0, 40.0), None)],
    );
    assert_eq!(sizes[0].x, 200.0);
    assert_eq!(sizes[1].x, 200.0);
}

/// Same on the block axis: two 300px-tall items in a 400px-tall column
/// shrink to 200 each.
#[test]
fn fixed_height_items_shrink_in_a_crowded_column() {
    let sizes = solve(
        column(100.0, 400.0),
        vec![(box_of(40.0, 300.0), None), (box_of(40.0, 300.0), None)],
    );
    assert_eq!(sizes[0].y, 200.0);
    assert_eq!(sizes[1].y, 200.0);
}

/// The floor is the item's content, not its authored size: the first item
/// holds a 250px child, so it stops shrinking at 250 and the second item
/// absorbs the rest of the overflow.
#[test]
fn shrinking_stops_at_the_content_size() {
    let sizes = solve(
        row(400.0, 100.0),
        vec![
            (box_of(300.0, 40.0), Some(box_of(250.0, 20.0))),
            (box_of(300.0, 40.0), None),
        ],
    );
    assert_eq!(sizes[0].x, 250.0, "content floors the first item at 250");
    assert_eq!(sizes[1].x, 150.0, "the empty item takes the rest");
}

/// `min-width: 0` removes the content floor, so the item shrinks past its
/// own content and overflows it instead.
#[test]
fn min_width_zero_shrinks_past_the_content_size() {
    let sizes = solve(
        row(400.0, 100.0),
        vec![
            (
                Style {
                    min_width: Length::Px(0.0),
                    ..box_of(300.0, 40.0)
                },
                Some(box_of(250.0, 20.0)),
            ),
            (box_of(300.0, 40.0), None),
        ],
    );
    assert_eq!(
        sizes[0].x, 200.0,
        "min-width: 0 overrides the content floor"
    );
    assert_eq!(sizes[1].x, 200.0);
}

/// The block-axis half of the same override.
#[test]
fn min_height_zero_shrinks_past_the_content_size() {
    let sizes = solve(
        column(100.0, 400.0),
        vec![
            (
                Style {
                    min_height: Length::Px(0.0),
                    ..box_of(40.0, 300.0)
                },
                Some(box_of(20.0, 250.0)),
            ),
            (box_of(40.0, 300.0), None),
        ],
    );
    assert_eq!(sizes[0].y, 200.0);
    assert_eq!(sizes[1].y, 200.0);
}

/// `flex-shrink: 0` keeps the authored size and overflows the line. This is
/// also the condition an automatic [`RelayoutBoundary`] rests on: nothing
/// inside the item can move its own outer box.
#[test]
fn flex_shrink_zero_keeps_the_authored_size() {
    let rigid = Style {
        shrink: 0.0,
        ..box_of(300.0, 40.0)
    };
    let sizes = solve(
        row(400.0, 100.0),
        vec![(rigid.clone(), None), (rigid, None)],
    );
    assert_eq!(sizes[0].x, 300.0);
    assert_eq!(sizes[1].x, 300.0);
}

/// A long child never widens an item past its authored width: the automatic
/// minimum size is capped by the item's own specified size.
#[test]
fn a_long_child_does_not_widen_a_fixed_item() {
    let sizes = solve(
        row(1000.0, 100.0),
        vec![(box_of(200.0, 40.0), Some(box_of(600.0, 20.0)))],
    );
    assert_eq!(sizes[0].x, 200.0);
}
