//! Turning recorded calls into a drawing the renderer replays.
//!
//! One pass per tick, over whatever the script recorded since the last one.
//! The drawing is retained: an encode appends to what is already there, so a
//! canvas that drew a background once keeps it without redrawing, and a tick
//! that recorded nothing costs nothing.
//!
//! Two things a script cannot do itself happen here. Text is shaped with the
//! app's own shaper, so canvas text uses the fonts the rest of the app does.
//! And a pixel buffer arrives as the snapshot of its tiles the draw call took:
//! the drawing keeps the pixels the buffer held at the call, and the renderer,
//! which keeps one resident copy per buffer, re-uploads only the tiles written
//! since it last drew.
//!
//! A composite operation other than `source-over`, and a clip, become layers
//! around the one draw they apply to. The layer that carries the blend is the
//! outermost, so the blend sees everything drawn before it; the clip regions
//! are plain clip layers inside it. Consecutive `source-over` draws under the
//! same clip share one set of clip layers.

use std::collections::HashMap;
use std::sync::Arc;

use lumen_module::lumen_paint::kurbo::{
    Affine, Cap, Join, Rect, Shape as _, Stroke as KurboStroke,
};
use lumen_module::lumen_paint::peniko;
use lumen_module::lumen_paint::peniko::{Blob, Fill, ImageQuality, ImageSampler};
use lumen_module::lumen_paint::{GlyphRun, Painter, PixelImage, Recording, Shape};
use lumen_module::lumen_text::{ShapeOptions, TextShaper};

use crate::color::Rgba;
use crate::ops::{Clip, CompositeOperation, Gfx, LineCap, LineJoin, Op};
use crate::store::{PendingRead, Surface};

/// The font blobs an encode hands the renderer, kept across ticks.
///
/// A renderer keys its upload caches off blob identity, so a freshly built
/// blob means a fresh upload. This keeps one blob per face, so a font's bytes
/// are uploaded once for the life of the process rather than once per
/// `fill_text`.
#[derive(Default)]
pub struct BlobCache {
    /// Font bytes, keyed by the shaper's own font id and the face index
    /// within the file.
    fonts: HashMap<(u64, u32), Blob<u8>>,
}

impl BlobCache {
    /// The blob for a shaped run's font. The shaper hands out the same
    /// `font_id` for the same face every time, which is what makes one entry
    /// serve every `fill_text` drawn in that font.
    fn font(&mut self, font_id: u64, index: u32, data: &Arc<Vec<u8>>) -> Blob<u8> {
        self.fonts
            .entry((font_id, index))
            .or_insert_with(|| Blob::new(data.clone()))
            .clone()
    }
}

/// A peniko color from the module's own.
fn peniko_color(c: Rgba) -> peniko::Color {
    peniko::Color::new([c.r, c.g, c.b, c.a])
}

