//! End-to-end check of the native paint seam, with no GPU in the picture.
//!
//! A small plugin stands in for a real drawing extension: it owns a main-world component, extracts
//! an `ExtractedNative` leaf from it, registers a painter, and raises the frame-dirty flag when its
//! own state changes. The tests follow that leaf through the tick - into the retained tree, across
//! frames, out of the tree when an ancestor hides it, and not into the tree at all on a clean tick.

use lumen_core::prelude::*;
use lumen_core::render_world::RenderEntityMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The plugin's main-world state: a series a real extension would draw as a chart.
#[derive(Component)]
struct Sparkline {
    samples: Vec<f32>,
    revision: u64,
}

impl Sparkline {
    fn new(samples: Vec<f32>) -> Self {
        Self {
            samples,
            revision: next_revision(),
        }
    }

    fn push(&mut self, sample: f32) {
        self.samples.push(sample);
        self.revision = next_revision();
    }
}

/// What the extract fn hands the painter. The `extension_id` is the promise that the payload is
/// this type.
struct SparklinePayload {
    samples: Vec<f32>,
    opacity: f32,
}

const SPARKLINE: &str = "test.sparkline";

/// Counts the samples every paint call was handed, so a test can tell "the backend called us with
/// our payload" from "the leaf was in the tree".
static PAINTED_SAMPLES: AtomicUsize = AtomicUsize::new(0);

struct SparklinePainter;

impl NativePainter for SparklinePainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        if let Some(payload) = ctx.payload_as::<SparklinePayload>() {
            PAINTED_SAMPLES.fetch_add(payload.samples.len(), Ordering::Relaxed);
        }
    }
}

struct SparklinePlugin;

impl Plugin for SparklinePlugin {
    fn name(&self) -> &'static str {
        "SparklinePlugin"
    }

    fn build(self, app: &mut App) {
        app.add_extract_fn(extract_sparklines);
        app.register_native_painter(SPARKLINE, SparklinePainter);
        app.add_systems(TickStage::Systems, redraw_when_samples_change);
    }
}

/// The seam does not observe plugin state, so the plugin says when its pixels changed.
fn redraw_when_samples_change(
    changed: Query<(), Changed<Sparkline>>,
    mut frame_dirty: ResMut<FrameDirty>,
) {
    if !changed.is_empty() {
        frame_dirty.dirty = true;
    }
}

/// Places every leaf through the shared extract context, so scroll offsets, inherited opacity, and
/// hidden subtrees are handled the same way the built-in extractors handle them.
fn extract_sparklines(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Sparkline, Option<&Opacity>)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, sparkline, opacity)| {
            let placed = place.place(e, transform, opacity)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: SPARKLINE.into(),
                    payload: Arc::new(SparklinePayload {
                        samples: sparkline.samples.clone(),
                        opacity: placed.opacity,
                    }),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: sparkline.revision,
                    clip_to_bounds: true,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, SPARKLINE, leaves);
}

fn app_with_one_sparkline() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugin(SparklinePlugin);
    let entity = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::new(10.0, 20.0),
                size: glam::Vec2::new(120.0, 40.0),
                baseline_y: None,
            },
            Sparkline::new(vec![1.0, 2.0, 3.0]),
        ))
        .id();
    (app, entity)
}

/// Every native leaf in the retained tree, in paint order.
fn native_leaves(app: &App) -> Vec<(String, u64)> {
    native_nodes(app)
        .iter()
        .map(|node| match node.as_ref() {
            Node::Native {
                extension_id,
                revision,
                ..
            } => (extension_id.to_string(), *revision),
            other => panic!("expected Native, got {other:?}"),
        })
        .collect()
}

fn native_nodes(app: &App) -> Vec<Arc<Node>> {
    let mut found = Vec::new();
    if let Some(root) = app.render_world.resource::<RetainedScene>().root.as_ref() {
        collect(root, &mut found);
    }
    found
}

fn collect(node: &Arc<Node>, out: &mut Vec<Arc<Node>>) {
    match node.as_ref() {
        Node::Container { children } => {
            for child in children {
                collect(child, out);
            }
        }
        Node::Transform { child, .. } | Node::Opacity { child, .. } | Node::Clip { child, .. } => {
            collect(child, out)
        }
        Node::Native { .. } => out.push(node.clone()),
        _ => {}
    }
}

fn clear_dirty(app: &mut App) {
    app.world.resource_mut::<FrameDirty>().dirty = false;
}

