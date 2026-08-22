//! End-to-end check of the native paint seam, with no GPU in the picture.
//!
//! A small plugin stands in for a real drawing extension: it owns a main-world component, extracts
//! an `ExtractedNative` leaf from it, registers a painter, and raises the frame-dirty flag when its
//! own state changes. The tests follow that leaf through the tick - into the retained tree, across
//! frames, out of the tree when an ancestor hides it, and not into the tree at all on a clean tick.

use lumen_core::prelude::*;
use lumen_core::render_world::{RenderEntityMap, build_parent_map, paint_order_of};
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

/// Deliberately does not filter hidden entities: the render world's hidden sweep is what has to
/// keep this leaf out of a hidden subtree, and that is what the hidden test checks.
fn extract_sparklines(main: &mut World, render: &mut World) {
    let (parents, mut depth_cache) = build_parent_map(main);
    let mut q = main.query::<(Entity, &Transform, &Sparkline)>();
    let pairs: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .map(|(e, transform, sparkline)| {
            (
                e,
                ExtractedNative {
                    extension_id: SPARKLINE.into(),
                    payload: Arc::new(SparklinePayload {
                        samples: sparkline.samples.clone(),
                    }),
                    bounds: Rect::new(transform.absolute, transform.size),
                    order: paint_order_of(e, &parents, &mut depth_cache),
                    revision: sparkline.revision,
                    clip_to_bounds: true,
                },
            )
        })
        .collect();

    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().native);
    let mut next: std::collections::HashMap<Entity, Entity> = std::collections::HashMap::new();
    for (main_e, leaf) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(leaf);
                re
            }
            None => render.spawn(leaf).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().native = next;
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
    let mut found = Vec::new();
    if let Some(root) = app.render_world.resource::<RetainedScene>().root.as_ref() {
        collect(root, &mut found);
    }
    found
}

fn collect(node: &Arc<Node>, out: &mut Vec<(String, u64)>) {
    match node.as_ref() {
        Node::Container { children } => {
            for child in children {
                collect(child, out);
            }
        }
        Node::Transform { child, .. } | Node::Opacity { child, .. } | Node::Clip { child, .. } => {
            collect(child, out)
        }
        Node::Native {
            extension_id,
            revision,
            ..
        } => out.push((extension_id.to_string(), *revision)),
        _ => {}
    }
}

fn clear_dirty(app: &mut App) {
    app.world.resource_mut::<FrameDirty>().dirty = false;
}

/// The whole point of the seam: a plugin contributes a leaf and it arrives in the tree the renderer
/// walks, with the geometry and the registered painter the plugin gave it.
#[test]
fn a_plugin_leaf_reaches_the_retained_tree_and_its_painter() {
    let (mut app, _) = app_with_one_sparkline();
    app.tick();

    assert_eq!(native_leaves(&app).len(), 1, "one leaf in the tree");

    let painters = app.render_world.resource::<NativePainters>().clone();
    let painter = painters.get(SPARKLINE).expect("registered painter").clone();
    let before = PAINTED_SAMPLES.load(Ordering::Relaxed);
    let payload: Arc<dyn std::any::Any + Send + Sync> = Arc::new(SparklinePayload {
        samples: vec![1.0, 2.0],
    });
    let mut target = ();
    painter.paint(&mut NativePaintCtx::new(
        payload.as_ref(),
        &mut target,
        "test.backend",
        Rect::new(glam::Vec2::ZERO, glam::Vec2::new(1.0, 1.0)),
        lumen_core::node_ir::Affine2::IDENTITY,
        1.0,
        1.0,
    ));
    assert_eq!(PAINTED_SAMPLES.load(Ordering::Relaxed), before + 2);
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
