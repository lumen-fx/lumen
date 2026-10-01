//! A [`Painter`] that keeps what it is told, to paint it again later.
//!
//! For drawings made outside a frame and shown in one: a canvas records its
//! script's calls as they arrive and the render backend replays the
//! recording into whatever sink it paints with. The recording owns its
//! geometry, brushes, images, and glyphs, so it can cross into the render
//! world and outlive the call that made it.

use crate::{GlyphRun, Painter, Shape};
use lumen_text::GlyphPosition;
use peniko::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use peniko::{BlendMode, Brush, BrushRef, Color, Fill, FontData, ImageBrush};
use std::any::Any;

/// [`Shape`] with its path owned.
#[derive(Clone, Debug)]
enum OwnedShape {
    Rect(Rect),
    RoundedRect(RoundedRect),
    Path(BezPath),
}

impl OwnedShape {
    fn from_shape(shape: &Shape<'_>) -> Self {
        match shape {
            Shape::Rect(r) => OwnedShape::Rect(*r),
            Shape::RoundedRect(r) => OwnedShape::RoundedRect(*r),
            Shape::Path(p) => OwnedShape::Path((*p).clone()),
        }
    }

    fn as_shape(&self) -> Shape<'_> {
        match self {
            OwnedShape::Rect(r) => Shape::Rect(*r),
            OwnedShape::RoundedRect(r) => Shape::RoundedRect(*r),
            OwnedShape::Path(p) => Shape::Path(p),
        }
    }
}

/// One recorded call.
#[derive(Clone, Debug)]
enum Command {
    Fill {
        style: Fill,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        shape: OwnedShape,
    },
    Stroke {
        style: Stroke,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        shape: OwnedShape,
    },
    PushLayer {
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: OwnedShape,
    },
    PopLayer,
    BlurredRoundedRect {
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    },
    Image {
        image: ImageBrush,
        transform: Affine,
    },
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
                } => painter.draw_image(image, transform * *t),
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

    /// Popping with nothing open is ignored rather than recorded, so a
    /// replay can never close a layer the recording did not open.
    #[test]
    fn a_stray_pop_is_not_recorded() {
        let mut drawing = Recording::default();
        drawing.pop_layer();
        assert!(drawing.is_empty());
    }
}
