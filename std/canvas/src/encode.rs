//! Turning recorded calls into a vello scene.
//!
//! One pass per tick, over whatever the script recorded since the last one.
//! The scene is retained: an encode appends to what is already there, so a
//! canvas that drew a background once keeps it without redrawing, and a tick
//! that recorded nothing costs nothing.
//!
//! Two things a script cannot do itself happen here. Text is shaped with the
//! app's own shaper, so canvas text uses the fonts the rest of the app does.
//! And a pixel buffer becomes an image the renderer can upload, cached by the
//! buffer's write count so a buffer drawn every frame and never edited
//! uploads once.

use std::collections::HashMap;
use std::sync::Arc;

use lumen_module::lumen_render_wgpu::vello::Scene;
use lumen_module::lumen_render_wgpu::vello::peniko;
use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::{
    Affine, Cap, Join, Rect, Stroke as KurboStroke,
};
use lumen_module::lumen_render_wgpu::vello::peniko::{Blob, Fill};
use lumen_module::lumen_text::{ShapeOptions, TextShaper};

use crate::buffer::PixBuf;
use crate::color::Rgba;
use crate::ops::{LineCap, LineJoin, Op};
use crate::store::Surface;

/// The peniko image for a buffer, kept until the buffer is written again.
///
/// vello keys its GPU upload cache off the blob's identity, so handing it a
/// freshly built blob every frame would re-upload every frame. The write
/// count is what says the pixels changed.
#[derive(Default)]
pub struct BlobCache(HashMap<u32, (u64, Blob<u8>)>);

impl BlobCache {
    /// The blob for a buffer, rebuilt only when it has been written since.
    fn blob(&mut self, handle: u32, buffer: &PixBuf) -> Blob<u8> {
        let generation = buffer.generation();
        match self.0.get(&handle) {
            Some((cached, blob)) if *cached == generation => blob.clone(),
            _ => {
                let blob = Blob::new(Arc::new(buffer.bytes().to_vec()));
                self.0.insert(handle, (generation, blob.clone()));
                blob
            }
        }
    }

    /// Drop the blobs of buffers that no longer exist, so a script that
    /// creates and frees buffers in a loop does not grow this forever.
    pub fn retain(&mut self, buffers: &std::collections::BTreeMap<u32, PixBuf>) {
        if self.0.len() > buffers.len() {
            self.0.retain(|handle, _| buffers.contains_key(handle));
        }
    }
}

/// A peniko color from the module's own.
fn peniko_color(c: Rgba) -> peniko::Color {
    peniko::Color::new([c.r, c.g, c.b, c.a])
}

/// Replay `ops` into `surface`'s scene. Returns whether anything drew, which
/// is what decides whether the canvas needs a new frame.
///
/// `shaper` is the app's; without one (a headless app with no text stack)
/// text is skipped and the rest of the drawing still lands.
pub fn encode(
    surface: &mut Surface,
    ops: Vec<Op>,
    buffers: &std::collections::BTreeMap<u32, PixBuf>,
    blobs: &mut BlobCache,
    mut shaper: Option<&mut dyn TextShaper>,
) -> bool {
    if ops.is_empty() {
        return false;
    }
    let mut drew = false;
    for op in &ops {
        // Emptying the canvas is journalled like everything else, so a fill
        // and the `clear` after it land in the order the script wrote them
        // rather than the order they reached the store.
        match op {
            Op::Clear => {
                reset(surface);
                drew = true;
                continue;
            }
            Op::Resize(width, height) => {
                surface.logical = (*width, *height);
                reset(surface);
                drew = true;
                continue;
            }
            _ => {}
        }
        if surface.gfx.apply(op) {
            continue;
        }
        let scene = Arc::make_mut(&mut surface.scene);
        let gfx = &surface.gfx;
        match op {
            Op::Fill => {
                scene.fill(
                    Fill::NonZero,
                    gfx.state.transform,
                    peniko_color(gfx.fill_brush()),
                    None,
                    &gfx.path,
                );
                drew = true;
            }
            Op::Stroke => {
                scene.stroke(
                    &stroke_style(gfx),
                    gfx.state.transform,
                    peniko_color(gfx.stroke_brush()),
                    None,
                    &gfx.path,
                );
                drew = true;
            }
            Op::FillRect(x, y, w, h) => {
                scene.fill(
                    Fill::NonZero,
                    gfx.state.transform,
                    peniko_color(gfx.fill_brush()),
                    None,
                    &Rect::new(*x, *y, x + w, y + h),
                );
                drew = true;
            }
            Op::StrokeRect(x, y, w, h) => {
                scene.stroke(
                    &stroke_style(gfx),
                    gfx.state.transform,
                    peniko_color(gfx.stroke_brush()),
                    None,
                    &Rect::new(*x, *y, x + w, y + h),
                );
                drew = true;
            }
            Op::FillText { text, x, y } => {
                if let Some(shaper) = shaper.as_deref_mut()
                    && draw_text(scene, gfx, shaper, text, *x, *y)
                {
                    drew = true;
                }
            }
            Op::DrawBuffer { buffer, x, y } => {
                if let Some(buf) = buffers.get(buffer) {
                    let size = (f64::from(buf.width()), f64::from(buf.height()));
                    draw_buffer(scene, gfx, blobs, *buffer, buf, (*x, *y), size);
                    drew = true;
                }
            }
            Op::DrawBufferScaled {
                buffer,
                x,
                y,
                width,
                height,
            } => {
                if let Some(buf) = buffers.get(buffer) {
                    draw_buffer(scene, gfx, blobs, *buffer, buf, (*x, *y), (*width, *height));
                    drew = true;
                }
            }
            // Every remaining variant is state, and `Gfx::apply` took it.
            _ => {}
        }
    }
    drew
}

