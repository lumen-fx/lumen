//! W3.2: text-editing message bus.
//!
//! Every text mutation flows through [`TextEditRequest`]. The single
//! [`lumen_text_edit::text_apply_edits`] system drains the bus and is the
//! ONLY system that mutates [`crate::text_model::TextBuffer`].
//!
//! Producers (`route_ime_events`, `type_into_focused`, pointer drag,
//! script `set_text(id, ...)`, paste) emit `TextEditRequest`; consumers
//! react to the post-mutation [`TextEditApplied`] event.

use bevy_ecs::message::Message;
use bevy_ecs::prelude::{Entity, SystemSet};
use std::ops::Range;
use std::sync::Arc;

use crate::text_model::TextPos;

/// Cross-crate [`SystemSet`] labels for the text-editing pipeline.
///
/// `lumen-input` tags its request producers (`type_into_focused`,
/// `route_ime_events`, `text_pointer_to_caret`, `text_pointer_drag_select`,
/// `cycle_focus_on_tab`) with [`Self::Producers`];
/// `lumen_text_edit::TextEditPlugin` schedules the single mutator
/// [`Self::Apply`] after that set and the content mirror [`Self::Mirror`]
/// after the mutator. Anchoring the edges on shared set labels (rather
/// than function references) keeps the two crates decoupled: either
/// plugin can be installed alone and the `.after(set)` edges are inert
/// against an empty set.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TextEditSet {
    /// Systems that emit [`TextEditRequest`] or mutate the legacy
    /// `TextContent` / `TextInput` pair directly.
    Producers,
    /// The single mutator (`text_apply_edits`).
    Apply,
    /// Post-mutation mirroring back into `TextContent` / `TextInput`.
    Mirror,
}

/// Symbolic anchor inside an edit request. Resolved against the live
/// [`crate::text_model::TextCursor`] / [`crate::text_model::TextBuffer`]
/// inside the mutator.
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    /// Resolve to the cursor head.
    Cursor,
    /// Explicit position.
    Position(TextPos),
    /// Resolve to the lower end of the selection (== cursor head when
    /// no selection).
    SelectionStart,
    /// Resolve to the upper end of the selection.
    SelectionEnd,
    /// Buffer start.
    DocumentStart,
    /// Buffer end.
    DocumentEnd,
}

/// Single-axis cursor motion (mirrors `QTextCursor::movePosition` + GTK
/// `GtkMovementStep`).
#[derive(Clone, Copy, Debug)]
pub enum CursorMotion {
    /// One extended grapheme cluster left.
    CharLeft,
    /// One extended grapheme cluster right.
    CharRight,
    /// Previous word boundary.
    WordLeft,
    /// Next word boundary.
    WordRight,
    /// Start of current line.
    LineStart,
    /// End of current line.
    LineEnd,
    /// Up one visual line (multi-line buffers).
    LineUp,
    /// Down one visual line.
    LineDown,
    /// Start of document.
    DocStart,
    /// End of document.
    DocEnd,
}

/// Selection modifier for [`CursorMotion`] requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveMode {
    /// Anchor follows head (collapses selection).
    #[default]
    MoveAnchor,
    /// Anchor stays put (extends selection).
    KeepAnchor,
}

/// One text-editing request addressed to `entity`'s [`crate::text_model::TextBuffer`].
///
/// Producers write into a [`bevy_ecs::message::Messages`] queue; the
/// single [`lumen_text_edit::text_apply_edits`] system drains them.
#[derive(Message, Clone, Debug)]
pub enum TextEditRequest {
    /// Insert `text` at `at`.
    Insert {
        /// Target editable.
        entity: Entity,
        /// Resolved insertion position.
        at: TextPos,
        /// Inserted bytes (Arc'd so producers don't always allocate).
        text: Arc<str>,
    },
    /// Delete the byte range.
    Delete {
        /// Target editable.
        entity: Entity,
        /// Range to delete, in bytes.
        range: Range<usize>,
    },
    /// Replace `range` with `text` (IME's `replacementStart/Length`).
    Replace {
        /// Target editable.
        entity: Entity,
        /// Range to replace, in bytes.
        range: Range<usize>,
        /// Replacement bytes.
        text: Arc<str>,
    },
    /// Move the cursor.
    MoveCursor {
        /// Target editable.
        entity: Entity,
        /// Motion axis.
        motion: CursorMotion,
        /// Selection modifier.
        mode: MoveMode,
    },
    /// Set selection range (in bytes).
    Select {
        /// Target editable.
        entity: Entity,
        /// Range of bytes to select.
        range: Range<usize>,
    },
    /// Set cursor to an explicit position (collapses selection).
    SetCursor {
        /// Target editable.
        entity: Entity,
        /// New cursor position.
        pos: TextPos,
    },
    /// Move the selection head to `pos` while keeping the current anchor
    /// (Shift+click, pointer drag). Unlike [`Self::Select`] the anchor
    /// side is preserved, so repeated extends pivot around the same
    /// fixed end regardless of direction.
    ExtendSelection {
        /// Target editable.
        entity: Entity,
        /// New selection head.
        pos: TextPos,
    },
    /// Select all.
    SelectAll {
        /// Target editable.
        entity: Entity,
    },
    /// Pop one entry off the undo stack.
    Undo {
        /// Target editable.
        entity: Entity,
    },
    /// Re-apply the next redo entry.
    Redo {
        /// Target editable.
        entity: Entity,
    },
    /// Begin an IME composition (no-op if already active).
    ImeBegin {
        /// Target editable.
        entity: Entity,
    },
    /// Update the IME preedit.
    ImeUpdate {
        /// Target editable.
        entity: Entity,
        /// New preedit string.
        text: Arc<str>,
        /// Caret byte offset inside `text`.
        caret_in_preedit: usize,
    },
    /// Commit the IME preedit; replaces `replace_range` with `text` if
    /// `replace_range` is `Some`, otherwise inserts `text` at the cursor.
    ImeCommit {
        /// Target editable.
        entity: Entity,
        /// Final text.
        text: Arc<str>,
        /// Optional IME-requested replacement range (W3.5).
        replace_range: Option<Range<usize>>,
    },
    /// Cancel any active IME preedit without committing.
    ImeCancel {
        /// Target editable.
        entity: Entity,
    },
}

