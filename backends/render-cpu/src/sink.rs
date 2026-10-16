//! The [`Painter`] sink this backend paints frames through: a
//! `vello_cpu::RenderContext`.
//!
//! The context is immediate-mode with state: each call sets the transform,
//! fill rule, and paint it needs, then draws. It has no fragments, so the
//! walker paints every leaf in place.
//!
//! An offscreen layer is a second context the size of the layer. While one
//! is open every call goes to it, shifted from frame coordinates to the
//! layer's own; closing it rasterizes the layer into a pixmap the frame then
//! draws as an image, faded by a tint rather than a compositing layer.

use lumen_paint::pixels::TileView;
use lumen_paint::{GlyphOutlines, GlyphRun, MaskKind, OffscreenLayer, Painter, PixelImage, Shape};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use vello_common::paint::{Tint, TintMode};
use vello_common::peniko::color::PremulRgba8;
use vello_cpu::kurbo::{Affine, BezPath, Rect, Shape as _, Stroke};
use vello_cpu::peniko::{
    BlendMode, Brush, BrushRef, Color, Fill, ImageBrush, ImageQuality, ImageSampler,
};
use vello_cpu::{
    Image, ImageSource, Mask, PaintType, Pixmap, RenderContext, RenderSettings, Resources,
};

/// Names this backend in [`Painter::backend_id`] and
/// [`lumen_core::native::NativePaintCtx::backend_id`]. [`Painter::native`]
/// on this backend's sink is a [`CpuPainter`].
pub const BACKEND_ID: &str = "lumen.render-cpu";

/// Flattening tolerance for curves the rasterizer takes as paths, in target
/// pixels. The same tolerance vello encodes shapes with.
const TOLERANCE: f64 = 0.1;

/// Frame pixels per worker thread. A frame rasterizes on one worker per
/// this many pixels, so a large frame spreads over every core while a small
/// one does not pay for threads that would mostly wait.
const PIXELS_PER_WORKER: u32 = 32 * 1024;

/// Most worker threads one frame rasterizes on.
const MAX_WORKERS: usize = 64;

/// The cores this process may run on.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// The worker threads a `width` x `height` frame rasterizes on with `cores`
/// cores: one per [`PIXELS_PER_WORKER`] pixels, no more than the cores or
/// [`MAX_WORKERS`], and none when that comes to fewer than two, because a
/// single worker leaves the calling thread waiting on it.
///
/// Below the cap the count steps in powers of two, so a window being resized
/// rebuilds its pool only when its size crosses a step.
fn workers_for(width: u16, height: u16, cores: usize) -> u16 {
    let cap = cores.min(MAX_WORKERS);
    let wanted = (u32::from(width) * u32::from(height) / PIXELS_PER_WORKER) as usize;
    let workers = if wanted >= cap {
        cap
    } else if wanted < 2 {
        0
    } else {
        1 << wanted.ilog2()
    };
    if workers < 2 { 0 } else { workers as u16 }
}

/// A vello_cpu context rasterizing on `workers` worker threads, or on the
/// calling thread for 0.
fn context(width: u16, height: u16, workers: u16) -> RenderContext {
    RenderContext::new_with(
        width,
        height,
        RenderSettings {
            num_threads: workers,
            ..RenderSettings::default()
        },
    )
}

/// A [`Painter`] rasterizing on the CPU with vello_cpu.
pub struct CpuPainter {
    ctx: RenderContext,
    /// The cores the frame may rasterize on; 1 for a painter that always
    /// draws on the calling thread.
    cores: usize,
    /// The context the frame used before its size last changed how many
    /// workers it takes, kept with its thread pool for when a resize goes
    /// back.
    spare: Option<RenderContext>,
    resources: Resources,
    depth: usize,
    /// Every image this painter keeps between frames. Shared like the other
    /// caches: an open offscreen layer or mask holds it.
    images: Images,
    /// Outline glyphs, flattened the way every backend draws them. Shared
    /// like the other caches: an open offscreen layer or mask holds it.
    outlines: GlyphOutlines,
    /// Maps frame coordinates into this painter's target: identity for the
    /// frame, a shift to the layer's corner for an offscreen layer.
    base: Affine,
    /// The offscreen layer open inside this painter, if any. Every call goes
    /// to it, and through it to any layer open inside that one.
    offscreen: Option<Box<Offscreen>>,
}

/// The images a painter keeps between frames.
#[derive(Default)]
struct Images {
    /// Decoded image pixels, premultiplied once and kept by blob id. An entry
    /// a frame does not draw is dropped when the next frame starts.
    decoded: HashMap<u64, (ImageSource, bool)>,
    /// Pixel images, premultiplied and kept by image id. A frame that draws
    /// one again premultiplies only the tiles written since; an entry a frame
    /// does not draw is dropped when the next frame starts.
    resident: HashMap<u64, Resident>,
}

impl Images {
    /// Drop what the frame that just ended did not draw.
    fn end_frame(&mut self) {
        self.decoded.retain(|_, (_, used)| std::mem::take(used));
        self.resident.retain(|_, r| std::mem::take(&mut r.used));
    }
}

