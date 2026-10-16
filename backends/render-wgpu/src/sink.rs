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
//! Outline glyphs reach the scene as the polygons [`GlyphOutlines`] flattens
//! them to, the same lines every backend fills. Each flattened glyph is kept
//! encoded, and a run copies those encodings into the scene under one
//! translation per glyph: the layout vello's own glyph runs resolve to, so a
//! frame of text costs a copy per glyph rather than a re-encode.

use lumen_paint::glyphs::{Contours, GlyphShape, brush_transform};
use lumen_paint::{
    Fragment, GlyphOutlines, GlyphRun, MaskKind, OffscreenLayer, Painter, PixelImage, Shape,
};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{
    BlendMode, Blob, BrushRef, Color, Compose, Fill, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat, ImageQuality, ImageSampler, Mix,
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

/// An image with a blob of its own and no bytes: a unique image id the
/// renderer stands a texture in for.
fn stand_in(width: u32, height: u32) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new([0u8; 0])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
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
        match self.offscreen.last_mut() {
            Some((layer, _)) => layer,
            None => &mut self.scene,
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
        let scene = self.target();
        match clip {
            Shape::Rect(r) => scene.push_layer(clip_style, blend, alpha, transform, r),
            Shape::RoundedRect(r) => scene.push_layer(clip_style, blend, alpha, transform, r),
            Shape::Path(p) => scene.push_layer(clip_style, blend, alpha, transform, *p),
        }
    }

    fn pop_layer(&mut self) {
        self.target().pop_layer();
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
        self.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, region);
        content(self);
        while self.layer_depth() > outer + 1 {
            self.pop_layer();
        }
        match kind {
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
    }

    fn layer_depth(&self) -> usize {
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
            // Larger than any texture, or vello's atlas, holds.
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
        self.target().draw_image(
            &ImageBrush {
                image: stand_in,
                sampler,
            },
            transform,
        );
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
        self.target().draw_image(
            &brush,
            Affine::translate((layer.bounds.x0, layer.bounds.y0)),
        );
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
