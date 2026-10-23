//! The [`Painter`] sink this backend paints frames through: a `vello::Scene`.
//!
//! Every call encodes straight into the scene vello renders. Fragments are
//! sub-scenes: [`Painter::begin_fragment`] redirects encoding into a fresh
//! scene, and replaying one is a `Scene::append`, so a repeated appearance is
//! encoded once and appended at every position it takes.
//!
//! An offscreen layer is a sub-scene too, recorded in frame coordinates. The
//! frame draws it as an image whose pixels a texture of the layer's size
//! supplies; the renderer renders the recorded sub-scene into that texture
//! before it renders the frame (see [`VelloPainter::take_offscreen`]).
//!
//! A frame whose images would not fit vello's image atlas is painted again
//! in split mode, through a `SplitFrame`: the renderer then renders it in
//! several passes (see the crate's `atlas` module).
//!
//! Outline glyphs reach the scene as the polygons [`GlyphOutlines`] flattens
//! them to, the same lines every backend fills. Each flattened glyph is kept
//! encoded, and a run copies those encodings into the scene under one
//! translation per glyph: the layout vello's own glyph runs resolve to, so a
//! frame of text costs a copy per glyph rather than a re-encode.

use lumen_paint::glyphs::{Contours, GlyphShape, brush_transform};
use lumen_paint::recording::OwnedShape;
use lumen_paint::{
    BackendImage, Fragment, GlyphOutlines, GlyphRun, MaskKind, OffscreenLayer, Painter, PixelImage,
    Shape, TextureId, TextureRefusal,
};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use crate::atlas::{Copies, Job, Opened, SplitFrame, stand_in};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{
    BlendMode, BrushRef, Color, Compose, Fill, ImageBrush, ImageData, ImageQuality, ImageSampler,
    Mix,
};
use vello_encoding::{Encoding, PathTag, Transform};

/// Names this backend in [`Painter::backend_id`] and
/// [`lumen_core::native::NativePaintCtx::backend_id`]. [`Painter::native`]
/// on this backend's sink is a [`VelloPainter`].
pub const BACKEND_ID: &str = "lumen.render-wgpu";

/// The longest side an offscreen layer may have: the largest image vello's
/// atlas holds.
const MAX_LAYER_SIDE: u32 = 8192;

/// A [`Painter`] encoding into a [`vello::Scene`].
pub struct VelloPainter {
    scene: Scene,
    /// Fragments being recorded, innermost last. Encoding goes to the top
    /// of this stack when it is not empty.
    recording: Vec<Scene>,
    /// Offscreen layers being recorded, innermost last, with their bounds.
    /// Encoding goes to the top when no fragment is recording.
    offscreen: Vec<(Scene, Rect)>,
    /// Layers closed this frame, in the order they closed, waiting for the
    /// renderer to render them into their textures.
    finished: Vec<FinishedLayer>,
    /// The longest side an offscreen layer may have here.
    max_layer_side: u32,
    /// Outline glyphs, flattened the way every backend draws them and kept
    /// encoded.
    outlines: GlyphOutlines<EncodedGlyph>,
    /// The frame's target bounds, when the renderer has said; glyphs
    /// outside them are not encoded.
    visible: Option<Rect>,
    /// The pixel images this frame drew, by id, in the state it drew them.
    /// The renderer brings each one's texture up to that state before it
    /// renders (see [`VelloPainter::take_pixels`]).
    pixels: HashMap<u64, PixelImage>,
    /// The image each pixel image is drawn through, by id, kept across
    /// frames: one blob id per pixel image is what lets vello keep one atlas
    /// slot for it, and the renderer stands a texture in for the blob.
    stand_ins: HashMap<u64, ImageData>,
    /// Set while a frame paints in split mode: frame-level calls go through
    /// it rather than straight into `scene`.
    split: Option<SplitFrame>,
    /// The copies of shrunk image draws split frames keep across frames.
    copies: Copies,
    /// The layers open in the frame itself, outermost first, as they were
    /// opened: what a hole for a backend texture has to be cut through.
    frame_layers: Vec<OpenLayer>,
    /// How many masked draws are painting their content or mask right now.
    masked: usize,
    /// The backend textures the frame shows, in paint order.
    placements: Vec<Placement>,
}

/// One layer open in the frame, kept so it can be closed and opened again
/// around a hole.
#[derive(Clone, Debug)]
struct OpenLayer {
    clip_style: Fill,
    blend: BlendMode,
    alpha: f32,
    transform: Affine,
    clip: OwnedShape,
}

impl OpenLayer {
    /// Whether the layer only clips: it composites what it holds at full
    /// alpha, source-over, so closing it early and opening it again changes
    /// nothing.
    fn clips_only(&self) -> bool {
        self.alpha >= 1.0 && self.blend == BlendMode::default()
    }
}

