//! W3.4: pointer -> caret hit-test + word/line expansion.
//!
//! No-dep helpers - these run without needing a shaped layout (the
//! caller passes the text and an approximate average advance) so they're
//! usable from unit tests and from the input crate even when no shaper
//! is wired (e.g. headless tests).
//!
//! The real shaping-aware hit-test path is intended to consult the
//! per-entity `ShapedRunIndex` cache (W3.6) - once that exists, the
//! input crate can ask the shaper for `xy_to_byte(x, y)` and feed the
//! result through [`select_word_at_byte`] / [`select_line_at_byte`]
//! unchanged.

#![allow(missing_docs)]

use unicode_segmentation::UnicodeSegmentation;

/// Hit-test against a single-line run with a uniform per-char advance.
/// Returns the byte offset whose glyph the pointer landed nearest.
///
/// `pointer_x` is in the same coordinate space as `origin_x` (typically
/// the text run's leading edge, in logical pixels). `avg_advance` is the
/// average glyph advance for the run; callers without a real shaper can
/// pass `size_px * 0.55` as a rough fallback.
///
/// This is a tide-over implementation for tests and for input.rs while
/// the full ShapedRunIndex path is being wired; the renderer-side
/// `cosmic-text Buffer::hit` is the canonical path once the cache lands.
pub fn hit_test_text(text: &str, origin_x: f32, pointer_x: f32, avg_advance: f32) -> usize {
    if text.is_empty() || avg_advance <= 0.0 {
        return 0;
    }
    let dx = (pointer_x - origin_x).max(0.0);
    let target_col = (dx / avg_advance).round() as usize;
    let mut col = 0usize;
    let mut last_idx = 0usize;
    for (idx, _) in text.grapheme_indices(true) {
        if col == target_col {
            return idx;
        }
        last_idx = idx;
        col += 1;
    }
    // Past end -> snap to end-of-text.
    if target_col >= col {
        text.len()
    } else {
        last_idx
    }
}

/// Expand a byte offset into a word range using Unicode word boundaries.
/// Returns `(start, end)` in bytes; `start == end == byte` when the
/// click landed on whitespace.
pub fn select_word_at_byte(text: &str, byte: usize) -> (usize, usize) {
    let byte = clamp_to_char_boundary(text, byte);
    for (off, w) in text.split_word_bound_indices() {
        let end = off + w.len();
        if byte >= off && byte < end {
            // Filter pure-whitespace words.
            if w.chars().all(|c| c.is_whitespace()) {
                return (byte, byte);
            }
            return (off, end);
        }
        if byte == end && end == text.len() {
            return (off, end);
        }
    }
    (byte, byte)
}

/// Expand a byte offset into a line range. `\n` is excluded from the
/// end of the range so a triple-click select-then-replace doesn't eat
/// the newline.
pub fn select_line_at_byte(text: &str, byte: usize) -> (usize, usize) {
    let byte = clamp_to_char_boundary(text, byte);
    let start = text[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[byte..]
        .find('\n')
        .map(|i| byte + i)
        .unwrap_or(text.len());
    (start, end)
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

/// Byte offset of the start of the Unicode-word segment strictly before
/// `from`. Used for directional word motion (ctrl+<-, ctrl+Backspace) -
/// shares the `split_word_bound_indices` walk [`select_word_at_byte`]
/// uses to expand a point into a range, but returns the boundary itself
/// rather than a `(start, end)` pair.
///
/// Whitespace runs count as their own segment (matching
/// `unicode-segmentation`'s word-boundary rules), so repeated calls step
/// through a text one word *or* one whitespace run at a time - the same
/// granularity `lumen-text-edit`'s `CursorMotion::WordLeft` uses.
pub fn prev_word_boundary(text: &str, from: usize) -> usize {
    let from = clamp_to_char_boundary(text, from);
    let mut prev = 0usize;
    for (off, _) in text.split_word_bound_indices() {
        if off >= from {
            return prev;
        }
        prev = off;
    }
    prev
}

/// Byte offset of the end of the Unicode-word segment at/after `from`.
/// See [`prev_word_boundary`].
pub fn next_word_boundary(text: &str, from: usize) -> usize {
    let from = clamp_to_char_boundary(text, from);
    for (off, w) in text.split_word_bound_indices() {
        let end = off + w.len();
        if end > from {
            return end;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_returns_zero_for_left_of_origin() {
        let b = hit_test_text("hello", 100.0, 50.0, 8.0);
        assert_eq!(b, 0);
    }

    #[test]
    fn hit_test_lands_in_middle() {
        // 5 chars at ~8px each -> middle (~16px in) = index 2.
        let b = hit_test_text("hello", 0.0, 16.0, 8.0);
        assert_eq!(b, 2);
    }

    #[test]
    fn hit_test_past_end_snaps_to_end() {
        let b = hit_test_text("hi", 0.0, 1000.0, 8.0);
        assert_eq!(b, 2);
    }

    #[test]
    fn select_word_expands_around_byte() {
        let (s, e) = select_word_at_byte("hello world", 7);
        assert_eq!(&"hello world"[s..e], "world");
    }

    #[test]
    fn select_word_on_whitespace_collapses() {
        let (s, e) = select_word_at_byte("hello world", 5);
        assert_eq!(s, e);
    }

    #[test]
    fn select_line_excludes_newline() {
        let text = "line one\nline two\nline three";
        let (s, e) = select_line_at_byte(text, 12);
        assert_eq!(&text[s..e], "line two");
    }

    #[test]
    fn select_line_first_line() {
        let text = "first\nsecond";
        let (s, e) = select_line_at_byte(text, 2);
        assert_eq!(&text[s..e], "first");
    }

    #[test]
    fn select_line_last_line_no_trailing_newline() {
        let text = "a\nb\nc";
        let (s, e) = select_line_at_byte(text, 4);
        assert_eq!(&text[s..e], "c");
    }

    #[test]
    fn prev_word_boundary_steps_back_one_word() {
        let text = "hello world";
        assert_eq!(prev_word_boundary(text, 11), 6); // end -> start of "world"
        assert_eq!(prev_word_boundary(text, 6), 5); // start of "world" -> start of " "
        assert_eq!(prev_word_boundary(text, 5), 0); // start of " " -> start of "hello"
        assert_eq!(prev_word_boundary(text, 0), 0); // already at start
    }

    #[test]
    fn next_word_boundary_steps_forward_one_word() {
        let text = "hello world";
        assert_eq!(next_word_boundary(text, 0), 5); // start -> end of "hello"
        assert_eq!(next_word_boundary(text, 5), 6); // end of "hello" -> end of " "
        assert_eq!(next_word_boundary(text, 6), 11); // end of " " -> end of "world"
        assert_eq!(next_word_boundary(text, 11), 11); // already at end
    }

    #[test]
    fn word_boundary_respects_multibyte_char_boundaries() {
        // The accented letters are each 2 bytes; word segmentation must not split
        // mid-codepoint even when `from` lands inside one.
        let text = "h\u{e9}llo w\u{f6}rld"; // bytes: h(1) e-acute(2) l l o(1) ' '(1) w o-umlaut(2) r l d(1)
        // byte 2 is mid-'\u{e9}' (starts at 1, len 2) - clamp down to 1.
        assert_eq!(prev_word_boundary(text, 2), 0);
        assert_eq!(next_word_boundary(text, 2), "h\u{e9}llo".len());
        // Full round trip from the end lands back at the "w\u{f6}rld" start.
        let world_start = text.rfind(' ').unwrap() + 1;
        assert_eq!(prev_word_boundary(text, text.len()), world_start);
    }
}
