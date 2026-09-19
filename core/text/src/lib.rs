//! Text shaping backend trait.
//!
//! Implementations (e.g. `lumen-text-cosmic`) take a UTF-8 string + size and
//! return positioned glyphs plus the resolved font data. Backend-agnostic so
//! the WGPU/vello renderer and any future renderer can both consume the
//! output.
//!
//! ## Multi-segment output (W5.6)
//!
//! A real paragraph can mix scripts (Latin + Arabic), fonts (`SansSerif`
//! falling back to a CJK face), and BiDi levels (LTR runs of even level,
//! RTL runs of odd level). cosmic-text already emits the right itemisation
//! internally; the W5.6 work surfaces it through [`ShapedRun::segments`].
//! Each [`ShapedSegment`] is a maximal slice with one font + one BiDi
//! level, so the renderer can call `draw_glyphs` once per font and the
//! caret/selection math can split logical ranges at segment boundaries to
//! reproduce the BiDi-correct visual rectangles.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod edit;
pub mod geometry;
pub mod hit_test;
pub mod shaped;
pub mod undo;

pub use edit::*;
pub use geometry::{CaretGeometry, SelectionBand, TextGeometry};
pub use hit_test::{hit_test_text, select_line_at_byte, select_word_at_byte};
pub use shaped::{ShapedText, TextViewport, build_shaped_text};
pub use undo::{UndoEntry, UndoKind, UndoStack};

use std::sync::Arc;

/// One positioned glyph within a shaped run.
#[derive(Debug, Clone, Copy)]
pub struct GlyphPosition {
    /// Glyph index into the resolved font.
    pub id: u32,
    /// X position of the glyph's leading edge, from the run origin, in
    /// logical pixels. cosmic-text emits glyphs in **visual** order, so
    /// in a mixed-script paragraph `glyphs[i].x` can be smaller than
    /// `glyphs[i-1].x` when the run crosses a BiDi boundary.
    pub x: f32,
    /// Y offset from the run's baseline, in logical pixels.
    pub y: f32,
    /// X advance (the leading edge of the next glyph would land at
    /// `x + advance`). Critical for caret placement: kerning makes
    /// `next.x - this.x` slightly different from `this.advance`, and
    /// trailing whitespace produces no glyph at all unless we record
    /// the advance separately.
    pub advance: f32,
    /// Byte range in the source string that produced this glyph cluster
    /// (`start..end`). Empty range = synthesised (e.g. ellipsis).
    /// Stored so the renderer can map logical byte ranges to visual
    /// glyph clusters for caret + selection in mixed-script text.
    pub byte_start: u32,
    /// Byte range end (exclusive). See [`Self::byte_start`].
    pub byte_end: u32,
}

/// One maximal `(font_id, BiDi level)` slice within a shaped run.
///
/// Emitted in **logical source order** so callers can walk the segment
/// list and snap selection ranges at boundaries. Each segment's
/// `glyphs` are still in cosmic-text's visual order - the renderer paints
/// them in that order, while caret/selection math uses
/// [`GlyphPosition::byte_start`] / [`GlyphPosition::byte_end`] to map
/// back to logical bytes.
#[derive(Debug, Clone)]
pub struct ShapedSegment {
    /// Opaque font identifier from the shaper's font database. Two
    /// segments with the same `font_id` resolve through the same
    /// `font_data` so the renderer can cache by id.
    pub font_id: u64,
    /// TTF/OTF bytes of the resolved face for this segment.
    pub font_data: Arc<Vec<u8>>,
    /// Face index inside the font file.
    pub font_index: u32,
    /// Normalized variation coordinates of the instance this segment was
    /// shaped at, in the face's axis order, as F2Dot14 raw bits. Empty for
    /// a static face, which has one instance and needs none.
    ///
    /// A variable face shapes at the instance the authored `font-weight`
    /// selects, and the advances in `glyphs` come from that instance. The
    /// renderer hands these coordinates to the glyph rasterizer so the
    /// outlines it paints belong to the same one; without them the outlines
    /// come from the face's default instance and the text pairs one weight's
    /// spacing with another weight's strokes.
    pub normalized_coords: Vec<i16>,
    /// BiDi embedding level (Unicode BiDi algorithm). Even = LTR, odd =
    /// RTL. From cosmic-text's `LayoutGlyph.level` (unicode-bidi crate).
    pub level: u8,
    /// Glyphs belonging to this segment, in cosmic-text's visual
    /// order. The renderer issues one `draw_glyphs(&font_data)` per
    /// segment.
    pub glyphs: Vec<GlyphPosition>,
    /// Total visual advance of this segment in logical pixels.
    pub width: f32,
}

