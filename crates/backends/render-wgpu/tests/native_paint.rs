//! What a plugin's painter puts on the target, end to end.
//!
//! Drives the full main-world -> extract -> Node IR -> offscreen renderer path with a small
//! painting extension installed, and reads the framebuffer back to see what it drew.
//!
//! Skips itself when the machine has no GPU: either no wgpu adapter at all, or only a software
//! rasterizer.

use lumen_core::prelude::*;
use lumen_core::render_world::{RenderEntityMap, build_parent_map, paint_order_of};
use lumen_render_wgpu::vello::peniko::Fill as VelloFill;
use lumen_render_wgpu::vello::peniko::color::{AlphaColor, Srgb};
use lumen_render_wgpu::vello::peniko::kurbo::{Affine, Rect as KurboRect};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;
const EXTENSION: &str = "test.solid";

/// Main-world state: the extension paints this colour over a rect `overhang` pixels wider than the
/// bounds it declares, so a test can see whether the clip held.
#[derive(Component, Clone)]
struct Solid {
    color: Color,
    overhang: f32,
    clip_to_bounds: bool,
    revision: u64,
}

struct SolidPayload {
    color: Color,
    overhang: f32,
}

struct SolidPainter;

impl NativePainter for SolidPainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(payload) = ctx.payload_as::<SolidPayload>() else {
            return;
        };
        let bounds = ctx.bounds;
        let over = payload.overhang;
        let [r, g, b, a] = payload.color.to_rgba8();
        let color = AlphaColor::<Srgb>::from_rgba8(r, g, b, a);
        let transform = Affine::new(ctx.device_transform().coeffs);
        let Some(scene) = ctx.target_as::<lumen_render_wgpu::vello::Scene>() else {
            return;
        };
        scene.fill(
            VelloFill::NonZero,
            transform,
            color,
            None,
            &KurboRect::new(
                (bounds.origin.x - over) as f64,
                (bounds.origin.y - over) as f64,
                (bounds.origin.x + bounds.size.x + over) as f64,
                (bounds.origin.y + bounds.size.y + over) as f64,
            ),
        );
    }
}

