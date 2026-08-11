//! W3.3: Undo / redo stack with typing coalesce.
//!
//! Per-editable component. Each [`UndoEntry`] is a structural delta -
//! `removed` and `inserted` carry the bytes a single edit displaced.
//! Pure-insert edits have empty `removed`; pure-delete edits have empty
//! `inserted`; replace edits have both.
//!
//! Coalescing: consecutive single-character insertions within
//! [`UndoStack::group_window`] (default 500 ms) collapse into one entry
//! so undo restores word-sized chunks, not character-sized ones (Qt's
//! `QUndoStack::beginMacro` analogue).

#![allow(missing_docs)]

use std::time::Instant;

use bevy_ecs::prelude::*;
use lumen_core::text_model::{TextBuffer, TextCursor, TextPos};

/// What kind of delta one entry encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoKind {
    /// `inserted` non-empty, `removed` empty.
    Insert,
    /// `removed` non-empty, `inserted` empty.
    Delete,
    /// Both non-empty.
    Replace,
}

/// One reversible delta on a [`TextBuffer`].
#[derive(Clone, Debug)]
pub struct UndoEntry {
    /// Kind tag (derived from removed / inserted but stored for fast match).
    pub kind: UndoKind,
    /// Byte offset where the edit landed.
    pub position: usize,
    /// Bytes that the edit displaced (empty for pure insert).
    pub removed: String,
    /// Bytes the edit introduced (empty for pure delete).
    pub inserted: String,
    /// Cursor head position before the edit.
    pub cursor_before: TextPos,
}

/// Per-editable undo stack (W3.3).
///
/// Stores entries in chronological order. `head` is the next-undo
/// index - entries at index `0..head` are undoable; entries at
/// `head..len` are redoable (post-undo). Any new edit truncates the
/// redo side.
#[derive(Component, Debug, Clone)]
pub struct UndoStack {
    entries: Vec<UndoEntry>,
    head: usize,
    last_edit: Option<Instant>,
    /// Typing-coalesce window. Consecutive single-char inserts within
    /// this duration of each other collapse into one entry.
    pub group_window: std::time::Duration,
    /// Maximum stack depth. Older entries dropped from the front.
    pub max_depth: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            head: 0,
            last_edit: None,
            group_window: std::time::Duration::from_millis(500),
            max_depth: 512,
        }
    }
}

impl UndoStack {
    /// Number of stored entries (undo + redo halves).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `true` when there's something to undo.
    pub fn can_undo(&self) -> bool {
        self.head > 0
    }

    /// `true` when there's something to redo.
    pub fn can_redo(&self) -> bool {
        self.head < self.entries.len()
    }

    /// Clear all entries (external buffer rewrite - recorded offsets no
    /// longer describe the live rope). Mirrors `QLineEdit::setText`.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.head = 0;
        self.last_edit = None;
    }

    /// Record a pure-insertion edit.
    pub fn record_insert(&mut self, position: usize, inserted: &str, cursor_before: TextPos) {
        let now = Instant::now();
        // Try to coalesce with the previous insert at the same edge.
        // `starts_new_group` breaks the run at word boundaries (typing
        // "hello world" undoes as "world", then " ", then "hello") and
        // the `group_window` pause breaks it on hesitation - standard
        // word-ish undo granularity.
        if self.head == self.entries.len()
            && let Some(last) = self.entries.last_mut()
            && last.kind == UndoKind::Insert
            && last.position + last.inserted.len() == position
            && is_single_grapheme(inserted)
            && !starts_new_group(&last.inserted, inserted)
            && let Some(prev_time) = self.last_edit
            && now.saturating_duration_since(prev_time) < self.group_window
        {
            last.inserted.push_str(inserted);
            self.last_edit = Some(now);
            return;
        }
        self.push(UndoEntry {
            kind: UndoKind::Insert,
            position,
            removed: String::new(),
            inserted: inserted.to_string(),
            cursor_before,
        });
        self.last_edit = Some(now);
    }

    /// Record a pure-deletion edit.
    pub fn record_delete(&mut self, position: usize, removed: &str, cursor_before: TextPos) {
        self.push(UndoEntry {
            kind: UndoKind::Delete,
            position,
            removed: removed.to_string(),
            inserted: String::new(),
            cursor_before,
        });
        self.last_edit = Some(Instant::now());
    }

    /// Record a replace edit (removed + inserted).
    pub fn record_replace(
        &mut self,
        position: usize,
        removed: &str,
        inserted: &str,
        cursor_before: TextPos,
    ) {
        self.push(UndoEntry {
            kind: UndoKind::Replace,
            position,
            removed: removed.to_string(),
            inserted: inserted.to_string(),
            cursor_before,
        });
        self.last_edit = Some(Instant::now());
    }

    fn push(&mut self, entry: UndoEntry) {
        // New edit truncates the redo half.
        if self.head < self.entries.len() {
            self.entries.truncate(self.head);
        }
        self.entries.push(entry);
        self.head = self.entries.len();
        if self.entries.len() > self.max_depth {
            let drop = self.entries.len() - self.max_depth;
            self.entries.drain(..drop);
            self.head = self.head.saturating_sub(drop);
        }
    }

    /// Pop one undo entry. Returns `true` when an entry was applied.
    pub fn undo(&mut self, buf: &mut TextBuffer, cur: &mut TextCursor) -> bool {
        if !self.can_undo() {
            return false;
        }
        let entry = self.entries[self.head - 1].clone();
        // Defensive: an entry whose recorded span no longer fits the
        // live rope (external rewrite that bypassed the mirror/reflect
        // pair) would splice at stale offsets. Drop the whole stack
        // rather than corrupt the buffer.
        let span = match entry.kind {
            UndoKind::Insert | UndoKind::Replace => entry.position + entry.inserted.len(),
            UndoKind::Delete => entry.position,
        };
        if span > buf.len_bytes() {
            self.clear();
            return false;
        }
        self.head -= 1;
        apply_inverse(&entry, buf, cur);
        true
    }

    /// Re-apply the next redo entry. Returns `true` when an entry was applied.
    pub fn redo(&mut self, buf: &mut TextBuffer, cur: &mut TextCursor) -> bool {
        if !self.can_redo() {
            return false;
        }
        let entry = self.entries[self.head].clone();
        let span = match entry.kind {
            UndoKind::Insert => entry.position,
            UndoKind::Delete | UndoKind::Replace => entry.position + entry.removed.len(),
        };
        if span > buf.len_bytes() {
            self.clear();
            return false;
        }
        apply_forward(&entry, buf, cur);
        self.head += 1;
        true
    }
}

