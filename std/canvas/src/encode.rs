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

/// The peniko blobs an encode hands the renderer, kept across ticks.
///
/// vello keys its GPU upload caches off blob identity, so a freshly built
/// blob means a fresh upload. Both halves of this exist to stop that: a
/// buffer's pixels re-upload only when the buffer has been written since, and
/// a font's bytes are uploaded once for the life of the process rather than
/// once per `fill_text`.
#[derive(Default)]
pub struct BlobCache {
    /// Buffer pixels, keyed by handle and stamped with the write count that
    /// produced them.
    buffers: HashMap<u32, (u64, Blob<u8>)>,
    /// Font bytes, keyed by the shaper's own font id and the face index
    /// within the file.
    fonts: HashMap<(u64, u32), Blob<u8>>,
}

impl BlobCache {
    /// The blob for a buffer, rebuilt only when it has been written since.
    fn buffer(&mut self, handle: u32, buffer: &PixBuf) -> Blob<u8> {
        let generation = buffer.generation();
        match self.buffers.get(&handle) {
            Some((cached, blob)) if *cached == generation => blob.clone(),
            _ => {
                let blob = Blob::new(Arc::new(buffer.bytes().to_vec()));
                self.buffers.insert(handle, (generation, blob.clone()));
                blob
            }
        }
    }

    /// The blob for a shaped run's font. The shaper hands out the same
    /// `font_id` for the same face every time, which is what makes one entry
    /// serve every `fill_text` drawn in that font.
    fn font(&mut self, font_id: u64, index: u32, data: &Arc<Vec<u8>>) -> Blob<u8> {
        self.fonts
            .entry((font_id, index))
            .or_insert_with(|| Blob::new(data.clone()))
            .clone()
    }

    /// Drop the blobs of buffers that no longer exist, so a script that
    /// creates and frees buffers in a loop does not grow this forever.
    ///
    /// Unconditional: a tick that frees one buffer and creates another leaves
    /// the map the same length while its contents have moved on, so a length
    /// comparison would keep the freed buffer's pixels alive for good.
    pub fn retain(&mut self, buffers: &std::collections::BTreeMap<u32, PixBuf>) {
        self.buffers
            .retain(|handle, _| buffers.contains_key(handle));
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
        // The scene is shared with whatever the render world is still
        // holding, so the first draw after a publish copies it and every draw
        // after that writes in place. A `clear` costs nothing at all, because
        // `reset` hands over a fresh scene instead of copying one to throw
        // away - which is the shape an animation that redraws each frame
        // takes.
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
                    && draw_text(scene, gfx, blobs, shaper, text, *x, *y)
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
            // Unreachable: `Gfx::apply` answered for every state op above and
            // the two that empty the canvas were taken before it. A no-op
            // rather than a panic, because a drawing loop is the last place
            // to discover a new variant by crashing.
            _ => {}
        }
    }
    drew
}

