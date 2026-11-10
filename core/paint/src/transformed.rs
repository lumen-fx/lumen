//! A [`Painter`] that moves everything it is handed by one transform before
//! passing it on: what the walker paints a CSS `transform` subtree through.
//!
//! Every geometry-carrying call composes the transform in front of its own,
//! so leaves, clips and glyph runs all move together. Fragments recorded
//! elsewhere replay under the transform; no new fragment is recorded through
//! it, because its calls are already moved and could not be replayed at
//! another place. Offscreen layers are declined the same way, so a faded
//! group inside a transform paints into a plain layer that moves with it.
//! [`Painter::native`] answers with the wrapper, not the backend: a caller
//! that needs the backend's own type paints untransformed or not at all, so
//! inside a transform it paints through [`Painter`].

use std::any::Any;

use peniko::kurbo::{Affine, Rect, Stroke};
use peniko::{BlendMode, BrushRef, Color, Fill, ImageBrush, ImageSampler};

use crate::recording::Recording;
use crate::{
    BackendImage, Fragment, GlyphRun, MaskKind, OffscreenLayer, Painter, PixelImage, Shape,
    TextureRefusal,
};

/// A sink that paints into `inner` under `transform`.
pub struct TransformedPainter {
    inner: Box<dyn Painter>,
    transform: Affine,
}

impl TransformedPainter {
    /// Paint into `inner`, each call moved by `transform` first.
    pub fn new(inner: Box<dyn Painter>, transform: Affine) -> Self {
        Self { inner, transform }
    }

    /// The sink this paints into.
    pub fn into_inner(self) -> Box<dyn Painter> {
        self.inner
    }
}

impl Painter for TransformedPainter {
    fn fill(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.inner.fill(
            style,
            self.transform * transform,
            brush,
            brush_transform,
            shape,
        );
    }

    fn stroke(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.inner.stroke(
            style,
            self.transform * transform,
            brush,
            brush_transform,
            shape,
        );
    }

    fn push_layer(
        &mut self,
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) {
        self.inner
            .push_layer(clip_style, blend, alpha, self.transform * transform, clip);
    }

    fn pop_layer(&mut self) {
        self.inner.pop_layer();
    }

    fn draw_masked(
        &mut self,
        kind: MaskKind,
        transform: Affine,
        region: &Shape<'_>,
        mask: &mut dyn FnMut(&mut dyn Painter),
        content: &mut dyn FnMut(&mut dyn Painter),
    ) {
        // The callbacks paint in the caller's coordinates; record them and
        // replay them moved, so mask and content land where the region does.
        let mut recorded_mask = Recording::default();
        mask(&mut recorded_mask);
        let mut recorded_content = Recording::default();
        content(&mut recorded_content);
        let moved = self.transform;
        self.inner.draw_masked(
            kind,
            moved * transform,
            region,
            &mut |p| recorded_mask.replay(p, moved),
            &mut |p| recorded_content.replay(p, moved),
        );
    }

    fn layer_depth(&self) -> usize {
        self.inner.layer_depth()
    }

    fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.inner.draw_blurred_rounded_rect(
            self.transform * transform,
            rect,
            color,
            radius,
            std_dev,
        );
    }

    fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
        self.inner.draw_image(image, self.transform * transform);
    }

    fn draw_pixels(&mut self, image: &PixelImage, sampler: ImageSampler, transform: Affine) {
        self.inner
            .draw_pixels(image, sampler, self.transform * transform);
    }

    fn draw_texture(
        &mut self,
        image: &BackendImage<'_>,
        sampler: ImageSampler,
        transform: Affine,
    ) -> Result<(), TextureRefusal> {
        self.inner
            .draw_texture(image, sampler, self.transform * transform)
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        self.inner.draw_glyphs(&GlyphRun {
            transform: self.transform * run.transform,
            ..*run
        });
    }

    fn append_fragment(&mut self, fragment: &Fragment, transform: Affine) -> bool {
        self.inner
            .append_fragment(fragment, self.transform * transform)
    }

    fn draw_offscreen(&mut self, _layer: &OffscreenLayer, _alpha: f32) -> bool {
        false
    }

    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }

    /// This wrapper itself, so the walker can take its sink back out: a caller reaching for the
    /// backend's own type finds none inside a transform, and paints through [`Painter`] there.
    fn native(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::Command;

    /// Every call reaches the inner sink moved by the transform, the
    /// callbacks of a masked draw included.
    #[test]
    fn moves_fills_layers_and_masked_content() {
        let moved = Affine::translate((5.0, 7.0));
        let mut sink = TransformedPainter::new(Box::new(Recording::default()), moved);
        let shape = Shape::Rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        sink.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE.into(),
            None,
            &shape,
        );
        sink.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &shape,
        );
        sink.pop_layer();
        sink.draw_masked(
            MaskKind::Alpha,
            Affine::IDENTITY,
            &shape,
            &mut |p| {
                p.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::WHITE.into(),
                    None,
                    &Shape::Rect(Rect::new(0.0, 0.0, 1.0, 1.0)),
                )
            },
            &mut |_| {},
        );
        let mut inner = sink.into_inner();
        let recorded = inner
            .native()
            .downcast_mut::<Recording>()
            .expect("the inner sink");
        let transforms: Vec<Affine> = recorded
            .commands()
            .iter()
            .filter_map(|c| match c {
                Command::Fill { transform, .. } | Command::PushLayer { transform, .. } => {
                    Some(*transform)
                }
                Command::Masked {
                    transform, mask, ..
                } => {
                    let Some(Command::Fill { transform: m, .. }) = mask.commands().first() else {
                        panic!("the mask callback was recorded");
                    };
                    assert_eq!(*m, moved, "mask content moves with the region");
                    Some(*transform)
                }
                _ => None,
            })
            .collect();
        assert_eq!(transforms, vec![moved, moved, moved]);
    }
}