/// Where a frame shows a backend texture: beneath the frame, at the rect a
/// hole was cut for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// The texture.
    pub(crate) id: TextureId,
    /// Where its corners land, in target pixels: `x0, y0` is texture pixel
    /// `0, 0`.
    pub(crate) rect: Rect,
    /// Its size in texture pixels.
    pub(crate) size: (u32, u32),
    /// Whether it samples the nearest texel rather than filtering.
    pub(crate) nearest: bool,
}

impl Default for VelloPainter {
    fn default() -> Self {
        Self {
            scene: Scene::new(),
            recording: Vec::new(),
            offscreen: Vec::new(),
            finished: Vec::new(),
            max_layer_side: MAX_LAYER_SIDE,
            outlines: GlyphOutlines::new(),
            visible: None,
            pixels: HashMap::new(),
            stand_ins: HashMap::new(),
            split: None,
            copies: Copies::default(),
            frame_layers: Vec::new(),
            masked: 0,
            placements: Vec::new(),
        }
    }
}

/// One flattened glyph as vello path segments, relative to the glyph's
/// origin: the tags and the coordinates a path encoder writes for it.
struct EncodedGlyph {
    tags: Box<[PathTag]>,
    data: Box<[u32]>,
    segments: u32,
    /// The segments' bounding box, relative to the glyph's origin.
    bounds: Rect,
}

impl GlyphShape for EncodedGlyph {
    fn from_contours(contours: Contours<'_>) -> Self {
        let bounds = contours.bounds();
        let mut encoding = Encoding::new();
        let mut path = encoding.encode_path(true);
        for contour in contours.iter() {
            let mut points = contour.iter();
            let Some(&[x, y]) = points.next() else {
                continue;
            };
            path.move_to(x, y);
            for &[x, y] in points {
                path.line_to(x, y);
            }
            path.close();
        }
        // No path marker: a run's glyphs end in one path object together.
        let segments = path.finish(false);
        Self {
            tags: encoding.path_tags.into(),
            data: encoding.path_data.into(),
            segments,
            bounds,
        }
    }
}

/// A closed offscreen layer waiting to be rendered: what it painted, where,
/// and the image the frame draws it through.
pub(crate) struct FinishedLayer {
    /// The layer's paint, in frame coordinates.
    pub(crate) scene: Scene,
    /// Where the layer sits in the frame, in whole target pixels.
    pub(crate) bounds: Rect,
    /// The image a texture of the layer's size stands in for.
    pub(crate) image: ImageData,
    /// The layer as handed out; its texture goes once nothing holds it.
    pub(crate) layer: Weak<GpuLayer>,
    /// How many layers it was painted inside of. A layer draws only layers
    /// nested deeper than itself.
    pub(crate) depth: usize,
}

/// A rasterized offscreen layer as the frame draws it.
pub(crate) struct GpuLayer {
    /// The image the layer's texture stands in for.
    pub(crate) image: ImageData,
    /// Where the layer sits in the frame.
    pub(crate) bounds: Rect,
}

/// Encode `run`'s outline glyphs into `encoding` as one filled path, each
/// glyph's cached segments under a translation to where it lands, closed by
/// the run's brush. Mirrors how vello resolves a glyph run of its own.
/// Glyphs wholly outside `visible`, in target pixels, are left out.
fn encode_outline_run(
    encoding: &mut Encoding,
    outlines: &mut GlyphOutlines<EncodedGlyph>,
    run: &GlyphRun<'_>,
    visible: Option<Rect>,
) {
    let start = encoding.stream_offsets();
    let segments_before = encoding.n_path_segments;
    encoding.encode_fill_style(Fill::NonZero);
    outlines.for_each(run, |origin, glyph| {
        if glyph.segments == 0
            || visible.is_some_and(|v| !(glyph.bounds + origin.to_vec2()).overlaps(v))
        {
            return;
        }
        encoding.path_tags.push(PathTag::TRANSFORM);
        encoding.transforms.push(Transform {
            matrix: [1.0, 0.0, 0.0, 1.0],
            translation: [origin.x as f32, origin.y as f32],
        });
        encoding.path_tags.extend_from_slice(&glyph.tags);
        encoding.path_data.extend_from_slice(&glyph.data);
        encoding.n_path_segments += glyph.segments;
    });
    if encoding.n_path_segments == segments_before {
        // Nothing to fill: leave the encoding as it was.
        encoding.path_tags.truncate(start.path_tags);
        encoding.transforms.truncate(start.transforms);
        encoding.styles.truncate(start.styles);
        return;
    }
    // The brush maps through the transform current at the path marker.
    let paint = brush_transform(run).unwrap_or(Affine::IDENTITY);
    encoding.path_tags.push(PathTag::TRANSFORM);
    encoding.transforms.push(Transform::from_kurbo(&paint));
    encoding.path_tags.push(PathTag::PATH);
    encoding.n_paths += 1;
    encoding.encode_brush(run.brush, 1.0);
    // The transform and style streams moved behind the encoder's back.
    encoding.force_next_transform_and_style();
}