/// Empty a surface: a fresh scene and a fresh drawing state. A resize does
/// this too, which is what writing `width` on an HTML canvas does.
///
/// A fresh `Arc` rather than resetting the one that is there: the scene is
/// shared with the render world, so resetting it in place would first copy
/// every command it accumulated, only to discard the copy. A canvas that
/// clears and redraws every frame would pay for the whole previous frame each
/// time.
fn reset(surface: &mut Surface) {
    surface.scene = Arc::new(Scene::new());
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
    blobs: &mut BlobCache,
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
        let blob = blobs.font(seg.font_id, seg.font_index, &seg.font_data);
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
        data: blobs.buffer(handle, buffer),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{FontSpec, LineCap, LineJoin};
    use crate::store::Surface;
    use lumen_text_cosmic::CosmicShaper;

    /// Replay a list of calls into a fresh surface, and hand back the surface
    /// and whether anything drew.
    ///
    /// Encoding is CPU work - a scene is a command buffer, and only
    /// presenting one needs a device - so every case here runs with no
    /// adapter.
    fn encode_ops(ops: Vec<Op>) -> (Surface, bool) {
        let mut surface = Surface::default();
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();
        let drew = encode(&mut surface, ops, &buffers, &mut blobs, None);
        (surface, drew)
    }

    /// How many paths the surface's scene holds.
    fn paths(surface: &Surface) -> u32 {
        surface.scene.encoding().n_paths
    }

    #[test]
    fn nothing_recorded_encodes_nothing() {
        let (surface, drew) = encode_ops(Vec::new());
        assert!(!drew);
        assert!(surface.scene.encoding().is_empty());
    }

    #[test]
    fn state_alone_draws_nothing() {
        // Setting a colour is not drawing with it. A tick that only changed
        // state must not tell the renderer the canvas moved.
        let (surface, drew) = encode_ops(vec![
            Op::SetFill(Rgba::new(1.0, 0.0, 0.0, 1.0)),
            Op::SetLineWidth(4.0),
            Op::Save,
            Op::Restore,
        ]);
        assert!(!drew);
        assert!(surface.scene.encoding().is_empty());
    }

    #[test]
    fn each_way_of_filling_and_stroking_reaches_the_scene() {
        for (name, ops) in [
            ("fill_rect", vec![Op::FillRect(0.0, 0.0, 10.0, 10.0)]),
            ("stroke_rect", vec![Op::StrokeRect(0.0, 0.0, 10.0, 10.0)]),
            (
                "fill",
                vec![
                    Op::MoveTo(0.0, 0.0),
                    Op::LineTo(10.0, 0.0),
                    Op::LineTo(10.0, 10.0),
                    Op::ClosePath,
                    Op::Fill,
                ],
            ),
            (
                "stroke",
                vec![Op::MoveTo(0.0, 0.0), Op::LineTo(10.0, 10.0), Op::Stroke],
            ),
        ] {
            let (surface, drew) = encode_ops(ops);
            assert!(drew, "{name} drew nothing");
            assert_eq!(paths(&surface), 1, "{name}");
        }
    }

    #[test]
    fn every_line_style_encodes() {
        // The cap and join names map onto kurbo's, and a wrong mapping is
        // invisible until someone looks at a stroke end.
        for cap in [LineCap::Butt, LineCap::Round, LineCap::Square] {
            for join in [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel] {
                let (surface, drew) = encode_ops(vec![
                    Op::SetLineCap(cap),
                    Op::SetLineJoin(join),
                    Op::SetLineWidth(2.0),
                    Op::MoveTo(0.0, 0.0),
                    Op::LineTo(10.0, 10.0),
                    Op::LineTo(20.0, 0.0),
                    Op::Stroke,
                ]);
                assert!(drew, "{cap:?}/{join:?}");
                assert_eq!(paths(&surface), 1, "{cap:?}/{join:?}");
            }
        }
    }

    #[test]
    fn clearing_hands_over_a_fresh_scene_rather_than_copying_one() {
        let mut surface = Surface::default();
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();

        encode(
            &mut surface,
            vec![Op::FillRect(0.0, 0.0, 10.0, 10.0)],
            &buffers,
            &mut blobs,
            None,
        );
        assert_eq!(paths(&surface), 1);
        let before = std::sync::Arc::as_ptr(&surface.scene);

        // What the render world holding the previous frame looks like.
        let _published = surface.scene.clone();
        let drew = encode(&mut surface, vec![Op::Clear], &buffers, &mut blobs, None);

        assert!(drew, "emptying the canvas is a change the renderer sees");
        assert!(surface.scene.encoding().is_empty());
        assert_ne!(
            std::sync::Arc::as_ptr(&surface.scene),
            before,
            "a clear swaps the scene out; copying one to discard it is the \
             cost an animation would pay every frame"
        );
    }

    #[test]
    fn a_resize_sets_the_drawing_space_and_empties_the_canvas() {
        let (surface, drew) = encode_ops(vec![
            Op::FillRect(0.0, 0.0, 10.0, 10.0),
            Op::SetGlobalAlpha(0.25),
            Op::Resize(64.0, 32.0),
        ]);
        assert!(drew);
        assert_eq!(surface.logical, (64.0, 32.0));
        assert!(surface.scene.encoding().is_empty());
        assert_eq!(surface.gfx.state.global_alpha, 1.0);
    }

    #[test]
    fn the_transform_places_what_is_drawn_after_it() {
        // A path is stored in canvas units and placed when it is filled, so
        // the transform in force at the fill is the one that counts.
        let (moved, _) = encode_ops(vec![
            Op::Translate(100.0, 100.0),
            Op::FillRect(0.0, 0.0, 10.0, 10.0),
        ]);
        let (still, _) = encode_ops(vec![Op::FillRect(0.0, 0.0, 10.0, 10.0)]);
        assert_ne!(
            moved.scene.encoding().transforms.len() + moved.scene.encoding().path_data.len(),
            0
        );
        assert_eq!(paths(&moved), paths(&still));
        assert_ne!(
            moved.scene.encoding().transforms,
            still.scene.encoding().transforms,
            "the translate reached the encoded transform"
        );
    }

    #[test]
    fn a_buffer_is_drawn_at_its_own_size_and_stretched() {
        let mut buffers = std::collections::BTreeMap::new();
        let mut pixels = PixBuf::new(2, 2);
        pixels.fill_rect(0, 0, 2, 2, 0xff0000ff);
        buffers.insert(1, pixels);
        let mut blobs = BlobCache::default();

        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            vec![
                Op::DrawBuffer {
                    buffer: 1,
                    x: 0.0,
                    y: 0.0,
                },
                Op::DrawBufferScaled {
                    buffer: 1,
                    x: 4.0,
                    y: 4.0,
                    width: 32.0,
                    height: 32.0,
                },
            ],
            &buffers,
            &mut blobs,
            None,
        );
        assert!(drew);
        assert_eq!(
            surface.scene.encoding().draw_tags.len(),
            2,
            "both draws reached the scene"
        );
    }

    #[test]
    fn a_buffer_drawn_under_a_global_alpha_goes_through_a_layer() {
        let mut buffers = std::collections::BTreeMap::new();
        buffers.insert(1, PixBuf::new(2, 2));
        let mut blobs = BlobCache::default();

        let mut surface = Surface::default();
        encode(
            &mut surface,
            vec![
                Op::SetGlobalAlpha(0.5),
                Op::DrawBuffer {
                    buffer: 1,
                    x: 0.0,
                    y: 0.0,
                },
            ],
            &buffers,
            &mut blobs,
            None,
        );
        assert!(surface.scene.encoding().n_clips > 0);
        assert_eq!(surface.scene.encoding().n_open_clips, 0);
    }

    #[test]
    fn a_buffer_that_is_not_there_draws_nothing() {
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();
        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            vec![
                Op::DrawBuffer {
                    buffer: 9,
                    x: 0.0,
                    y: 0.0,
                },
                Op::DrawBufferScaled {
                    buffer: 9,
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
            ],
            &buffers,
            &mut blobs,
            None,
        );
        assert!(!drew, "a stale handle is not a reason to repaint");
        assert!(surface.scene.encoding().is_empty());
    }

    #[test]
    fn a_buffer_with_no_pixels_draws_nothing() {
        let mut buffers = std::collections::BTreeMap::new();
        buffers.insert(1, PixBuf::new(0, 0));
        let mut blobs = BlobCache::default();
        let mut surface = Surface::default();
        encode(
            &mut surface,
            vec![Op::DrawBuffer {
                buffer: 1,
                x: 0.0,
                y: 0.0,
            }],
            &buffers,
            &mut blobs,
            None,
        );
        assert!(surface.scene.encoding().is_empty());
    }

    #[test]
    fn text_needs_a_shaper_and_uses_the_apps_own_fonts() {
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();
        let ops = || {
            vec![
                Op::SetFont(FontSpec::parse("16px").expect("a size is all it needs")),
                Op::FillText {
                    text: "hi".to_string(),
                    x: 0.0,
                    y: 12.0,
                },
            ]
        };

        // Without one - a headless app with no text stack - the drawing is
        // skipped and the rest of the canvas still encodes.
        let mut bare = Surface::default();
        let drew = encode(&mut bare, ops(), &buffers, &mut blobs, None);
        assert!(!drew);
        assert!(bare.scene.encoding().is_empty());

        // With the app's shaper, the glyphs land.
        let mut shaper = CosmicShaper::new();
        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            ops(),
            &buffers,
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
        );
        assert!(drew, "the run reached the scene");
        assert!(
            !surface.scene.encoding().resources.glyph_runs.is_empty(),
            "shaped glyphs, not an outline the canvas drew itself"
        );
    }

    #[test]
    fn text_that_shapes_to_nothing_visible_draws_nothing() {
        // A zero-width space is text the shaper accepts and has no glyph for.
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();
        let mut shaper = CosmicShaper::new();
        let mut surface = Surface::default();
        encode(
            &mut surface,
            vec![
                Op::SetFont(FontSpec::parse("16px").expect("a size")),
                Op::FillText {
                    text: "\u{200b}".to_string(),
                    x: 0.0,
                    y: 12.0,
                },
            ],
            &buffers,
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
        );
        // Whether it produced an empty run or an empty segment, nothing
        // visible reached the scene.
        assert!(surface.scene.encoding().is_empty());
    }

    #[test]
    fn text_with_nothing_in_it_draws_nothing() {
        let buffers = std::collections::BTreeMap::new();
        let mut blobs = BlobCache::default();
        let mut shaper = CosmicShaper::new();
        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            vec![Op::FillText {
                text: String::new(),
                x: 0.0,
                y: 0.0,
            }],
            &buffers,
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
        );
        assert!(!drew);
    }

    #[test]
    fn a_buffers_pixels_upload_once_until_they_change() {
        let mut blobs = BlobCache::default();
        let mut buffer = PixBuf::new(2, 2);

        let first = blobs.buffer(1, &buffer);
        let again = blobs.buffer(1, &buffer);
        assert_eq!(
            first.id(),
            again.id(),
            "an untouched buffer keeps the blob the renderer already uploaded"
        );

        buffer.set_pixel(0, 0, 0xffffffff);
        let after = blobs.buffer(1, &buffer);
        assert_ne!(
            first.id(),
            after.id(),
            "a written buffer has to upload again"
        );
    }

    #[test]
    fn a_freed_buffer_takes_its_pixels_with_it() {
        // The tick that frees one buffer and creates another leaves the cache
        // the same length while its contents have moved on, which is why the
        // sweep cannot be gated on a length comparison.
        let mut blobs = BlobCache::default();
        blobs.buffer(1, &PixBuf::new(64, 64));
        blobs.buffer(2, &PixBuf::new(64, 64));
        assert_eq!(blobs.buffers.len(), 2);

        let mut live = std::collections::BTreeMap::new();
        live.insert(2, PixBuf::new(64, 64));
        live.insert(3, PixBuf::new(64, 64));
        blobs.retain(&live);

        assert_eq!(blobs.buffers.len(), 1, "buffer 1 was freed");
        assert!(blobs.buffers.contains_key(&2));
    }
}