/// Replay `ops` into `surface`'s drawing. Returns whether anything drew, which
/// is what decides whether the canvas needs a new frame.
///
/// `shaper` is the app's; without one (a headless app with no text stack)
/// text is skipped and the rest of the drawing still lands. A read among the
/// ops lands in `reads`, holding the drawing as it was at that point.
pub fn encode(
    surface: &mut Surface,
    ops: Vec<Op>,
    blobs: &mut BlobCache,
    mut shaper: Option<&mut dyn TextShaper>,
    reads: &mut Vec<PendingRead>,
) -> bool {
    if ops.is_empty() {
        return false;
    }
    let mut drew = false;
    // The clip layers a run of source-over draws is painting inside.
    let mut open: Option<OpenClip> = None;
    for op in &ops {
        // Emptying the canvas is journalled like everything else, so a fill
        // and the `clear` after it land in the order the script wrote them
        // rather than the order they reached the store.
        match op {
            Op::Clear => {
                reset(surface);
                open = None;
                drew = true;
                continue;
            }
            Op::Resize(width, height) => {
                surface.logical = (*width, *height);
                reset(surface);
                open = None;
                drew = true;
                continue;
            }
            Op::GetImageData { x, y, buffer } => {
                reads.push(PendingRead {
                    drawing: surface.drawing.clone(),
                    x: *x,
                    y: *y,
                    buffer: *buffer,
                    waited: 0,
                });
                continue;
            }
            Op::ClearRect(x, y, w, h)
                if clears_everything(&surface.gfx, surface.logical, (*x, *y, *w, *h)) =>
            {
                // Clearing the whole canvas is the common case (an animation
                // clears before it redraws), and the drawing it clears is
                // never seen again. Dropping it keeps the recording from
                // growing by a frame's worth every frame.
                surface.drawing = Arc::new(Recording::default());
                open = None;
                drew = true;
                continue;
            }
            _ => {}
        }
        if surface.gfx.apply(op) {
            continue;
        }
        // The drawing is shared with whatever the render world is still
        // holding, so the first draw after a publish copies it and every draw
        // after that writes in place. A `clear` costs nothing at all, because
        // `reset` hands over a fresh drawing instead of copying one to throw
        // away - which is the shape an animation that redraws each frame
        // takes.
        let logical = surface.logical;
        let gfx = &surface.gfx;
        let composite = match op {
            // A clear is a clear whatever the composite operation says, as
            // on the web: it erases with destination-out.
            Op::ClearRect(..) => CompositeOperation::DestinationOut,
            _ => gfx.state.composite,
        };
        let drawing: &mut dyn Painter = Arc::<Recording>::make_mut(&mut surface.drawing);
        let open = &mut open;
        let mut layered = |bounds: Option<Rect>, draw: &mut dyn FnMut(&mut dyn Painter) -> bool| {
            composited(drawing, gfx, composite, logical, bounds, open, draw)
        };
        let reach = stroke_reach(gfx);
        match op {
            Op::Fill(rule) => {
                drew |= layered(Some(gfx.path.bounding_box()), &mut |drawing| {
                    drawing.fill(
                        *rule,
                        gfx.state.transform,
                        peniko_color(gfx.fill_brush()).into(),
                        None,
                        &Shape::Path(&gfx.path),
                    );
                    true
                });
            }
            Op::Stroke => {
                drew |= layered(
                    Some(gfx.path.bounding_box().inflate(reach, reach)),
                    &mut |drawing| {
                        drawing.stroke(
                            &stroke_style(gfx),
                            gfx.state.transform,
                            peniko_color(gfx.stroke_brush()).into(),
                            None,
                            &Shape::Path(&gfx.path),
                        );
                        true
                    },
                );
            }
            Op::FillRect(x, y, w, h) => {
                drew |= layered(Some(rect_of(*x, *y, *w, *h)), &mut |drawing| {
                    drawing.fill(
                        Fill::NonZero,
                        gfx.state.transform,
                        peniko_color(gfx.fill_brush()).into(),
                        None,
                        &Shape::Rect(Rect::new(*x, *y, x + w, y + h)),
                    );
                    true
                });
            }
            Op::ClearRect(x, y, w, h) => {
                // Opaque, so destination-out takes what is there out
                // entirely; neither the alpha nor the fill applies.
                drew |= layered(Some(rect_of(*x, *y, *w, *h)), &mut |drawing| {
                    drawing.fill(
                        Fill::NonZero,
                        gfx.state.transform,
                        peniko::Color::BLACK.into(),
                        None,
                        &Shape::Rect(Rect::new(*x, *y, x + w, y + h)),
                    );
                    true
                });
            }
            Op::StrokeRect(x, y, w, h) => {
                let bounds = rect_of(*x, *y, *w, *h).inflate(reach, reach);
                drew |= layered(Some(bounds), &mut |drawing| {
                    drawing.stroke(
                        &stroke_style(gfx),
                        gfx.state.transform,
                        peniko_color(gfx.stroke_brush()).into(),
                        None,
                        &Shape::Rect(Rect::new(*x, *y, x + w, y + h)),
                    );
                    true
                });
            }
            Op::FillText { text, x, y } => {
                if let Some(shaper) = shaper.as_deref_mut() {
                    // A run's extent is the shaper's to know; the whole canvas
                    // is a bound that is always right.
                    drew |= layered(None, &mut |drawing| {
                        draw_text(drawing, gfx, blobs, shaper, text, *x, *y)
                    });
                }
            }
            Op::DrawBuffer { image, x, y } => {
                let size = (f64::from(image.width()), f64::from(image.height()));
                drew |= layered(Some(rect_of(*x, *y, size.0, size.1)), &mut |drawing| {
                    draw_buffer(drawing, gfx, image, (*x, *y), size)
                });
            }
            Op::DrawBufferScaled {
                image,
                x,
                y,
                width,
                height,
            } => {
                drew |= layered(Some(rect_of(*x, *y, *width, *height)), &mut |drawing| {
                    draw_buffer(drawing, gfx, image, (*x, *y), (*width, *height))
                });
            }
            // Unreachable: `Gfx::apply` answered for every state op above and
            // the two that empty the canvas were taken before it. A no-op
            // rather than a panic, because a drawing loop is the last place
            // to discover a new variant by crashing.
            _ => {}
        }
    }
    if let Some(open) = open {
        close(Arc::<Recording>::make_mut(&mut surface.drawing), open);
    }
    drew
}

