//! W3.1: rope-backed text model components.
//!
//! These components REPLACE the byte-offset `TextInput.cursor` model for the
//! W3 rewrite, but coexist with the existing `TextContent` / `TextInput` pair
//! so the bd23f51 surgical fix keeps working through the rewrite landing.
//!
//! Component layering:
//! - `TextContent` (existing) - rendered text for non-editable labels.
//! - `TextBuffer` (new) - rope-backed authoritative text for editable
//!   entities (`<input>` / `<textarea>`). Mirrored into `TextContent` after
//!   each edit so the existing renderer / binding path keeps working
//!   unchanged.
//! - `TextCursor` (new) - caret + selection anchor + affinity. Supplants
//!   the byte-offset fields on `TextInput`; the mirror system also writes
//!   the byte offset back into `TextInput.cursor` for legacy code paths
//!   (W3 stage; later waves remove the legacy fields).
//! - `ImePreedit` (new) - replaces `ImeState`; carries the in-progress
//!   composition string plus the caret position inside it.
//!
//! The `From`/`Into` impls follow the project-memory rule: convert between
//! types via trait impls, never bespoke `convert_x_to_y` helpers.

use bevy_ecs::prelude::*;
use ropey::Rope;
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// Rope-backed authoritative text buffer for editable entities (W3.1).
///
/// - Attached alongside (and mirrored to) [`crate::components::TextContent`]
///   on `<input>` / `<textarea>` spawn (see the bootstrap system in
///   `lumen-text-edit`).
/// - `version` bumps on every successful edit so derived caches (shape,
///   validation, syntax) can be invalidated without `Changed<T>` overuse.
/// - `kind` selects single-line vs multi-line semantics - single-line
///   buffers reject `\n` at insert time (matches Qt's `QLineEdit::setText`).
#[derive(Component, Clone, Debug)]
pub struct TextBuffer {
    /// The rope. Use [`Self::as_str`] / [`Self::slice`] for read access;
    /// mutate only through `lumen-text-edit::text_apply_edits`.
    pub rope: Rope,
    /// Monotonic edit counter. Bump on every mutation.
    pub version: u64,
    /// Single-line vs multiline policy.
    pub kind: TextBufferKind,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            rope: Rope::new(),
            version: 0,
            kind: TextBufferKind::default(),
        }
    }
}

impl TextBuffer {
    /// Build a single-line buffer from an initial string.
    pub fn single_line(s: &str) -> Self {
        Self {
            rope: Rope::from_str(s),
            version: 0,
            kind: TextBufferKind::SingleLine,
        }
    }

    /// Build a multi-line buffer from an initial string.
    pub fn multi_line(s: &str) -> Self {
        Self {
            rope: Rope::from_str(s),
            version: 0,
            kind: TextBufferKind::MultiLine,
        }
    }

    /// Length in bytes.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// `true` when the rope is empty.
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Slice as a string. `range` is in bytes; out-of-range / non-boundary
    /// inputs are clamped to the nearest valid char boundary.
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.rope.len_bytes();
        let s = range.start.min(len);
        let e = range.end.min(len);
        if s >= e {
            return String::new();
        }
        // ropey slicing is by char index. Convert through byte_to_char.
        let s_char = self.rope.byte_to_char(s);
        let e_char = self.rope.byte_to_char(e);
        self.rope.slice(s_char..e_char).to_string()
    }

    /// `true` when the buffer is single-line (rejects `\n` at insert time).
    pub fn is_single_line(&self) -> bool {
        matches!(self.kind, TextBufferKind::SingleLine)
    }
}

impl std::fmt::Display for TextBuffer {
    /// Materialise the rope as a `String`. Allocates; prefer
    /// [`Self::slice`] when only a sub-range is needed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.rope)
    }
}

/// `Arc<str>` snapshot of the buffer for binding-push (`buffer -> signal`).
impl From<&TextBuffer> for Arc<str> {
    fn from(buf: &TextBuffer) -> Self {
        Arc::<str>::from(buf.rope.to_string())
    }
}

/// `String` snapshot.
impl From<&TextBuffer> for String {
    fn from(buf: &TextBuffer) -> Self {
        buf.rope.to_string()
    }
}

/// Build a multi-line buffer from a string.
impl From<&str> for TextBuffer {
    fn from(s: &str) -> Self {
        Self::multi_line(s)
    }
}

