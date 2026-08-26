//! The draw-command list Lumen passes around as data.
//!
//! A [`PaintList`] is a flat, serializable sequence of drawing operations: a
//! producer records what it wants on screen, and the engine replays it through
//! whichever renderer is installed. Because the list is plain data it survives
//! being written to bytes, so a producer living outside the engine's address
//! space can hand one over the same way an in-process one does.
//!
//! The format is renderer-neutral by construction. Paths carry line and Bezier
//! segments, brushes carry solid colors and gradients, and nothing here names a
//! graphics API or a geometry crate; a renderer maps the ops onto its own
//! primitives (`vello` reads them straight onto `kurbo` paths).
//!
//! All coordinates are node-local logical pixels with the origin at the
//! top-left corner of the node being painted, y growing downwards. Scaling to
//! physical pixels is the renderer's job.

use serde::{Deserialize, Serialize};

use crate::components::Color;

/// Encoding version of the types in this module.
///
/// Covers [`PaintList`], [`PaintOp`], [`PaintPath`], [`PathEl`], [`PaintBrush`],
/// [`GradientStop`], [`FillRule`], [`Cap`], and [`Join`].
///
/// Enum variants are append-only. The encoding writes a variant by its index,
/// so inserting or reordering one silently reinterprets every variant after it
/// as a different op. Add new variants at the end of their enum. Any other
/// change to a shape here (a renamed or retyped field, a removed variant) bumps
/// this constant.
pub const PAINT_WIRE_VERSION: u16 = 1;

/// An ordered list of drawing operations.
///
/// Ops apply in order against a renderer state stack that starts empty:
/// [`PaintOp::Save`] and [`PaintOp::Restore`] bracket transform and clip
/// changes, and [`PaintOp::PushLayer`] and [`PaintOp::PopLayer`] bracket
/// composited groups.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaintList {
    /// The operations, in the order they are drawn.
    pub ops: Vec<PaintOp>,
}

/// One drawing operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaintOp {
    /// Push the current transform and clip onto the state stack.
    Save,
    /// Pop the state stack, restoring the transform and clip `Save` recorded.
    Restore,
    /// Multiply the current transform by an affine matrix, given in column
    /// order as `[a, b, c, d, e, f]`: `x' = a*x + c*y + e`,
    /// `y' = b*x + d*y + f`.
    Transform([f32; 6]),
    /// Intersect the current clip with a path.
    Clip(PaintPath),
    /// Fill a path.
    Fill {
        /// Region to fill.
        path: PaintPath,
        /// Paint to fill it with.
        brush: PaintBrush,
        /// How overlapping subpaths decide what is inside.
        rule: FillRule,
    },
    /// Stroke a path.
    Stroke {
        /// Path to trace.
        path: PaintPath,
        /// Paint to stroke with.
        brush: PaintBrush,
        /// Stroke width in logical pixels.
        width: f32,
        /// Shape drawn at each open end.
        cap: Cap,
        /// Shape drawn where two segments meet.
        join: Join,
    },
    /// Blit a rectangle of RGBA8 pixels, unpremultiplied, row-major, four
    /// bytes per pixel.
    Pixels {
        /// Producer-assigned id for the pixel buffer, stable across frames.
        buffer: u64,
        /// Bumped by the producer whenever the buffer's contents change, so a
        /// renderer can keep an upload from the previous frame.
        epoch: u64,
        /// Width in pixels.
        w: u32,
        /// Height in pixels.
        h: u32,
        /// The pixels themselves. `None` means the producer expects the host
        /// to already hold this `buffer` at this `epoch`; the field exists so
        /// a host-side blob cache can drop unchanged bytes from a later frame
        /// rather than resending them.
        data: Option<Vec<u8>>,
        /// Destination rectangle as `[x, y, width, height]`. The image scales
        /// to fill it.
        dst: [f32; 4],
    },
    /// Draw a run of text.
    Text {
        /// Text baseline start.
        origin: [f32; 2],
        /// The string to shape and draw.
        text: String,
        /// Font size in logical pixels.
        size_px: f32,
        /// Font family name; `None` takes the renderer's default family.
        family: Option<String>,
        /// CSS-style weight in `1..=1000`, where 400 is regular.
        weight: u16,
        /// Whether to select an italic face.
        italic: bool,
        /// Paint for the glyphs.
        brush: PaintBrush,
    },
    /// Begin a group composited as a whole once [`PaintOp::PopLayer`] closes
    /// it, so its alpha applies to the result rather than to each op.
    PushLayer {
        /// Group opacity in `0.0..=1.0`.
        alpha: f32,
        /// Clip applied to the group; `None` inherits the current clip.
        clip: Option<PaintPath>,
    },
    /// Close the innermost layer and composite it.
    PopLayer,
}

/// A path as a sequence of segments.
///
/// Maps one-to-one onto the path representation every 2D renderer speaks; a
/// vello-backed renderer reads it directly into a `kurbo::BezPath`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaintPath {
    /// The segments, in order.
    pub els: Vec<PathEl>,
}

/// One path segment. Points are absolute.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathEl {
    /// Start a new subpath at a point.
    MoveTo([f32; 2]),
    /// Straight line to a point.
    LineTo([f32; 2]),
    /// Quadratic Bezier: control point, then end point.
    QuadTo([f32; 2], [f32; 2]),
    /// Cubic Bezier: two control points, then the end point.
    CurveTo([f32; 2], [f32; 2], [f32; 2]),
    /// Close the current subpath back to its start.
    Close,
}

/// What a fill or stroke is painted with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaintBrush {
    /// One flat color.
    Solid(#[serde(with = "color_wire")] Color),
    /// Color ramp along the line from `start` to `end`.
    LinearGradient {
        /// Ramp start point, at offset 0.
        start: [f32; 2],
        /// Ramp end point, at offset 1.
        end: [f32; 2],
        /// Stops in increasing offset order.
        stops: Vec<GradientStop>,
    },
    /// Color ramp outwards from `center`.
    RadialGradient {
        /// Ramp origin, at offset 0.
        center: [f32; 2],
        /// Distance from the center at offset 1, in logical pixels.
        radius: f32,
        /// Stops in increasing offset order.
        stops: Vec<GradientStop>,
    },
}

/// One color stop on a gradient ramp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Position along the ramp in `0.0..=1.0`.
    pub offset: f32,
    /// Color at that position.
    #[serde(with = "color_wire")]
    pub color: Color,
}

/// How a fill decides which regions of a self-overlapping path are inside.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    /// Non-zero winding: the usual rule, and the default.
    #[default]
    NonZero,
    /// Even-odd winding.
    EvenOdd,
}

/// The shape drawn at an open end of a stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cap {
    /// Cut flat at the endpoint.
    #[default]
    Butt,
    /// Half-disc centred on the endpoint.
    Round,
    /// Square extending half the stroke width past the endpoint.
    Square,
}

/// The shape drawn where two stroke segments meet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Join {
    /// Extend both outer edges until they meet.
    #[default]
    Miter,
    /// Round off the corner.
    Round,
    /// Cut the corner flat.
    Bevel,
}

/// [`Color`] on the wire, as its four channels. Keeps the encoding of this
/// module independent of how the component type happens to be laid out.
mod color_wire {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::components::Color;

    pub fn serialize<S: Serializer>(color: &Color, s: S) -> Result<S::Ok, S::Error> {
        [color.r, color.g, color.b, color.a].serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        let [r, g, b, a] = <[f32; 4]>::deserialize(d)?;
        Ok(Color { r, g, b, a })
    }
}