impl ShapedSegment {
    /// Convenience: even-level = LTR.
    pub const fn is_ltr(&self) -> bool {
        self.level % 2 == 0
    }
    /// Convenience: odd-level = RTL.
    pub const fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// A shaped paragraph of text.
///
/// `font_data` / `font_index` reflect the **first** segment's font and
/// stay for renderer back-compat with the W3.6 single-font path. The
/// authoritative per-(font, level) breakdown lives in [`Self::segments`].
/// `font_data` is the raw TTF/OTF bytes. Renderers wrap these in their own
/// font type (e.g. `peniko::Font`) and should cache by [`Arc::as_ptr`].
#[derive(Debug, Clone)]
pub struct ShapedRun {
    /// TTF/OTF bytes of the resolved font face (first segment).
    pub font_data: Arc<Vec<u8>>,
    /// Face index inside the font file (0 for single-face TTF).
    pub font_index: u32,
    /// All glyphs across every segment, in cosmic-text's visual order.
    /// Kept for back-compat with single-font renderer paths and tests;
    /// new code should iterate [`Self::segments`] instead.
    pub glyphs: Vec<GlyphPosition>,
    /// Per-(font, BiDi level) maximal slices in logical order.
    /// Non-empty whenever `glyphs` is non-empty.
    pub segments: Vec<ShapedSegment>,
    /// Total advance of the run (logical pixels). For an empty input,
    /// `0.0`. Includes trailing whitespace that may not appear in
    /// `glyphs`. Use this - not `glyphs.last().x` - for end-of-run
    /// caret placement.
    pub width: f32,
}

/// Wrap policy passed to [`TextShaper::shape_wrapped`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WrapMode {
    /// No automatic wrapping (`white-space: nowrap`). Default.
    #[default]
    None,
    /// Word-break wrapping at the available width.
    Word,
    /// Glyph-level wrapping (no word boundary respect - best for very
    /// narrow CJK columns).
    Glyph,
}

impl From<lumen_core::components::TextWrap> for WrapMode {
    /// 1:1 variant map from the core `TextWrap` style enum to the shaper
    /// wrap policy. Lets layout + render backends call `.into()` instead of
    /// hand-matching the three variants.
    fn from(w: lumen_core::components::TextWrap) -> Self {
        use lumen_core::components::TextWrap;
        match w {
            TextWrap::None => WrapMode::None,
            TextWrap::Word => WrapMode::Word,
            TextWrap::Glyph => WrapMode::Glyph,
        }
    }
}

/// Extension shape parameters. Backends that don't override
/// [`TextShaper::shape_wrapped`] fall through to the no-wrap path so old
/// callers keep working.
#[derive(Debug, Clone)]
pub struct ShapeOptions {
    /// Available width for wrapping, in logical pixels. `None` = unbounded.
    pub width: Option<f32>,
    /// Wrap policy.
    pub wrap: WrapMode,
    /// Hard cap on line count after shaping (callers cull). `None` = unbounded.
    pub max_lines: Option<u32>,
    /// CSS `font-family` fallback chain as authored (comma-separated,
    /// possibly quoted names, generic keywords allowed). `None` = the
    /// platform sans-serif. The backend resolves the first family
    /// present in its font database.
    pub family: Option<Arc<str>>,
    /// CSS `font-weight` (1-1000; 400 = normal, 700 = bold).
    pub weight: u16,
    /// Resolved CSS `line-height`, in the same unit space as the `size_px`
    /// argument passed alongside these options to [`TextShaper::shape`]
    /// (a caller that scales `size_px` by DPR must scale this the same
    /// way). `None` => the backend's own `line-height: normal` fallback
    /// ([`DEFAULT_LINE_HEIGHT_MULTIPLIER`]). Callers with a `TextStyle`-ish
    /// source resolve one via
    /// `lumen_core::components::resolve_line_height` before constructing
    /// these options; this field never re-derives the multiplier itself.
    pub line_height: Option<f32>,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self {
            width: None,
            wrap: WrapMode::None,
            max_lines: None,
            family: None,
            weight: 400,
            line_height: None,
        }
    }
}

impl ShapeOptions {
    /// Line height in the same unit space as `size_px`: the authored CSS
    /// `line-height` when the caller resolved one, else `size_px` times
    /// [`DEFAULT_LINE_HEIGHT_MULTIPLIER`]. Shaping, measuring, and
    /// painting all route through this so one paragraph cannot end up
    /// with three different line heights.
    pub fn resolved_line_height(&self, size_px: f32) -> f32 {
        self.line_height
            .unwrap_or(size_px * DEFAULT_LINE_HEIGHT_MULTIPLIER)
    }
}

/// Re-export of [`lumen_core::components::DEFAULT_LINE_HEIGHT_MULTIPLIER`]
/// for crates (e.g. `lumen-text-cosmic`) that depend on `lumen-text` but
/// not directly on `lumen-core`.
pub use lumen_core::components::DEFAULT_LINE_HEIGHT_MULTIPLIER;