/// One pixel image's premultiplied copy, and the stamp each of its tiles
/// carried when it was last copied.
struct Resident {
    pixmap: Arc<Pixmap>,
    size: (u32, u32),
    stamps: Vec<u64>,
    /// Which tiles hold a pixel that is not fully opaque, and how many do.
    /// An image with none paints through vello_cpu's opaque path, which is
    /// what a decoded image gets too.
    translucent: Vec<bool>,
    translucent_tiles: usize,
    used: bool,
}

impl Resident {
    fn new(width: u16, height: u16, tiles: usize) -> Self {
        Resident {
            pixmap: Arc::new(Pixmap::new(width, height)),
            size: (u32::from(width), u32::from(height)),
            // A fresh pixmap is transparent black, which is what a tile that
            // was never written holds: stamp 0 needs no copy.
            stamps: vec![0; tiles],
            translucent: vec![true; tiles],
            translucent_tiles: tiles,
            used: true,
        }
    }

    /// Bring the copy up to `image`: premultiply each tile whose stamp moved.
    ///
    /// A pixmap the frame already drew is shared with the context until the
    /// frame renders, so writing it then copies it first. That is what keeps
    /// two states of one image drawn in one frame apart.
    fn update(&mut self, image: &PixelImage) {
        if image
            .tiles()
            .all(|tile| self.stamps[tile.index] == tile.stamp)
        {
            return;
        }
        let width = self.size.0 as usize;
        let pixmap = Arc::make_mut(&mut self.pixmap);
        let data = pixmap.data_mut();
        for tile in image.tiles() {
            if self.stamps[tile.index] == tile.stamp {
                continue;
            }
            let mut opaque = true;
            for row in 0..tile.height as usize {
                let src = &tile.pixels[row * TileView::STRIDE..][..tile.width as usize * 4];
                let start = (tile.y as usize + row) * width + tile.x as usize;
                for (dst, px) in data[start..start + tile.width as usize]
                    .iter_mut()
                    .zip(src.chunks_exact(4))
                {
                    // The multiply vello_cpu applies to a straight image, so a
                    // pixel image paints exactly as the same pixels drawn
                    // through `draw_image` would.
                    opaque &= px[3] == 255;
                    let alpha = u16::from(px[3]);
                    let multiply = |c: u8| ((alpha * u16::from(c)) / 255) as u8;
                    *dst = PremulRgba8 {
                        r: multiply(px[0]),
                        g: multiply(px[1]),
                        b: multiply(px[2]),
                        a: px[3],
                    };
                }
            }
            self.stamps[tile.index] = tile.stamp;
            let was = std::mem::replace(&mut self.translucent[tile.index], !opaque);
            match (was, !opaque) {
                (true, false) => self.translucent_tiles -= 1,
                (false, true) => self.translucent_tiles += 1,
                _ => {}
            }
        }
        pixmap.set_may_have_transparency(self.translucent_tiles > 0);
    }
}

/// An offscreen layer being painted: its own painter, and where it sits in
/// the frame.
struct Offscreen {
    painter: CpuPainter,
    bounds: Rect,
}

/// A rasterized offscreen layer: premultiplied pixels and where they go.
struct CpuLayer {
    pixels: ImageSource,
    bounds: Rect,
}

impl std::fmt::Debug for CpuPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPainter")
            .field("width", &self.ctx.width())
            .field("height", &self.ctx.height())
            .field("depth", &self.depth)
            .field("workers", &self.worker_threads())
            .finish()
    }
}

impl CpuPainter {
    /// A painter for a target of `width` x `height` pixels. A large frame
    /// rasterizes on worker threads, more of them the larger it is; a small
    /// one on the calling thread. Both paint the same pixels, except that a
    /// translucent rect ending exactly on the frame's right edge can differ
    /// by one step of rounding in the last four columns.
    pub fn new(width: u16, height: u16) -> Self {
        Self::with_cores(width, height, cores())
    }

    /// A painter that always rasterizes on the calling thread: a mask or an
    /// offscreen layer, built for one use and not worth a thread pool.
    fn single_threaded(width: u16, height: u16) -> Self {
        Self::with_cores(width, height, 1)
    }

    /// A painter whose frames may rasterize on up to `cores` cores.
    fn with_cores(width: u16, height: u16, cores: usize) -> Self {
        Self {
            ctx: context(width, height, workers_for(width, height, cores)),
            cores,
            spare: None,
            resources: Resources::new(),
            depth: 0,
            images: Images::default(),
            outlines: GlyphOutlines::new(),
            base: Affine::IDENTITY,
            offscreen: None,
        }
    }

    /// The worker threads the frame rasterizes on; 0 when it rasterizes on
    /// the calling thread.
    pub fn worker_threads(&self) -> u16 {
        self.ctx.render_settings().num_threads
    }

    /// The painter calls go to: the innermost open offscreen layer, else
    /// this one.
    fn active(&mut self) -> &mut CpuPainter {
        match self.offscreen {
            Some(ref mut layer) => layer.painter.active(),
            None => self,
        }
    }

    /// Close every offscreen layer still open, dropping what it painted and
    /// handing the shared caches back.
    fn abandon_offscreen(&mut self) {
        if let Some(mut layer) = self.offscreen.take() {
            layer.painter.abandon_offscreen();
            std::mem::swap(&mut layer.painter.resources, &mut self.resources);
            std::mem::swap(&mut layer.painter.images, &mut self.images);
            std::mem::swap(&mut layer.painter.outlines, &mut self.outlines);
        }
    }

