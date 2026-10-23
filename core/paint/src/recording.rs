//! A [`Painter`] that keeps what it is told, to paint it again later.
//!
//! For drawings made outside a frame and shown in one: a canvas records its
//! script's calls as they arrive and the render backend replays the
//! recording into whatever sink it paints with. The recording owns its
//! geometry, brushes, images, and glyphs, so it can cross into the render
//! world and outlive the call that made it.

use crate::{
    BackendImage, GlyphRun, MaskKind, Painter, PixelImage, Shape, TextureId, TextureRefusal,
};
use lumen_text::GlyphPosition;
use peniko::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use peniko::{
    BlendMode, Brush, BrushRef, Color, Fill, FontData, ImageBrush, ImageQuality, ImageSampler,
};
use std::any::Any;

/// [`Shape`] with its path owned.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedShape {
    /// An axis-aligned rectangle.
    Rect(Rect),
    /// A rectangle with per-corner radii.
    RoundedRect(RoundedRect),
    /// Any path.
    Path(BezPath),
}

impl From<&Shape<'_>> for OwnedShape {
    fn from(shape: &Shape<'_>) -> Self {
        match shape {
            Shape::Rect(r) => OwnedShape::Rect(*r),
            Shape::RoundedRect(r) => OwnedShape::RoundedRect(*r),
            Shape::Path(p) => OwnedShape::Path((*p).clone()),
        }
    }
}

impl OwnedShape {
    fn from_shape(shape: &Shape<'_>) -> Self {
        Self::from(shape)
    }

    /// The borrowed form a [`Painter`] takes.
    pub fn as_shape(&self) -> Shape<'_> {
        match self {
            OwnedShape::Rect(r) => Shape::Rect(*r),
            OwnedShape::RoundedRect(r) => Shape::RoundedRect(*r),
            OwnedShape::Path(p) => Shape::Path(p),
        }
    }
}

/// One recorded call, with the arguments [`Painter`] was given.
#[derive(Clone, Debug)]
#[allow(
    missing_docs,
    reason = "each field is the Painter argument of the same name"
)]
pub enum Command {
    /// [`Painter::fill`].
    Fill {
        style: Fill,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        shape: OwnedShape,
    },
    /// [`Painter::stroke`].
    Stroke {
        style: Stroke,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        shape: OwnedShape,
    },
    /// [`Painter::push_layer`].
    PushLayer {
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: OwnedShape,
    },
    /// [`Painter::pop_layer`].
    PopLayer,
    /// [`Painter::draw_masked`], with what each callback painted.
    Masked {
        kind: MaskKind,
        transform: Affine,
        region: OwnedShape,
        mask: Recording,
        content: Recording,
    },
    /// [`Painter::draw_blurred_rounded_rect`].
    BlurredRoundedRect {
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    },
    /// [`Painter::draw_image`].
    Image {
        image: ImageBrush,
        transform: Affine,
    },
    /// [`Painter::draw_pixels`]. The image is a clone, so the recording keeps
    /// the pixels it was drawn with.
    Pixels {
        image: PixelImage,
        sampler: ImageSampler,
        transform: Affine,
    },
    /// [`Painter::draw_texture`]. The fallback is a clone, like
    /// [`Command::Pixels`]'s image.
    Texture {
        id: TextureId,
        width: u32,
        height: u32,
        fallback: Option<PixelImage>,
        sampler: ImageSampler,
        transform: Affine,
    },
    /// [`Painter::draw_glyphs`].
    Glyphs {
        font: FontData,
        font_size: f32,
        normalized_coords: Vec<i16>,
        transform: Affine,
        brush: Brush,
        glyphs: Vec<GlyphPosition>,
    },
}

/// The calls a [`Painter`] was given, in order, ready to replay.
#[derive(Clone, Debug, Default)]
pub struct Recording {
    commands: Vec<Command>,
    depth: usize,
}

impl Recording {
    /// Number of recorded calls.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// The recorded calls, in order.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Forget every recorded call.
    pub fn clear(&mut self) {
        self.commands.clear();
        self.depth = 0;
    }

