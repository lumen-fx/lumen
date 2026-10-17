//! Many large pixel images in one frame, on the GPU.
//!
//! Every image a frame draws passes through vello's image atlas, which holds
//! an 8192 pixel square: four 4096 x 4096 images fill it. A frame that draws
//! more than fits renders in passes, each holding what fits, so every image
//! still reaches the frame. These tests draw more than one atlas holds and
//! read the frame back to see that all of them did.
//!
//! Skips itself when the machine has no GPU: either no wgpu adapter at all, or
//! only a software rasterizer.

use lumen_core::prelude::*;
use lumen_paint::kurbo::{Affine, Rect as KurboRect};
use lumen_paint::peniko::{
    BlendMode, Compose, Fill as PaintFill, ImageBrush, ImageData, ImageQuality, ImageSampler, Mix,
};
use lumen_paint::{PaintTarget, PixelImage, Shape};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use std::sync::Arc;

const EXTENSION: &str = "test.pixel-images";

/// How the leaf draws its images.
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// Each image in a cell of its own.
    Grid,
    /// Every image over the whole leaf, each but the first at half alpha
    /// under a layer, inside a clip: the stack of a paint program's layers.
    Stack,
    /// One image whose bytes, and so whose identity in vello's atlas, stay
    /// the same from frame to frame, as a loaded image file's do.
    Fixed,
}

/// The image [`Layout::Fixed`] draws.
static FIXED: std::sync::OnceLock<ImageData> = std::sync::OnceLock::new();

#[derive(Component, Clone)]
struct Images {
    images: Arc<Vec<PixelImage>>,
    layout: Layout,
    revision: u64,
}

struct Payload {
    images: Arc<Vec<PixelImage>>,
    layout: Layout,
}

struct ImagesPainter;

impl NativePainter for ImagesPainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(payload) = ctx.payload_as::<Payload>() else {
            return;
        };
        let bounds = ctx.bounds;
        let device = Affine::new(ctx.device_transform().coeffs);
        let images = payload.images.clone();
        let layout = payload.layout;
        let Some(target) = ctx.target_as::<PaintTarget>() else {
            return;
        };
        let nearest = ImageSampler::default().with_quality(ImageQuality::Low);
        let (x0, y0) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
        let (w, h) = (f64::from(bounds.size.x), f64::from(bounds.size.y));
        match layout {
            Layout::Fixed => {
                let image = FIXED
                    .get_or_init(|| crate::images(1, 16)[0].to_image_data())
                    .clone();
                target.draw_image(
                    &ImageBrush {
                        image,
                        sampler: nearest,
                    },
                    device * Affine::scale(4.0),
                );
            }
            Layout::Grid => {
                let cols = (images.len() as f64).sqrt().ceil() as usize;
                let cell = w / cols as f64;
                for (i, image) in images.iter().enumerate() {
                    let (col, row) = ((i % cols) as f64, (i / cols) as f64);
                    let scale = cell / f64::from(image.width());
                    let transform = device
                        * Affine::translate((x0 + col * cell, y0 + row * cell))
                        * Affine::scale(scale);
                    target.draw_pixels(image, nearest, transform);
                }
            }
            Layout::Stack => {
                let leaf = Shape::Rect(KurboRect::new(x0, y0, x0 + w, y0 + h));
                target.push_layer(PaintFill::NonZero, BlendMode::default(), 1.0, device, &leaf);
                for (i, image) in images.iter().enumerate() {
                    let transform = device
                        * Affine::translate((x0, y0))
                        * Affine::scale(w / f64::from(image.width()));
                    if i == 0 {
                        target.draw_pixels(image, nearest, transform);
                        continue;
                    }
                    target.push_layer(
                        PaintFill::NonZero,
                        BlendMode::new(Mix::Normal, Compose::SrcOver),
                        0.5,
                        device,
                        &leaf,
                    );
                    target.draw_pixels(image, nearest, transform);
                    target.pop_layer();
                }
                target.pop_layer();
            }
        }
    }
}

fn extract_images(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Images)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, images)| {
            let placed = place.place(e, transform)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: EXTENSION.into(),
                    payload: Arc::new(Payload {
                        images: images.images.clone(),
                        layout: images.layout,
                    }),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: images.revision,
                    clip_to_bounds: false,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, EXTENSION, leaves);
}

/// A distinct, fully opaque colour per image.
fn color_of(i: usize) -> [u8; 4] {
    let i = i as u32 + 1;
    [
        (i * 37 % 200 + 40) as u8,
        (i * 91 % 200 + 40) as u8,
        (i * 53 % 200 + 40) as u8,
        255,
    ]
}

/// `count` images of `side` x `side`, each filled with its own colour.
fn images(count: usize, side: u32) -> Arc<Vec<PixelImage>> {
    Arc::new(
        (0..count)
            .map(|i| {
                let mut image = PixelImage::new(side, side);
                image.fill_rgba8(0, 0, side, side, color_of(i));
                image
            })
            .collect(),
    )
}