/// `true` when appending `next` to a coalesced insert run that currently
/// ends with `prev` should start a new undo group. Groups break at
/// whitespace boundaries in both directions ("hello| |world" becomes
/// three entries) so undo restores word-sized chunks.
fn starts_new_group(prev: &str, next: &str) -> bool {
    let prev_ws = prev.chars().next_back().map(char::is_whitespace);
    let next_ws = next.chars().next().map(char::is_whitespace);
    matches!((prev_ws, next_ws), (Some(a), Some(b)) if a != b)
}

fn apply_inverse(entry: &UndoEntry, buf: &mut TextBuffer, cur: &mut TextCursor) {
    match entry.kind {
        UndoKind::Insert => {
            // Remove inserted bytes.
            let end = entry.position + entry.inserted.len();
            let s_char = buf.rope.byte_to_char(entry.position);
            let e_char = buf.rope.byte_to_char(end);
            buf.rope.remove(s_char..e_char);
        }
        UndoKind::Delete => {
            let s_char = buf.rope.byte_to_char(entry.position);
            buf.rope.insert(s_char, &entry.removed);
        }
        UndoKind::Replace => {
            let end = entry.position + entry.inserted.len();
            let s_char = buf.rope.byte_to_char(entry.position);
            let e_char = buf.rope.byte_to_char(end);
            buf.rope.remove(s_char..e_char);
            let s_char = buf.rope.byte_to_char(entry.position);
            buf.rope.insert(s_char, &entry.removed);
        }
    }
    buf.version = buf.version.wrapping_add(1);
    cur.head = entry.cursor_before;
    cur.anchor = entry.cursor_before;
}

fn apply_forward(entry: &UndoEntry, buf: &mut TextBuffer, cur: &mut TextCursor) {
    match entry.kind {
        UndoKind::Insert => {
            let s_char = buf.rope.byte_to_char(entry.position);
            buf.rope.insert(s_char, &entry.inserted);
        }
        UndoKind::Delete => {
            let end = entry.position + entry.removed.len();
            let s_char = buf.rope.byte_to_char(entry.position);
            let e_char = buf.rope.byte_to_char(end);
            buf.rope.remove(s_char..e_char);
        }
        UndoKind::Replace => {
            let end = entry.position + entry.removed.len();
            let s_char = buf.rope.byte_to_char(entry.position);
            let e_char = buf.rope.byte_to_char(end);
            buf.rope.remove(s_char..e_char);
            let s_char = buf.rope.byte_to_char(entry.position);
            buf.rope.insert(s_char, &entry.inserted);
        }
    }
    buf.version = buf.version.wrapping_add(1);
    // Land cursor at end of inserted (or at position for pure delete).
    let new_byte = match entry.kind {
        UndoKind::Insert | UndoKind::Replace => entry.position + entry.inserted.len(),
        UndoKind::Delete => entry.position,
    };
    let s = buf.rope.to_string();
    let p = TextPos::from_byte(&s, new_byte);
    cur.head = p;
    cur.anchor = p;
}