/// Empty a surface: a fresh scene and a fresh drawing state. A resize does
/// this too, which is what writing `width` on an HTML canvas does.
fn reset(surface: &mut Surface) {
    Arc::make_mut(&mut surface.scene).reset();
    surface.gfx = crate::ops::Gfx::default();
}

/// The stroke style the current state describes.
fn stroke_style(gfx: &crate::ops::Gfx) -> KurboStroke {
    KurboStroke::new(gfx.state.line_width)
        .with_caps(match gfx.state.line_cap {
            LineCap::Butt => Cap::Butt,
            LineCap::Round => Cap::Round,
            LineCap::Square => Cap::Square,
        })
        .with_join(match gfx.state.line_join {
            LineJoin::Miter => Join::Miter,
            LineJoin::Round => Join::Round,
            LineJoin::Bevel => Join::Bevel,
        })
}

/// Shape and draw one run, `(x, y)` on the alphabetic baseline. Returns
/// whether any glyph landed.
fn draw_text(
    scene: &mut Scene,
    gfx: &crate::ops::Gfx,
    shaper: &mut dyn TextShaper,
    text: &str,
    x: f64,
    y: f64,
) -> bool {
    let font = &gfx.state.font;
    let opts = ShapeOptions {
        weight: font.weight,
        family: (!font.family.is_empty()).then(|| font.family.as_str().into()),
        ..Default::default()
    };
    let Some(run) = shaper.shape(text, font.size, opts) else {
        return false;
    };
    let brush = peniko_color(gfx.fill_brush());
    let mut drew = false;
    for seg in &run.segments {
        if seg.glyphs.is_empty() {
            continue;
        }
        let blob = Blob::new(seg.font_data.clone());
        let font_data = peniko::FontData::new(blob, seg.font_index);
        scene
            .draw_glyphs(&font_data)
            .font_size(font.size)
            .normalized_coords(&seg.normalized_coords)
            .brush(brush)
            .transform(gfx.state.transform * Affine::translate((x, y)))
            .draw(
                Fill::NonZero,
                seg.glyphs
                    .iter()
                    .map(|g| lumen_module::lumen_render_wgpu::vello::Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }),
            );
        drew = true;
    }
    drew
}

/// Draw a buffer into a box at `origin` of `size`.
fn draw_buffer(
    scene: &mut Scene,
    gfx: &crate::ops::Gfx,
    blobs: &mut BlobCache,
    handle: u32,
    buffer: &PixBuf,
    origin: (f64, f64),
    size: (f64, f64),
) {
    if buffer.width() == 0 || buffer.height() == 0 {
        return;
    }
    let image = peniko::ImageData {
        data: blobs.blob(handle, buffer),
        format: peniko::ImageFormat::Rgba8,
        // Straight, because that is how a buffer stores its pixels; the
        // renderer multiplies, so nothing rounds on the way in.
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: buffer.width(),
        height: buffer.height(),
    };
    let sx = size.0 / f64::from(buffer.width());
    let sy = size.1 / f64::from(buffer.height());
    let transform =
        gfx.state.transform * Affine::translate(origin) * Affine::scale_non_uniform(sx, sy);
    let alpha = gfx.state.global_alpha.clamp(0.0, 1.0);
    if alpha < 1.0 {
        scene.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            gfx.state.transform,
            &Rect::new(origin.0, origin.1, origin.0 + size.0, origin.1 + size.1),
        );
    }
    scene.draw_image(&peniko::ImageBrush::new(image), transform);
    if alpha < 1.0 {
        scene.pop_layer();
    }
}
