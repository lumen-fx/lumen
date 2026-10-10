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

use lumen_paint::{Fragment, GlyphRun, MaskKind, OffscreenLayer, Painter, Shape};
use std::any::Any;
use std::sync::{Arc, Weak};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{
    BlendMode, Blob, BrushRef, Color, Compose, Fill, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat, ImageQuality, ImageSampler, Mix,
};

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
}

impl Default for VelloPainter {
    fn default() -> Self {
        Self {
            scene: Scene::new(),
            recording: Vec::new(),
            offscreen: Vec::new(),
            finished: Vec::new(),
            max_layer_side: MAX_LAYER_SIDE,
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

    /// Empty the scene for the next frame, dropping any recording a caller
    /// left open.
    pub fn reset(&mut self) {
        self.scene.reset();
        self.recording.clear();
        self.offscreen.clear();
        self.finished.clear();
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

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
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
        let image = ImageData {
            data: Blob::new(Arc::new([0u8; 0])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: bounds.width().round() as u32,
            height: bounds.height().round() as u32,
        };
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