impl VelloPainter {
    /// A painter over an empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// The scene a frame was painted into.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The scene the next call encodes into, for a painter that encodes
    /// with vello directly.
    pub fn scene_mut(&mut self) -> &mut Scene {
        self.target()
    }

    /// Tell the painter the frame's target is `width` x `height` pixels, so
    /// glyphs wholly outside it are not encoded. Until it is told, every
    /// glyph is.
    pub fn set_target_size(&mut self, width: u32, height: u32) {
        self.visible = Some(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
    }

    /// Empty the scene for the next frame, dropping any recording a caller
    /// left open.
    pub fn reset(&mut self) {
        self.scene.reset();
        self.recording.clear();
        self.offscreen.clear();
        self.finished.clear();
        self.outlines.end_frame();
        self.pixels.clear();
        self.split = None;
        self.frame_layers.clear();
        self.masked = 0;
        self.placements.clear();
    }

    /// The backend textures the frame shows, in paint order.
    pub(crate) fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Start painting the frame again, in split mode, over a target of
    /// `width` x `height` pixels: the first paint drew more images than one
    /// render holds. The offscreen layers the first paint closed are kept,
    /// since the layer cache may hand them out again rather than repaint them.
    pub(crate) fn begin_split(&mut self, width: u32, height: u32) {
        self.scene.reset();
        self.recording.clear();
        self.offscreen.clear();
        self.pixels.clear();
        self.frame_layers.clear();
        self.masked = 0;
        self.placements.clear();
        self.split = Some(SplitFrame::new(width, height));
    }

    /// End split mode: the frame's own scene goes back to where
    /// [`Self::scene`] reads it, and the renders to run before it come back,
    /// in order.
    pub(crate) fn finish_split(&mut self) -> Vec<Job> {
        let Some(split) = self.split.take() else {
            return Vec::new();
        };
        let (scene, jobs) = split.finish();
        self.scene = scene;
        jobs
    }

    /// Forget the kept copies this frame did not draw, handing back the
    /// images their textures stand in for.
    pub(crate) fn end_copies(&mut self) -> Vec<ImageData> {
        self.copies.end_frame()
    }

    /// Whether the next call paints into the frame itself, rather than into
    /// a fragment or an offscreen layer.
    fn at_frame_level(&self) -> bool {
        self.recording.is_empty() && self.offscreen.is_empty()
    }

    /// The split frame, when the frame paints in split mode and the next call
    /// paints into the frame itself.
    fn split_frame(&mut self) -> Option<&mut SplitFrame> {
        if self.at_frame_level() {
            self.split.as_mut()
        } else {
            None
        }
    }

    /// The pixel images the frame drew, each with the image vello draws it
    /// through. The renderer has to stand an up-to-date texture in for every
    /// one of them before the frame (or an offscreen layer) renders.
    ///
    /// A stand-in the frame did not draw is forgotten here, and handed back
    /// in the second list so the renderer can release its texture.
    pub(crate) fn take_pixels(&mut self) -> (Vec<(PixelImage, ImageData)>, Vec<ImageData>) {
        let drawn: Vec<(PixelImage, ImageData)> = self
            .pixels
            .drain()
            .filter_map(|(id, image)| Some((image, self.stand_ins.get(&id)?.clone())))
            .collect();
        let mut gone = Vec::new();
        self.stand_ins.retain(|id, stand_in| {
            let live = drawn.iter().any(|(image, _)| image.id() == *id);
            if !live {
                gone.push(stand_in.clone());
            }
            live
        });
        (drawn, gone)
    }

    /// The offscreen layers closed since the last call. Each must be rendered
    /// into a texture standing in for its image before the frame renders,
    /// and a layer drawn inside another (a greater `depth`) before the one
    /// that draws it.
    pub(crate) fn take_offscreen(&mut self) -> Vec<FinishedLayer> {
        std::mem::take(&mut self.finished)
    }

    /// Refuse offscreen layers with a side longer than `side`, the largest
    /// texture the device makes, when that is less than vello's own limit.
    pub(crate) fn limit_layer_side(&mut self, side: u32) {
        self.max_layer_side = side.min(MAX_LAYER_SIDE);
    }

    /// Where the next call encodes: the innermost open fragment, else the
    /// innermost open offscreen layer, else the frame. A fragment never
    /// spans a layer, so an open one is always the innermost.
    fn target(&mut self) -> &mut Scene {
        if let Some(fragment) = self.recording.last_mut() {
            return fragment;
        }
        if let Some((layer, _)) = self.offscreen.last_mut() {
            return layer;
        }
        match &mut self.split {
            Some(split) => split.scene(),
            None => &mut self.scene,
        }
    }
}

/// Say once per run that a pixel image too large to draw was left out.
fn report_too_large(width: u32, height: u32, side: u32) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        lumen_core::warn_line!(
            "lumen-render-wgpu: a {width}x{height} pixel image is not drawn: the GPU renderer \
             draws images up to {side} pixels a side"
        );
    }
}