/// The whole point of the seam: a plugin contributes a leaf, and what a backend finds in the tree
/// is enough to reach that plugin's painter with that leaf's own payload. This is the lookup a
/// backend walker performs, done here without one.
#[test]
fn a_leaf_in_the_tree_routes_to_its_own_painter() {
    let (mut app, _) = app_with_one_sparkline();
    app.tick();

    let leaves = native_nodes(&app);
    assert_eq!(leaves.len(), 1, "one leaf in the tree");
    let Node::Native {
        extension_id,
        payload,
        bounds,
        ..
    } = leaves[0].as_ref()
    else {
        panic!("expected a native leaf");
    };
    assert_eq!(bounds.origin, glam::Vec2::new(10.0, 20.0));

    let painters = app.render_world.resource::<NativePainters>().clone();
    let painter = painters
        .get(extension_id)
        .expect("the leaf's id resolves to the plugin's painter")
        .clone();
    let before = PAINTED_SAMPLES.load(Ordering::Relaxed);
    let mut target = ();
    painter.paint(&mut NativePaintCtx::new(
        payload.as_ref(),
        &mut target,
        "test.backend",
        *bounds,
        lumen_core::node_ir::Affine2::IDENTITY,
        1.0,
        1.0,
    ));

    assert_eq!(
        PAINTED_SAMPLES.load(Ordering::Relaxed),
        before + 3,
        "the painter was handed the three samples the plugin extracted",
    );
}

/// The leaf lives on the same per-frame lifecycle as every other extracted drawable: re-extracting
/// it updates the entity it already has instead of stacking a second copy.
#[test]
fn re_extracting_updates_the_leaf_instead_of_duplicating_it() {
    let (mut app, entity) = app_with_one_sparkline();
    app.tick();
    let first = native_leaves(&app);
    assert_eq!(first.len(), 1);

    for sample in [4.0, 5.0, 6.0] {
        clear_dirty(&mut app);
        app.world
            .entity_mut(entity)
            .get_mut::<Sparkline>()
            .expect("sparkline")
            .push(sample);
        app.tick();
    }

    let later = native_leaves(&app);
    assert_eq!(later.len(), 1, "one leaf per frame, not one per tick");
    assert_ne!(later[0].1, first[0].1, "the content stamp moved");
    assert_eq!(
        app.render_world.resource::<RenderEntityMap>().native.len(),
        1,
    );
}

/// Two plugins painting in the same frame keep their own leaves. The lifecycle is keyed by
/// extension, so one plugin's extract fn retiring its leaves cannot evict the other's - which is
/// what a single shared map would have done, every frame, in whichever order they ran.
#[test]
fn two_extensions_extracting_in_one_frame_do_not_evict_each_other() {
    const OTHER: &str = "test.gauge";

    fn extract_gauges(main: &mut World, render: &mut World) {
        let mut place = NativeExtract::new(main);
        let mut q = main.query::<(Entity, &Transform, &Gauge)>();
        let leaves: Vec<(Entity, ExtractedNative)> = q
            .iter(main)
            .filter_map(|(e, transform, _)| {
                let placed = place.place(e, transform, None)?;
                Some((
                    e,
                    ExtractedNative {
                        extension_id: OTHER.into(),
                        payload: Arc::new(()),
                        bounds: placed.bounds,
                        order: placed.order,
                        revision: 1,
                        clip_to_bounds: false,
                    },
                ))
            })
            .collect();
        upsert_native_leaves(render, OTHER, leaves);
    }

    #[derive(Component)]
    struct Gauge;

    let (mut app, _) = app_with_one_sparkline();
    app.add_extract_fn(extract_gauges);
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(50.0, 60.0),
            size: glam::Vec2::new(20.0, 20.0),
            baseline_y: None,
        },
        Gauge,
    ));

    for _ in 0..3 {
        app.world.resource_mut::<FrameDirty>().dirty = true;
        app.tick();

        let ids: Vec<String> = native_leaves(&app).into_iter().map(|(id, _)| id).collect();
        assert!(
            ids.contains(&SPARKLINE.to_string()) && ids.contains(&OTHER.to_string()),
            "both extensions should be in the tree, got {ids:?}",
        );
        assert_eq!(
            app.render_world.resource::<RenderEntityMap>().native.len(),
            2,
            "one render entity per extension per entity",
        );
    }
}

/// A leaf inside a scroll container moves with the content. Placing it from the entity's absolute
/// position alone would pin it while everything around it scrolled.
#[test]
fn a_leaf_inside_a_scroll_container_moves_with_the_content() {
    let mut app = App::new();
    app.add_plugin(SparklinePlugin);
    let scroller = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(200.0, 200.0),
                baseline_y: None,
            },
            ScrollOffset(glam::Vec2::new(0.0, 30.0)),
        ))
        .id();
    app.world.spawn((
        ChildOf(scroller),
        Transform {
            absolute: glam::Vec2::new(10.0, 100.0),
            size: glam::Vec2::new(120.0, 40.0),
            baseline_y: None,
        },
        Sparkline::new(vec![1.0]),
    ));
    app.tick();

    let bounds = match native_nodes(&app)[0].as_ref() {
        Node::Native { bounds, .. } => *bounds,
        other => panic!("expected Native, got {other:?}"),
    };
    assert_eq!(
        bounds.origin,
        glam::Vec2::new(10.0, 70.0),
        "the ancestor's scroll offset should have moved the leaf",
    );
}

