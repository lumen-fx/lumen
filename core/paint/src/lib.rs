//! What a frame looks like, independent of what draws it.
//!
//! The retained [`lumen_core::node_ir::Node`] tree becomes pixels in two
//! steps. This crate does the first: [`walk_node`] visits the tree and the
//! leaf emitters turn each node into calls on a [`Painter`]. A render backend
//! does the second: it implements [`Painter`] over its own draw target and
//! rasterizes what it was handed. Every backend paints through the same walk,
//! so they agree on the frame by construction rather than by keeping two
//! copies of it in step.
//!
//! The trait is immediate-mode and object-safe. Types are peniko's and
//! kurbo's, the vocabulary vello and vello_cpu both speak, so a sink is a
//! thin adapter.
//!
//! A sink that can record and replay encoded work offers fragments
//! ([`Painter::begin_fragment`]); the walker then encodes each repeated
//! appearance once and replays it at every position through the
//! [`FragmentCache`]. A sink that cannot answers `false` and the walker paints
//! every leaf directly.
//!
//! The frame logic around the walk is shared too: [`wants_frame`] decides
//! whether a frame is worth painting, [`paint_frame`] walks the render
//! world's retained scene into a sink, and [`answer_capture`] hands a
//! pending screenshot the result, so every renderer gates, paints, and
//! captures the same way on every kind of target.

#![warn(missing_docs)]

pub mod cache;
pub mod emit;
pub mod frame;
pub mod recording;
pub mod svg;
pub mod walker;

pub use cache::{CacheStats, FragmentCache, FragmentKey};
pub use emit::{
    draw_glyph_run, draw_image, draw_text, emit_border, emit_outline, emit_outline_cached,
    emit_rect, emit_rect_cached, emit_shadow, emit_shadow_cached, emit_svg, fit_box, folded,
    peniko_color,
};
pub use frame::{answer_capture, capture_requested, clear_color, paint_frame, wants_frame};
pub use peniko;
pub use peniko::kurbo;
pub use recording::Recording;
pub use walker::{
    ClipStack, WalkContext, diff_retained_scenes, scene_has_damage, walk_node, walk_retained_scene,
};

use lumen_text::GlyphPosition;
use peniko::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use peniko::{BlendMode, BrushRef, Color, Fill, FontData, ImageBrush};
use std::any::Any;
use std::sync::Arc;

/// The geometry a [`Painter`] fills, strokes, or clips to.
///
/// Rects and rounded rects stay distinct from general paths so a sink can
/// take the fast path its rasterizer has for them.
#[derive(Clone, Copy, Debug)]
pub enum Shape<'a> {
    /// An axis-aligned rectangle.
    Rect(Rect),
    /// A rectangle with per-corner radii.
    RoundedRect(RoundedRect),
    /// Any path.
    Path(&'a BezPath),
}

impl From<Rect> for Shape<'_> {
    fn from(rect: Rect) -> Self {
        Shape::Rect(rect)
    }
}

impl From<RoundedRect> for Shape<'_> {
    fn from(rect: RoundedRect) -> Self {
        Shape::RoundedRect(rect)
    }
}

impl<'a> From<&'a BezPath> for Shape<'a> {
    fn from(path: &'a BezPath) -> Self {
        Shape::Path(path)
    }
}

/// One run of glyphs from one font face, positioned on a shared baseline.
#[derive(Clone, Copy, Debug)]
pub struct GlyphRun<'a> {
    /// The face the glyph ids index into.
    pub font: &'a FontData,
    /// Size in target pixels.
    pub font_size: f32,
    /// Variation coordinates of the instance the run was shaped at, as
    /// F2Dot14 bits. Empty for a static face.
    pub normalized_coords: &'a [i16],
    /// Maps run coordinates (glyph `x`, `y` from the baseline origin) to the
    /// target.
    pub transform: Affine,
    /// How the glyphs are filled.
    pub brush: BrushRef<'a>,
    /// The glyphs, positioned relative to the run origin.
    pub glyphs: &'a [GlyphPosition],
}

/// Encoded work a sink recorded between [`Painter::begin_fragment`] and
/// [`Painter::end_fragment`], replayable at any transform.
///
/// Opaque to everything but the sink that made it: a fragment from one sink
/// replays on that sink only.
#[derive(Clone)]
pub struct Fragment(Arc<dyn Any + Send + Sync>);

impl Fragment {
    /// Wrap a sink's recorded work.
    pub fn new<T: Any + Send + Sync>(recorded: Arc<T>) -> Self {
        Self(recorded)
    }