    /// Paint every recorded call into `painter`, each under `transform`
    /// followed by the transform it was recorded with. Layers left open by
    /// the recording are closed at the end, so a replay leaves `painter` at
    /// the depth it started at.
    pub fn replay(&self, painter: &mut dyn Painter, transform: Affine) {
        self.replay_images_at(painter, transform, None);
    }

    /// [`Self::replay`], with every recorded image and pixel buffer sampled at
    /// `quality` whatever it was recorded with: what CSS `image-rendering` on
    /// the element showing the recording asks for.
    pub fn replay_sampled(
        &self,
        painter: &mut dyn Painter,
        transform: Affine,
        quality: ImageQuality,
    ) {
        self.replay_images_at(painter, transform, Some(quality));
    }

    fn replay_images_at(
        &self,
        painter: &mut dyn Painter,
        transform: Affine,
        quality: Option<ImageQuality>,
    ) {
        let mut opened = 0usize;
        for command in &self.commands {
            match command {
                Command::Fill {
                    style,
                    transform: t,
                    brush,
                    brush_transform,
                    shape,
                } => painter.fill(
                    *style,
                    transform * *t,
                    BrushRef::from(brush),
                    *brush_transform,
                    &shape.as_shape(),
                ),
                Command::Stroke {
                    style,
                    transform: t,
                    brush,
                    brush_transform,
                    shape,
                } => painter.stroke(
                    style,
                    transform * *t,
                    BrushRef::from(brush),
                    *brush_transform,
                    &shape.as_shape(),
                ),
                Command::PushLayer {
                    clip_style,
                    blend,
                    alpha,
                    transform: t,
                    clip,
                } => {
                    painter.push_layer(
                        *clip_style,
                        *blend,
                        *alpha,
                        transform * *t,
                        &clip.as_shape(),
                    );
                    opened += 1;
                }
                Command::PopLayer => {
                    if opened > 0 {
                        painter.pop_layer();
                        opened -= 1;
                    }
                }
                Command::Masked {
                    kind,
                    transform: t,
                    region,
                    mask,
                    content,
                } => painter.draw_masked(
                    *kind,
                    transform * *t,
                    &region.as_shape(),
                    &mut |p| mask.replay_images_at(p, transform, quality),
                    &mut |p| content.replay_images_at(p, transform, quality),
                ),
                Command::BlurredRoundedRect {
                    transform: t,
                    rect,
                    color,
                    radius,
                    std_dev,
                } => painter.draw_blurred_rounded_rect(
                    transform * *t,
                    *rect,
                    *color,
                    *radius,
                    *std_dev,
                ),
                Command::Image {
                    image,
                    transform: t,
                } => match quality {
                    Some(quality) if image.sampler.quality != quality => {
                        let mut image = image.clone();
                        image.sampler.quality = quality;
                        painter.draw_image(&image, transform * *t);
                    }
                    _ => painter.draw_image(image, transform * *t),
                },
                Command::Pixels {
                    image,
                    sampler,
                    transform: t,
                } => {
                    let sampler = quality.map_or(*sampler, |q| sampler.with_quality(q));
                    painter.draw_pixels(image, sampler, transform * *t);
                }
                Command::Texture {
                    id,
                    width,
                    height,
                    fallback,
                    sampler,
                    transform: t,
                } => {
                    let sampler = quality.map_or(*sampler, |q| sampler.with_quality(q));
                    // A refusal is the sink's to report; a replay has nobody
                    // to hand it to.
                    let _ = painter.draw_texture(
                        &BackendImage {
                            id: *id,
                            width: *width,
                            height: *height,
                            fallback: fallback.as_ref(),
                        },
                        sampler,
                        transform * *t,
                    );
                }
                Command::Glyphs {
                    font,
                    font_size,
                    normalized_coords,
                    transform: t,
                    brush,
                    glyphs,
                } => painter.draw_glyphs(&GlyphRun {
                    font,
                    font_size: *font_size,
                    normalized_coords,
                    transform: transform * *t,
                    brush: BrushRef::from(brush),
                    glyphs,
                }),
            }
        }
        for _ in 0..opened {
            painter.pop_layer();
        }
    }
}