/// Empty a surface: a fresh drawing and a fresh drawing state. A resize does
/// this too, which is what writing `width` on an HTML canvas does.
///
/// A fresh `Arc` rather than clearing the one that is there: the drawing is
/// shared with the render world, so clearing it in place would first copy
/// every command it accumulated, only to discard the copy. A canvas that
/// clears and redraws every frame would pay for the whole previous frame each
/// time.
fn reset(surface: &mut Surface) {
    surface.drawing = Arc::new(Recording::default());
    surface.gfx = crate::ops::Gfx::default();
}

/// Whether a `clear_rect` of `rect` erases the whole canvas: no clip, and a
/// transform that keeps it an axis-aligned rectangle covering every unit of
/// the drawing space.
fn clears_everything(gfx: &Gfx, logical: (f32, f32), rect: (f64, f64, f64, f64)) -> bool {
    if !gfx.state.clips.is_empty() {
        return false;
    }
    let [a, b, c, d, e, f] = gfx.state.transform.as_coeffs();
    if b != 0.0 || c != 0.0 {
        return false;
    }
    let (x, y, w, h) = rect;
    let (x0, x1) = (a * x + e, a * (x + w) + e);
    let (y0, y1) = (d * y + f, d * (y + h) + f);
    x0.min(x1) <= 0.0
        && y0.min(y1) <= 0.0
        && x0.max(x1) >= f64::from(logical.0)
        && y0.max(y1) >= f64::from(logical.1)
}

/// The clip layers a run of `source-over` draws paints inside, left open
/// between the draws of the run: what they clip to, and the depth to close
/// back to.
struct OpenClip {
    clips: Vec<Arc<Clip>>,
    depth: usize,
}

