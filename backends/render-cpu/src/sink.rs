//! The [`Painter`] sink this backend paints frames through: a
//! `vello_cpu::RenderContext`.
//!
//! The context is immediate-mode with state: each call sets the transform,
//! fill rule, and paint it needs, then draws. It has no fragments, so the
//! walker paints every leaf in place.

use lumen_paint::{GlyphRun, MaskKind, Painter, Shape};
use std::any::Any;
use std::collections::HashMap;
use vello_cpu::kurbo::{Affine, BezPath, Rect, Shape as _, Stroke};
use vello_cpu::peniko::{BlendMode, Brush, BrushRef, Color, Fill, ImageBrush};
use vello_cpu::{Image, ImageSource, Mask, PaintType, Pixmap, RenderContext, Resources};

/// Names this backend in [`Painter::backend_id`] and
/// [`lumen_core::native::NativePaintCtx::backend_id`]. [`Painter::native`]
/// on this backend's sink is a [`CpuPainter`].
pub const BACKEND_ID: &str = "lumen.render-cpu";

/// Flattening tolerance for curves the rasterizer takes as paths, in target
/// pixels. The same tolerance vello encodes shapes with.
const TOLERANCE: f64 = 0.1;

/// A [`Painter`] rasterizing on the CPU with vello_cpu.
pub struct CpuPainter {
    ctx: RenderContext,
    resources: Resources,
    depth: usize,
    /// Decoded image pixels, premultiplied once and kept by blob id. An entry
    /// a frame does not draw is dropped when the next frame starts.
    images: HashMap<u64, (ImageSource, bool)>,
}

impl std::fmt::Debug for CpuPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPainter")
            .field("width", &self.ctx.width())
            .field("height", &self.ctx.height())
            .field("depth", &self.depth)
            .finish()
    }
}

impl CpuPainter {
    /// A painter for a target of `width` x `height` pixels.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            ctx: RenderContext::new(width, height),
            resources: Resources::new(),
            depth: 0,
            images: HashMap::new(),
        }
    }

    /// Target size in pixels.
    pub fn size(&self) -> (u16, u16) {
        (self.ctx.width(), self.ctx.height())
    }

    /// Start a frame of `width` x `height` pixels: forget the previous
    /// frame's drawing and fill the target with `clear`.
    pub fn begin_frame(&mut self, width: u16, height: u16, clear: Color) {
        if self.size() == (width, height) {
            self.ctx.reset();
        } else {
            self.ctx.reset_and_resize(width, height);
        }
        self.depth = 0;
        self.images.retain(|_, (_, used)| std::mem::take(used));
        self.ctx.set_paint(clear);
        self.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
    }

    /// Rasterize the frame into `target`, which must be the frame's size.
    /// Closes any layer a caller left open first.
    pub fn render_into(&mut self, target: &mut Pixmap) {
        while self.depth > 0 {
            self.pop_layer();
        }
        self.ctx.flush();
        self.ctx.render(target, &mut self.resources);
    }

    /// The vello_cpu context the frame is painted into, for a painter that
    /// draws with vello_cpu directly. Leave every layer it opens closed.
    pub fn render_context(&mut self) -> &mut RenderContext {
        &mut self.ctx
    }

    /// The paint for a brush, decoding an image's pixels the first time a
    /// blob is drawn.
    fn paint(&mut self, brush: BrushRef<'_>) -> PaintType {
        match brush {
            Brush::Solid(color) => PaintType::Solid(color),
            Brush::Gradient(gradient) => PaintType::Gradient(gradient.clone()),
            Brush::Image(image) => PaintType::Image(Image {
                image: self.image_source(image.image),
                sampler: image.sampler,
            }),
        }
    }

    fn image_source(&mut self, data: &vello_cpu::peniko::ImageData) -> ImageSource {
        let entry = self
            .images
            .entry(data.data.id())
            .or_insert_with(|| (ImageSource::from_peniko_image_data(data), true));
        entry.1 = true;
        entry.0.clone()
    }

    /// Set the state every draw call starts from.
    fn prepare(&mut self, transform: Affine, brush: BrushRef<'_>, brush_transform: Option<Affine>) {
        let paint = self.paint(brush);
        self.ctx.set_transform(transform);
        self.ctx.set_paint(paint);
        self.ctx
            .set_paint_transform(brush_transform.unwrap_or(Affine::IDENTITY));
    }
}