    /// The recorded work as `T`, or `None` when another kind of sink made it.
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for Fragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fragment")
    }
}

/// A draw target the walker and the leaf emitters paint into.
///
/// Every call is immediate: it paints over what is already there, in call
/// order. Layers nest; each [`Self::push_layer`] is closed by one
/// [`Self::pop_layer`].
pub trait Painter: Send + 'static {
    /// Fill `shape` under `transform` with `brush`, under the `style` fill
    /// rule. `brush_transform` maps the brush separately from the shape.
    fn fill(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    );

    /// Stroke the outline of `shape` under `transform` with `brush`.
    fn stroke(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: BrushRef<'_>,
        brush_transform: Option<Affine>,
        shape: &Shape<'_>,
    );

    /// Open a layer clipped to `clip` (under `transform`, with the
    /// `clip_style` fill rule) that composites at `alpha` with `blend` when
    /// it closes.
    fn push_layer(
        &mut self,
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    );

    /// Close the innermost open layer.
    fn pop_layer(&mut self);

    /// How many layers are open. Lets a caller close exactly the layers a
    /// callee opened and no others.
    fn layer_depth(&self) -> usize;

    /// Fill `rect` (rounded by `radius`) in `color` with a Gaussian blur of
    /// standard deviation `std_dev`: the box-shadow primitive.
    fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    );

    /// Draw `image` with its top-left corner at the origin of `transform`,
    /// one image pixel per unit.
    fn draw_image(&mut self, image: &ImageBrush, transform: Affine);

    /// Fill one run of glyphs.
    fn draw_glyphs(&mut self, run: &GlyphRun<'_>);

    /// Start recording into a fragment instead of the frame. Returns `false`
    /// when this sink has no fragments, in which case nothing changed and the
    /// caller paints directly.
    fn begin_fragment(&mut self) -> bool {
        false
    }

    /// Stop the recording [`Self::begin_fragment`] started and hand it back.
    /// `None` when no recording was open.
    fn end_fragment(&mut self) -> Option<Fragment> {
        None
    }

    /// Replay `fragment` under `transform`. Returns `false` when the fragment
    /// came from another kind of sink and nothing was painted.
    fn append_fragment(&mut self, fragment: &Fragment, transform: Affine) -> bool {
        let _ = (fragment, transform);
        false
    }

    /// Names the backend behind this sink; it is what
    /// [`lumen_core::native::NativePaintCtx::backend_id`] reports, and it
    /// tells a caller of [`Self::native`] what to downcast to.
    fn backend_id(&self) -> &'static str;

    /// This sink as its concrete type, for a caller that needs something the
    /// trait does not offer. Downcast it to the type the backend documents
    /// beside its [`Self::backend_id`].
    fn native(&mut self) -> &mut dyn Any;
}

/// The draw target every backend hands a [`lumen_core::native::NativePainter`].
///
/// A native painter downcasts the context's target to this type with
/// `ctx.target_as::<PaintTarget>()` and paints through [`Painter`], which
/// draws the same on every backend.
pub type PaintTarget = Box<dyn Painter>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Each geometry converts into the shape variant a sink fast-paths.
    #[test]
    fn geometry_converts_into_its_own_shape() {
        let rect = Rect::new(0.0, 0.0, 2.0, 2.0);
        let rounded = RoundedRect::from_rect(rect, 1.0);
        let path = BezPath::from_vec(Vec::new());
        assert!(matches!(Shape::from(rect), Shape::Rect(r) if r == rect));
        assert!(matches!(Shape::from(rounded), Shape::RoundedRect(r) if r == rounded));
        assert!(matches!(Shape::from(&path), Shape::Path(p) if p.elements().is_empty()));
    }

    /// A fragment hands its work back only to the sink type that made it.
    #[test]
    fn a_fragment_downcasts_only_to_the_type_it_wraps() {
        let fragment = Fragment::new(Arc::new(7u32));
        assert_eq!(fragment.downcast::<u32>(), Some(&7));
        assert!(fragment.downcast::<String>().is_none());
        assert_eq!(format!("{fragment:?}"), "Fragment");
    }

    /// A sink that does not override the fragment calls has no fragments:
    /// it refuses to record, has nothing to end, and paints no replay.
    #[test]
    fn a_sink_without_fragments_refuses_every_fragment_call() {
        let mut sink = Recording::default();
        assert!(!sink.begin_fragment());
        assert!(sink.end_fragment().is_none());
        let foreign = Fragment::new(Arc::new(()));
        assert!(!sink.append_fragment(&foreign, Affine::IDENTITY));
        assert!(sink.is_empty());
    }
}
