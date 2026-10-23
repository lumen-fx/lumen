//! Backend textures shown beneath the frame, end to end.
//!
//! A test plugin makes a texture on the device the renderer publishes,
//! presents it under a texture id, and paints that id from a native leaf.
//! Reading the frame back shows the texture where the leaf drew it, under
//! whatever painted after it and inside the clips around it; presenting a new
//! texture puts up a frame without painting the scene again.
//!
//! Skips itself when the machine has no GPU: either no wgpu adapter at all,
//! or only a software rasterizer.

use lumen_core::prelude::*;
use lumen_paint::kurbo::{Affine, Rect as KurboRect};
use lumen_paint::peniko::color::{AlphaColor, Srgb};
use lumen_paint::peniko::{BlendMode, Fill as PaintFill, ImageSampler};
use lumen_paint::{BackendImage, PaintTarget, Shape, TextureId, TextureRefusal};
use lumen_render_wgpu::vello::wgpu;
use lumen_render_wgpu::{
    GpuContext, GpuTextures, WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const W: u32 = 64;
const H: u32 = 64;
const EXTENSION: &str = "test.backend-texture";

/// A leaf that shows a backend texture, optionally clipped to a rect first,
/// and optionally faded (which refuses it).
#[derive(Component, Clone)]
struct Shows {
    id: TextureId,
    size: (u32, u32),
    clip: Option<(f32, f32, f32, f32)>,
    fade: bool,
}

/// A leaf that fills its bounds with a colour.
#[derive(Component, Clone)]
struct Solid(Color);

enum Payload {
    Texture(Shows),
    Solid(Color),
}

/// What the texture painter's last draw answered.
static ANSWERS: Mutex<Vec<Result<(), TextureRefusal>>> = Mutex::new(Vec::new());

struct Painter;

impl NativePainter for Painter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(payload) = ctx.payload_as::<Payload>() else {
            return;
        };
        let b = ctx.bounds;
        let device = Affine::new(ctx.device_transform().coeffs);
        let Some(target) = ctx.target_as::<PaintTarget>() else {
            return;
        };
        let rect = KurboRect::new(
            f64::from(b.origin.x),
            f64::from(b.origin.y),
            f64::from(b.origin.x + b.size.x),
            f64::from(b.origin.y + b.size.y),
        );
        match payload {
            Payload::Solid(color) => {
                let [r, g, bl, a] = color.to_rgba8();
                target.fill(
                    PaintFill::NonZero,
                    device,
                    AlphaColor::<Srgb>::from_rgba8(r, g, bl, a).into(),
                    None,
                    &Shape::Rect(rect),
                );
            }
            Payload::Texture(shows) => {
                let mut opened = 0;
                if let Some((x0, y0, x1, y1)) = shows.clip {
                    target.push_layer(
                        PaintFill::NonZero,
                        BlendMode::default(),
                        1.0,
                        device,
                        &Shape::Rect(KurboRect::new(
                            f64::from(x0),
                            f64::from(y0),
                            f64::from(x1),
                            f64::from(y1),
                        )),
                    );
                    opened += 1;
                }
                if shows.fade {
                    target.push_layer(
                        PaintFill::NonZero,
                        BlendMode::default(),
                        0.5,
                        device,
                        &Shape::Rect(rect),
                    );
                    opened += 1;
                }
                let scale = Affine::scale_non_uniform(
                    rect.width() / f64::from(shows.size.0),
                    rect.height() / f64::from(shows.size.1),
                );
                let answer = target.draw_texture(
                    &BackendImage {
                        id: shows.id,
                        width: shows.size.0,
                        height: shows.size.1,
                        fallback: None,
                    },
                    ImageSampler::default(),
                    device * Affine::translate((rect.x0, rect.y0)) * scale,
                );
                ANSWERS.lock().unwrap().push(answer);
                for _ in 0..opened {
                    target.pop_layer();
                }
            }
        }
    }
}

fn extract(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut leaves = Vec::new();
    let mut shows = main.query::<(Entity, &Transform, &Shows)>();
    let found: Vec<_> = shows
        .iter(main)
        .map(|(e, t, s)| (e, *t, Payload::Texture(s.clone())))
        .collect();
    let mut solids = main.query::<(Entity, &Transform, &Solid)>();
    let found: Vec<_> = found
        .into_iter()
        .chain(
            solids
                .iter(main)
                .map(|(e, t, s)| (e, *t, Payload::Solid(s.0))),
        )
        .collect();
    for (e, t, payload) in found {
        let Some(placed) = place.place(e, &t) else {
            continue;
        };
        leaves.push((
            e,
            ExtractedNative {
                extension_id: EXTENSION.into(),
                payload: Arc::new(payload),
                bounds: placed.bounds,
                order: placed.order,
                revision: 1,
                clip_to_bounds: false,
            },
        ));
    }
    upsert_native_leaves(render, EXTENSION, leaves);
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    app.add_extract_fn(extract);
    app.register_native_painter(EXTENSION, Painter);
    let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
    vp.size = glam::Vec2::new(W as f32, H as f32);
    vp.clear = Color::rgb(0.0, 0.0, 0.0);
    app
}