impl VelloPainter {
    /// Open a layer without recording it as one of the frame's.
    fn open_layer(
        &mut self,
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) {
        if let Some(split) = self.split_frame() {
            split.open(Opened::layer(clip_style, blend, alpha, transform, clip));
            return;
        }
        let scene = self.target();
        match clip {
            Shape::Rect(r) => scene.push_layer(clip_style, blend, alpha, transform, r),
            Shape::RoundedRect(r) => scene.push_layer(clip_style, blend, alpha, transform, r),
            Shape::Path(p) => scene.push_layer(clip_style, blend, alpha, transform, *p),
        }
    }

    /// Close the innermost layer without touching the frame's record of
    /// them.
    fn close_layer(&mut self) {
        if let Some(split) = self.split_frame() {
            split.close();
            return;
        }
        self.target().pop_layer();
    }

    /// Why a texture drawn now under `transform` cannot show, if it cannot.
    fn texture_refusal(&self, transform: Affine) -> Option<TextureRefusal> {
        if !self.at_frame_level() {
            return Some(TextureRefusal::Layer);
        }
        if self.masked > 0 {
            return Some(TextureRefusal::Mask);
        }
        if let Some(layer) = self.frame_layers.iter().find(|l| !l.clips_only()) {
            return Some(if layer.blend != BlendMode::default() {
                TextureRefusal::Blend
            } else {
                TextureRefusal::Opacity
            });
        }
        let [a, b, c, d, _, _] = transform.as_coeffs();
        if b != 0.0 || c != 0.0 || a <= 0.0 || d <= 0.0 {
            return Some(TextureRefusal::Transform);
        }
        None
    }

    /// Clear `rect` (under `transform`) to transparent in what the frame has
    /// painted so far, inside every clip open in the frame.
    ///
    /// A layer composites what it holds over what is under it, so a clear
    /// inside one would only clear the layer. The open layers all only clip
    /// (anything else refuses the texture first), and closing a clip layer
    /// early and opening it again paints the same, so they close, the clear
    /// runs in the frame itself through the same clips, and they open again.
    fn cut_hole(&mut self, transform: Affine, rect: Rect) {
        let open = self.frame_layers.clone();
        for _ in &open {
            self.close_layer();
        }
        let erase = BlendMode::new(Mix::Normal, Compose::DestOut);
        match open.split_first() {
            Some((outer, inner)) => {
                self.open_layer(
                    outer.clip_style,
                    erase,
                    1.0,
                    outer.transform,
                    &outer.clip.as_shape(),
                );
                for layer in inner {
                    self.open_layer(
                        layer.clip_style,
                        BlendMode::default(),
                        1.0,
                        layer.transform,
                        &layer.clip.as_shape(),
                    );
                }
            }
            None => self.open_layer(Fill::NonZero, erase, 1.0, transform, &Shape::Rect(rect)),
        }
        self.target()
            .fill(Fill::NonZero, transform, Color::BLACK, None, &rect);
        for _ in 0..open.len().max(1) {
            self.close_layer();
        }
        for layer in &open {
            self.open_layer(
                layer.clip_style,
                layer.blend,
                layer.alpha,
                layer.transform,
                &layer.clip.as_shape(),
            );
        }
    }
}