/// Build a multi-line buffer from an owned `String`.
impl From<String> for TextBuffer {
    fn from(s: String) -> Self {
        Self::multi_line(&s)
    }
}

/// Single-line vs multi-line policy for [`TextBuffer`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextBufferKind {
    /// Single line; insert paths strip `\n`. Default for `<input>`.
    #[default]
    SingleLine,
    /// Multi-line; `\n` preserved. Default for `<textarea>`.
    MultiLine,
}

/// Caret behaviour at line-wrap boundaries (Qt's `Affinity`).
///
/// - `Upstream`: caret renders at the end of the wrapped-from line.
/// - `Downstream`: caret renders at the start of the wrapped-to line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Affinity {
    /// End-of-previous-line behaviour.
    Upstream,
    /// Start-of-next-line behaviour (default).
    #[default]
    Downstream,
}

/// Position inside a [`TextBuffer`]; carries both byte (for slicing) and
/// grapheme (for cursor display) axes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextPos {
    /// Byte offset into the buffer.
    pub byte: usize,
    /// Grapheme-cluster offset, computed from `byte` against the live
    /// buffer (use [`TextPos::from_byte`]).
    pub grapheme: usize,
}

impl TextPos {
    /// Origin.
    pub const ZERO: Self = Self {
        byte: 0,
        grapheme: 0,
    };

    /// Build a position from a byte offset into `text`. Clamps to the
    /// nearest char boundary, then counts graphemes up to that boundary.
    pub fn from_byte(text: &str, byte: usize) -> Self {
        let byte = clamp_to_char_boundary(text, byte);
        let grapheme = text[..byte].graphemes(true).count();
        Self { byte, grapheme }
    }

    /// Build a position from a byte offset into a [`TextBuffer`].
    pub fn from_buffer_byte(buf: &TextBuffer, byte: usize) -> Self {
        let s = buf.rope.to_string();
        Self::from_byte(&s, byte)
    }
}

impl From<&TextBuffer> for TextPos {
    /// End-of-buffer.
    fn from(buf: &TextBuffer) -> Self {
        let s = buf.rope.to_string();
        Self::from_byte(&s, s.len())
    }
}

fn clamp_to_char_boundary(s: &str, mut at: usize) -> usize {
    if at > s.len() {
        at = s.len();
    }
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Caret + selection state on an editable entity (W3.1).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TextCursor {
    /// Caret position (the moving end of the selection).
    pub head: TextPos,
    /// Selection anchor (the fixed end). Equal to `head` => no selection.
    pub anchor: TextPos,
    /// Affinity at the head.
    pub affinity: Affinity,
    /// D5: sticky visual x for vertical motion (Qt `verticalMovementX`).
    /// `None` means "recompute from the caret's current x on the next
    /// vertical motion"; any horizontal motion or edit resets it to `None`.
    /// A pixel x (not a byte column) so it tracks through proportional
    /// glyphs and wrapped lines.
    pub goal_x: Option<f32>,
}

impl TextCursor {
    /// `true` when there is no selection (head == anchor).
    pub fn is_empty(&self) -> bool {
        self.head.byte == self.anchor.byte
    }

    /// Selection byte range, sorted low -> high. `None` when empty.
    pub fn selection_range(&self) -> Option<Range<usize>> {
        if self.is_empty() {
            return None;
        }
        let lo = self.head.byte.min(self.anchor.byte);
        let hi = self.head.byte.max(self.anchor.byte);
        Some(lo..hi)
    }

    /// Collapse selection to the head.
    pub fn collapse(&mut self) {
        self.anchor = self.head;
    }

    /// Move head to `pos`; if `keep_anchor` is false, anchor follows.
    pub fn move_head(&mut self, pos: TextPos, keep_anchor: bool) {
        self.head = pos;
        if !keep_anchor {
            self.anchor = pos;
        }
    }
}

/// Stable identifier for a [`TextMark`]. Marks survive edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MarkId(pub u64);

/// Insertion-at-mark bias (Qt's `QTextCursor::MoveMode::KeepAnchor` for
/// marks): when an insertion lands AT a mark's byte position, should the
/// mark stay before (`Backward`) or after (`Forward`) the new text?
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bias {
    /// Mark stays before inserted-at-mark text.
    #[default]
    Backward,
    /// Mark moves to the end of inserted-at-mark text.
    Forward,
}

/// A point inside a [`TextBuffer`] that survives edits (GTK's
/// `GtkTextMark`). The `text_apply_edits` mutator shifts marks during
/// insert/delete according to [`Self::bias`].
#[derive(Clone, Copy, Debug)]
pub struct TextMark {
    /// Stable id.
    pub id: MarkId,
    /// Current byte offset; updated by the edit mutator.
    pub byte: usize,
    /// Insertion-at-mark policy.
    pub bias: Bias,
}

/// Tag flavours carried by [`TextTagRange`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagKind {
    /// IME preedit underline (uncommitted composition).
    PreeditUnderline,
    /// IME preedit converted segment (committed-but-uncommitted in CJK).
    PreeditConverted,
    /// User selection highlight (renderer paints translucent background).
    SelectedHighlight,
    /// Syntax-colour overlay (paint with carried `u32` colour packed RGBA).
    SyntaxColour(u32),
    /// Generic highlight (link / search hit).
    Highlight(u32),
}