fn at(x: f32, y: f32, w: f32, h: f32) -> Transform {
    Transform {
        absolute: glam::Vec2::new(x, y),
        size: glam::Vec2::new(w, h),
        baseline_y: None,
    }
}

/// A `size` texture on the published device, filled with one sRGB colour.
fn solid_texture(ctx: &GpuContext, size: (u32, u32), rgba: [u8; 4]) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test producer frame"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = rgba.repeat((size.0 * size.1) as usize);
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn read_back(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
        .expect("readback")
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 3)
}

fn counts(app: &App) -> (u64, u64) {
    let r = app
        .render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer");
    (r.render_count(), r.compose_count())
}

/// The renderer publishes its device before the first frame; a texture
/// presented on it shows where a leaf drew it, under a leaf painted after it,
/// and inside the clip the leaf opened.
#[test]
fn a_presented_texture_shows_beneath_the_frame() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = app();
    let ctx = app
        .render_world
        .get_resource::<GpuContext>()
        .cloned()
        .expect("the device is published at attach, before any frame");
    let textures = app
        .render_world
        .get_resource::<GpuTextures>()
        .cloned()
        .expect("with an empty texture table");

    let id = TextureId::new();
    textures.present(id, solid_texture(&ctx, (8, 8), [0, 200, 0, 255]), 1, None);
    // Leaves outside the element tree paint in the order of their entity
    // bits, which this world hands out downwards: the leaf spawned first
    // paints last.
    let over = app
        .world
        .spawn((at(20.0, 30.0, 10.0, 10.0), Solid(Color::rgb(1.0, 0.0, 0.0))))
        .id();
    let under = app
        .world
        .spawn((
            at(8.0, 8.0, 32.0, 32.0),
            Shows {
                id,
                size: (8, 8),
                clip: Some((8.0, 8.0, 24.0, 40.0)),
                fade: false,
            },
        ))
        .id();
    assert!(over.to_bits() > under.to_bits(), "the overlay paints after");
    app.tick();

    let pixels = read_back(&app);
    assert!(
        near(pixel(&pixels, 12, 12), [0, 200, 0]),
        "the texture shows"
    );
    assert!(near(pixel(&pixels, 30, 12), [0, 0, 0]), "outside the clip");
    assert!(near(pixel(&pixels, 4, 4), [0, 0, 0]), "the background");
    assert!(
        near(pixel(&pixels, 22, 35), [255, 0, 0]),
        "what paints after covers it: {:?}",
        pixel(&pixels, 22, 35)
    );
    assert!(ANSWERS.lock().unwrap().contains(&Ok(())));
}

/// A new texture under the same id puts up a frame without painting the
/// scene again, and the texture it replaced is released once the GPU is done
/// with it.
#[test]
fn a_new_frame_composes_without_repainting_and_releases_the_old_one() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = app();
    let ctx = app.render_world.resource::<GpuContext>().clone();
    let textures = app.render_world.resource::<GpuTextures>().clone();
    let id = TextureId::new();
    let released = Arc::new(AtomicUsize::new(0));
    let first = released.clone();
    textures.present(
        id,
        solid_texture(&ctx, (4, 4), [0, 0, 220, 255]),
        1,
        Some(Box::new(move || {
            first.fetch_add(1, Ordering::SeqCst);
        })),
    );
    app.world.spawn((
        at(0.0, 0.0, 32.0, 32.0),
        Shows {
            id,
            size: (4, 4),
            clip: None,
            fade: false,
        },
    ));
    app.tick();
    assert!(near(pixel(&read_back(&app), 10, 10), [0, 0, 220]));
    let (renders, composes) = counts(&app);

    textures.present(id, solid_texture(&ctx, (4, 4), [220, 220, 0, 255]), 2, None);
    assert!(textures.changed());
    // Nothing in the world changed: only what the renderer holds did.
    app.world.resource_mut::<FrameDirty>().present = true;
    app.tick();
    assert!(near(pixel(&read_back(&app), 10, 10), [220, 220, 0]));
    assert_eq!(
        counts(&app),
        (renders, composes + 1),
        "composed again, scene not painted again"
    );
    let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
    assert_eq!(
        released.load(Ordering::SeqCst),
        1,
        "the first frame is free"
    );

    // A tick with nothing new presents nothing.
    app.world.resource_mut::<FrameDirty>().present = true;
    app.tick();
    assert_eq!(counts(&app), (renders, composes + 1));
}

/// A texture inside a fading group is refused and not drawn: the frame shows
/// what the group painted, not the texture.
#[test]
fn a_faded_texture_is_refused() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = app();
    let ctx = app.render_world.resource::<GpuContext>().clone();
    let textures = app.render_world.resource::<GpuTextures>().clone();
    let id = TextureId::new();
    textures.present(id, solid_texture(&ctx, (4, 4), [0, 200, 0, 255]), 1, None);
    app.world.spawn((
        at(0.0, 0.0, 32.0, 32.0),
        Shows {
            id,
            size: (4, 4),
            clip: None,
            fade: true,
        },
    ));
    app.tick();
    assert!(near(pixel(&read_back(&app), 10, 10), [0, 0, 0]));
    assert!(
        ANSWERS
            .lock()
            .unwrap()
            .contains(&Err(TextureRefusal::Opacity))
    );
}
