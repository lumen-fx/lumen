//! End-to-end partial-repaint measurement on the real render pipeline.
//!
//! Drives the full main-world -> extract -> `transform_extracted_to_nodes`
//! (Node IR, fresh `Arc`s every frame) -> offscreen `wgpu_render_system` path
//! and asserts that the damage-driven gate skips the GPU encode+submit when the
//! visual tree is unchanged, while still repainting a localized change.
//!
//! Model mirrored: Qt `QWidget::update()` - a queued repaint whose accumulated
//! dirty region is empty performs no backing-store flush; a small dirty region
//! repaints without touching the rest.
//!
//! Skips itself when no wgpu adapter is available (headless CI without a GPU).

use lumen_core::prelude::*;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};

const W: u32 = 400;
const H: u32 = 300;
const GRID: i32 = 20; // 20x10 = 200 widgets

fn spawn_grid(app: &mut App) -> Vec<bevy_ecs::entity::Entity> {
    let mut ids = Vec::new();
    for row in 0..(GRID / 2) {
        for col in 0..GRID {
            let e = app
                .world
                .spawn((
                    Transform {
                        absolute: glam::Vec2::new(col as f32 * 20.0 + 1.0, row as f32 * 28.0 + 1.0),
                        size: glam::Vec2::new(18.0, 26.0),
                        baseline_y: None,
                    },
                    Visuals {
                        fill: Some(Fill::Solid(Color::rgb(0.2, 0.4, 0.6))),
                        ..Default::default()
                    },
                ))
                .id();
            ids.push(e);
        }
    }
    ids
}

#[test]
fn empty_damage_skips_encode_localized_change_repaints() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(0.05, 0.05, 0.07);
    }

    let ids = spawn_grid(&mut app);
    let widget_count = ids.len();

    // Frame 1 - first paint. `PreviousScene` is empty, so the whole scene
    // renders.
    app.tick();
    let after_first = render_count(&app);
    assert_eq!(after_first, 1, "first frame must render");

    // Frame 2 - a false-positive dirty flag with NO visual change. The IR is
    // rebuilt (fresh Arcs for all {widget_count} widgets) but is content-
    // identical, so the diff yields an empty region and the GPU encode is
    // skipped entirely.
    force_dirty(&mut app);
    app.tick();
    let after_noop = render_count(&app);
    assert_eq!(
        after_noop, after_first,
        "unchanged frame must be skipped (no encode/present), \
         but render_count advanced {after_first} -> {after_noop}"
    );

    // Frame 3 - flip a SINGLE widget's color among the {widget_count}. The diff
    // finds one changed leaf, so the frame repaints.
    if let Some(mut v) = app.world.get_mut::<Visuals>(ids[widget_count / 2]) {
        v.fill = Some(Fill::Solid(Color::rgb(0.9, 0.1, 0.1)));
    }
    app.tick();
    let after_change = render_count(&app);
    assert_eq!(
        after_change,
        after_noop + 1,
        "a localized change must trigger exactly one repaint"
    );

    // Pixel check: the repaint is correct - the changed widget (#100: row 5,
    // col 0 -> rect at x in [1,19], y in [141,167]) is now red, while its right
    // neighbour (#101, x in [21,39]) is still blue.
    let frame_after_change = readback(&app);
    let px = |img: &[u8], x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (img[i], img[i + 1], img[i + 2])
    };
    let (cr, cg, cb) = px(&frame_after_change, 9, 154);
    assert!(
        cr > 180 && cg < 80 && cb < 80,
        "changed widget should be red, got ({cr},{cg},{cb})"
    );
    let (nr, ng, nb) = px(&frame_after_change, 30, 154);
    assert!(
        nb > nr && nb > 60,
        "neighbour widget should still be blue, got ({nr},{ng},{nb})"
    );

    // Frame 4 - settle again with no change: skipped once more, and the
    // retained target must be byte-identical to the last painted frame (the
    // skip preserves pixels exactly - no partial-repaint artefact).
    force_dirty(&mut app);
    app.tick();
    let after_settle = render_count(&app);
    assert_eq!(
        after_settle, after_change,
        "second unchanged frame must be skipped too"
    );
    let frame_after_skip = readback(&app);
    assert!(
        frame_after_change == frame_after_skip,
        "a skipped frame must leave the framebuffer pixel-identical"
    );

    eprintln!(
        "partial-repaint measurement: {widget_count} widgets - \
         GPU encode/present passes across 4 ticks \
         (paint, no-op, 1-widget change, no-op) = [1, {after_noop}, {after_change}, {after_settle}]; \
         2 of 3 post-initial dirty frames did ZERO GPU work"
    );
}

fn render_count(app: &App) -> u64 {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("offscreen renderer present")
        .render_count()
}

fn readback(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("offscreen renderer present")
        .read_rgba8()
        .expect("framebuffer readback")
}

/// Raise `FrameDirty` without changing any visual, modelling a false-positive
/// dirty flag (a signal re-set to the same value, a no-op hover restyle).
fn force_dirty(app: &mut App) {
    if let Some(mut fd) = app
        .world
        .get_resource_mut::<lumen_core::render_world::FrameDirty>()
    {
        fd.dirty = true;
    }
}