/// Extracts one native leaf per `Solid`, under whichever extension id the test registered.
fn extract_solids(main: &mut World, render: &mut World) {
    let extension: Arc<str> = main
        .get_resource::<ExtensionId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| EXTENSION.into());
    let (parents, mut depth_cache) = build_parent_map(main);
    let mut q = main.query::<(Entity, &Transform, &Solid)>();
    let pairs: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .map(|(e, transform, solid)| {
            (
                e,
                ExtractedNative {
                    extension_id: extension.clone(),
                    payload: Arc::new(SolidPayload {
                        color: solid.color,
                        overhang: solid.overhang,
                    }),
                    bounds: Rect::new(transform.absolute, transform.size),
                    order: paint_order_of(e, &parents, &mut depth_cache),
                    revision: solid.revision,
                    clip_to_bounds: solid.clip_to_bounds,
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

/// The id the extract fn stamps on its leaves, so a test can ship a leaf no painter answers for.
#[derive(Resource)]
struct ExtensionId(Arc<str>);

fn app_with_painter(register: bool) -> App {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    app.add_extract_fn(extract_solids);
    if register {
        app.register_native_painter(EXTENSION, SolidPainter);
    }
    let mut vp = app.render_world.resource_mut::<Viewport>();
    vp.size = glam::Vec2::new(W as f32, H as f32);
    vp.clear = Color::rgb(0.0, 0.0, 0.0);
    app
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (pixels[i], pixels[i + 1], pixels[i + 2])
}

fn read_back(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8()
        .expect("readback")
}

fn spawn_solid(app: &mut App, at: (f32, f32), size: (f32, f32), solid: Solid) -> Entity {
    app.world
        .spawn((
            Transform {
                absolute: glam::Vec2::new(at.0, at.1),
                size: glam::Vec2::new(size.0, size.1),
                baseline_y: None,
            },
            solid,
        ))
        .id()
}

/// The seam's headline: a plugin registers a painter and its pixels land on the target, inside the
/// bounds it declared and nowhere else.
#[test]
fn a_registered_painter_puts_its_pixels_on_the_target() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app_with_painter(true);
    spawn_solid(
        &mut app,
        (16.0, 16.0),
        (32.0, 32.0),
        Solid {
            color: Color::rgb(0.0, 1.0, 0.0),
            overhang: 0.0,
            clip_to_bounds: true,
            revision: next_revision(),
        },
    );
    app.tick();

    let pixels = read_back(&app);
    let (r, g, b) = pixel(&pixels, 32, 32);
    assert!(
        g > 200 && r < 40 && b < 40,
        "leaf centre not green: {r},{g},{b}"
    );
    let (cr, cg, cb) = pixel(&pixels, 2, 2);
    assert!(
        cr < 40 && cg < 40 && cb < 40,
        "outside the leaf should stay clear: {cr},{cg},{cb}"
    );
}

/// A scene can carry an extension this backend does not implement. That leaf draws nothing and the
/// rest of the frame is unaffected.
#[test]
fn an_unregistered_extension_draws_nothing() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app_with_painter(true);
    app.world
        .insert_resource(ExtensionId("test.unknown".into()));
    spawn_solid(
        &mut app,
        (16.0, 16.0),
        (32.0, 32.0),
        Solid {
            color: Color::rgb(0.0, 1.0, 0.0),
            overhang: 0.0,
            clip_to_bounds: true,
            revision: next_revision(),
        },
    );
    app.tick();

    let pixels = read_back(&app);
    let (r, g, b) = pixel(&pixels, 32, 32);
    assert!(
        r < 40 && g < 40 && b < 40,
        "a leaf with no painter must leave the target alone: {r},{g},{b}"
    );
}

/// `clip_to_bounds` is what keeps an extension inside the box it was given. Without it a painter
/// that draws past its bounds spills, which is why the bounds contract is on the producer.
#[test]
fn clip_to_bounds_confines_a_painter_that_draws_too_far() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let overhanging = |clip_to_bounds: bool| {
        let mut app = app_with_painter(true);
        spawn_solid(
            &mut app,
            (24.0, 24.0),
            (16.0, 16.0),
            Solid {
                color: Color::rgb(0.0, 1.0, 0.0),
                overhang: 12.0,
                clip_to_bounds,
                revision: next_revision(),
            },
        );
        app.tick();
        read_back(&app)
    };

    // (16, 32) is outside the 24..40 bounds but inside the painter's overhanging rect.
    let clipped = pixel(&overhanging(true), 16, 32);
    assert!(
        clipped.1 < 40,
        "the clip should have cut the overhang: {clipped:?}"
    );
    let spilled = pixel(&overhanging(false), 16, 32);
    assert!(
        spilled.1 > 200,
        "without the clip the painter reaches past its bounds: {spilled:?}"
    );
}

/// A leaf that reuses its revision costs no repaint even though its payload is a new allocation
/// every frame, and bumping the revision repaints only the leaf's own region.
#[test]
fn a_reused_revision_skips_the_frame_and_a_bump_repaints_the_leaf() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app_with_painter(true);
    let entity = spawn_solid(
        &mut app,
        (16.0, 16.0),
        (32.0, 32.0),
        Solid {
            color: Color::rgb(0.0, 1.0, 0.0),
            overhang: 0.0,
            clip_to_bounds: true,
            revision: next_revision(),
        },
    );
    app.tick();
    let after_first = app
        .render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .render_count();

    // Same revision, fresh payload Arc: the frame is dirty, the pixels are not.
    for _ in 0..3 {
        app.world.resource_mut::<FrameDirty>().dirty = true;
        app.tick();
    }
    assert_eq!(
        app.render_world
            .get_non_send::<WgpuRenderer>()
            .expect("renderer")
            .render_count(),
        after_first,
        "an unchanged leaf must not drive a GPU submit",
    );

    app.world
        .entity_mut(entity)
        .get_mut::<Solid>()
        .expect("solid")
        .revision = next_revision();
    app.world.resource_mut::<FrameDirty>().dirty = true;
    app.tick();

    assert_eq!(
        app.render_world
            .get_non_send::<WgpuRenderer>()
            .expect("renderer")
            .render_count(),
        after_first + 1,
        "a new revision repaints once",
    );
    let damage = &app.render_world.resource::<FrameDamage>().0;
    assert!(!damage.is_empty());
    for r in damage {
        assert!(
            r.origin.x >= 15.0
                && r.origin.x + r.size.x <= 49.0
                && r.origin.y >= 15.0
                && r.origin.y + r.size.y <= 49.0,
            "damage {r:?} left the leaf's bounds (16,16,32,32)"
        );
    }
}

/// The leaf takes its place in document order, so an extension painted over its own styled box
/// covers that box rather than hiding under it.
#[test]
fn a_native_leaf_paints_over_the_box_that_styles_it() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app_with_painter(true);
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(16.0, 16.0),
            size: glam::Vec2::new(32.0, 32.0),
            baseline_y: None,
        },
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
            ..Default::default()
        },
        Solid {
            color: Color::rgb(0.0, 1.0, 0.0),
            overhang: 0.0,
            clip_to_bounds: true,
            revision: next_revision(),
        },
    ));
    app.tick();

    let (r, g, _) = pixel(&read_back(&app), 32, 32);
    assert!(
        g > 200 && r < 40,
        "the extension should cover its own background: {r},{g}"
    );
}