/// A tagged byte range inside a [`TextBuffer`]; survives edits via two
/// [`TextMark`]s at the endpoints.
#[derive(Clone, Copy, Debug)]
pub struct TextTagRange {
    /// Range start.
    pub start: TextMark,
    /// Range end (inclusive byte; convention: `end.byte > start.byte`).
    pub end: TextMark,
    /// Tag flavour.
    pub tag: TagKind,
}

/// IME preedit (composition) state on a focused editable. Replaces the
/// pre-W3 `ImeState` for entities with a [`TextBuffer`]; the legacy
/// `ImeState` stays for backwards compatibility with the old `TextInput`
/// path until the W3 migration completes.
#[derive(Component, Clone, Debug, Default)]
pub struct ImePreedit {
    /// In-progress composition string.
    pub text: String,
    /// Caret byte offset INSIDE [`Self::text`].
    pub caret_in_preedit: usize,
}

/// Marker: this entity is editable text (W3.1).
///
/// - Attached alongside `TextInput` by the bootstrap system in
///   `lumen-text-edit` so spawn-side compatibility holds without touching
///   `lumenc/src/spawn.rs`.
/// - Future migration: replace `TextInput` with this + a policy bundle.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TextEditable;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_pos_from_byte_clamps_to_boundary() {
        let s = "h\u{e9}llo"; // '\u{e9}' = 2 bytes
        // byte 2 lands mid-'\u{e9}'? no, 'h'=1, '\u{e9}'=2 -> 1..3, so byte 2 is mid.
        let p = TextPos::from_byte(s, 2);
        assert_eq!(p.byte, 1);
        assert_eq!(p.grapheme, 1);
    }

    #[test]
    fn from_str_makes_multiline_buffer() {
        let buf: TextBuffer = "hi".into();
        assert!(!buf.is_single_line());
        assert_eq!(buf.to_string(), "hi");
    }

    #[test]
    fn arc_str_snapshot_roundtrip() {
        let buf = TextBuffer::single_line("hi");
        let s: Arc<str> = (&buf).into();
        assert_eq!(&*s, "hi");
    }

    #[test]
    fn cursor_selection_range_sorted() {
        let c = TextCursor {
            head: TextPos {
                byte: 5,
                grapheme: 5,
            },
            anchor: TextPos {
                byte: 2,
                grapheme: 2,
            },
            ..Default::default()
        };
        assert_eq!(c.selection_range(), Some(2..5));
    }

    #[test]
    fn cursor_collapse_clears_selection() {
        let mut c = TextCursor {
            head: TextPos {
                byte: 5,
                grapheme: 5,
            },
            anchor: TextPos {
                byte: 2,
                grapheme: 2,
            },
            ..Default::default()
        };
        c.collapse();
        assert!(c.is_empty());
    }

    #[test]
    fn buffer_slice_clamps() {
        let buf = TextBuffer::multi_line("hello");
        // Past-end clamps.
        assert_eq!(buf.slice(0..100), "hello");
        // Mid-range works.
        assert_eq!(buf.slice(1..4), "ell");
        // Inverted range yields empty.
        #[allow(clippy::reversed_empty_ranges)]
        let inverted = 4..1;
        assert_eq!(buf.slice(inverted), "");
    }
}