/// The owned brush for a borrowed one.
fn owned(brush: BrushRef<'_>) -> Brush {
    match brush {
        Brush::Solid(color) => Brush::Solid(color),
        Brush::Gradient(gradient) => Brush::Gradient(gradient.clone()),
        Brush::Image(image) => Brush::Image(image.to_owned()),
    }
}

impl Painter for Recording {
    fn fill(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.commands.push(Command::Fill {
            style,
            transform,
            brush: owned(brush),
            brush_transform,
            shape: OwnedShape::from_shape(shape),
        });
    }

    fn stroke(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    ) {
        self.commands.push(Command::Stroke {
            style: style.clone(),
            transform,
            brush: owned(brush),
            brush_transform,
            shape: OwnedShape::from_shape(shape),
        });
    }

    fn push_layer(
        &mut self,
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) {
        self.depth += 1;
        self.commands.push(Command::PushLayer {
            clip_style,
            blend,
            alpha,
            transform,
            clip: OwnedShape::from_shape(clip),
        });
    }

    fn pop_layer(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
            self.commands.push(Command::PopLayer);
        }
    }

    fn layer_depth(&self) -> usize {
        self.depth
    }

    fn draw_masked(
        &mut self,
        kind: MaskKind,
        transform: Affine,
        region: &Shape<'_>,
        mask: &mut dyn FnMut(&mut dyn Painter),
        content: &mut dyn FnMut(&mut dyn Painter),
    ) {
        let mut mask_drawing = Recording::default();
        mask(&mut mask_drawing);
        let mut content_drawing = Recording::default();
        content(&mut content_drawing);
        self.commands.push(Command::Masked {
            kind,
            transform,
            region: OwnedShape::from_shape(region),
            mask: mask_drawing,
            content: content_drawing,
        });
    }

    fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.commands.push(Command::BlurredRoundedRect {
            transform,
            rect,
            color,
            radius,
            std_dev,
        });
    }

    fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
        self.commands.push(Command::Image {
            image: image.clone(),
            transform,
        });
    }

    fn draw_pixels(&mut self, image: &PixelImage, sampler: ImageSampler, transform: Affine) {
        self.commands.push(Command::Pixels {
            image: image.clone(),
            sampler,
            transform,
        });
    }

    /// Recorded as it was asked; whether the texture shows is up to the sink
    /// the recording replays into.
    fn draw_texture(
        &mut self,
        image: &BackendImage<'_>,
        sampler: ImageSampler,
        transform: Affine,
    ) -> Result<(), TextureRefusal> {
        self.commands.push(Command::Texture {
            id: image.id,
            width: image.width,
            height: image.height,
            fallback: image.fallback.cloned(),
            sampler,
            transform,
        });
        Ok(())
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        self.commands.push(Command::Glyphs {
            font: run.font.clone(),
            font_size: run.font_size,
            normalized_coords: run.normalized_coords.to_vec(),
            transform: run.transform,
            brush: owned(run.brush),
            glyphs: run.glyphs.to_vec(),
        });
    }

    fn backend_id(&self) -> &'static str {
        "lumen.recording"
    }

    fn native(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Shape<'static> {
        Shape::Rect(Rect::new(0.0, 0.0, 10.0, 10.0))
    }

    /// A replay hands every call on in the order it was recorded, with the
    /// replay's transform in front of each call's own.
    #[test]
    fn a_replay_paints_every_call_in_order_under_the_outer_transform() {
        let mut drawing = Recording::default();
        drawing.fill(
            Fill::NonZero,
            Affine::translate((1.0, 2.0)),
            Color::new([1.0, 0.0, 0.0, 1.0]).into(),
            None,
            &square(),
        );
        drawing.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            0.5,
            Affine::IDENTITY,
            &square(),
        );
        drawing.pop_layer();

        let mut copy = Recording::default();
        drawing.replay(&mut copy, Affine::translate((10.0, 0.0)));

        assert_eq!(copy.len(), 3);
        match &copy.commands[0] {
            Command::Fill { transform, .. } => {
                assert_eq!(transform.translation(), (11.0, 2.0).into());
            }
            other => panic!("expected a fill first, got {other:?}"),
        }
        assert_eq!(copy.layer_depth(), 0);
    }

    /// A recording that left a layer open must not leave the frame it is
    /// replayed into one layer deeper, or the walker's own pops would close
    /// the wrong layers.
    #[test]
    fn a_replay_closes_the_layers_its_recording_left_open() {
        let mut drawing = Recording::default();
        drawing.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &square(),
        );

        let mut frame = Recording::default();
        drawing.replay(&mut frame, Affine::IDENTITY);

        assert_eq!(frame.layer_depth(), 0);
        assert_eq!(frame.len(), 2, "the push and the closing pop");
    }

    /// A masked draw keeps what its mask and its content painted, and a
    /// replay hands both on as one masked draw under the replay's transform.
    #[test]
    fn a_masked_draw_records_both_halves_and_replays_them_moved() {
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        let mut drawing = Recording::default();
        drawing.draw_masked(
            MaskKind::Alpha,
            Affine::translate((1.0, 0.0)),
            &square(),
            &mut |p| p.fill(Fill::NonZero, Affine::IDENTITY, red.into(), None, &square()),
            &mut |p| {
                p.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    Affine::IDENTITY,
                    &square(),
                );
                p.fill(Fill::NonZero, Affine::IDENTITY, red.into(), None, &square());
            },
        );
        assert_eq!(drawing.len(), 1);
        assert_eq!(drawing.layer_depth(), 0);

        let mut copy = Recording::default();
        drawing.replay(&mut copy, Affine::translate((10.0, 0.0)));
        let Command::Masked {
            kind,
            transform,
            region,
            mask,
            content,
        } = &copy.commands()[0]
        else {
            panic!("expected a masked draw, got {:?}", copy.commands());
        };
        assert_eq!(*kind, MaskKind::Alpha);
        assert_eq!(transform.translation(), (11.0, 0.0).into());
        assert_eq!(*region, OwnedShape::from_shape(&square()));
        assert!(
            matches!(&mask.commands()[0], Command::Fill { transform, .. } if transform.translation() == (10.0, 0.0).into())
        );
        assert_eq!(
            content.len(),
            3,
            "the layer the content left open is closed: {:?}",
            content.commands(),
        );
        assert_eq!(copy.layer_depth(), 0);
    }

    /// Popping with nothing open is ignored rather than recorded, so a
    /// replay can never close a layer the recording did not open.
    #[test]
    fn a_stray_pop_is_not_recorded() {
        let mut drawing = Recording::default();
        drawing.pop_layer();
        assert!(drawing.is_empty());
    }

    /// Every kind of call survives the round trip: recorded with its own
    /// arguments, owned, and replayed with the replay's transform in front.
    #[test]
    fn every_call_kind_records_and_replays() {
        use peniko::kurbo::{Point, Shape as _};
        use peniko::{Blob, Gradient, ImageAlphaType, ImageData, ImageFormat};
        use std::sync::Arc;

        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((4.0, 0.0));
        path.line_to((0.0, 4.0));
        path.close_path();
        let rounded = RoundedRect::new(0.0, 0.0, 8.0, 8.0, 2.0);
        let gradient = Gradient::new_linear(Point::ZERO, Point::new(8.0, 0.0)).with_stops(
            [
                Color::new([1.0, 0.0, 0.0, 1.0]),
                Color::new([0.0, 0.0, 1.0, 1.0]),
            ]
            .as_slice(),
        );
        let image = ImageBrush::new(ImageData {
            data: Blob::new(Arc::new(vec![255u8; 4])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        });
        let font = FontData::new(Blob::new(Arc::new(Vec::new())), 0);
        let glyphs = [GlyphPosition {
            id: 7,
            x: 1.0,
            y: 2.0,
            advance: 3.0,
            byte_start: 0,
            byte_end: 1,
        }];
        let at = Affine::translate((1.0, 1.0));

        let mut drawing = Recording::default();
        drawing.fill(
            Fill::EvenOdd,
            at,
            BrushRef::Gradient(&gradient),
            Some(Affine::scale(2.0)),
            &Shape::Path(&path),
        );
        drawing.stroke(
            &Stroke::new(3.0),
            at,
            BrushRef::Image(image.as_ref()),
            None,
            &Shape::RoundedRect(rounded),
        );
        drawing.draw_blurred_rounded_rect(
            at,
            Rect::new(0.0, 0.0, 4.0, 4.0),
            Color::new([0.0, 0.0, 0.0, 0.5]),
            1.0,
            2.0,
        );
        drawing.draw_image(&image, at);
        drawing.draw_glyphs(&GlyphRun {
            font: &font,
            font_size: 12.0,
            normalized_coords: &[1, 2],
            transform: at,
            brush: Color::new([0.0, 1.0, 0.0, 1.0]).into(),
            glyphs: &glyphs,
        });
        assert_eq!(drawing.len(), 5);
        assert!(!drawing.is_empty());
        assert_eq!(drawing.backend_id(), "lumen.recording");
        assert!(drawing.native().downcast_mut::<Recording>().is_some());

        let mut copy = Recording::default();
        drawing.replay(&mut copy, Affine::translate((10.0, 0.0)));
        let moved = |t: &Affine| t.translation() == (11.0, 1.0).into();

        match &copy.commands()[0] {
            Command::Fill {
                style,
                transform,
                brush: Brush::Gradient(g),
                brush_transform,
                shape: OwnedShape::Path(p),
            } => {
                assert_eq!(*style, Fill::EvenOdd);
                assert!(moved(transform));
                assert_eq!(g.stops.len(), 2);
                assert_eq!(*brush_transform, Some(Affine::scale(2.0)));
                assert_eq!(p.bounding_box(), path.bounding_box());
            }
            other => panic!("expected the gradient fill, got {other:?}"),
        }
        match &copy.commands()[1] {
            Command::Stroke {
                style,
                transform,
                brush: Brush::Image(_),
                shape: OwnedShape::RoundedRect(r),
                ..
            } => {
                assert_eq!(style.width, 3.0);
                assert!(moved(transform));
                assert_eq!(*r, rounded);
            }
            other => panic!("expected the image stroke, got {other:?}"),
        }
        match &copy.commands()[2] {
            Command::BlurredRoundedRect {
                transform,
                radius,
                std_dev,
                ..
            } => {
                assert!(moved(transform));
                assert_eq!((*radius, *std_dev), (1.0, 2.0));
            }
            other => panic!("expected the blurred rect, got {other:?}"),
        }
        match &copy.commands()[3] {
            Command::Image { image, transform } => {
                assert!(moved(transform));
                assert_eq!(image.image.width, 1);
            }
            other => panic!("expected the image, got {other:?}"),
        }
        match &copy.commands()[4] {
            Command::Glyphs {
                font_size,
                normalized_coords,
                transform,
                glyphs,
                ..
            } => {
                assert_eq!(*font_size, 12.0);
                assert_eq!(normalized_coords, &[1, 2]);
                assert!(moved(transform));
                assert_eq!(glyphs[0].id, 7);
            }
            other => panic!("expected the glyph run, got {other:?}"),
        }

        drawing.clear();
        assert!(drawing.is_empty());
        assert_eq!(drawing.layer_depth(), 0);
    }

    /// A recorded pixel image replays with the pixels it was drawn with,
    /// whatever was written to the image afterwards.
    #[test]
    fn a_recorded_pixel_image_keeps_the_pixels_it_was_drawn_with() {
        let mut image = PixelImage::new(4, 4);
        image.fill_rgba8(0, 0, 4, 4, [255, 0, 0, 255]);
        let mut drawing = Recording::default();
        let nearest = ImageSampler::default().with_quality(peniko::ImageQuality::Low);
        drawing.draw_pixels(&image, nearest, Affine::IDENTITY);
        image.fill_rgba8(0, 0, 4, 4, [0, 0, 255, 255]);

        let mut copy = Recording::default();
        drawing.replay(&mut copy, Affine::translate((3.0, 0.0)));
        match &copy.commands()[0] {
            Command::Pixels {
                image: drawn,
                sampler,
                transform,
            } => {
                assert_eq!(drawn.pixel(1, 1), [255, 0, 0, 255]);
                assert_eq!(drawn.id(), image.id());
                assert_eq!(sampler.quality, peniko::ImageQuality::Low);
                assert_eq!(transform.translation(), (3.0, 0.0).into());
            }
            other => panic!("expected the pixels, got {other:?}"),
        }
    }

    /// `image-rendering` on the element showing a recording reaches the pixel
    /// buffers drawn on it as well as its images.
    #[test]
    fn a_sampled_replay_overrides_how_a_pixel_image_is_sampled() {
        let image = PixelImage::new(2, 2);
        let mut drawing = Recording::default();
        drawing.draw_pixels(&image, ImageSampler::default(), Affine::IDENTITY);

        let mut copy = Recording::default();
        drawing.replay_sampled(&mut copy, Affine::IDENTITY, peniko::ImageQuality::Low);
        match &copy.commands()[0] {
            Command::Pixels { sampler, .. } => {
                assert_eq!(sampler.quality, peniko::ImageQuality::Low);
            }
            other => panic!("expected the pixels, got {other:?}"),
        }
    }

    /// A sink that keeps no resident copies draws a pixel image as a flat
    /// image, sampled the same way.
    #[test]
    fn the_default_draws_pixels_as_a_flat_image() {
        struct Flat(Vec<ImageBrush>);
        impl Painter for Flat {
            fn fill(
                &mut self,
                _: Fill,
                _: Affine,
                _: BrushRef<'_>,
                _: Option<Affine>,
                _: &Shape<'_>,
            ) {
            }
            fn stroke(
                &mut self,
                _: &Stroke,
                _: Affine,
                _: BrushRef<'_>,
                _: Option<Affine>,
                _: &Shape<'_>,
            ) {
            }
            fn push_layer(&mut self, _: Fill, _: BlendMode, _: f32, _: Affine, _: &Shape<'_>) {}
            fn pop_layer(&mut self) {}
            fn draw_masked(
                &mut self,
                _: MaskKind,
                _: Affine,
                _: &Shape<'_>,
                _: &mut dyn FnMut(&mut dyn Painter),
                _: &mut dyn FnMut(&mut dyn Painter),
            ) {
            }
            fn layer_depth(&self) -> usize {
                0
            }
            fn draw_blurred_rounded_rect(&mut self, _: Affine, _: Rect, _: Color, _: f64, _: f64) {}
            fn draw_image(&mut self, image: &ImageBrush, _: Affine) {
                self.0.push(image.clone());
            }
            fn draw_glyphs(&mut self, _: &GlyphRun<'_>) {}
            fn backend_id(&self) -> &'static str {
                "test.flat"
            }
            fn native(&mut self) -> &mut dyn Any {
                self
            }
        }
        let mut image = PixelImage::new(3, 2);
        image.fill_rgba8(2, 1, 1, 1, [1, 2, 3, 4]);
        let mut sink = Flat(Vec::new());
        let nearest = ImageSampler::default().with_quality(peniko::ImageQuality::Low);
        sink.draw_pixels(&image, nearest, Affine::IDENTITY);
        sink.draw_pixels(&PixelImage::new(0, 5), nearest, Affine::IDENTITY);
        assert_eq!(sink.0.len(), 1, "an empty image draws nothing");
        assert_eq!(sink.0[0].sampler.quality, peniko::ImageQuality::Low);
        assert_eq!(&sink.0[0].image.data.data()[20..24], &[1, 2, 3, 4]);
    }

    /// An owned shape hands back the same geometry it was made from.
    #[test]
    fn an_owned_shape_borrows_back_as_the_same_shape() {
        let rounded = RoundedRect::new(0.0, 0.0, 4.0, 4.0, 1.0);
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((1.0, 1.0));
        for shape in [square(), Shape::RoundedRect(rounded), Shape::Path(&path)] {
            let owned = OwnedShape::from_shape(&shape);
            assert_eq!(OwnedShape::from_shape(&owned.as_shape()), owned);
        }
    }

    /// A painter with no backend textures of its own: everything else goes
    /// to a recording, so the trait's own `draw_texture` runs.
    struct NoTextures(Recording);

    impl Painter for NoTextures {
        fn fill(
            &mut self,
            style: Fill,
            transform: Affine,
            brush: BrushRef<'_>,
            brush_transform: Option<Affine>,
            shape: &Shape<'_>,
        ) {
            self.0.fill(style, transform, brush, brush_transform, shape);
        }
        fn stroke(
            &mut self,
            style: &Stroke,
            transform: Affine,
            brush: BrushRef<'_>,
            brush_transform: Option<Affine>,
            shape: &Shape<'_>,
        ) {
            self.0
                .stroke(style, transform, brush, brush_transform, shape);
        }
        fn push_layer(
            &mut self,
            clip_style: Fill,
            blend: BlendMode,
            alpha: f32,
            transform: Affine,
            clip: &Shape<'_>,
        ) {
            self.0.push_layer(clip_style, blend, alpha, transform, clip);
        }
        fn pop_layer(&mut self) {
            self.0.pop_layer();
        }
        fn draw_masked(
            &mut self,
            kind: MaskKind,
            transform: Affine,
            region: &Shape<'_>,
            mask: &mut dyn FnMut(&mut dyn Painter),
            content: &mut dyn FnMut(&mut dyn Painter),
        ) {
            self.0.draw_masked(kind, transform, region, mask, content);
        }
        fn layer_depth(&self) -> usize {
            self.0.layer_depth()
        }
        fn draw_blurred_rounded_rect(
            &mut self,
            transform: Affine,
            rect: Rect,
            color: Color,
            radius: f64,
            std_dev: f64,
        ) {
            self.0
                .draw_blurred_rounded_rect(transform, rect, color, radius, std_dev);
        }
        fn draw_image(&mut self, image: &ImageBrush, transform: Affine) {
            self.0.draw_image(image, transform);
        }
        fn draw_pixels(&mut self, image: &PixelImage, sampler: ImageSampler, transform: Affine) {
            self.0.draw_pixels(image, sampler, transform);
        }
        fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
            self.0.draw_glyphs(run);
        }
        fn backend_id(&self) -> &'static str {
            "no-textures"
        }
        fn native(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// A recorded texture replays as a texture, under the replay's
    /// transform; a painter that cannot show textures draws the fallback
    /// pixels in its place, scaled to the texture's size, and says so.
    #[test]
    fn a_texture_replays_and_falls_back_to_its_pixels() {
        let fallback = PixelImage::new(2, 1);
        let image = BackendImage {
            id: TextureId::new(),
            width: 8,
            height: 4,
            fallback: Some(&fallback),
        };
        let mut drawing = Recording::default();
        assert_eq!(
            drawing.draw_texture(
                &image,
                ImageSampler::default(),
                Affine::translate((1.0, 0.0))
            ),
            Ok(())
        );
        let mut copy = Recording::default();
        drawing.replay(&mut copy, Affine::translate((10.0, 0.0)));
        let Command::Texture {
            id,
            width,
            transform,
            fallback: kept,
            ..
        } = &copy.commands()[0]
        else {
            panic!("expected a texture, got {:?}", copy.commands());
        };
        assert_eq!((*id, *width), (image.id, 8));
        assert_eq!(transform.translation(), (11.0, 0.0).into());
        assert!(kept.is_some());

        let mut plain = NoTextures(Recording::default());
        assert_eq!(
            plain.draw_texture(&image, ImageSampler::default(), Affine::IDENTITY),
            Err(TextureRefusal::Unavailable)
        );
        let Command::Pixels { transform, .. } = &plain.0.commands()[0] else {
            panic!("expected the fallback, got {:?}", plain.0.commands());
        };
        assert_eq!(
            transform.as_coeffs()[0],
            4.0,
            "two pixels stretched to eight"
        );
        let bare = BackendImage {
            fallback: None,
            ..image
        };
        drawing.replay(&mut plain, Affine::IDENTITY);
        assert_eq!(
            plain.draw_texture(&bare, ImageSampler::default(), Affine::IDENTITY),
            Err(TextureRefusal::Unavailable)
        );
        assert_eq!(plain.0.len(), 2, "no fallback draws nothing");
    }
}