/// Render `images` once into a `frame` x `frame` target and read it back.
fn render(images: Arc<Vec<PixelImage>>, layout: Layout, frame: u32) -> Vec<u8> {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(frame, frame));
    app.add_extract_fn(extract_images);
    app.register_native_painter(EXTENSION, ImagesPainter);
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::splat(frame as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::splat(frame as f32),
            baseline_y: None,
        },
        Images {
            images,
            layout,
            revision: 1,
        },
    ));
    app.tick();
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8()
        .expect("readback")
}

fn pixel(pixels: &[u8], frame: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * frame + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// The images whose cell does not show their colour.
fn missing(pixels: &[u8], frame: u32, count: usize) -> Vec<usize> {
    let cols = (count as f64).sqrt().ceil() as u32;
    let cell = frame / cols;
    (0..count)
        .filter(|&i| {
            let (col, row) = (i as u32 % cols, i as u32 / cols);
            let got = pixel(pixels, frame, col * cell + cell / 2, row * cell + cell / 2);
            let want = color_of(i);
            (0..3).any(|c| got[c].abs_diff(want[c]) > 2)
        })
        .collect()
}

/// Whether two frames agree at their centres, to within rounding.
fn assert_same_centre(got: &[u8], want: &[u8], frame: u32) {
    let (got, want) = (
        pixel(got, frame, frame / 2, frame / 2),
        pixel(want, frame, frame / 2, frame / 2),
    );
    assert!(
        (0..3).all(|c| got[c].abs_diff(want[c]) <= 2),
        "the frame composites to {got:?} where the reference does {want:?}"
    );
}

/// An image drawn on a frame, left off the next, and drawn again stays drawn.
/// The frame without it samples nothing at all, and a vello render of such a
/// scene empties the atlas while still counting the image as placed in it.
#[test]
fn an_image_drawn_again_after_a_frame_that_sampled_nothing_draws() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let frame = 64;
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(frame, frame));
    app.add_extract_fn(extract_images);
    app.register_native_painter(EXTENSION, ImagesPainter);
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::splat(frame as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    let leaf = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::splat(frame as f32),
                baseline_y: None,
            },
            Images {
                images: Arc::new(Vec::new()),
                layout: Layout::Fixed,
                revision: 1,
            },
        ))
        .id();
    let centre = |app: &App| {
        let pixels = app
            .render_world
            .get_non_send::<WgpuRenderer>()
            .expect("renderer")
            .read_rgba8()
            .expect("readback");
        pixel(&pixels, frame, 32, 32)
    };
    let want = color_of(0);
    for (revision, layout, drawn) in [
        (2, Layout::Fixed, true),
        (3, Layout::Grid, false),
        (4, Layout::Fixed, true),
    ] {
        *app.world.get_mut::<Images>(leaf).expect("the leaf") = Images {
            images: Arc::new(Vec::new()),
            layout,
            revision,
        };
        app.tick();
        let got = centre(&app);
        let shows = (0..3).all(|c| got[c].abs_diff(want[c]) <= 2);
        assert_eq!(shows, drawn, "frame {revision}: the centre reads {got:?}");
    }
}

/// #521: more 4096 x 4096 images than vello's atlas holds at once all draw.
#[test]
fn more_large_images_than_one_atlas_holds_all_draw() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let count = 9;
    let pixels = render(images(count, 4096), Layout::Grid, 256);
    assert_eq!(missing(&pixels, 256, count), Vec::<usize>::new());
}

/// #521: twenty 2048 x 2048 images, a quarter more than one atlas holds.
#[test]
fn twenty_2048_images_all_draw() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let count = 20;
    let pixels = render(images(count, 2048), Layout::Grid, 256);
    assert_eq!(missing(&pixels, 256, count), Vec::<usize>::new());
}

/// #521: a stack of layers past the atlas, each but the bottom one at half
/// alpha inside a clip, composites to what the same stack of small images
/// does, which fits one atlas many times over. Drawn small, each layer goes
/// through a copy of the size it lands at.
#[test]
fn a_shrunk_layer_stack_past_the_atlas_composites_like_one_that_fits() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let count = 7;
    let large = render(images(count, 4096), Layout::Stack, 256);
    let small = render(images(count, 64), Layout::Stack, 256);
    assert_same_centre(&large, &small, 256);
}

/// #521: the same stack drawn at its own size, so no copy is smaller than
/// its image: the layers' content renders in passes inside the layers, and
/// the stack still composites like one that fits.
#[test]
fn a_full_size_layer_stack_past_the_atlas_composites_like_one_that_fits() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let count = 18;
    let large = render(images(count, 2048), Layout::Stack, 2048);
    let small = render(images(count, 64), Layout::Stack, 2048);
    assert_same_centre(&large, &small, 2048);
}
