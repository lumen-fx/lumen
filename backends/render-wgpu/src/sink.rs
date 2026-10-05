//! The [`Painter`] sink this backend paints frames through: a `vello::Scene`.
//!
//! Every call encodes straight into the scene vello renders. Fragments are
//! sub-scenes: [`Painter::begin_fragment`] redirects encoding into a fresh
//! scene, and replaying one is a `Scene::append`, so a repeated appearance is
//! encoded once and appended at every position it takes.

use lumen_paint::{Fragment, GlyphRun, MaskKind, Painter, Shape};
use std::any::Any;
use std::sync::Arc;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{BlendMode, BrushRef, Color, Compose, Fill, ImageBrush, Mix};

/// Names this backend in [`Painter::backend_id`] and
/// [`lumen_core::native::NativePaintCtx::backend_id`]. [`Painter::native`]
/// on this backend's sink is a [`VelloPainter`].
pub const BACKEND_ID: &str = "lumen.render-wgpu";

/// A [`Painter`] encoding into a [`vello::Scene`].
#[derive(Default)]
pub struct VelloPainter {
    scene: Scene,
    /// Fragments being recorded, innermost last. Encoding goes to the top
    /// of this stack when it is not empty.
    recording: Vec<Scene>,
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
    }

    /// Where the next call encodes: the innermost open fragment, else the
    /// frame.
    fn target(&mut self) -> &mut Scene {
        self.recording.last_mut().unwrap_or(&mut self.scene)
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
        self.recording
            .last()
            .unwrap_or(&self.scene)
            .encoding()
            .n_open_clips as usize
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