/// Backend that turns strings into [`ShapedRun`]s.
pub trait TextShaper {
    /// Shape `text` at `size_px` under the given [`ShapeOptions`].
    /// Returns `None` when the input is empty, when no font can be
    /// resolved, or when shaping fails (rare).
    ///
    /// Pass [`ShapeOptions::default()`] for the no-wrap, unbounded case
    /// (caret / selection prefix measurement, label sizing without
    /// a container width).
    fn shape(&mut self, text: &str, size_px: f32, opts: ShapeOptions) -> Option<ShapedRun>;

    /// Measure a text run for layout. Returns the tight `(width, height)`
    /// in logical pixels of the shaped paragraph at `size_px` under the
    /// `wrap` / `max_lines` policy, clamped to `max_width` when `Some`.
    ///
    /// The layout engine calls this from taffy's measure callback to size
    /// text leaves, and the caret-scroll pass calls it to place the caret
    /// on the shaped prefix.
    fn measure(
        &mut self,
        text: &str,
        size_px: f32,
        max_width: Option<f32>,
        wrap: WrapMode,
        max_lines: Option<u32>,
    ) -> (f32, f32) {
        let opts = ShapeOptions {
            width: max_width,
            wrap,
            max_lines,
            ..ShapeOptions::default()
        };
        let (w, h, _baseline) = self.measure_with_baseline(text, size_px, &opts);
        (w, h)
    }

    /// Same as [`Self::measure`], plus the first-line alphabetic baseline
    /// offset from the top of the leaf box - the y at which Latin,
    /// Cyrillic, and Greek glyphs sit. `FlexAlign::Baseline` and the
    /// AccessKit text-position report consume it.
    ///
    /// The default walks the shaped glyphs: the width is the widest laid
    /// out line (taffy's intrinsic content size), the height is the line
    /// count times [`ShapeOptions::resolved_line_height`], and the
    /// baseline is `0.8` of that line height, the ratio the common font
    /// metrics tables produce. Empty input measures `(0, 0, 0)` so the
    /// leaf collapses. Override it when the backend can answer more
    /// cheaply or more precisely than by shaping.
    fn measure_with_baseline(
        &mut self,
        text: &str,
        size_px: f32,
        opts: &ShapeOptions,
    ) -> (f32, f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let max_width = opts.width;
        let line_height = opts.resolved_line_height(size_px).max(1.0);
        match self.shape(text, size_px, opts.clone()) {
            Some(run) => {
                let mut rows = 1u32;
                let mut last_y = run.glyphs.first().map(|g| g.y).unwrap_or(0.0);
                for g in run.glyphs.iter().skip(1) {
                    if (g.y - last_y).abs() > line_height * 0.5 {
                        rows += 1;
                        last_y = g.y;
                    }
                }
                let mut width = run.width;
                if let Some(cap) = max_width {
                    width = width.min(cap);
                }
                (width, (rows as f32) * line_height, line_height * 0.8)
            }
            None => (0.0, 0.0, 0.0),
        }
    }

    /// Number of entries the backend currently holds in its shape cache.
    /// `0` for a backend that caches nothing.
    fn cache_len(&self) -> usize {
        0
    }

    /// Resize the shape cache to the given entry count. The per-tick
    /// memory-budget sweep calls this when the cache outgrows
    /// [`lumen_core::components::MemoryBudget`]. No-op for a backend that
    /// caches nothing.
    fn set_capacity(&mut self, _entries: usize) {}
}

/// The text shaper an app runs, held as a `NonSend` resource (font
/// databases are rarely `Send`).
///
/// It derefs to the shaper, so [`TextShaper`] methods work directly on it.
/// Build one from any backend with `ShaperService::from(backend)`, and
/// replace the runtime's default by inserting your own in an app hook.
/// The layout engine reads it for text measurement, so a swapped backend
/// changes measuring and painting together.
pub struct ShaperService(Box<dyn TextShaper>);

impl ShaperService {
    /// Wrap a shaper.
    pub fn new<S: TextShaper + 'static>(shaper: S) -> Self {
        Self(Box::new(shaper))
    }
}

impl<S: TextShaper + 'static> From<S> for ShaperService {
    fn from(shaper: S) -> Self {
        Self::new(shaper)
    }
}

impl From<Box<dyn TextShaper>> for ShaperService {
    fn from(shaper: Box<dyn TextShaper>) -> Self {
        Self(shaper)
    }
}

impl std::ops::Deref for ShaperService {
    type Target = dyn TextShaper;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl std::ops::DerefMut for ShaperService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}

impl Default for ShaperService {
    fn default() -> Self {
        Self::new(NullShaper)
    }
}

/// The shaper that shapes nothing.
///
/// Every run comes back empty, so text measures to a zero-sized box and
/// no glyph is painted. A build with no text backend selected runs this,
/// and a layout test that only cares about boxes can use it directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullShaper;

impl TextShaper for NullShaper {
    fn shape(&mut self, _text: &str, _size_px: f32, _opts: ShapeOptions) -> Option<ShapedRun> {
        None
    }
}

/// Marker trait for a font database / system-font accessor. Currently empty.
pub trait FontDB {}