impl OpenClip {
    /// Whether these are the layers `clips` would push.
    fn matches(&self, clips: &[Arc<Clip>]) -> bool {
        self.clips.len() == clips.len()
            && self.clips.iter().zip(clips).all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

/// Close the layers a run of draws left open.
fn close(drawing: &mut dyn Painter, open: OpenClip) {
    while drawing.layer_depth() > open.depth {
        drawing.pop_layer();
    }
}

/// Push one plain clip layer per region.
fn push_clips(drawing: &mut dyn Painter, clips: &[Arc<Clip>]) {
    for clip in clips {
        drawing.push_layer(
            clip.rule,
            peniko::BlendMode::default(),
            1.0,
            clip.transform,
            &Shape::Path(&clip.path),
        );
    }
}

/// Paint one draw through the composite operation and the clip in force.
/// Returns what `draw` returned: whether anything was painted. `bounds`
/// covers everything the draw paints, under the current transform, when it
/// is known.
///
/// `source-over` with no clip is the draw and nothing else, which is the
/// path every canvas that never asks for more stays on. `source-over` under
/// a clip paints inside clip layers, which stay open for the next draw under
/// the same clip: compositing a run of source-over draws as one group is the
/// same as compositing them one by one, away from the clip's antialiased
/// edge.
///
/// Any other operation puts the blend on the outermost layer, with the clip
/// regions as plain clip layers inside it. A blend layer composites
/// everything inside its own clip, so that clip is what the operation
/// reaches. An operation that leaves alone what the draw does not cover
/// reaches only the draw's bounds, which keeps its layer small. One that
/// clears it (`copy` and the like) reaches the first clip region, or the
/// whole canvas with no clip, as on the web.
fn composited(
    drawing: &mut dyn Painter,
    gfx: &Gfx,
    composite: CompositeOperation,
    logical: (f32, f32),
    bounds: Option<Rect>,
    open: &mut Option<OpenClip>,
    draw: &mut dyn FnMut(&mut dyn Painter) -> bool,
) -> bool {
    let clips = &gfx.state.clips;
    if composite == CompositeOperation::SourceOver {
        if open.as_ref().is_some_and(|o| o.matches(clips)) {
            return draw(drawing);
        }
        if let Some(previous) = open.take() {
            close(drawing, previous);
        }
        if !clips.is_empty() {
            *open = Some(OpenClip {
                clips: clips.clone(),
                depth: drawing.layer_depth(),
            });
            push_clips(drawing, clips);
        }
        return draw(drawing);
    }
    if let Some(previous) = open.take() {
        close(drawing, previous);
    }
    let blend = composite.blend_mode();
    let depth = drawing.layer_depth();
    let inner = match (bounds, clips.first()) {
        (Some(bounds), _) if !composite.clears_outside_the_draw() => {
            drawing.push_layer(
                Fill::NonZero,
                blend,
                1.0,
                gfx.state.transform,
                &Shape::Rect(bounds),
            );
            &clips[..]
        }
        (_, Some(clip)) => {
            drawing.push_layer(
                clip.rule,
                blend,
                1.0,
                clip.transform,
                &Shape::Path(&clip.path),
            );
            &clips[1..]
        }
        (_, None) => {
            drawing.push_layer(
                Fill::NonZero,
                blend,
                1.0,
                Affine::IDENTITY,
                &Shape::Rect(Rect::new(
                    0.0,
                    0.0,
                    f64::from(logical.0),
                    f64::from(logical.1),
                )),
            );
            &clips[..]
        }
    };
    push_clips(drawing, inner);
    let drew = draw(drawing);
    while drawing.layer_depth() > depth {
        drawing.pop_layer();
    }
    drew
}

/// A rectangle from a corner and a size, either of which may be negative.
fn rect_of(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x, y, x + w, y + h).abs()
}

/// How far past its path a stroke can paint: half the width, times the
/// miter limit a sharp corner can extend to, plus a unit for antialiasing.
fn stroke_reach(gfx: &Gfx) -> f64 {
    let style = stroke_style(gfx);
    style.width / 2.0 * style.miter_limit.max(1.0) + 1.0
}

/// The stroke style the current state describes.
fn stroke_style(gfx: &Gfx) -> KurboStroke {
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
    drawing: &mut dyn Painter,
    gfx: &Gfx,
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
        drawing.draw_glyphs(&GlyphRun {
            font: &font_data,
            font_size: font.size,
            normalized_coords: &seg.normalized_coords,
            transform: gfx.state.transform * Affine::translate((x, y)),
            brush: brush.into(),
            glyphs: &seg.glyphs,
        });
        drew = true;
    }
    drew
}

/// How a buffer drawn at another size is sampled: bilinear while image
/// smoothing is on, nearest neighbour when it is off.
fn sampler(gfx: &Gfx) -> ImageSampler {
    let quality = if gfx.state.image_smoothing {
        ImageQuality::Medium
    } else {
        ImageQuality::Low
    };
    ImageSampler::default().with_quality(quality)
}