    /// Target size in pixels.
    pub fn size(&self) -> (u16, u16) {
        (self.ctx.width(), self.ctx.height())
    }

    /// Start a frame of `width` x `height` pixels: forget the previous
    /// frame's drawing and fill the target with `clear`.
    pub fn begin_frame(&mut self, width: u16, height: u16, clear: Color) {
        self.abandon_offscreen();
        let workers = workers_for(width, height, self.cores);
        if workers != self.worker_threads() {
            let next = match self.spare.take() {
                Some(spare) if spare.render_settings().num_threads == workers => spare,
                _ => context(width, height, workers),
            };
            self.spare = Some(std::mem::replace(&mut self.ctx, next));
        }
        if self.size() == (width, height) {
            self.ctx.reset();
        } else {
            self.ctx.reset_and_resize(width, height);
        }
        self.depth = 0;
        self.images.end_frame();
        self.outlines.end_frame();
        self.ctx.set_paint(clear);
        self.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
    }

    /// Rasterize the frame into `target`, which must be the frame's size.
    /// Closes any layer a caller left open first.
    pub fn render_into(&mut self, target: &mut Pixmap) {
        self.abandon_offscreen();
        while self.depth > 0 {
            self.pop_layer();
        }
        self.ctx.flush();
        self.ctx.render(target, &mut self.resources);
    }

    /// The vello_cpu context the frame is painted into, for a painter that
    /// draws with vello_cpu directly. Leave every layer it opens closed. The
    /// walker never paints a native leaf into an offscreen layer, so this is
    /// the frame's context whenever a native painter asks.
    pub fn render_context(&mut self) -> &mut RenderContext {
        &mut self.active().ctx
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
            .decoded
            .entry(data.data.id())
            .or_insert_with(|| (ImageSource::from_peniko_image_data(data), true));
        entry.1 = true;
        entry.0.clone()
    }

    /// The premultiplied pixels for a pixel image, brought up to date by
    /// copying only the tiles written since this painter last drew it.
    /// `None` for an image larger than vello_cpu draws.
    fn pixel_source(&mut self, image: &PixelImage) -> Option<ImageSource> {
        let width = u16::try_from(image.width()).ok()?;
        let height = u16::try_from(image.height()).ok()?;
        let resident = self
            .images
            .resident
            .entry(image.id())
            .or_insert_with(|| Resident::new(width, height, image.tile_count()));
        if resident.size != (image.width(), image.height())
            || resident.stamps.len() != image.tile_count()
        {
            *resident = Resident::new(width, height, image.tile_count());
        }
        resident.used = true;
        resident.update(image);
        Some(ImageSource::Pixmap(Arc::clone(&resident.pixmap)))
    }