/// Inherited opacity reaches the plugin as a number it folds into its own drawing, the same way the
/// built-in extractors fold it into their colours.
#[test]
fn inherited_opacity_reaches_the_payload() {
    let mut app = App::new();
    app.add_plugin(SparklinePlugin);
    let parent = app.world.spawn(Opacity(0.5)).id();
    app.world.spawn((
        ChildOf(parent),
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::new(10.0, 10.0),
            baseline_y: None,
        },
        Opacity(0.5),
        Sparkline::new(vec![1.0]),
    ));
    app.tick();

    let payload = match native_nodes(&app)[0].as_ref() {
        Node::Native { payload, .. } => payload.clone(),
        other => panic!("expected Native, got {other:?}"),
    };
    let payload = payload
        .downcast_ref::<SparklinePayload>()
        .expect("the plugin's own payload type");
    assert!(
        (payload.opacity - 0.25).abs() < 1e-6,
        "own opacity times the ancestor's, got {}",
        payload.opacity,
    );
}

/// A painter registered while the app is running has to repaint the leaves already on screen.
/// Nothing about the scene changed when it arrived, so the frame diff cannot notice it.
#[test]
fn registering_a_painter_mid_run_forces_the_next_frame() {
    let mut app = App::new();
    app.add_extract_fn(extract_sparklines);
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::new(10.0, 10.0),
            baseline_y: None,
        },
        Sparkline::new(vec![1.0]),
    ));
    app.tick();
    clear_dirty(&mut app);
    app.render_world
        .resource_mut::<lumen_core::node_ir::PreviousScene>()
        .root = app.render_world.resource::<RetainedScene>().root.clone();

    app.register_native_painter(SPARKLINE, SparklinePainter);

    assert!(
        app.world.resource::<FrameDirty>().dirty,
        "registration marks the frame dirty",
    );
    assert!(
        app.render_world
            .resource::<lumen_core::node_ir::PreviousScene>()
            .root
            .is_none(),
        "and drops the last painted tree so the damage gate cannot skip the frame",
    );
}

/// A hidden subtree paints nothing, and that guarantee covers plugin leaves too - including one
/// whose extract fn never checked.
#[test]
fn hiding_an_ancestor_takes_the_leaf_out_of_the_tree() {
    let mut app = App::new();
    app.add_plugin(SparklinePlugin);
    let parent = app.world.spawn(Visible(true)).id();
    app.world.spawn((
        ChildOf(parent),
        Transform {
            absolute: glam::Vec2::new(10.0, 20.0),
            size: glam::Vec2::new(120.0, 40.0),
            baseline_y: None,
        },
        Sparkline::new(vec![1.0, 2.0]),
    ));
    app.tick();
    assert_eq!(native_leaves(&app).len(), 1);

    app.world.entity_mut(parent).insert(Visible(false));
    app.world.resource_mut::<FrameDirty>().dirty = true;
    app.tick();

    assert!(
        native_leaves(&app).is_empty(),
        "a hidden ancestor suppresses the leaf",
    );
    assert!(
        app.render_world
            .resource::<RenderEntityMap>()
            .native
            .is_empty(),
        "and drops its render-world entity",
    );
}

/// A leaf that sits entirely off screen is dropped before it reaches the tree, like any other
/// drawable. Keeping it would call its painter every frame and report damage nobody can see.
#[test]
fn a_leaf_entirely_off_screen_is_culled() {
    let mut app = App::new();
    app.add_plugin(SparklinePlugin);
    let viewport = app.render_world.resource::<Viewport>().size;
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(viewport.x + 50.0, 10.0),
            size: glam::Vec2::new(120.0, 40.0),
            baseline_y: None,
        },
        Sparkline::new(vec![1.0]),
    ));
    app.tick();

    assert!(
        native_leaves(&app).is_empty(),
        "a leaf past the right edge should not reach the tree",
    );
}

/// A tick that changes nothing skips extract entirely, so a plugin that wants a repaint has to say
/// so. This is why the seam has no dirty mechanism of its own.
#[test]
fn a_clean_tick_skips_the_leaf_and_the_plugins_own_flag_brings_it_back() {
    let (mut app, entity) = app_with_one_sparkline();
    app.tick();
    let first = native_leaves(&app);
    assert_eq!(first.len(), 1);

    clear_dirty(&mut app);
    app.tick();
    assert!(
        !app.world.resource::<FrameDirty>().dirty,
        "nothing render-relevant changed, so the tick stays clean",
    );
    assert_eq!(
        native_leaves(&app),
        first,
        "a clean tick leaves the previous frame's tree in place",
    );

    clear_dirty(&mut app);
    app.world
        .entity_mut(entity)
        .get_mut::<Sparkline>()
        .expect("sparkline")
        .push(9.0);
    app.tick();

    let later = native_leaves(&app);
    assert_eq!(later.len(), 1);
    assert_ne!(
        later[0].1, first[0].1,
        "the plugin's flag drove a re-extract"
    );
}
