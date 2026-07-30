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
pub use geometry::{CaretGeometry, TextGeometry};
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
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self {
            width: None,
            wrap: WrapMode::None,
            max_lines: None,
            family: None,
            weight: 400,
        }
    }
}

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
}

/// Marker trait for a font database / system-font accessor. Currently empty.
pub trait FontDB {}