fn is_single_grapheme(s: &str) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).count() == 1 && s != "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::text_model::TextBuffer;

    #[test]
    fn empty_stack_cannot_undo() {
        let s = UndoStack::default();
        assert!(!s.can_undo());
        assert!(!s.can_redo());
    }

    #[test]
    fn insert_then_undo_clears_buffer() {
        let mut s = UndoStack::default();
        let mut buf = TextBuffer::multi_line("hi");
        let mut cur = TextCursor::default();
        s.record_insert(0, "hi", TextPos::ZERO);
        assert!(s.can_undo());
        let ok = s.undo(&mut buf, &mut cur);
        assert!(ok);
        assert_eq!(buf.to_string(), "");
    }

    #[test]
    fn delete_then_undo_restores_text() {
        let mut s = UndoStack::default();
        let mut buf = TextBuffer::multi_line("");
        let mut cur = TextCursor::default();
        // Pretend we deleted "hi" from position 0 - restore via undo.
        s.record_delete(
            0,
            "hi",
            TextPos {
                byte: 2,
                grapheme: 2,
            },
        );
        assert!(s.can_undo());
        s.undo(&mut buf, &mut cur);
        assert_eq!(buf.to_string(), "hi");
    }

    #[test]
    fn redo_reapplies_after_undo() {
        let mut s = UndoStack::default();
        let mut buf = TextBuffer::multi_line("abc");
        let mut cur = TextCursor::default();
        s.record_insert(
            3,
            "!",
            TextPos {
                byte: 3,
                grapheme: 3,
            },
        );
        // simulate the buffer carrying the inserted char already
        buf.rope = ropey::Rope::from_str("abc!");
        s.undo(&mut buf, &mut cur);
        assert_eq!(buf.to_string(), "abc");
        assert!(s.can_redo());
        s.redo(&mut buf, &mut cur);
        assert_eq!(buf.to_string(), "abc!");
    }

    #[test]
    fn typing_coalesce_groups_single_chars() {
        let mut s = UndoStack::default();
        // Adjacent single-char inserts collapse.
        s.record_insert(0, "a", TextPos::ZERO);
        s.record_insert(1, "b", TextPos::ZERO);
        s.record_insert(2, "c", TextPos::ZERO);
        assert_eq!(s.len(), 1);
        assert_eq!(s.entries[0].inserted, "abc");
    }

    #[test]
    fn typing_coalesce_breaks_at_word_boundaries() {
        let mut s = UndoStack::default();
        // "hello world" typed one char at a time -> three word-ish
        // groups: "hello", " ", "world".
        for (i, ch) in "hello world".chars().enumerate() {
            s.record_insert(i, &ch.to_string(), TextPos::ZERO);
        }
        assert_eq!(s.len(), 3);
        assert_eq!(s.entries[0].inserted, "hello");
        assert_eq!(s.entries[1].inserted, " ");
        assert_eq!(s.entries[2].inserted, "world");
    }

    #[test]
    fn typing_coalesce_breaks_on_pause() {
        let mut s = UndoStack {
            group_window: std::time::Duration::ZERO,
            ..UndoStack::default()
        };
        // Zero group window => every keystroke is its own entry even
        // with no word boundary in between.
        s.record_insert(0, "a", TextPos::ZERO);
        s.record_insert(1, "b", TextPos::ZERO);
        assert_eq!(s.len(), 2, "elapsed pause breaks the coalesce group");
    }

    #[test]
    fn clear_drops_all_entries() {
        let mut s = UndoStack::default();
        s.record_insert(0, "abc", TextPos::ZERO);
        assert!(s.can_undo());
        s.clear();
        assert!(!s.can_undo());
        assert!(!s.can_redo());
        assert!(s.is_empty());
    }

    #[test]
    fn stale_entry_is_dropped_instead_of_panicking() {
        let mut s = UndoStack::default();
        // Recorded against a longer buffer than the live one.
        s.record_insert(10, "abc", TextPos::ZERO);
        let mut buf = TextBuffer::multi_line("x"); // len 1 < 13
        let mut cur = TextCursor::default();
        assert!(!s.undo(&mut buf, &mut cur), "stale span must not apply");
        assert!(s.is_empty(), "stack cleared after detecting staleness");
        assert_eq!(buf.to_string(), "x", "buffer untouched");
    }

    #[test]
    fn new_edit_truncates_redo_half() {
        let mut s = UndoStack::default();
        let mut buf = TextBuffer::multi_line("");
        let mut cur = TextCursor::default();
        s.record_insert(0, "X", TextPos::ZERO);
        buf.rope = ropey::Rope::from_str("X");
        s.undo(&mut buf, &mut cur);
        assert!(s.can_redo());
        // New edit invalidates redo.
        s.record_insert(0, "Y", TextPos::ZERO);
        assert!(!s.can_redo());
    }
}