    /// Set the state every draw call starts from.
    fn prepare(&mut self, transform: Affine, brush: BrushRef<'_>, brush_transform: Option<Affine>) {
        let paint = self.paint(brush);
        self.ctx.set_transform(self.base * transform);
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
        let this = self.active();
        this.prepare(transform, brush, brush_transform);
        this.ctx.set_fill_rule(style);
        let this = &mut this.ctx;
        match shape {
            Shape::Rect(r) => this.fill_rect(r),
            Shape::RoundedRect(r) => this.fill_path(&r.to_path(TOLERANCE)),
            Shape::Path(p) => this.fill_path(p),
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
        let this = self.active();
        this.prepare(transform, brush, brush_transform);
        this.ctx.set_stroke(style.clone());
        let this = &mut this.ctx;
        match shape {
            Shape::Rect(r) => this.stroke_rect(r),
            Shape::RoundedRect(r) => this.stroke_path(&r.to_path(TOLERANCE)),
            Shape::Path(p) => this.stroke_path(p),
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
        let this = self.active();
        this.ctx.set_transform(this.base * transform);
        this.ctx.set_fill_rule(clip_style);
        let path = to_path(clip);
        this.ctx.push_layer(
            Some(&path),
            Some(blend),
            Some(alpha.clamp(0.0, 1.0)),
            None,
            None,
        );
        this.depth += 1;
    }

    fn pop_layer(&mut self) {
        let this = self.active();
        if this.depth > 0 {
            this.ctx.pop_layer();
            this.depth -= 1;
        }
    }

    fn layer_depth(&self) -> usize {
        match &self.offscreen {
            Some(layer) => layer.painter.layer_depth(),
            None => self.depth,
        }
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
        if let Some(layer) = self.offscreen.as_deref_mut() {
            return layer
                .painter
                .draw_masked(kind, transform, region, mask, content);
        }
        let (width, height) = self.size();
        let region = to_path(region);
        let mut aside = CpuPainter::single_threaded(width, height);
        aside.base = self.base;
        std::mem::swap(&mut aside.resources, &mut self.resources);
        std::mem::swap(&mut aside.images, &mut self.images);
        std::mem::swap(&mut aside.outlines, &mut self.outlines);
        aside.ctx.set_transform(self.base * transform);
        aside.ctx.set_fill_rule(Fill::NonZero);
        aside.ctx.push_clip_layer(&region);
        aside.depth = 1;
        mask(&mut aside);
        let mut pixels = Pixmap::new(width, height);
        aside.render_into(&mut pixels);
        std::mem::swap(&mut aside.resources, &mut self.resources);
        std::mem::swap(&mut aside.images, &mut self.images);
        std::mem::swap(&mut aside.outlines, &mut self.outlines);
        let coverage = match kind {
            MaskKind::Luminance => Mask::new_luminance(&pixels),
            MaskKind::Alpha => Mask::new_alpha(&pixels),
        };

        self.ctx.set_transform(self.base * transform);
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
        let this = self.active();
        this.ctx.set_transform(this.base * transform);
        this.ctx.set_paint(color);
        this.ctx.reset_paint_transform();
        this.ctx
            .fill_blurred_rounded_rect(&rect, radius as f32, std_dev as f32, false);
    }

    fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
        let (w, h) = (image.image.width, image.image.height);
        if w == 0 || h == 0 {
            return;
        }
        let this = self.active();
        let paint = PaintType::Image(Image {
            image: this.image_source(&image.image),
            sampler: image.sampler,
        });
        this.ctx.set_transform(this.base * transform);
        this.ctx.set_paint(paint);
        this.ctx.reset_paint_transform();
        this.ctx.set_fill_rule(Fill::NonZero);
        this.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
    }

    fn draw_pixels(&mut self, image: &PixelImage, sampler: ImageSampler, transform: Affine) {
        let (w, h) = (image.width(), image.height());
        if w == 0 || h == 0 {
            return;
        }
        let this = self.active();
        let Some(source) = this.pixel_source(image) else {
            return;
        };
        this.ctx.set_transform(this.base * transform);
        this.ctx.set_paint(PaintType::Image(Image {
            image: source,
            sampler,
        }));
        this.ctx.reset_paint_transform();
        this.ctx.set_fill_rule(Fill::NonZero);
        this.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
    }

    /// Outline glyphs are filled as the polygons [`GlyphOutlines`] flattens
    /// them to, so they match every other backend; vello_cpu's own glyph
    /// renderer draws only the colour-glyph runs it declines.
    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        if run.glyphs.is_empty() {
            return;
        }
        let this = self.active();
        let mut outlines = std::mem::take(&mut this.outlines);
        // The polygons are in frame pixels and the fill maps them through
        // `base`, so the glyphs worth filling are the ones over this
        // target's own pixels, located in the frame.
        let (width, height) = this.size();
        let visible = this.base.inverse().transform_rect_bbox(Rect::new(
            0.0,
            0.0,
            f64::from(width),
            f64::from(height),
        ));
        let drawn = outlines.draw(this, run, visible);
        this.outlines = outlines;
        if drawn {
            return;
        }
        this.prepare(run.transform, run.brush, None);
        this.ctx.set_fill_rule(Fill::NonZero);
        this.ctx
            .glyph_run(&mut this.resources, run.font)
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

    fn begin_offscreen(&mut self, bounds: Rect) -> bool {
        if let Some(layer) = self.offscreen.as_deref_mut() {
            return layer.painter.begin_offscreen(bounds);
        }
        let (w, h) = (bounds.width().round(), bounds.height().round());
        if !(1.0..=f64::from(u16::MAX)).contains(&w) || !(1.0..=f64::from(u16::MAX)).contains(&h) {
            return false;
        }
        let mut painter = CpuPainter::single_threaded(w as u16, h as u16);
        painter.base = Affine::translate((-bounds.x0, -bounds.y0));
        // The layer paints with this painter's glyph caches and decoded
        // images, and hands them back when it closes.
        std::mem::swap(&mut painter.resources, &mut self.resources);
        std::mem::swap(&mut painter.images, &mut self.images);
        std::mem::swap(&mut painter.outlines, &mut self.outlines);
        self.offscreen = Some(Box::new(Offscreen { painter, bounds }));
        true
    }

    fn end_offscreen(&mut self) -> Option<OffscreenLayer> {
        let layer = self.offscreen.as_deref_mut()?;
        if layer.painter.offscreen.is_some() {
            return layer.painter.end_offscreen();
        }
        let Offscreen {
            mut painter,
            bounds,
        } = *self.offscreen.take()?;
        let (w, h) = painter.size();
        let mut pixels = Pixmap::new(w, h);
        painter.render_into(&mut pixels);
        std::mem::swap(&mut painter.resources, &mut self.resources);
        std::mem::swap(&mut painter.images, &mut self.images);
        std::mem::swap(&mut painter.outlines, &mut self.outlines);
        Some(OffscreenLayer::new(Arc::new(CpuLayer {
            pixels: ImageSource::Pixmap(Arc::new(pixels)),
            bounds,
        })))
    }

    fn draw_offscreen(&mut self, layer: &OffscreenLayer, alpha: f32) -> bool {
        let Some(layer) = layer.downcast::<CpuLayer>() else {
            return false;
        };
        let alpha = alpha.clamp(0.0, 1.0);
        let this = self.active();
        let (w, h) = (layer.bounds.width(), layer.bounds.height());
        this.ctx
            .set_transform(this.base * Affine::translate((layer.bounds.x0, layer.bounds.y0)));
        // One layer pixel per target pixel: the layer sits on whole pixels.
        this.ctx.set_paint(PaintType::Image(Image {
            image: layer.pixels.clone(),
            sampler: ImageSampler {
                quality: ImageQuality::Low,
                ..ImageSampler::default()
            },
        }));
        this.ctx.reset_paint_transform();
        this.ctx.set_fill_rule(Fill::NonZero);
        // The pixels are premultiplied, so scaling every channel by the
        // alpha fades them.
        if alpha < 1.0 {
            this.ctx.set_tint(Some(Tint {
                color: Color::new([1.0, 1.0, 1.0, alpha]),
                mode: TintMode::Multiply,
            }));
        }
        this.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        this.ctx.reset_tint();
        true
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
        assert_eq!(painter.images.decoded.len(), 1);
        let target = frame(&mut painter, 4, 4);
        assert_eq!(target.sample(0, 0).r, 255);

        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(painter.images.decoded.len(), 1, "drawn last frame, kept");
        painter.begin_frame(4, 4, Color::new([0.0, 0.0, 0.0, 1.0]));
        assert!(
            painter.images.decoded.is_empty(),
            "not drawn last frame, dropped"
        );
    }

    fn black() -> Color {
        Color::new([0.0, 0.0, 0.0, 1.0])
    }

    /// Every shape kind fills and strokes where it is, whatever the brush:
    /// rounded rects and paths go through the path rasterizer, gradients and
    /// images through their paints.
    fn nearest() -> ImageSampler {
        ImageSampler::default().with_quality(ImageQuality::Low)
    }

    /// A pixel image drawn again after a write shows the write, and its
    /// resident copy took only the tile that was written.
    #[test]
    fn a_pixel_image_updates_only_the_tiles_written() {
        let mut image = PixelImage::new(128, 64);
        image.fill_rgba8(0, 0, 128, 64, [0, 0, 255, 255]);
        let mut painter = CpuPainter::new(128, 64);
        painter.begin_frame(128, 64, black());
        painter.draw_pixels(&image, nearest(), Affine::IDENTITY);
        let first = frame(&mut painter, 128, 64);
        let p = first.sample(100, 10);
        assert_eq!((p.r, p.g, p.b), (0, 0, 255));
        assert!(
            !painter.images.resident[&image.id()]
                .pixmap
                .may_have_transparency(),
            "an opaque image takes the opaque path"
        );

        image.fill_rgba8(100, 10, 1, 1, [255, 0, 0, 255]);
        let before: Vec<u64> = painter.images.resident[&image.id()].stamps.clone();
        painter.begin_frame(128, 64, black());
        painter.draw_pixels(&image, nearest(), Affine::IDENTITY);
        let second = frame(&mut painter, 128, 64);
        let p = second.sample(100, 10);
        assert_eq!((p.r, p.g, p.b), (255, 0, 0));
        let after = &painter.images.resident[&image.id()].stamps;
        let moved = before.iter().zip(after).filter(|(a, b)| a != b).count();
        assert_eq!(moved, 1, "one tile copied, not the whole image");

        // Not drawn for a frame, dropped.
        painter.begin_frame(128, 64, black());
        painter.begin_frame(128, 64, black());
        assert!(painter.images.resident.is_empty());
    }

    /// Two states of one image drawn in one frame each paint their own
    /// pixels.
    #[test]
    fn two_states_of_one_pixel_image_in_one_frame_stay_apart() {
        let mut image = PixelImage::new(4, 4);
        image.fill_rgba8(0, 0, 4, 4, [255, 0, 0, 255]);
        let red = image.clone();
        image.fill_rgba8(0, 0, 4, 4, [0, 255, 0, 255]);
        let mut painter = CpuPainter::new(8, 4);
        painter.begin_frame(8, 4, black());
        painter.draw_pixels(&red, nearest(), Affine::IDENTITY);
        painter.draw_pixels(&image, nearest(), Affine::translate((4.0, 0.0)));
        let target = frame(&mut painter, 8, 4);
        let (a, b) = (target.sample(1, 1), target.sample(6, 1));
        assert_eq!((a.r, a.g), (255, 0));
        assert_eq!((b.r, b.g), (0, 255));
    }

    /// Low quality is nearest-neighbour: a scaled pixel image keeps hard
    /// edges, where the default smooths across them.
    #[test]
    fn a_pixel_image_scales_with_the_sampler_it_is_given() {
        let mut image = PixelImage::new(2, 1);
        image.fill_rgba8(0, 0, 1, 1, [255, 0, 0, 255]);
        image.fill_rgba8(1, 0, 1, 1, [0, 0, 255, 255]);
        let draw = |sampler: ImageSampler| {
            let mut painter = CpuPainter::new(16, 8);
            painter.begin_frame(16, 8, black());
            painter.draw_pixels(&image, sampler, Affine::scale(8.0));
            frame(&mut painter, 16, 8)
        };
        let sharp = draw(nearest());
        let p = sharp.sample(7, 4);
        assert_eq!((p.r, p.b), (255, 0), "the last red pixel stays red");
        let p = sharp.sample(8, 4);
        assert_eq!((p.r, p.b), (0, 255), "the first blue pixel is blue");
        let smooth = draw(ImageSampler::default());
        let p = smooth.sample(7, 4);
        assert!(p.b > 0, "smoothing mixes across the edge: {p:?}");
    }

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
        assert_eq!(painter.images.decoded.len(), 1, "the mask's image is kept");
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

    fn paint(painter: &mut CpuPainter, color: [f32; 4], rect: Rect) {
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::new(color).into(),
            None,
            &Shape::Rect(rect),
        );
    }