impl TextEditRequest {
    /// The target entity of this request.
    pub fn entity(&self) -> Entity {
        match self {
            TextEditRequest::Insert { entity, .. }
            | TextEditRequest::Delete { entity, .. }
            | TextEditRequest::Replace { entity, .. }
            | TextEditRequest::MoveCursor { entity, .. }
            | TextEditRequest::Select { entity, .. }
            | TextEditRequest::SetCursor { entity, .. }
            | TextEditRequest::ExtendSelection { entity, .. }
            | TextEditRequest::SelectAll { entity }
            | TextEditRequest::Undo { entity }
            | TextEditRequest::Redo { entity }
            | TextEditRequest::ImeBegin { entity }
            | TextEditRequest::ImeUpdate { entity, .. }
            | TextEditRequest::ImeCommit { entity, .. }
            | TextEditRequest::ImeCancel { entity } => *entity,
        }
    }
}

/// Classification of an applied edit for downstream observers (binding
/// push, validators, undo coalescing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppliedKind {
    /// User-driven insertion (a `Key::Character` arm, IME commit, paste).
    Insert,
    /// User-driven deletion (Backspace, Delete, selection-replace).
    Delete,
    /// Replacement (IME commit with replace_range, paste-over-selection).
    Replace,
    /// Pure cursor / selection move; no text mutation.
    CursorMove,
    /// Undo / Redo replay.
    UndoRedo,
}

/// Emitted by [`lumen_text_edit::text_apply_edits`] after every successful
/// mutation. Replaces ad-hoc `Changed<TextContent>` snooping so signal
/// binding / validators / undo coalescing react only to real edits.
#[derive(Message, Clone, Debug)]
pub struct TextEditApplied {
    /// Mutated editable.
    pub entity: Entity,
    /// Buffer version after the edit.
    pub version: u64,
    /// What kind of edit.
    pub kind: AppliedKind,
    /// Cursor byte position before the edit (for undo).
    pub before_byte: usize,
    /// Cursor byte position after the edit.
    pub after_byte: usize,
}

/// Reasons an edit might be rejected by validators / single-line guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// Single-line buffer received `\n`.
    NewlineInSingleLine,
    /// Validator rejected.
    Validator,
    /// Undo stack empty.
    NothingToUndo,
    /// Redo stack empty.
    NothingToRedo,
    /// Target entity missing required components.
    BadTarget,
}

/// Emitted when [`lumen_text_edit::text_apply_edits`] drops a request.
#[derive(Message, Clone, Debug)]
pub struct TextEditRejected {
    /// Target.
    pub entity: Entity,
    /// Why.
    pub reason: RejectReason,
}

/// Backend -> core: the OS IME asked for surrounding text (W3.5).
/// `text_update_surrounding_response` replies with [`ImeSurroundingResponse`].
#[derive(Message, Clone, Copy, Debug)]
pub struct ImeSurroundingRequested {
    /// Target editable (typically the focused entity).
    pub entity: Entity,
}

/// Core -> backend: surrounding-text reply. Backend forwards to the OS
/// IME (Wayland text-input-v3 / IBus).
#[derive(Message, Clone, Debug)]
pub struct ImeSurroundingResponse {
    /// Target editable.
    pub entity: Entity,
    /// Snapshot of the buffer text.
    pub text: Arc<str>,
    /// Selection anchor byte offset (== cursor when no selection).
    pub anchor_byte: usize,
    /// Cursor byte offset.
    pub cursor_byte: usize,
}
