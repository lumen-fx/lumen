//! What a plugin's painter puts on the target, end to end.
//!
//! Drives the full main-world -> extract -> Node IR -> offscreen renderer path with a small
//! painting extension installed, and reads the framebuffer back to see what it drew.
//!
//! Skips itself when the machine has no GPU: either no wgpu adapter at all, or only a software
//! rasterizer.

use lumen_core::prelude::*;
use lumen_render_wgpu::vello::peniko::BlendMode;
use lumen_render_wgpu::vello::peniko::Fill as VelloFill;
use lumen_render_wgpu::vello::peniko::color::{AlphaColor, Srgb};
use lumen_render_wgpu::vello::peniko::kurbo::{Affine, Rect as KurboRect};
use lumen_render_wgpu::{
    WalkContext, WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason, walk_node,
};
use std::sync::{Arc, Mutex};

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
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Solid)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, solid)| {
            let placed = place.place(e, transform, None)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: extension.clone(),
                    payload: Arc::new(SolidPayload {
                        color: solid.color,
                        overhang: solid.overhang,
                    }),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: solid.revision,
                    clip_to_bounds: solid.clip_to_bounds,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, &extension, leaves);
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

/// Fills the leaf's bounds with one opaque colour and records the opacity it was handed. It does
/// not apply that opacity itself, so anything the backend composited on top shows up in the pixels.
struct OpaqueFill {
    color: Color,
    seen_opacity: Arc<Mutex<Vec<f32>>>,
}

impl NativePainter for OpaqueFill {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        self.seen_opacity.lock().expect("lock").push(ctx.opacity);
        let bounds = ctx.bounds;
        let [r, g, b, a] = self.color.to_rgba8();
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
                bounds.origin.x as f64,
                bounds.origin.y as f64,
                (bounds.origin.x + bounds.size.x) as f64,
                (bounds.origin.y + bounds.size.y) as f64,
            ),
        );
    }
}

/// A painter that opens a clip layer and never closes it - the shape of an extension bug, or of a
/// painter that returned early out of the middle of its own drawing.
struct LeavesALayerOpen;

impl NativePainter for LeavesALayerOpen {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let transform = Affine::new(ctx.device_transform().coeffs);
        let Some(scene) = ctx.target_as::<lumen_render_wgpu::vello::Scene>() else {
            return;
        };
        scene.push_layer(
            VelloFill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &KurboRect::new(0.0, 0.0, (W / 2) as f64, (H / 2) as f64),
        );
    }
}

fn native_node(bounds: (f32, f32, f32, f32), clip_to_bounds: bool) -> Arc<Node> {
    Arc::new(Node::Native {
        extension_id: EXTENSION.into(),
        payload: Arc::new(()),
        bounds: Rect::new(
            glam::Vec2::new(bounds.0, bounds.1),
            glam::Vec2::new(bounds.2, bounds.3),
        ),
        revision: 1,
        clip_to_bounds,
    })
}

fn rect_node(bounds: (f32, f32, f32, f32), color: Color) -> Arc<Node> {
    Arc::new(Node::Rect {
        bounds: Rect::new(
            glam::Vec2::new(bounds.0, bounds.1),
            glam::Vec2::new(bounds.2, bounds.3),
        ),
        brush: Brush::Solid(color),
        corner: 0.0,
        corners: None,
    })
}

/// Walks a hand-built tree straight into an offscreen renderer, so a test can place a leaf under
/// nodes the tree builder does not emit yet, like opacity groups.
fn render_tree(root: &Arc<Node>, painters: &NativePainters) -> Vec<u8> {
    let mut renderer = WgpuRenderer::new_offscreen(W, H).expect("offscreen renderer");
    renderer.scene.reset();
    {
        let mut ctx = WalkContext::new_with_dpr(&mut renderer.scene, None, None, 1.0)
            .with_native_painters(painters);
        walk_node(&mut ctx, root);
    }
    renderer.render_current(Color::rgb(0.0, 0.0, 0.0));
    renderer.read_rgba8().expect("readback")
}

/// Opacity has one owner, the painter. A bounds clip composites nothing, so asking to be clipped
/// cannot change what a leaf's alpha comes out as.
#[test]
fn asking_for_a_bounds_clip_does_not_change_the_leafs_alpha() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let seen_opacity = Arc::new(Mutex::new(Vec::new()));
    let mut painters = NativePainters::default();
    painters.register(
        EXTENSION,
        OpaqueFill {
            color: Color::rgb(0.0, 1.0, 0.0),
            seen_opacity: seen_opacity.clone(),
        },
    );

    let under_half_opacity = |clip_to_bounds: bool| {
        let tree = Arc::new(Node::Opacity {
            alpha: 0.5,
            child: native_node((16.0, 16.0, 32.0, 32.0), clip_to_bounds),
        });
        render_tree(&tree, &painters)
    };

    let clipped = under_half_opacity(true);
    let unclipped = under_half_opacity(false);
    assert_eq!(
        pixel(&clipped, 32, 32),
        pixel(&unclipped, 32, 32),
        "the clip must not composite: same painter, same pixels",
    );
    assert_eq!(
        seen_opacity.lock().expect("lock").as_slice(),
        [0.5, 0.5],
        "the ancestor opacity reaches the painter, both times",
    );
}

/// A painter that leaves a layer open must not cost the rest of the frame its clips. The walker
/// closes whatever the painter left behind, and closes no more than that.
#[test]
fn a_painter_that_leaves_a_layer_open_does_not_disturb_the_rest_of_the_scene() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut painters = NativePainters::default();
    painters.register(EXTENSION, LeavesALayerOpen);

    let half = (W / 2) as f32;
    // A clip over the left half holds a rogue leaf and a green rect covering everything. After the
    // clip closes, a blue rect covers the right half.
    let tree = Arc::new(Node::Container {
        children: vec![
            Arc::new(Node::Clip {
                shape: ClipShape::Rect(Rect::new(
                    glam::Vec2::ZERO,
                    glam::Vec2::new(half, H as f32),
                )),
                child: Arc::new(Node::Container {
                    children: vec![
                        native_node((0.0, 0.0, 4.0, 4.0), false),
                        rect_node((0.0, 0.0, W as f32, H as f32), Color::rgb(0.0, 1.0, 0.0)),
                    ],
                }),
            }),
            rect_node((half, 0.0, half, H as f32), Color::rgb(0.0, 0.0, 1.0)),
        ],
    });

    let pixels = render_tree(&tree, &painters);

    let (_, g, _) = pixel(&pixels, 8, H - 8);
    assert!(
        g > 200,
        "the rect after the rogue leaf keeps the clip it was given, not the painter's",
    );
    let (_, _, b) = pixel(&pixels, W - 8, H - 8);
    assert!(
        b > 200,
        "the clip closes where the walker said it does, so later content is not clipped away",
    );
}