    /// Offscreen layers nest: the calls go to the innermost one, an inner layer composites into
    /// its parent, and the parent composites into the frame where it was painted.
    #[test]
    fn offscreen_layers_nest() {
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, black());
        assert!(painter.begin_offscreen(Rect::new(0.0, 0.0, 16.0, 16.0)));
        paint(
            &mut painter,
            [1.0, 0.0, 0.0, 1.0],
            Rect::new(0.0, 0.0, 8.0, 16.0),
        );
        assert!(painter.begin_offscreen(Rect::new(8.0, 8.0, 16.0, 16.0)));
        painter.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
        );
        assert_eq!(painter.layer_depth(), 1, "the inner layer's depth");
        paint(
            &mut painter,
            [0.0, 1.0, 0.0, 1.0],
            Rect::new(0.0, 0.0, 16.0, 16.0),
        );
        painter.pop_layer();
        let inner = painter.end_offscreen().expect("the inner layer");
        assert_eq!(painter.layer_depth(), 0);
        assert!(painter.draw_offscreen(&inner, 1.0));
        let outer = painter.end_offscreen().expect("the outer layer");
        assert!(painter.end_offscreen().is_none(), "nothing left open");
        assert!(painter.draw_offscreen(&outer, 1.0));

        let target = frame(&mut painter, 16, 16);
        let px = |x, y| {
            let p = target.sample(x, y);
            (p.r, p.g, p.b)
        };
        assert_eq!(px(2, 2), (255, 0, 0), "the outer layer's own paint");
        assert_eq!(
            px(12, 12),
            (0, 255, 0),
            "the inner layer, clipped to its bounds"
        );
        assert_eq!(px(12, 2), (0, 0, 0), "outside both paints");
    }

    /// A layer with no pixels, or too many, is turned down, and painting stays on the frame.
    #[test]
    fn an_unsized_layer_is_refused() {
        let mut painter = CpuPainter::new(8, 8);
        painter.begin_frame(8, 8, black());
        assert!(!painter.begin_offscreen(Rect::new(2.0, 2.0, 2.0, 6.0)));
        assert!(!painter.begin_offscreen(Rect::new(0.0, 0.0, 1.0e6, 4.0)));
        assert!(painter.end_offscreen().is_none());
        paint(
            &mut painter,
            [1.0, 0.0, 0.0, 1.0],
            Rect::new(0.0, 0.0, 8.0, 8.0),
        );
        assert_eq!(frame(&mut painter, 8, 8).sample(4, 4).r, 255);
    }

    /// A layer another kind of sink made is not drawn.
    #[test]
    fn a_foreign_layer_is_not_drawn() {
        let mut painter = CpuPainter::new(4, 4);
        painter.begin_frame(4, 4, black());
        assert!(!painter.draw_offscreen(&OffscreenLayer::new(Arc::new(1u8)), 1.0));
        assert!(
            frame(&mut painter, 4, 4)
                .data()
                .iter()
                .all(|p| p.r == 0 && p.g == 0)
        );
    }

    /// A mask inside an offscreen layer masks the layer's content, which then composites with
    /// the layer.
    #[test]
    fn a_mask_paints_inside_an_offscreen_layer() {
        let mut painter = CpuPainter::new(16, 16);
        painter.begin_frame(16, 16, black());
        assert!(painter.begin_offscreen(Rect::new(0.0, 0.0, 16.0, 16.0)));
        painter.draw_masked(
            MaskKind::Alpha,
            Affine::IDENTITY,
            &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
            &mut |p| {
                p.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::new([1.0, 1.0, 1.0, 1.0]).into(),
                    None,
                    &Shape::Rect(Rect::new(0.0, 0.0, 8.0, 16.0)),
                )
            },
            &mut |p| {
                p.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::new([0.0, 0.0, 1.0, 1.0]).into(),
                    None,
                    &Shape::Rect(Rect::new(0.0, 0.0, 16.0, 16.0)),
                )
            },
        );
        let layer = painter.end_offscreen().expect("the layer");
        assert!(painter.draw_offscreen(&layer, 1.0));
        let target = frame(&mut painter, 16, 16);
        assert_eq!(target.sample(4, 8).b, 255, "under the mask");
        assert_eq!(target.sample(12, 8).b, 0, "outside the mask");
    }

    /// A frame that starts with layers still open drops them, nested ones included, and paints
    /// on the frame again with the caches handed back.
    #[test]
    fn a_new_frame_abandons_open_layers() {
        let mut painter = CpuPainter::new(8, 8);
        painter.begin_frame(8, 8, black());
        assert!(painter.begin_offscreen(Rect::new(0.0, 0.0, 8.0, 8.0)));
        assert!(painter.begin_offscreen(Rect::new(0.0, 0.0, 4.0, 4.0)));
        paint(
            &mut painter,
            [0.0, 1.0, 0.0, 1.0],
            Rect::new(0.0, 0.0, 8.0, 8.0),
        );
        painter.begin_frame(8, 8, black());
        assert!(painter.end_offscreen().is_none(), "nothing still open");
        assert!(
            frame(&mut painter, 8, 8).data().iter().all(|p| p.g == 0),
            "the layers' paint is gone"
        );
        paint(
            &mut painter,
            [1.0, 0.0, 0.0, 1.0],
            Rect::new(0.0, 0.0, 8.0, 8.0),
        );
        assert_eq!(frame(&mut painter, 8, 8).sample(4, 4).r, 255);
    }

    /// A frame takes one worker per share of its pixels, up to the cores,
    /// and none at all when that is fewer than two.
    #[test]
    fn workers_follow_the_frame_size() {
        assert_eq!(
            workers_for(64, 64, 24),
            0,
            "a tiny frame stays on the caller"
        );
        assert_eq!(
            workers_for(256, 128, 24),
            0,
            "one worker's worth is not shared"
        );
        assert_eq!(workers_for(400, 300, 24), 2);
        assert_eq!(workers_for(800, 600, 24), 8, "steps down to a power of two");
        assert_eq!(
            workers_for(1024, 768, 24),
            24,
            "every core once the frame fills them"
        );
        assert_eq!(workers_for(3840, 2160, 24), 24);
        assert_eq!(
            workers_for(3840, 2160, 1),
            0,
            "one core has nothing to share"
        );
        assert_eq!(workers_for(3840, 2160, 2), 2);
        assert_eq!(workers_for(u16::MAX, u16::MAX, 512), MAX_WORKERS as u16);
    }

    /// A large frame rasterizes on workers, a small one on the calling
    /// thread, and a resize back to the large size takes the kept pool
    /// rather than building another.
    #[test]
    fn a_large_frame_shares_its_rasterizing_out() {
        let mut painter = CpuPainter::with_cores(1024, 768, 8);
        assert_eq!(painter.worker_threads(), 8);
        painter.begin_frame(64, 64, black());
        assert_eq!(painter.worker_threads(), 0);
        painter.begin_frame(1024, 768, black());
        assert_eq!(painter.worker_threads(), 8);
        assert_eq!(
            painter
                .spare
                .as_ref()
                .map(|c| c.render_settings().num_threads),
            Some(0),
            "the small frame's context is kept for the next swap"
        );
        assert_eq!(frame(&mut painter, 1024, 768).sample(512, 384).a, 255);

        if cores() >= 2 {
            assert!(
                RenderSettings::default().num_threads > 0,
                "vello_cpu is built without its worker threads"
            );
            assert!(CpuPainter::new(3840, 2160).worker_threads() >= 2);
        }
        let mut masked = CpuPainter::with_cores(1024, 768, 8);
        masked.begin_frame(1024, 768, black());
        assert!(masked.begin_offscreen(Rect::new(0.0, 0.0, 1024.0, 768.0)));
        let layer = masked.offscreen.as_ref().expect("the open layer");
        assert_eq!(
            layer.painter.worker_threads(),
            0,
            "a layer paints on the caller"
        );
    }

    /// Everything the walker draws, painted once on the calling thread and
    /// once on workers: the pixels match exactly, but for the case the next
    /// test covers.
    #[test]
    fn workers_paint_the_same_pixels() {
        use lumen_text::GlyphPosition;
        use vello_cpu::kurbo::{Point, RoundedRect};
        use vello_cpu::peniko::{Blob, FontData, Gradient, ImageAlphaType, ImageData, ImageFormat};
        let font_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../public/lumenc/tests/goldens/fonts/NotoSans-Regular.ttf"
        );
        let font = FontData::new(
            Blob::new(std::sync::Arc::new(
                std::fs::read(font_path).expect("the pinned golden font"),
            )),
            0,
        );
        let checker: Vec<u8> = (0..16 * 16)
            .flat_map(|i| {
                let on = (i % 16 + i / 16) % 2 == 0;
                if on {
                    [255, 200, 0, 255]
                } else {
                    [0, 40, 255, 128]
                }
            })
            .collect();
        let image = ImageBrush::new(ImageData {
            data: Blob::new(std::sync::Arc::new(checker)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 16,
            height: 16,
        });
        let mut pixels = PixelImage::new(64, 64);
        pixels.fill_rgba8(0, 0, 64, 64, [20, 160, 90, 255]);
        pixels.fill_rgba8(8, 8, 20, 30, [250, 10, 10, 100]);
        let gradient = Gradient::new_linear(Point::new(0.0, 0.0), Point::new(512.0, 384.0))
            .with_stops(
                [
                    Color::new([0.9, 0.1, 0.3, 1.0]),
                    Color::new([0.1, 0.3, 0.9, 0.6]),
                ]
                .as_slice(),
            );
        let (w, h) = (512, 384);
        let scene = |painter: &mut CpuPainter| {
            painter.begin_frame(w, h, Color::new([0.95, 0.95, 0.9, 1.0]));
            painter.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                BrushRef::Gradient(&gradient),
                None,
                // Short of the right edge: see the next test.
                &Shape::Rect(Rect::new(0.0, 0.0, 500.0, 120.0)),
            );
            for i in 0..40 {
                let x = f64::from(i) * 12.5;
                painter.fill(
                    Fill::NonZero,
                    Affine::translate((x, 130.0)).then_rotate(0.01 * f64::from(i)),
                    Color::new([0.1, 0.02 * i as f32, 0.5, 0.7]).into(),
                    None,
                    &Shape::RoundedRect(RoundedRect::new(0.0, 0.0, 10.3, 60.7, 3.0)),
                );
            }
            let mut star = BezPath::new();
            star.move_to((300.0, 200.0));
            star.curve_to((420.0, 150.0), (500.0, 330.0), (320.0, 370.0));
            star.quad_to((200.0, 300.0), (300.0, 200.0));
            star.close_path();
            painter.fill(
                Fill::EvenOdd,
                Affine::IDENTITY,
                Color::new([0.2, 0.7, 0.2, 0.8]).into(),
                None,
                &Shape::Path(&star),
            );
            painter.stroke(
                &Stroke::new(3.5),
                Affine::IDENTITY,
                Color::new([0.0, 0.0, 0.0, 1.0]).into(),
                None,
                &Shape::Path(&star),
            );
            painter.draw_blurred_rounded_rect(
                Affine::IDENTITY,
                Rect::new(40.0, 220.0, 180.0, 330.0),
                Color::new([0.0, 0.0, 0.0, 0.5]),
                8.0,
                6.0,
            );
            painter.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                0.6,
                Affine::IDENTITY,
                &Shape::RoundedRect(RoundedRect::new(30.0, 210.0, 190.0, 340.0, 12.0)),
            );
            painter.draw_image(
                &image.clone().with_quality(ImageQuality::Medium),
                Affine::translate((30.0, 210.0))
                    .then_scale(1.0)
                    .pre_scale(10.0),
            );
            painter.pop_layer();
            painter.draw_pixels(&pixels, nearest(), Affine::translate((420.0, 10.0)));
            painter.draw_masked(
                MaskKind::Alpha,
                Affine::IDENTITY,
                &Shape::Rect(Rect::new(200.0, 10.0, 400.0, 110.0)),
                &mut |p| p.draw_image(&image, Affine::translate((200.0, 10.0)).pre_scale(6.0)),
                &mut |p| {
                    p.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::new([1.0, 1.0, 1.0, 1.0]).into(),
                        None,
                        &Shape::Rect(Rect::new(200.0, 10.0, 400.0, 110.0)),
                    );
                },
            );
            let glyphs: Vec<GlyphPosition> = (0..24)
                .map(|i| GlyphPosition {
                    id: 40 + i,
                    x: i as f32 * 13.0,
                    y: 0.0,
                    advance: 13.0,
                    byte_start: 0,
                    byte_end: 1,
                })
                .collect();
            painter.draw_glyphs(&GlyphRun {
                font: &font,
                font_size: 18.0,
                normalized_coords: &[],
                transform: Affine::translate((10.4, 370.3)),
                brush: Color::new([0.1, 0.1, 0.1, 1.0]).into(),
                glyphs: &glyphs,
            });
            assert!(painter.begin_offscreen(Rect::new(220.0, 250.0, 300.0, 330.0)));
            painter.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::new([0.8, 0.2, 0.9, 1.0]).into(),
                None,
                &Shape::RoundedRect(RoundedRect::new(225.0, 255.0, 295.0, 325.0, 20.0)),
            );
            let layer = painter.end_offscreen().expect("the layer");
            assert!(painter.draw_offscreen(&layer, 0.5));
            frame(painter, w, h)
        };

        let mut alone = CpuPainter::with_cores(w, h, 1);
        let mut shared = CpuPainter::with_cores(w, h, 8);
        assert_eq!(alone.worker_threads(), 0);
        assert!(shared.worker_threads() >= 2);
        let one = scene(&mut alone);
        // Twice on workers: the second frame reuses the pool and its buffers.
        for _ in 0..2 {
            let many = scene(&mut shared);
            let differ = one
                .data()
                .iter()
                .zip(many.data())
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(differ, 0, "pixels that differ on workers");
        }
    }

    /// The one place workers paint other pixels: a translucent rect whose
    /// right side lies exactly on the frame's right edge. vello_cpu fills a
    /// rect on the calling thread through a rect path and on workers as a
    /// path, and the path's last column of tiles blends one step of
    /// rounding apart. Only those four columns move, by at most one.
    #[test]
    fn a_translucent_rect_on_the_right_edge_rounds_at_most_one_apart() {
        let (w, h) = (512, 384);
        let paint = |cores| {
            let mut painter = CpuPainter::with_cores(w, h, cores);
            painter.begin_frame(w, h, Color::new([0.95, 0.95, 0.9, 1.0]));
            painter.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::new([0.2, 0.5, 0.7, 0.5]).into(),
                None,
                &Shape::Rect(Rect::new(0.0, 0.0, f64::from(w), f64::from(h))),
            );
            frame(&mut painter, w, h)
        };
        let (alone, shared) = (paint(1), paint(8));
        for (i, (a, b)) in alone.data().iter().zip(shared.data()).enumerate() {
            if a == b {
                continue;
            }
            let x = i % usize::from(w);
            assert!(x >= usize::from(w) - 4, "column {x} moved");
            for (p, q) in [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)] {
                assert!(p.abs_diff(q) <= 1, "column {x}: {a:?} against {b:?}");
            }
        }
    }
}