/// Draw a buffer's pixels into a box at `origin` of `size`. Returns whether
/// anything drew: a buffer with no pixels does not.
fn draw_buffer(
    drawing: &mut dyn Painter,
    gfx: &Gfx,
    image: &PixelImage,
    origin: (f64, f64),
    size: (f64, f64),
) -> bool {
    if image.width() == 0 || image.height() == 0 {
        return false;
    }
    let sx = size.0 / f64::from(image.width());
    let sy = size.1 / f64::from(image.height());
    let transform =
        gfx.state.transform * Affine::translate(origin) * Affine::scale_non_uniform(sx, sy);
    let alpha = gfx.state.global_alpha.clamp(0.0, 1.0);
    if alpha < 1.0 {
        drawing.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            gfx.state.transform,
            &Shape::Rect(Rect::new(
                origin.0,
                origin.1,
                origin.0 + size.0,
                origin.1 + size.1,
            )),
        );
    }
    // Straight pixels, as the buffer stores them; the renderer multiplies,
    // so nothing rounds on the way in.
    drawing.draw_pixels(image, sampler(gfx), transform);
    if alpha < 1.0 {
        drawing.pop_layer();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{FontSpec, LineCap, LineJoin};
    use crate::store::Surface;
    use lumen_module::lumen_paint::recording::Command;
    use lumen_text_cosmic::CosmicShaper;

    /// Replay a list of calls into a fresh surface, and hand back the surface
    /// and whether anything drew.
    ///
    /// Encoding records calls and paints nothing, so every case here runs
    /// with no renderer at all.
    fn encode_ops(ops: Vec<Op>) -> (Surface, bool) {
        let mut surface = Surface::default();
        let mut blobs = BlobCache::default();
        let drew = encode(&mut surface, ops, &mut blobs, None, &mut Vec::new());
        (surface, drew)
    }

    /// How many fills and strokes the surface's drawing holds.
    fn paths(surface: &Surface) -> usize {
        surface
            .drawing
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Fill { .. } | Command::Stroke { .. }))
            .count()
    }

    /// How many recorded calls match `kind`.
    fn count(surface: &Surface, kind: fn(&Command) -> bool) -> usize {
        surface
            .drawing
            .commands()
            .iter()
            .filter(|c| kind(c))
            .count()
    }

    #[test]
    fn nothing_recorded_encodes_nothing() {
        let (surface, drew) = encode_ops(Vec::new());
        assert!(!drew);
        assert!(surface.drawing.is_empty());
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
        assert!(surface.drawing.is_empty());
    }

    #[test]
    fn each_way_of_filling_and_stroking_reaches_the_drawing() {
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
                    Op::Fill(Fill::NonZero),
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
    fn clearing_hands_over_a_fresh_drawing_rather_than_copying_one() {
        let mut surface = Surface::default();
        let mut blobs = BlobCache::default();

        encode(
            &mut surface,
            vec![Op::FillRect(0.0, 0.0, 10.0, 10.0)],
            &mut blobs,
            None,
            &mut Vec::new(),
        );
        assert_eq!(paths(&surface), 1);
        let before = std::sync::Arc::as_ptr(&surface.drawing);

        // What the render world holding the previous frame looks like.
        let _published = surface.drawing.clone();
        let drew = encode(
            &mut surface,
            vec![Op::Clear],
            &mut blobs,
            None,
            &mut Vec::new(),
        );

        assert!(drew, "emptying the canvas is a change the renderer sees");
        assert!(surface.drawing.is_empty());
        assert_ne!(
            std::sync::Arc::as_ptr(&surface.drawing),
            before,
            "a clear swaps the drawing out; copying one to discard it is the \
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
        assert!(surface.drawing.is_empty());
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
        assert_eq!(paths(&moved), 1);
        assert_eq!(paths(&moved), paths(&still));
        let transform = |surface: &Surface| match &surface.drawing.commands()[0] {
            Command::Fill { transform, .. } => *transform,
            other => panic!("expected a fill, got {other:?}"),
        };
        assert_ne!(
            transform(&moved),
            transform(&still),
            "the translate reached the recorded transform"
        );
    }

    /// A 2x2 image of one colour, the pixels a draw call snapshots.
    fn image(rgba: [u8; 4]) -> PixelImage {
        PixelImage::from_rgba8(2, 2, &rgba.repeat(4))
    }

    #[test]
    fn a_buffer_is_drawn_at_its_own_size_and_stretched() {
        let red = image([0xff, 0, 0, 0xff]);
        let mut blobs = BlobCache::default();

        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            vec![
                Op::DrawBuffer {
                    image: red.clone(),
                    x: 0.0,
                    y: 0.0,
                },
                Op::DrawBufferScaled {
                    image: red,
                    x: 4.0,
                    y: 4.0,
                    width: 32.0,
                    height: 32.0,
                },
            ],
            &mut blobs,
            None,
            &mut Vec::new(),
        );
        assert!(drew);
        assert_eq!(
            count(&surface, |c| matches!(c, Command::Pixels { .. })),
            2,
            "both draws reached the drawing"
        );
    }

    #[test]
    fn a_buffer_drawn_under_a_global_alpha_goes_through_a_layer() {
        let mut blobs = BlobCache::default();

        let mut surface = Surface::default();
        encode(
            &mut surface,
            vec![
                Op::SetGlobalAlpha(0.5),
                Op::DrawBuffer {
                    image: PixelImage::new(2, 2),
                    x: 0.0,
                    y: 0.0,
                },
            ],
            &mut blobs,
            None,
            &mut Vec::new(),
        );
        assert_eq!(
            count(&surface, |c| matches!(c, Command::PushLayer { .. })),
            1
        );
        assert_eq!(surface.drawing.layer_depth(), 0);
    }

    #[test]
    fn a_buffer_with_no_pixels_draws_nothing() {
        let mut blobs = BlobCache::default();
        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            vec![Op::DrawBuffer {
                image: PixelImage::new(0, 0),
                x: 0.0,
                y: 0.0,
            }],
            &mut blobs,
            None,
            &mut Vec::new(),
        );
        assert!(!drew, "an empty buffer is not a reason to repaint");
        assert!(surface.drawing.is_empty());
    }

    #[test]
    fn text_needs_a_shaper_and_uses_the_apps_own_fonts() {
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
        let drew = encode(&mut bare, ops(), &mut blobs, None, &mut Vec::new());
        assert!(!drew);
        assert!(bare.drawing.is_empty());

        // With the app's shaper, the glyphs land.
        let mut shaper = CosmicShaper::new();
        let mut surface = Surface::default();
        let drew = encode(
            &mut surface,
            ops(),
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
            &mut Vec::new(),
        );
        assert!(drew, "the run reached the drawing");
        assert!(
            count(&surface, |c| matches!(c, Command::Glyphs { .. })) > 0,
            "shaped glyphs, not an outline the canvas drew itself"
        );
    }

    #[test]
    fn text_that_shapes_to_nothing_visible_adds_no_geometry() {
        // A zero-width space is text the shaper accepts and has no ink for.
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
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
            &mut Vec::new(),
        );
        // Whether it produced an empty run, an empty segment, or a glyph with
        // no outline, the canvas added no geometry of its own around it.
        assert!(
            surface
                .drawing
                .commands()
                .iter()
                .all(|c| matches!(c, Command::Glyphs { .. }))
        );
    }

    #[test]
    fn text_with_nothing_in_it_draws_nothing() {
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
            &mut blobs,
            Some(&mut shaper as &mut dyn TextShaper),
            &mut Vec::new(),
        );
        assert!(!drew);
    }

    fn layers(surface: &Surface) -> Vec<peniko::BlendMode> {
        surface
            .drawing
            .commands()
            .iter()
            .filter_map(|c| match c {
                Command::PushLayer { blend, .. } => Some(*blend),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn source_over_with_no_clip_draws_with_no_layer() {
        // The path every canvas that never composites stays on: one fill,
        // nothing around it.
        let (surface, _) = encode_ops(vec![Op::FillRect(0.0, 0.0, 10.0, 10.0)]);
        assert!(layers(&surface).is_empty());
    }

    #[test]
    fn a_composite_and_each_clip_are_one_layer_around_the_draw() {
        let (surface, drew) = encode_ops(vec![
            Op::Rect(0.0, 0.0, 10.0, 10.0),
            Op::Clip(Fill::NonZero),
            Op::BeginPath,
            Op::Rect(2.0, 2.0, 4.0, 4.0),
            Op::Clip(Fill::EvenOdd),
            Op::SetComposite(CompositeOperation::Multiply),
            Op::FillRect(0.0, 0.0, 10.0, 10.0),
        ]);
        assert!(drew);
        // The blend rides on the outer layer, over the draw's bounds, so it
        // sees what is under the draw; each clip is a plain layer inside it.
        assert_eq!(
            layers(&surface),
            [
                CompositeOperation::Multiply.blend_mode(),
                peniko::BlendMode::default(),
                peniko::BlendMode::default()
            ]
        );
        assert_eq!(surface.drawing.layer_depth(), 0, "both closed again");
    }

    #[test]
    fn source_over_draws_under_one_clip_share_its_layer() {
        let (surface, _) = encode_ops(vec![
            Op::Rect(0.0, 0.0, 10.0, 10.0),
            Op::Clip(Fill::NonZero),
            Op::FillRect(0.0, 0.0, 5.0, 5.0),
            Op::SetFill(Rgba::new(0.0, 0.0, 1.0, 1.0)),
            Op::FillRect(5.0, 5.0, 5.0, 5.0),
            Op::Save,
            Op::FillRect(2.0, 2.0, 5.0, 5.0),
            // A new clip is a new set of layers.
            Op::BeginPath,
            Op::Rect(0.0, 0.0, 4.0, 4.0),
            Op::Clip(Fill::NonZero),
            Op::FillRect(0.0, 0.0, 10.0, 10.0),
        ]);
        assert_eq!(layers(&surface).len(), 1 + 2);
        assert_eq!(paths(&surface), 4);
        assert_eq!(
            surface.drawing.layer_depth(),
            0,
            "closed when the encode ends"
        );
    }

    #[test]
    fn an_operation_that_keeps_the_rest_is_layered_over_the_draw_only() {
        let layer_clip = |op: CompositeOperation| {
            let (surface, _) = encode_ops(vec![
                Op::SetComposite(op),
                Op::FillRect(10.0, 20.0, 5.0, 5.0),
            ]);
            surface
                .drawing
                .commands()
                .iter()
                .find_map(|c| match c {
                    Command::PushLayer { clip, .. } => Some(clip.clone()),
                    _ => None,
                })
                .expect("a layer")
        };
        use lumen_module::lumen_paint::recording::OwnedShape;
        assert_eq!(
            layer_clip(CompositeOperation::Multiply),
            OwnedShape::Rect(Rect::new(10.0, 20.0, 15.0, 25.0))
        );
        assert_eq!(
            layer_clip(CompositeOperation::Copy),
            OwnedShape::Rect(Rect::new(0.0, 0.0, 300.0, 150.0)),
            "copy clears the whole canvas, so its layer covers it"
        );
    }

    #[test]
    fn restore_drops_the_clip_save_took_it_from() {
        let (surface, _) = encode_ops(vec![
            Op::Save,
            Op::Rect(0.0, 0.0, 10.0, 10.0),
            Op::Clip(Fill::NonZero),
            Op::Restore,
            Op::FillRect(0.0, 0.0, 10.0, 10.0),
        ]);
        assert!(layers(&surface).is_empty());
        assert!(surface.gfx.state.clips.is_empty());
    }

    #[test]
    fn a_partial_clear_rect_erases_through_destination_out() {
        let (surface, drew) = encode_ops(vec![
            Op::FillRect(0.0, 0.0, 300.0, 150.0),
            Op::SetComposite(CompositeOperation::Lighter),
            Op::ClearRect(0.0, 0.0, 10.0, 10.0),
        ]);
        assert!(drew);
        assert_eq!(
            layers(&surface),
            [CompositeOperation::DestinationOut.blend_mode()],
            "a clear ignores the composite operation"
        );
    }

    #[test]
    fn a_read_takes_the_drawing_as_it_was_at_the_call() {
        let mut surface = Surface::default();
        let mut reads = Vec::new();
        encode(
            &mut surface,
            vec![
                Op::FillRect(0.0, 0.0, 10.0, 10.0),
                Op::GetImageData {
                    x: 0,
                    y: 0,
                    buffer: 7,
                },
                Op::FillRect(0.0, 0.0, 20.0, 20.0),
            ],
            &mut BlobCache::default(),
            None,
            &mut reads,
        );
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].buffer, 7);
        assert_eq!(reads[0].drawing.len(), 1, "the fill after it is not in it");
        assert_eq!(surface.drawing.len(), 2);
    }

    /// Image smoothing picks the sampler: bilinear by default, nearest once
    /// it is off, and `save` / `restore` carry it like the rest of the state.
    #[test]
    fn image_smoothing_picks_how_a_buffer_is_sampled() {
        let mut blobs = BlobCache::default();
        let mut surface = Surface::default();
        let draw = || Op::DrawBufferScaled {
            image: PixelImage::new(2, 2),
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
        };
        encode(
            &mut surface,
            vec![
                draw(),
                Op::Save,
                Op::SetImageSmoothing(false),
                draw(),
                Op::Restore,
                draw(),
            ],
            &mut blobs,
            None,
            &mut Vec::new(),
        );
        let qualities: Vec<ImageQuality> = surface
            .drawing
            .commands()
            .iter()
            .filter_map(|c| match c {
                Command::Pixels { sampler, .. } => Some(sampler.quality),
                _ => None,
            })
            .collect();
        assert_eq!(
            qualities,
            [
                ImageQuality::Medium,
                ImageQuality::Low,
                ImageQuality::Medium
            ]
        );
    }
}