/// The path vello_cpu takes for a shape that is not a plain rect.
fn to_path(shape: &Shape<'_>) -> BezPath {
    match shape {
        Shape::Rect(r) => r.to_path(TOLERANCE),
        Shape::RoundedRect(r) => r.to_path(TOLERANCE),
        Shape::Path(p) => (*p).clone(),
    }
}

impl Painter for CpuPainter {
    fn fill(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.prepare(transform, brush, brush_transform);
        self.ctx.set_fill_rule(style);
        match shape {
            Shape::Rect(r) => self.ctx.fill_rect(r),
            Shape::RoundedRect(r) => self.ctx.fill_path(&r.to_path(TOLERANCE)),
            Shape::Path(p) => self.ctx.fill_path(p),
        }
    }

    fn stroke(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.prepare(transform, brush, brush_transform);
        self.ctx.set_stroke(style.clone());
        match shape {
            Shape::Rect(r) => self.ctx.stroke_rect(r),
            Shape::RoundedRect(r) => self.ctx.stroke_path(&r.to_path(TOLERANCE)),
            Shape::Path(p) => self.ctx.stroke_path(p),
        }
    }

    fn push_layer(
        &mut self,
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) {
        self.ctx.set_transform(transform);
        self.ctx.set_fill_rule(clip_style);
        let path = to_path(clip);
        self.ctx.push_layer(
            Some(&path),
            Some(blend),
            Some(alpha.clamp(0.0, 1.0)),
            None,
            None,
        );
        self.depth += 1;
    }

    fn pop_layer(&mut self) {
        if self.depth > 0 {
            self.ctx.pop_layer();
            self.depth -= 1;
        }
    }

    fn layer_depth(&self) -> usize {
        self.depth
    }

    /// vello_cpu masks with a pixmap the size of the target, so the mask is
    /// rasterized on its own first, by a second painter that borrows this
    /// one's decoded images and glyph caches, then the content paints in a
    /// layer that applies it.
    fn draw_masked(
        &mut self,
        kind: MaskKind,
        transform: Affine,
        region: &Shape<'_>,
        mask: &mut dyn FnMut(&mut dyn Painter),
        content: &mut dyn FnMut(&mut dyn Painter),
    ) {
        let (width, height) = self.size();
        let region = to_path(region);
        let mut aside = CpuPainter::new(width, height);
        std::mem::swap(&mut aside.resources, &mut self.resources);
        std::mem::swap(&mut aside.images, &mut self.images);
        aside.ctx.set_transform(transform);
        aside.ctx.set_fill_rule(Fill::NonZero);
        aside.ctx.push_clip_layer(&region);
        aside.depth = 1;
        mask(&mut aside);
        let mut pixels = Pixmap::new(width, height);
        aside.render_into(&mut pixels);
        std::mem::swap(&mut aside.resources, &mut self.resources);
        std::mem::swap(&mut aside.images, &mut self.images);
        let coverage = match kind {
            MaskKind::Luminance => Mask::new_luminance(&pixels),
            MaskKind::Alpha => Mask::new_alpha(&pixels),
        };

        self.ctx.set_transform(transform);
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx
            .push_layer(Some(&region), None, None, Some(coverage), None);
        self.depth += 1;
        let depth = self.depth;
        content(self);
        while self.depth >= depth {
            self.pop_layer();
        }
    }

    fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.ctx.set_transform(transform);
        self.ctx.set_paint(color);
        self.ctx.reset_paint_transform();
        self.ctx
            .fill_blurred_rounded_rect(&rect, radius as f32, std_dev as f32, false);
    }

    fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
        let (w, h) = (image.image.width, image.image.height);
        if w == 0 || h == 0 {
            return;
        }
        let paint = PaintType::Image(Image {
            image: self.image_source(&image.image),
            sampler: image.sampler,
        });
        self.ctx.set_transform(transform);
        self.ctx.set_paint(paint);
        self.ctx.reset_paint_transform();
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        if run.glyphs.is_empty() {
            return;
        }
        self.prepare(run.transform, run.brush, None);
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx
            .glyph_run(&mut self.resources, run.font)
            .font_size(run.font_size)
            // The GPU backend paints unhinted outlines; the same pixels here.
            .hint(false)
            .normalized_coords(run.normalized_coords)
            .fill_glyphs(run.glyphs.iter().map(|g| vello_cpu::Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }));
    }

    fn backend_id(&self) -> &'static str {
        BACKEND_ID
    }

    fn native(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(painter: &mut CpuPainter, w: u16, h: u16) -> Pixmap {
        let mut target = Pixmap::new(w, h);
        painter.render_into(&mut target);
        target
    }

    /// A frame starts as the clear colour, edge to edge.
    #[test]
    fn a_frame_starts_as_the_clear_colour() {
        let mut painter = CpuPainter::new(8, 8);
        painter.begin_frame(8, 8, Color::new([0.0, 0.0, 1.0, 1.0]));
        let target = frame(&mut painter, 8, 8);
        for p in target.data() {
            assert_eq!((p.r, p.g, p.b, p.a), (0, 0, 255, 255));
        }
    }

    /// A fill lands where the shape is and nowhere else.
    #[test]
    fn a_fill_covers_its_shape() {
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, Color::new([0.0, 0.0, 0.0, 1.0]));
        painter.fill(
            Fill::NonZero,
            Affine::translate((4.0, 4.0)),
            Color::new([1.0, 0.0, 0.0, 1.0]).into(),
            None,
            &Shape::Rect(Rect::new(0.0, 0.0, 8.0, 8.0)),
        );
        let target = frame(&mut painter, 16, 16);
        assert_eq!(target.sample(8, 8).r, 255);
        assert_eq!(target.sample(1, 1).r, 0);
    }

    /// A layer clips what is drawn inside it, and the depth the walker reads
    /// to rebalance follows the pushes and pops.
    #[test]
    fn a_layer_clips_and_counts() {
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, Color::new([0.0, 0.0, 0.0, 1.0]));
        painter.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &Shape::Rect(Rect::new(0.0, 0.0, 8.0, 16.0)),
        );
        assert_eq!(painter.layer_depth(), 1);
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::new([0.0, 1.0, 0.0, 1.0]).into(),
            None,
            &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
        );
        painter.pop_layer();
        assert_eq!(painter.layer_depth(), 0);
        let target = frame(&mut painter, 16, 16);
        assert_eq!(target.sample(4, 8).g, 255);
        assert_eq!(target.sample(12, 8).g, 0);
    }

    /// A layer left open does not panic the rasterizer: the frame closes it.
    #[test]
    fn an_open_layer_is_closed_before_rasterizing() {
        let mut painter = CpuPainter::new(4, 4);
        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        painter.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &Shape::Rect(Rect::new(0.0, 0.0, 4.0, 4.0)),
        );
        let _ = frame(&mut painter, 4, 4);
        assert_eq!(painter.layer_depth(), 0);
    }

    /// An image's pixels are decoded once per blob and dropped once a frame
    /// stops drawing it.
    #[test]
    fn image_pixels_are_kept_while_drawn() {
        use vello_cpu::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
        let data = ImageData {
            data: Blob::new(std::sync::Arc::new(vec![255u8; 4 * 4])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        };
        let brush = ImageBrush::new(data);
        let mut painter = CpuPainter::new(4, 4);
        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        painter.draw_image(&brush, Affine::IDENTITY);
        painter.draw_image(&brush, Affine::translate((2.0, 2.0)));
        assert_eq!(painter.images.len(), 1);
        let target = frame(&mut painter, 4, 4);
        assert_eq!(target.sample(0, 0).r, 255);

        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(painter.images.len(), 1, "drawn last frame, kept");
        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        assert!(painter.images.is_empty(), "not drawn last frame, dropped");
    }

    fn black() -> Color {
        Color::new([0.0, 0.0, 0.0, 1.0])
    }

    /// Every shape kind fills and strokes where it is, whatever the brush:
    /// rounded rects and paths go through the path rasterizer, gradients and
    /// images through their paints.
    #[test]
    fn every_shape_and_brush_kind_paints() {
        use vello_cpu::kurbo::{Point, RoundedRect};
        use vello_cpu::peniko::{Blob, Gradient, ImageAlphaType, ImageData, ImageFormat};
        let mut painter = CpuPainter::new(32, 32);
        painter.begin_frame(32, 32, black());

        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            BrushRef::Solid(Color::new([1.0, 0.0, 0.0, 1.0])),
            None,
            &Shape::RoundedRect(RoundedRect::new(0.0, 0.0, 16.0, 16.0, 4.0)),
        );
        let gradient = Gradient::new_linear(Point::new(16.0, 0.0), Point::new(32.0, 0.0))
            .with_stops(
                [
                    Color::new([0.0, 1.0, 0.0, 1.0]),
                    Color::new([0.0, 1.0, 0.0, 1.0]),
                ]
                .as_slice(),
            );
        let mut triangle = BezPath::new();
        triangle.move_to((16.0, 0.0));
        triangle.line_to((32.0, 0.0));
        triangle.line_to((32.0, 16.0));
        triangle.close_path();
        painter.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            BrushRef::Gradient(&gradient),
            None,
            &Shape::Path(&triangle),
        );
        let image = ImageBrush::new(ImageData {
            data: Blob::new(std::sync::Arc::new([0u8, 0, 255, 255].repeat(4))),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        });
        painter.fill(
            Fill::NonZero,
            Affine::translate((0.0, 16.0)),
            BrushRef::Image(image.as_ref()),
            None,
            &Shape::Rect(Rect::new(0.0, 0.0, 2.0, 2.0)),
        );
        let white = Color::new([1.0, 1.0, 1.0, 1.0]);
        painter.stroke(
            &Stroke::new(2.0),
            Affine::IDENTITY,
            white.into(),
            None,
            &Shape::Rect(Rect::new(20.0, 20.0, 30.0, 30.0)),
        );
        painter.stroke(
            &Stroke::new(2.0),
            Affine::IDENTITY,
            white.into(),
            None,
            &Shape::RoundedRect(RoundedRect::new(4.0, 20.0, 14.0, 30.0, 2.0)),
        );
        painter.stroke(
            &Stroke::new(2.0),
            Affine::IDENTITY,
            white.into(),
            None,
            &Shape::Path(&triangle),
        );

        let target = frame(&mut painter, 32, 32);
        let px = |x, y| {
            let p = target.sample(x, y);
            (p.r, p.g, p.b)
        };
        assert_eq!(px(8, 8), (255, 0, 0), "the rounded rect");
        assert_eq!(px(0, 0), (0, 0, 0), "outside the rounded corner");
        assert_eq!(px(28, 4), (0, 255, 0), "inside the triangle");
        assert_eq!(px(18, 12), (0, 0, 0), "outside the triangle");
        assert_eq!(px(1, 17), (0, 0, 255), "the image paint");
        assert_eq!(px(20, 25), (255, 255, 255), "the rect stroke");
        assert_eq!(px(25, 25), (0, 0, 0), "a stroke does not fill");
        assert_eq!(px(9, 20), (255, 255, 255), "the rounded stroke");
    }

    /// A layer clipped to a path clips to the path, not its bounds.
    #[test]
    fn a_layer_clips_to_a_path() {
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, black());
        let mut triangle = BezPath::new();
        triangle.move_to((0.0, 0.0));
        triangle.line_to((16.0, 0.0));
        triangle.line_to((0.0, 16.0));
        triangle.close_path();
        painter.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &Shape::Path(&triangle),
        );
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::new([1.0, 0.0, 0.0, 1.0]).into(),
            None,
            &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
        );
        painter.pop_layer();
        let target = frame(&mut painter, 16, 16);
        assert_eq!(target.sample(2, 2).r, 255);
        assert_eq!(target.sample(14, 14).r, 0);
    }

    /// The mask rasterizes aside: what it paints never lands in the frame,
    /// the images it decodes stay with this painter, and the layers the
    /// content leaves open are closed with the masked draw.
    #[test]
    fn a_mask_paints_aside_and_the_masked_draw_balances() {
        use vello_cpu::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
        let white = ImageBrush::new(ImageData {
            data: Blob::new(std::sync::Arc::new(vec![255u8; 4 * 16 * 8])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 16,
            height: 8,
        });
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, black());
        painter.draw_masked(
            MaskKind::Luminance,
            Affine::IDENTITY,
            &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
            &mut |p| p.draw_image(&white, Affine::IDENTITY),
            &mut |p| {
                p.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::new([1.0, 0.0, 0.0, 1.0]).into(),
                    None,
                    &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
                );
                p.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    Affine::IDENTITY,
                    &Shape::Rect(Rect::new(0.0, 0.0, 1.0, 1.0)),
                );
            },
        );
        assert_eq!(painter.layer_depth(), 0);
        assert_eq!(painter.images.len(), 1, "the mask's image is kept");
        let target = frame(&mut painter, 16, 16);
        let top = target.sample(8, 4);
        let bottom = target.sample(8, 12);
        assert_eq!((top.r, top.g, top.b), (255, 0, 0), "under the white mask");
        assert_eq!((bottom.r, bottom.g, bottom.b), (0, 0, 0), "under no mask");
    }

    /// A box shadow is dense at its middle and fades out past its edge.
    #[test]
    fn a_blurred_rect_fades_past_its_edge() {
        let mut painter = CpuPainter::new(32, 32);
        painter.begin_frame(32, 32, black());
        painter.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            Rect::new(8.0, 8.0, 24.0, 24.0),
            Color::new([1.0, 1.0, 1.0, 1.0]),
            2.0,
            2.0,
        );
        let target = frame(&mut painter, 32, 32);
        let middle = target.sample(16, 16).r;
        let edge = target.sample(8, 16).r;
        let outside = target.sample(1, 16).r;
        assert!(middle > 240, "{middle}");
        assert!(edge > outside && edge < middle, "{edge}");
        assert_eq!(outside, 0);
    }

    /// Glyphs paint from the font they name; an empty run or an empty image
    /// paints nothing.
    #[test]
    fn glyphs_paint_and_empty_draws_do_not() {
        use lumen_text::GlyphPosition;
        use vello_cpu::peniko::{Blob, FontData, ImageAlphaType, ImageData, ImageFormat};
        let font_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../public/lumenc/tests/goldens/fonts/NotoSans-Regular.ttf"
        );
        let bytes = std::fs::read(font_path).expect("the pinned golden font");
        let font = FontData::new(Blob::new(std::sync::Arc::new(bytes)), 0);
        let mut painter = CpuPainter::new(32, 32);
        painter.begin_frame(32, 32, black());
        let white = Color::new([1.0, 1.0, 1.0, 1.0]);
        let run = |glyphs| GlyphRun {
            font: &font,
            font_size: 24.0,
            normalized_coords: &[],
            transform: Affine::translate((4.0, 26.0)),
            brush: white.into(),
            glyphs,
        };
        painter.draw_glyphs(&run(&[]));
        painter.draw_image(
            &ImageBrush::new(ImageData {
                data: Blob::new(std::sync::Arc::new(Vec::new())),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: 0,
                height: 0,
            }),
            Affine::IDENTITY,
        );
        assert!(
            frame(&mut painter, 32, 32).data().iter().all(|p| p.r == 0),
            "nothing to draw draws nothing",
        );

        painter.begin_frame(32, 32, black());
        let glyph = [GlyphPosition {
            // A filled capital in the golden font.
            id: 50,
            x: 0.0,
            y: 0.0,
            advance: 16.0,
            byte_start: 0,
            byte_end: 1,
        }];
        painter.draw_glyphs(&run(&glyph));
        let lit = frame(&mut painter, 32, 32)
            .data()
            .iter()
            .filter(|p| p.r > 128)
            .count();
        assert!(lit > 20, "the glyph covers pixels: {lit}");
    }

    /// The sink reports itself, resizes with the frame, and hands out its
    /// context and its concrete type.
    #[test]
    fn the_sink_resizes_and_names_itself() {
        let mut painter = CpuPainter::new(4, 4);
        painter.begin_frame(8, 2, black());
        assert_eq!(painter.size(), (8, 2));
        assert_eq!(painter.render_context().width(), 8);
        assert_eq!(painter.backend_id(), BACKEND_ID);
        assert!(painter.native().downcast_mut::<CpuPainter>().is_some());
        let debug = format!("{painter:?}");
        assert!(
            debug.contains("width: 8") && debug.contains("height: 2"),
            "{debug}"
        );
    }
}