impl Painter for VelloPainter {
    fn fill(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        let scene = self.target();
        match shape {
            Shape::Rect(r) => scene.fill(style, transform, brush, brush_transform, r),
            Shape::RoundedRect(r) => scene.fill(style, transform, brush, brush_transform, r),
            Shape::Path(p) => scene.fill(style, transform, brush, brush_transform, *p),
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
        let scene = self.target();
        match shape {
            Shape::Rect(r) => scene.stroke(style, transform, brush, brush_transform, r),
            Shape::RoundedRect(r) => scene.stroke(style, transform, brush, brush_transform, r),
            Shape::Path(p) => scene.stroke(style, transform, brush, brush_transform, *p),
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
        if self.at_frame_level() {
            self.frame_layers.push(OpenLayer {
                clip_style,
                blend,
                alpha,
                transform,
                clip: OwnedShape::from(clip),
            });
        }
        self.open_layer(clip_style, blend, alpha, transform, clip);
    }

    fn pop_layer(&mut self) {
        if self.at_frame_level() {
            self.frame_layers.pop();
        }
        self.close_layer();
    }

    /// The content paints first, in a layer of its own so the mask touches
    /// nothing under it. The mask then paints in a nested layer that
    /// multiplies into the content when it closes: vello's luminance mask
    /// layer for luminance, a destination-in composite for alpha.
    fn draw_masked(
        &mut self,
        kind: MaskKind,
        transform: Affine,
        region: &Shape<'_>,
        mask: &mut dyn FnMut(&mut dyn Painter),
        content: &mut dyn FnMut(&mut dyn Painter),
    ) {
        let outer = self.layer_depth();
        self.masked += 1;
        self.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, region);
        content(self);
        while self.layer_depth() > outer + 1 {
            self.pop_layer();
        }
        match kind {
            MaskKind::Luminance if self.at_frame_level() && self.split.is_some() => {
                if let Some(split) = self.split.as_mut() {
                    split.open(Opened::luminance_mask(
                        Fill::NonZero,
                        1.0,
                        transform,
                        region,
                    ));
                }
            }
            MaskKind::Luminance => {
                let scene = self.target();
                match region {
                    Shape::Rect(r) => {
                        scene.push_luminance_mask_layer(Fill::NonZero, 1.0, transform, r)
                    }
                    Shape::RoundedRect(r) => {
                        scene.push_luminance_mask_layer(Fill::NonZero, 1.0, transform, r)
                    }
                    Shape::Path(p) => {
                        scene.push_luminance_mask_layer(Fill::NonZero, 1.0, transform, *p)
                    }
                }
            }
            MaskKind::Alpha => self.push_layer(
                Fill::NonZero,
                BlendMode::new(Mix::Normal, Compose::DestIn),
                1.0,
                transform,
                region,
            ),
        }
        mask(self);
        while self.layer_depth() > outer {
            self.pop_layer();
        }
        self.masked -= 1;
    }

    fn layer_depth(&self) -> usize {
        if let (true, Some(split)) = (self.at_frame_level(), &self.split) {
            return split.depth();
        }
        let scene = match (self.recording.last(), self.offscreen.last()) {
            (Some(fragment), _) => fragment,
            (None, Some((layer, _))) => layer,
            (None, None) => &self.scene,
        };
        scene.encoding().n_open_clips as usize
    }

    fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.target()
            .draw_blurred_rounded_rect(transform, rect, color, radius, std_dev);
    }

    fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
        if self.at_frame_level()
            && let Some(split) = self.split.as_mut()
        {
            split.draw_image(image, transform, 0, &mut self.copies);
            return;
        }
        self.target().draw_image(image, transform);
    }

    /// Drawn through a stand-in image whose pixels a texture the renderer
    /// keeps per image supplies, so a write re-uploads only its tiles.
    ///
    /// Two cases draw the image flat instead, the way a sink with no
    /// resident copies does: inside a fragment, which may replay on a later
    /// frame after the texture is gone, and a second state of an image this
    /// frame already drew, since one texture holds one state.
    fn draw_pixels(&mut self, image: &PixelImage, sampler: ImageSampler, transform: Affine) {
        let (width, height) = (image.width(), image.height());
        if width == 0 || height == 0 {
            return;
        }
        if width > self.max_layer_side || height > self.max_layer_side {
            report_too_large(width, height, self.max_layer_side);
            return;
        }
        let flat = !self.recording.is_empty()
            || self
                .pixels
                .get(&image.id())
                .is_some_and(|drawn| drawn.version() != image.version());
        if flat {
            let brush = ImageBrush {
                image: image.to_image_data(),
                sampler,
            };
            self.target().draw_image(&brush, transform);
            return;
        }
        let stand_in = self
            .stand_ins
            .entry(image.id())
            .and_modify(|data| {
                // A resized image is a new atlas slot and a new texture.
                if (data.width, data.height) != (width, height) {
                    *data = stand_in(width, height);
                }
            })
            .or_insert_with(|| stand_in(width, height))
            .clone();
        self.pixels.insert(image.id(), image.clone());
        let brush = ImageBrush {
            image: stand_in,
            sampler,
        };
        if self.at_frame_level()
            && let Some(split) = self.split.as_mut()
        {
            split.draw_image(&brush, transform, image.version(), &mut self.copies);
            return;
        }
        self.target().draw_image(&brush, transform);
    }

    /// Shown beneath the frame through a hole cut where it goes; the
    /// renderer composes the texture under the frame when it presents. See
    /// [`Painter::draw_texture`] for what is refused.
    fn draw_texture(
        &mut self,
        image: &BackendImage<'_>,
        sampler: ImageSampler,
        transform: Affine,
    ) -> Result<(), TextureRefusal> {
        if image.width == 0 || image.height == 0 {
            return Ok(());
        }
        if let Some(refusal) = self.texture_refusal(transform) {
            return Err(refusal);
        }
        let size = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        self.cut_hole(transform, size);
        self.placements.push(Placement {
            id: image.id,
            rect: transform.transform_rect_bbox(size),
            size: (image.width, image.height),
            nearest: sampler.quality == ImageQuality::Low,
        });
        Ok(())
    }

    /// Outline glyphs are filled as the polygons [`GlyphOutlines`] flattens
    /// them to, so they match every other backend; vello's own glyph
    /// encoding draws only the colour-glyph runs it declines.
    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        if self.outlines.has_outlines(run) {
            let mut outlines = std::mem::take(&mut self.outlines);
            // A fragment replays under a transform of its own, so nothing
            // recorded into one is culled. An offscreen layer records in
            // frame coordinates and keeps only what falls inside its bounds;
            // the frame keeps what falls inside its target.
            let visible = if !self.recording.is_empty() {
                None
            } else if let Some(&(_, bounds)) = self.offscreen.last() {
                Some(bounds)
            } else {
                self.visible
            };
            encode_outline_run(self.target().encoding_mut(), &mut outlines, run, visible);
            self.outlines = outlines;
            return;
        }
        self.target()
            .draw_glyphs(run.font)
            .font_size(run.font_size)
            .normalized_coords(run.normalized_coords)
            .brush(run.brush)
            .transform(run.transform)
            .draw(
                Fill::NonZero,
                run.glyphs.iter().map(|g| vello::Glyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                }),
            );
    }

    fn begin_fragment(&mut self) -> bool {
        self.recording.push(Scene::new());
        true
    }

    fn end_fragment(&mut self) -> Option<Fragment> {
        self.recording
            .pop()
            .map(|scene| Fragment::new(Arc::new(scene)))
    }

    fn append_fragment(&mut self, fragment: &Fragment, transform: Affine) -> bool {
        let Some(scene) = fragment.downcast::<Scene>() else {
            return false;
        };
        if let Some(split) = self.split_frame() {
            split.append(scene, transform);
            return true;
        }
        self.target().append(scene, Some(transform));
        true
    }

    fn begin_offscreen(&mut self, bounds: Rect) -> bool {
        let (w, h) = (bounds.width().round(), bounds.height().round());
        let side = 1.0..=f64::from(self.max_layer_side);
        if !side.contains(&w) || !side.contains(&h) {
            return false;
        }
        self.offscreen.push((Scene::new(), bounds));
        true
    }

    fn end_offscreen(&mut self) -> Option<OffscreenLayer> {
        let (mut scene, bounds) = self.offscreen.pop()?;
        while scene.encoding().n_open_clips > 0 {
            scene.pop_layer();
        }
        // A blob of its own gives the layer an image id no other image has;
        // the renderer stands a texture in for its pixels.
        let image = stand_in(
            bounds.width().round() as u32,
            bounds.height().round() as u32,
        );
        let layer = Arc::new(GpuLayer {
            image: image.clone(),
            bounds,
        });
        self.finished.push(FinishedLayer {
            scene,
            bounds,
            image,
            layer: Arc::downgrade(&layer),
            depth: self.offscreen.len(),
        });
        Some(OffscreenLayer::new(layer))
    }

    fn draw_offscreen(&mut self, layer: &OffscreenLayer, alpha: f32) -> bool {
        let Some(layer) = layer.downcast::<GpuLayer>() else {
            return false;
        };
        // One layer pixel per target pixel: the layer sits on whole pixels.
        let brush = ImageBrush {
            image: layer.image.clone(),
            sampler: ImageSampler {
                quality: ImageQuality::Low,
                alpha: alpha.clamp(0.0, 1.0),
                ..ImageSampler::default()
            },
        };
        let at = Affine::translate((layer.bounds.x0, layer.bounds.y0));
        if self.at_frame_level()
            && let Some(split) = self.split.as_mut()
        {
            split.draw_image(&brush, at, 0, &mut self.copies);
            return true;
        }
        self.target().draw_image(&brush, at);
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
    use vello::kurbo::Rect;

    fn square() -> Shape<'static> {
        Shape::Rect(Rect::new(0.0, 0.0, 4.0, 4.0))
    }

    /// While a fragment records, the frame stays untouched; replaying the
    /// fragment is what lands its work in the frame.
    #[test]
    fn a_fragment_records_aside_and_replays_into_the_frame() {
        let mut painter = VelloPainter::new();
        assert!(painter.begin_fragment());
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::new([1.0, 0.0, 0.0, 1.0]).into(),
            None,
            &square(),
        );
        assert!(painter.scene().encoding().is_empty());
        let fragment = painter.end_fragment().expect("a fragment was open");
        assert!(painter.append_fragment(&fragment, Affine::translate((8.0, 8.0))));
        assert!(!painter.scene().encoding().is_empty());
    }

    /// A masked draw inside a fragment encodes into the fragment, the way an
    /// SVG asset is recorded, and closes every layer it opened, including
    /// one its content left open.
    #[test]
    fn a_masked_draw_records_into_the_open_fragment_and_balances() {
        let mut painter = VelloPainter::new();
        assert!(painter.begin_fragment());
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        for kind in [MaskKind::Luminance, MaskKind::Alpha] {
            painter.draw_masked(
                kind,
                Affine::IDENTITY,
                &square(),
                &mut |p| p.fill(Fill::NonZero, Affine::IDENTITY, red.into(), None, &square()),
                &mut |p| {
                    p.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        Affine::IDENTITY,
                        &square(),
                    )
                },
            );
            assert_eq!(painter.layer_depth(), 0);
        }
        assert!(painter.scene().encoding().is_empty());
        let fragment = painter.end_fragment().expect("a fragment was open");
        let scene = fragment.downcast::<Scene>().expect("a vello fragment");
        assert!(!scene.encoding().is_empty());
    }

    /// Layer depth follows the scene encoding takes, so the walker can close
    /// what a native painter left open.
    #[test]
    fn layer_depth_counts_open_layers() {
        let mut painter = VelloPainter::new();
        painter.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &square(),
        );
        assert_eq!(painter.layer_depth(), 1);
        painter.pop_layer();
        assert_eq!(painter.layer_depth(), 0);
    }

    /// An outline run encodes as one filled path of the glyphs' flattened
    /// lines; a run with nothing in view, or nothing to draw, encodes
    /// nothing.
    #[test]
    fn an_outline_run_encodes_one_path_of_lines() {
        use lumen_text::GlyphPosition;
        use vello::peniko::{Blob, FontData};
        let font_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../public/lumenc/tests/goldens/fonts/NotoSans-Regular.ttf"
        );
        let bytes = std::fs::read(font_path).expect("the pinned golden font");
        let font = FontData::new(Blob::new(Arc::new(bytes)), 0);
        let glyphs = [50, 51].map(|id| GlyphPosition {
            id,
            x: (id - 50) as f32 * 12.0,
            y: 0.0,
            advance: 12.0,
            byte_start: 0,
            byte_end: 1,
        });
        let run = |glyphs, x| GlyphRun {
            font: &font,
            font_size: 16.0,
            normalized_coords: &[],
            transform: Affine::translate((x, 30.0)),
            brush: Color::WHITE.into(),
            glyphs,
        };
        let mut painter = VelloPainter::new();
        painter.set_target_size(64, 64);
        painter.draw_glyphs(&run(&[], 4.0));
        painter.draw_glyphs(&run(&glyphs, 400.0));
        assert!(painter.scene().encoding().is_empty(), "nothing in view");

        painter.draw_glyphs(&run(&glyphs, 4.0));
        let encoding = painter.scene().encoding();
        assert_eq!(encoding.n_paths, 1, "one path for the run");
        assert!(encoding.n_path_segments > 20);
        assert!(
            encoding
                .path_tags
                .iter()
                .filter(|t| t.is_path_segment())
                .all(|t| t.path_segment_type() == vello_encoding::PathSegmentType::LINE_TO),
            "lines only"
        );
        assert!(
            encoding.resources.glyph_runs.is_empty(),
            "vello's glyph path unused"
        );
    }

    /// A pixel image keeps one stand-in across frames, so vello keeps one
    /// atlas slot for it; a frame that stops drawing it hands the stand-in
    /// back to be released.
    #[test]
    fn a_pixel_image_keeps_one_stand_in_while_it_is_drawn() {
        let mut image = PixelImage::new(8, 8);
        let mut painter = VelloPainter::new();
        let sampler = ImageSampler::default();
        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        let (drawn, gone) = painter.take_pixels();
        assert_eq!(drawn.len(), 1);
        assert!(gone.is_empty());
        let first = drawn[0].1.data.id();

        painter.reset();
        image.fill_rgba8(0, 0, 1, 1, [255; 4]);
        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        let (drawn, _) = painter.take_pixels();
        assert_eq!(drawn[0].1.data.id(), first, "a write keeps the slot");
        assert_eq!(drawn[0].0.version(), image.version(), "in the drawn state");

        painter.reset();
        image.resize(16, 8);
        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        let (drawn, _) = painter.take_pixels();
        assert_ne!(drawn[0].1.data.id(), first, "a resize takes a new one");

        painter.reset();
        let (drawn, gone) = painter.take_pixels();
        assert!(drawn.is_empty());
        assert_eq!(gone.len(), 1, "not drawn, released");
    }

    /// Inside a fragment, or as a second state of an image the frame already
    /// drew, a pixel image is drawn flat and needs no texture.
    #[test]
    fn a_pixel_image_the_texture_cannot_serve_is_drawn_flat() {
        let mut image = PixelImage::new(4, 4);
        let sampler = ImageSampler::default();
        let mut painter = VelloPainter::new();
        assert!(painter.begin_fragment());
        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        let fragment = painter.end_fragment().expect("open");
        assert!(painter.take_pixels().0.is_empty());
        let scene = fragment.downcast::<Scene>().expect("a vello fragment");
        assert!(!scene.encoding().is_empty());

        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        image.fill_rgba8(0, 0, 4, 4, [9; 4]);
        painter.draw_pixels(&image, sampler, Affine::IDENTITY);
        let (drawn, _) = painter.take_pixels();
        assert_eq!(drawn.len(), 1, "the second state went flat");
        assert_eq!(drawn[0].0.version(), 0, "the texture holds the first");
    }

    /// A backend texture shows where nothing but clips encloses it, and is
    /// refused, with the reason, inside a fade, a blend, a mask, a fragment,
    /// or under a rotation.
    #[test]
    fn a_backend_texture_shows_only_where_it_can_be_shown_as_it_is() {
        let image = BackendImage {
            id: TextureId::new(),
            width: 8,
            height: 8,
            fallback: None,
        };
        let sampler = ImageSampler::default();
        let clip = |p: &mut VelloPainter, alpha: f32, blend: BlendMode| {
            p.push_layer(Fill::NonZero, blend, alpha, Affine::IDENTITY, &square());
        };

        let mut painter = VelloPainter::new();
        clip(&mut painter, 1.0, BlendMode::default());
        let at = Affine::translate((2.0, 3.0)) * Affine::scale(2.0);
        assert_eq!(painter.draw_texture(&image, sampler, at), Ok(()));
        assert_eq!(
            painter.layer_depth(),
            1,
            "the clip is open again after the hole"
        );
        painter.pop_layer();
        assert_eq!(
            painter.placements(),
            &[Placement {
                id: image.id,
                rect: Rect::new(2.0, 3.0, 18.0, 19.0),
                size: (8, 8),
                nearest: false,
            }]
        );

        let mut painter = VelloPainter::new();
        clip(&mut painter, 0.5, BlendMode::default());
        assert_eq!(
            painter.draw_texture(&image, sampler, Affine::IDENTITY),
            Err(TextureRefusal::Opacity)
        );
        let mut painter = VelloPainter::new();
        clip(
            &mut painter,
            1.0,
            BlendMode::new(Mix::Multiply, Compose::SrcOver),
        );
        assert_eq!(
            painter.draw_texture(&image, sampler, Affine::IDENTITY),
            Err(TextureRefusal::Blend)
        );
        let mut painter = VelloPainter::new();
        assert_eq!(
            painter.draw_texture(&image, sampler, Affine::rotate(0.3)),
            Err(TextureRefusal::Transform)
        );
        assert_eq!(
            painter.draw_texture(&image, sampler, Affine::scale_non_uniform(-1.0, 1.0)),
            Err(TextureRefusal::Transform)
        );
        let mut refused = None;
        painter.draw_masked(
            MaskKind::Alpha,
            Affine::IDENTITY,
            &square(),
            &mut |_| {},
            &mut |p| refused = Some(p.draw_texture(&image, sampler, Affine::IDENTITY)),
        );
        assert_eq!(refused, Some(Err(TextureRefusal::Mask)));
        assert!(painter.begin_fragment());
        assert_eq!(
            painter.draw_texture(&image, sampler, Affine::IDENTITY),
            Err(TextureRefusal::Layer)
        );
        painter.end_fragment();
        assert!(painter.placements().is_empty());
    }

    /// The escape hatch hands out the sink itself, and through it the scene
    /// a painter written against vello encodes into.
    #[test]
    fn the_native_target_reaches_the_scene() {
        let mut painter = VelloPainter::new();
        let sink = painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the native target is the sink");
        assert!(sink.scene_mut().encoding().is_empty());
        assert_eq!(painter.backend_id(), BACKEND_ID);
    }
}
