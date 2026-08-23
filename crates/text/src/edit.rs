//! W3.2-W3.4: Lumen text-editing systems.
//!
//! Single-mutator architecture. Every text mutation (keyboard, IME,
//! script, paste, undo, pointer drag) flows through
//! [`lumen_core::text_events::TextEditRequest`]; the single
//! [`text_apply_edits`] system in this crate is the only place that
//! mutates [`lumen_core::text_model::TextBuffer`].
//!
//! Coexistence strategy. The existing `TextContent` + `TextInput` model
//! (bd23f51 surgical fix) keeps working unchanged through the W3
//! rollout: [`text_attach_buffer`] adds a [`TextBuffer`] / [`TextCursor`]
//! / [`UndoStack`] alongside every `TextInput`, and
//! [`text_mirror_buffer_to_content`] writes the rope back into
//! `TextContent` after each edit so the existing renderer / binding
//! systems keep reading the same shape.
//!
//! See `docs/audits/text-editing.md:172-275` for the rewrite spec.

#![allow(missing_docs)]

use crate::TextGeometry;
use crate::undo::UndoStack;

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_core::text_events::AppliedKind;
use lumen_core::time::Instant;
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// Query data for [`text_attach_buffer`]: `TextInput` entities that haven't
/// yet had a `TextBuffer` attached. Factored out to keep clippy's
/// `type_complexity` lint quiet.
type UnattachedTextInputData<'a> = (Entity, &'a TextContent, &'a TextInput);
/// Query filter for [`text_attach_buffer`]; see [`UnattachedTextInputData`].
type UnattachedTextInputFilter = (With<TextInput>, Without<TextBuffer>);

/// Bootstrap. For every `TextInput` entity without a `TextBuffer`, attach
/// `TextBuffer` (seeded from `TextContent`), `TextCursor` (seeded from
/// `TextInput.cursor`), an empty `UndoStack`, and the `TextEditable`
/// marker. Idempotent - runs every tick at near-zero cost when nothing
/// new is spawned.
pub fn text_attach_buffer(
    mut commands: Commands,
    q: Query<UnattachedTextInputData, UnattachedTextInputFilter>,
) {
    for (e, tc, ti) in &q {
        let kind = if ti.multiline {
            TextBufferKind::MultiLine
        } else {
            TextBufferKind::SingleLine
        };
        let mut buf = TextBuffer {
            rope: ropey::Rope::from_str(&tc.0),
            version: 0,
            kind,
        };
        let _ = &mut buf; // silence unused-mut on minimal builds
        let cursor = {
            let head = TextPos::from_byte(&tc.0, ti.cursor);
            let anchor = match ti.selection_anchor {
                Some(a) => TextPos::from_byte(&tc.0, a),
                None => head,
            };
            TextCursor {
                head,
                anchor,
                affinity: Affinity::default(),
                goal_x: None,
            }
        };
        commands
            .entity(e)
            .insert((buf, cursor, UndoStack::default(), TextEditable));
    }
}

/// The single text mutator (W3.2). Drains
/// [`lumen_core::text_events::TextEditRequest`] and mutates
/// [`TextBuffer`] / [`TextCursor`] / [`UndoStack`] / [`ImePreedit`] in
/// place. Emits [`TextEditApplied`] after each accepted edit and
/// [`TextEditRejected`] for validator / single-line newline / undo-empty
/// rejections.
///
/// Execution-order contract:
/// - Runs AFTER every producer (route_ime_events, type_into_focused,
///   pointer drag, script set_text, paste).
/// - Runs BEFORE `text_mirror_buffer_to_content` so the mirror sees the
///   post-mutation rope.
/// - Runs BEFORE bind-text push systems so they see only post-mutation
///   text.
#[allow(clippy::too_many_arguments)]
pub fn text_apply_edits(
    mut requests: MessageReader<TextEditRequest>,
    mut applied: MessageWriter<TextEditApplied>,
    mut rejected: MessageWriter<TextEditRejected>,
    mut q: Query<(
        &mut TextBuffer,
        &mut TextCursor,
        &mut UndoStack,
        Option<&mut ImePreedit>,
    )>,
    mut commands: Commands,
) {
    for req in requests.read() {
        let entity = req.entity();
        let Ok((mut buf, mut cur, mut undo, mut preedit)) = q.get_mut(entity) else {
            rejected.write(TextEditRejected {
                entity,
                reason: RejectReason::BadTarget,
            });
            continue;
        };
        let before_byte = cur.head.byte;
        let outcome = apply_one(
            req,
            entity,
            &mut buf,
            &mut cur,
            &mut undo,
            preedit.as_deref_mut(),
            &mut commands,
        );
        match outcome {
            ApplyOutcome::Applied(kind) => {
                applied.write(TextEditApplied {
                    entity,
                    version: buf.version,
                    kind,
                    before_byte,
                    after_byte: cur.head.byte,
                });
            }
            ApplyOutcome::Rejected(reason) => {
                rejected.write(TextEditRejected { entity, reason });
            }
        }
    }
}

enum ApplyOutcome {
    Applied(AppliedKind),
    Rejected(RejectReason),
}

#[allow(clippy::too_many_arguments)]
fn apply_one(
    req: &TextEditRequest,
    entity: Entity,
    buf: &mut TextBuffer,
    cur: &mut TextCursor,
    undo: &mut UndoStack,
    preedit: Option<&mut ImePreedit>,
    commands: &mut Commands,
) -> ApplyOutcome {
    use TextEditRequest::*;
    match req {
        Insert { at, text, .. } => {
            let sanitized = sanitize_for_buffer(buf, text);
            if let Some(reason) = sanitized.reject {
                return ApplyOutcome::Rejected(reason);
            }
            let pos = resolve_pos(buf, Anchor::Position(*at));
            insert_text(buf, cur, undo, pos.byte, &sanitized.text);
            ApplyOutcome::Applied(AppliedKind::Insert)
        }
        Delete { range, .. } => {
            let r = clamp_range(buf, range.clone());
            if r.is_empty() {
                return ApplyOutcome::Rejected(RejectReason::BadTarget);
            }
            delete_range(buf, cur, undo, r);
            ApplyOutcome::Applied(AppliedKind::Delete)
        }
        Replace { range, text, .. } => {
            let sanitized = sanitize_for_buffer(buf, text);
            if let Some(reason) = sanitized.reject {
                return ApplyOutcome::Rejected(reason);
            }
            let r = clamp_range(buf, range.clone());
            replace_range(buf, cur, undo, r, &sanitized.text);
            ApplyOutcome::Applied(AppliedKind::Replace)
        }
        MoveCursor { motion, mode, .. } => {
            let new_head = move_cursor(buf, cur.head, *motion);
            let keep_anchor = matches!(mode, MoveMode::KeepAnchor);
            cur.move_head(new_head, keep_anchor);
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        Select { range, .. } => {
            let r = clamp_range(buf, range.clone());
            cur.anchor = TextPos::from_buffer_byte(buf, r.start);
            cur.head = TextPos::from_buffer_byte(buf, r.end);
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        SetCursor { pos, .. } => {
            let p = resolve_pos(buf, Anchor::Position(*pos));
            cur.head = p;
            cur.anchor = p;
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        ExtendSelection { pos, .. } => {
            let p = resolve_pos(buf, Anchor::Position(*pos));
            cur.move_head(p, true);
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        SelectAll { .. } => {
            cur.anchor = TextPos::ZERO;
            cur.head = TextPos::from_buffer_byte(buf, buf.len_bytes());
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        Undo { .. } => {
            if !undo.undo(buf, cur) {
                return ApplyOutcome::Rejected(RejectReason::NothingToUndo);
            }
            ApplyOutcome::Applied(AppliedKind::UndoRedo)
        }
        Redo { .. } => {
            if !undo.redo(buf, cur) {
                return ApplyOutcome::Rejected(RejectReason::NothingToRedo);
            }
            ApplyOutcome::Applied(AppliedKind::UndoRedo)
        }
        ImeBegin { .. } => {
            if preedit.is_none() {
                commands.entity(entity).insert(ImePreedit::default());
            }
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        ImeUpdate {
            text,
            caret_in_preedit,
            ..
        } => {
            let caret = (*caret_in_preedit).min(text.len());
            match preedit {
                Some(p) => {
                    p.text = text.to_string();
                    p.caret_in_preedit = caret;
                }
                None => {
                    commands.entity(entity).insert(ImePreedit {
                        text: text.to_string(),
                        caret_in_preedit: caret,
                    });
                }
            }
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
        ImeCommit {
            text,
            replace_range,
            ..
        } => {
            let sanitized = sanitize_for_buffer(buf, text);
            if let Some(reason) = sanitized.reject {
                // Even a rejected commit clears preedit.
                commands.entity(entity).remove::<ImePreedit>();
                return ApplyOutcome::Rejected(reason);
            }
            // Drop selection / replace_range / cursor insertion in priority.
            let kind = if let Some(rr) = replace_range {
                let r = clamp_range(buf, rr.clone());
                replace_range_op(buf, cur, undo, r, &sanitized.text);
                AppliedKind::Replace
            } else if let Some(sel) = cur.selection_range() {
                replace_range_op(buf, cur, undo, sel, &sanitized.text);
                AppliedKind::Replace
            } else {
                insert_text(buf, cur, undo, cur.head.byte, &sanitized.text);
                AppliedKind::Insert
            };
            commands.entity(entity).remove::<ImePreedit>();
            ApplyOutcome::Applied(kind)
        }
        ImeCancel { .. } => {
            commands.entity(entity).remove::<ImePreedit>();
            ApplyOutcome::Applied(AppliedKind::CursorMove)
        }
    }
}

struct Sanitized {
    text: String,
    reject: Option<RejectReason>,
}

fn sanitize_for_buffer(buf: &TextBuffer, text: &str) -> Sanitized {
    if buf.is_single_line() && text.contains('\n') {
        // Per Qt's QLineEdit::setText: strip newlines silently for
        // single-line; we follow that convention rather than rejecting
        // the entire edit so paste-with-newlines still inserts visible
        // content.
        let stripped: String = text.replace('\n', " ");
        return Sanitized {
            text: stripped,
            reject: None,
        };
    }
    Sanitized {
        text: text.to_string(),
        reject: None,
    }
}

fn resolve_pos(buf: &TextBuffer, anchor: Anchor) -> TextPos {
    match anchor {
        Anchor::Cursor => TextPos::from_buffer_byte(buf, buf.len_bytes()),
        Anchor::Position(p) => {
            // Snap to char boundary.
            TextPos::from_buffer_byte(buf, p.byte)
        }
        Anchor::SelectionStart | Anchor::DocumentStart => TextPos::ZERO,
        Anchor::SelectionEnd | Anchor::DocumentEnd => {
            TextPos::from_buffer_byte(buf, buf.len_bytes())
        }
    }
}

fn clamp_range(buf: &TextBuffer, mut r: Range<usize>) -> Range<usize> {
    let len = buf.len_bytes();
    if r.start > len {
        r.start = len;
    }
    if r.end > len {
        r.end = len;
    }
    if r.start > r.end {
        std::mem::swap(&mut r.start, &mut r.end);
    }
    // Snap both ends to char boundaries.
    let s = buf.rope.to_string();
    let mut start = r.start;
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = r.end;
    while end < s.len() && !s.is_char_boundary(end) {
        end += 1;
    }
    start..end
}

/// Insert `text` at `at_byte` (must be a char boundary; producers clamp
/// via [`TextPos::from_byte`]), moving caret + anchor past the insertion
/// and recording an undo entry. Public so `lumen-input`'s keystroke
/// router can mutate the buffer through the same single implementation
/// the request mutator uses.
pub fn insert_text(
    buf: &mut TextBuffer,
    cur: &mut TextCursor,
    undo: &mut UndoStack,
    at_byte: usize,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    let char_idx = buf.rope.byte_to_char(at_byte);
    let before_pos = cur.head;
    buf.rope.insert(char_idx, text);
    buf.version = buf.version.wrapping_add(1);
    let new_byte = at_byte + text.len();
    let new_head = TextPos::from_buffer_byte(buf, new_byte);
    cur.head = new_head;
    cur.anchor = new_head;
    undo.record_insert(at_byte, text, before_pos);
}

/// Delete the byte `range` (char-boundary clamped by callers), collapse
/// the caret to `range.start`, and record an undo entry. See
/// [`insert_text`] for the visibility rationale.
pub fn delete_range(
    buf: &mut TextBuffer,
    cur: &mut TextCursor,
    undo: &mut UndoStack,
    range: Range<usize>,
) {
    if range.is_empty() {
        return;
    }
    let removed = {
        let s_char = buf.rope.byte_to_char(range.start);
        let e_char = buf.rope.byte_to_char(range.end);
        let removed = buf.rope.slice(s_char..e_char).to_string();
        buf.rope.remove(s_char..e_char);
        removed
    };
    buf.version = buf.version.wrapping_add(1);
    let new_head = TextPos::from_buffer_byte(buf, range.start);
    let before_pos = cur.head;
    cur.head = new_head;
    cur.anchor = new_head;
    undo.record_delete(range.start, &removed, before_pos);
}

fn replace_range_op(
    buf: &mut TextBuffer,
    cur: &mut TextCursor,
    undo: &mut UndoStack,
    range: Range<usize>,
    text: &str,
) {
    let removed = {
        let s_char = buf.rope.byte_to_char(range.start);
        let e_char = buf.rope.byte_to_char(range.end);
        let removed = buf.rope.slice(s_char..e_char).to_string();
        if !removed.is_empty() {
            buf.rope.remove(s_char..e_char);
        }
        removed
    };
    let s_char = buf.rope.byte_to_char(range.start);
    if !text.is_empty() {
        buf.rope.insert(s_char, text);
    }
    buf.version = buf.version.wrapping_add(1);
    let new_byte = range.start + text.len();
    let before_pos = cur.head;
    let new_head = TextPos::from_buffer_byte(buf, new_byte);
    cur.head = new_head;
    cur.anchor = new_head;
    undo.record_replace(range.start, &removed, text, before_pos);
}

/// Replace the byte `range` with `text`, landing the caret after the
/// replacement and recording a single undo entry. See [`insert_text`]
/// for the visibility rationale.
pub fn replace_range(
    buf: &mut TextBuffer,
    cur: &mut TextCursor,
    undo: &mut UndoStack,
    range: Range<usize>,
    text: &str,
) {
    replace_range_op(buf, cur, undo, range, text);
}

/// Resolve a [`CursorMotion`] from `from` against the buffer contents.
/// Line motions step logical (`\n`-delimited) lines with byte-column
/// preservation. Public so the keystroke router shares one motion
/// implementation with the request mutator.
pub fn move_cursor(buf: &TextBuffer, from: TextPos, motion: CursorMotion) -> TextPos {
    let text = buf.rope.to_string();
    let byte = from.byte.min(text.len());
    let new_byte = match motion {
        CursorMotion::CharLeft => prev_grapheme_boundary(&text, byte),
        CursorMotion::CharRight => next_grapheme_boundary(&text, byte),
        CursorMotion::WordLeft => prev_word_boundary(&text, byte),
        CursorMotion::WordRight => next_word_boundary(&text, byte),
        CursorMotion::LineStart => line_start(&text, byte),
        CursorMotion::LineEnd => line_end(&text, byte),
        CursorMotion::LineUp => {
            // Lacking shaped-line geometry here, we step one logical line.
            let ls = line_start(&text, byte);
            if ls == 0 {
                0
            } else {
                let prev_end = ls - 1;
                let prev_start = line_start(&text, prev_end);
                let col = byte - ls;
                (prev_start + col).min(prev_end)
            }
        }
        CursorMotion::LineDown => {
            let le = line_end(&text, byte);
            if le == text.len() {
                text.len()
            } else {
                let next_start = le + 1;
                let next_end = line_end(&text, next_start);
                let col = byte - line_start(&text, byte);
                (next_start + col).min(next_end)
            }
        }
        CursorMotion::DocStart => 0,
        CursorMotion::DocEnd => text.len(),
    };
    TextPos::from_byte(&text, new_byte)
}

/// D6: resolve a soft-wrap-aware vertical / line-bound motion against shaped
/// [`TextGeometry`]. Returns `(new head, new goal_x)`: the goal x to store
/// back on the cursor (Qt `verticalMovementX`, D5).
///
/// - `LineUp` / `LineDown`: step one visual line, landing on the byte whose
///   caret x is nearest the sticky `goal_x` (seeded from the caret's current
///   x when `goal_x` is `None`); the goal is preserved across the move so a
///   run of Up/Down tracks the original column. At the top / bottom visual
///   line the motion delegates to the byte-only [`move_cursor`] (doc start /
///   end), matching Qt.
/// - `LineStart` / `LineEnd`: snap to the VISUAL line's byte bounds (soft-wrap
///   aware), clearing the goal like any horizontal motion.
/// - Any other motion delegates to [`move_cursor`] and clears the goal.
///
/// No shaper dependency: the caller passes the geometry the producer already
/// shaped. Falls back naturally when the geometry is empty (single logical
/// line via [`move_cursor`]).
pub fn move_cursor_visual(
    geom: &TextGeometry,
    buf: &TextBuffer,
    cur: TextCursor,
    motion: CursorMotion,
) -> (TextPos, Option<f32>) {
    let text = buf.rope.to_string();
    let head = cur.head.byte.min(text.len());
    match motion {
        CursorMotion::LineUp | CursorMotion::LineDown => {
            let count = geom.line_count();
            if count == 0 {
                return (move_cursor(buf, cur.head, motion), None);
            }
            let line = geom.visual_line_of_byte(head);
            let target = match motion {
                CursorMotion::LineUp => line.saturating_sub(1),
                _ => (line + 1).min(count - 1),
            };
            if target == line {
                // Already on the top / bottom visual line: fall through to
                // the byte motion (doc start on Up, doc end on Down) but keep
                // the goal so a subsequent reversal still tracks the column.
                return (move_cursor(buf, cur.head, motion), cur.goal_x);
            }
            let goal = cur.goal_x.unwrap_or_else(|| geom.byte_to_caret(head).x);
            let landed = geom.byte_at_line_x(target, goal);
            (TextPos::from_byte(&text, landed), Some(goal))
        }
        CursorMotion::LineStart | CursorMotion::LineEnd => {
            let line = geom.visual_line_of_byte(head);
            let (s, e) = geom.visual_line_bounds(line);
            let b = if matches!(motion, CursorMotion::LineStart) {
                s
            } else {
                e
            };
            (TextPos::from_byte(&text, b), None)
        }
        _ => (move_cursor(buf, cur.head, motion), None),
    }
}

/// Snap `from` down to the nearest UTF-8 code point boundary.
fn snap_char_boundary(s: &str, from: usize) -> usize {
    let mut i = from.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Previous extended grapheme cluster boundary before `from`.
///
/// Drives [`CursorMotion::CharLeft`], so Left / Shift+Left / forward-word
/// fallbacks all step whole clusters (Qt `previousCursorPosition`, Slint
/// `GraphemeCursor::prev_boundary`). Backspace does NOT use this; see
/// [`prev_code_point_boundary`].
pub fn prev_grapheme_boundary(s: &str, from: usize) -> usize {
    let from = snap_char_boundary(s, from);
    let mut cursor = GraphemeCursor::new(from, s.len(), true);
    cursor.prev_boundary(s, 0).ok().flatten().unwrap_or(0)
}

/// Next extended grapheme cluster boundary after `from`.
///
/// Drives [`CursorMotion::CharRight`], which forward Delete also resolves
/// through, so Delete removes a whole cluster rather than one scalar.
pub fn next_grapheme_boundary(s: &str, from: usize) -> usize {
    let from = snap_char_boundary(s, from);
    let mut cursor = GraphemeCursor::new(from, s.len(), true);
    cursor.next_boundary(s, 0).ok().flatten().unwrap_or(s.len())
}

/// Previous code point boundary before `from`.
///
/// Backspace only. Qt's `backspace()` steps one code unit and Slint keeps a
/// dedicated `PreviousCharacter` direction for the same reason: peeling a
/// combining mark off the base character backwards is the expected editing
/// behavior. Keep this asymmetry with [`prev_grapheme_boundary`].
pub fn prev_code_point_boundary(s: &str, from: usize) -> usize {
    let from = snap_char_boundary(s, from);
    if from == 0 {
        return 0;
    }
    snap_char_boundary(s, from - 1)
}

fn prev_word_boundary(s: &str, from: usize) -> usize {
    let mut prev = 0usize;
    for (off, _) in s.split_word_bound_indices() {
        if off >= from {
            return prev;
        }
        prev = off;
    }
    prev
}

fn next_word_boundary(s: &str, from: usize) -> usize {
    for (off, w) in s.split_word_bound_indices() {
        let end = off + w.len();
        if end > from {
            return end;
        }
    }
    s.len()
}

fn line_start(s: &str, from: usize) -> usize {
    s[..from].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_end(s: &str, from: usize) -> usize {
    s[from..].find('\n').map(|i| from + i).unwrap_or(s.len())
}

/// Mirror system. After every successful edit, write the rope back into
/// [`TextContent`] so the existing renderer / `apply_text_bindings` push
/// path keeps reading the same shape. Also writes the byte cursor back
/// into [`TextInput.cursor`] for legacy keystroke routes.
///
/// Gated on [`TextEditApplied`] (W3.2) - no per-tick sweep.
#[allow(clippy::type_complexity)]
pub fn text_mirror_buffer_to_content(
    mut applied: MessageReader<TextEditApplied>,
    mut q: Query<(
        &TextBuffer,
        &TextCursor,
        &mut TextContent,
        Option<&mut TextInput>,
    )>,
) {
    for ev in applied.read() {
        let Ok((buf, cur, mut tc, ti)) = q.get_mut(ev.entity) else {
            continue;
        };
        let s = buf.rope.to_string();
        if tc.0 != s {
            tc.0 = s;
        }
        if let Some(mut ti) = ti {
            ti.cursor = cur.head.byte;
            ti.selection_anchor = if cur.is_empty() {
                None
            } else {
                Some(cur.anchor.byte)
            };
        }
    }
}

/// Pre-keystroke gate. When the existing `route_ime_events` /
/// `type_into_focused` path mutates [`TextContent`] directly (the
/// bd23f51 surgical fix), reflect those changes back into [`TextBuffer`]
/// so the W3 model stays in sync. Runs AFTER the legacy keystroke path
/// and BEFORE shaping/render.
///
/// Triggered by `TextContent`'s Changed<> bit, but the body is a no-op
/// when the rope already matches (i.e. the change came from
/// `text_mirror_buffer_to_content`).
#[allow(clippy::type_complexity)]
pub fn text_reflect_content_to_buffer(
    mut q: Query<
        (
            &TextContent,
            &mut TextBuffer,
            Option<&TextInput>,
            &mut TextCursor,
            Option<&mut UndoStack>,
        ),
        Changed<TextContent>,
    >,
) {
    for (tc, mut buf, ti, mut cur, undo) in &mut q {
        let rope_str = buf.rope.to_string();
        if rope_str == tc.0 {
            continue;
        }
        // External (signal binding, legacy keystroke fallback) wrote
        // into TextContent. Pull it into the rope, bump version, and
        // re-seed the cursor.
        buf.rope = ropey::Rope::from_str(&tc.0);
        buf.version = buf.version.wrapping_add(1);
        let byte = match ti {
            Some(ti) => ti.cursor.min(tc.0.len()),
            None => tc.0.len(),
        };
        let p = TextPos::from_byte(&tc.0, byte);
        cur.head = p;
        cur.anchor = p;
        // The recorded undo deltas are positioned against the previous
        // rope; replaying them against externally-replaced content
        // would splice at stale offsets. Clear, matching Qt's
        // `QLineEdit::setText` behavior.
        if let Some(mut undo) = undo {
            undo.clear();
        }
    }
}

/// Caret blink driver (W2 Qt-polish: text-editing core).
///
/// While a [`TextInput`] entity holds keyboard focus:
/// - toggles [`CaretBlink::visible`] every [`CaretBlink::period`]
///   (~530 ms, Qt's cadence),
/// - resets the phase to *visible* whenever the focused input's
///   `TextInput` state changes (any edit or caret/selection move - the
///   keystroke router and the mirror both write those fields in
///   lockstep with the buffer) or focus moves to a different entity,
/// - marks [`FrameDirty`] on each toggle so the backend re-encodes, and
/// - raises [`AnimationsActive`] so the redraw scheduler keeps ticking
///   between OS events (same self-scheduling contract the hover/press
///   tweens use).
///
/// When no text input is focused the system requests nothing: the flag
/// stays untouched at `visible`, `AnimationsActive` is not raised, and
/// the app reaches full idle quiescence (zero blink wakeups).
pub fn caret_blink(
    tracker: Res<lumen_core::input::FocusTracker>,
    inputs: Query<(), With<TextInput>>,
    changed_inputs: Query<(), Changed<TextInput>>,
    mut blink: ResMut<lumen_core::components::CaretBlink>,
    animations: Option<Res<lumen_core::render_world::AnimationsActive>>,
    frame_dirty: Option<ResMut<lumen_core::render_world::FrameDirty>>,
    mut last_focused: Local<Option<Entity>>,
) {
    let focused_input = tracker.0.filter(|e| inputs.contains(*e));
    let Some(entity) = focused_input else {
        // Unfocused: park fully. Leave the flag visible so the caret
        // shows immediately on the next focus.
        if !blink.visible {
            blink.reset();
        }
        *last_focused = None;
        return;
    };
    let was_visible = blink.visible;
    let fresh_focus = *last_focused != Some(entity);
    if fresh_focus || changed_inputs.contains(entity) {
        // Focus landed here, or the caret moved / text changed: restart
        // the phase at "visible" so the bar never blinks mid-keystroke.
        blink.reset();
        *last_focused = Some(entity);
    } else {
        let periods = blink
            .phase
            .elapsed()
            .as_millis()
            .checked_div(blink.period.as_millis())
            .unwrap_or(0);
        blink.visible = periods % 2 == 0;
    }
    // Repaint on a visibility flip, and on fresh focus (focus markers
    // aren't in `roll_up_frame_dirty`'s watch list, so nothing else
    // guarantees the caret's first frame).
    if (blink.visible != was_visible || fresh_focus)
        && let Some(mut fd) = frame_dirty
    {
        fd.dirty = true;
    }
    // Keep the event loop ticking while an input is focused so the next
    // half-phase actually gets evaluated (there is no OS event to wake
    // us otherwise). Cleared at tick start; not raised when unfocused.
    if let Some(anims) = animations {
        anims.request();
    }
}

/// Helper used by producers: build a `TextEditRequest::Insert` from a
/// string. Inserts at the cursor head by resolving inside the mutator.
pub fn make_insert(entity: Entity, at: TextPos, text: impl Into<Arc<str>>) -> TextEditRequest {
    TextEditRequest::Insert {
        entity,
        at,
        text: text.into(),
    }
}

/// Plugin that wires the text-edit systems.
///
/// Ordering inside `TickStage::Systems`:
/// - `text_attach_buffer` early (so freshly-spawned `<input>` has a buffer).
/// - `text_apply_edits` in [`TextEditSet::Apply`], after
///   [`TextEditSet::Producers`] - `lumen-input` tags its request
///   producers (`type_into_focused`, `text_pointer_to_caret`,
///   `text_pointer_drag_select`, `route_ime_events`,
///   `cycle_focus_on_tab`) with that shared label, so pointer edits
///   apply on the tick they were produced. The set edge is inert when
///   no producer is installed.
/// - `text_mirror_buffer_to_content` ([`TextEditSet::Mirror`]) after
///   `text_apply_edits`.
/// - `text_reflect_content_to_buffer` after the mirror (catches signal
///   bindings and any legacy direct `TextContent` writes).
/// - `caret_blink` after the mirror so a same-tick edit resets the
///   phase before extract reads it.
///
/// Bind-text mirroring runs in its own stage; the mirror system writes
/// `TextContent` so `push_textinput_to_signal` (which filters on
/// `Changed<TextContent>`) keeps picking up edits.
pub struct TextEditPlugin;

impl Plugin for TextEditPlugin {
    fn build(self, app: &mut lumen_core::app::App) {
        app.world
            .init_resource::<bevy_ecs::message::Messages<TextEditRequest>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<TextEditApplied>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<TextEditRejected>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<ImeSurroundingRequested>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<ImeSurroundingResponse>>();
        app.world
            .init_resource::<lumen_core::components::CaretBlink>();
        // Bootstrap runs every tick - idempotent and cheap.
        app.add_systems(TickStage::Systems, text_attach_buffer);
        app.add_systems(
            TickStage::Systems,
            text_apply_edits
                .in_set(lumen_core::text_events::TextEditSet::Apply)
                .after(text_attach_buffer)
                .after(lumen_core::text_events::TextEditSet::Producers),
        );
        app.add_systems(
            TickStage::Systems,
            text_mirror_buffer_to_content
                .in_set(lumen_core::text_events::TextEditSet::Mirror)
                .after(text_apply_edits),
        );
        app.add_systems(
            TickStage::Systems,
            text_reflect_content_to_buffer.after(text_mirror_buffer_to_content),
        );
        app.add_systems(
            TickStage::Systems,
            caret_blink.after(text_mirror_buffer_to_content),
        );
    }
}

/// Default plugin metadata for [`TextEditPlugin`].
impl Default for TextEditPlugin {
    fn default() -> Self {
        Self
    }
}

// Suppress unused-import warnings on minimal builds (some helpers
// referenced only inside conditionally-compiled tests below).
#[allow(dead_code)]
fn _suppress_unused_warnings() {
    let _ = Instant::now;
    let _: &dyn Fn(&str) -> Vec<&str> = &|s| s.unicode_words().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use lumen_core::app::App;

    fn build_app() -> App {
        let mut app = App::new();
        app.world.init_resource::<Messages<TextEditRequest>>();
        app.world.init_resource::<Messages<TextEditApplied>>();
        app.world.init_resource::<Messages<TextEditRejected>>();
        app
    }

    fn spawn_editable(app: &mut App, text: &str) -> Entity {
        let buf = TextBuffer::multi_line(text);
        let cursor = {
            let p = TextPos::from_byte(text, text.len());
            TextCursor {
                head: p,
                anchor: p,
                affinity: Affinity::default(),
                goal_x: None,
            }
        };
        app.world
            .spawn((
                TextContent(text.to_string()),
                TextInput {
                    placeholder: String::new(),
                    cursor: text.len(),
                    selection_anchor: None,
                    multiline: true,
                },
                buf,
                cursor,
                UndoStack::default(),
                TextEditable,
            ))
            .id()
    }

    #[test]
    fn insert_appends_text_and_bumps_version() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "hello");
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Insert {
                entity: e,
                at: TextPos {
                    byte: 5,
                    grapheme: 5,
                },
                text: Arc::from(" world"),
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let buf = app.world.entity(e).get::<TextBuffer>().unwrap();
        assert_eq!(buf.to_string(), "hello world");
        assert_eq!(buf.version, 1);
    }

    #[test]
    fn delete_range_clamps_to_boundary() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "h\u{e9}llo"); // '\u{e9}' = 2 bytes
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Delete {
                entity: e,
                range: 1..2, // mid '\u{e9}' -> clamps to 1..3
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let buf = app.world.entity(e).get::<TextBuffer>().unwrap();
        assert_eq!(buf.to_string(), "hllo");
    }

    #[test]
    fn replace_with_selection_uses_replace_kind() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "hello");
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Replace {
                entity: e,
                range: 1..4,
                text: Arc::from("XYZ"),
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let buf = app.world.entity(e).get::<TextBuffer>().unwrap();
        assert_eq!(buf.to_string(), "hXYZo");
    }

    #[test]
    fn cursor_motion_word_right() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "hello world");
        // Reset cursor to start.
        {
            let mut em = app.world.entity_mut(e);
            let mut c = em.get_mut::<TextCursor>().unwrap();
            c.head = TextPos::ZERO;
            c.anchor = TextPos::ZERO;
        }
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::MoveCursor {
                entity: e,
                motion: CursorMotion::WordRight,
                mode: MoveMode::MoveAnchor,
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let c = app.world.entity(e).get::<TextCursor>().unwrap();
        assert_eq!(c.head.byte, 5);
    }

    #[test]
    fn ime_commit_with_replace_range_replaces() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "abcdef");
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::ImeCommit {
                entity: e,
                text: Arc::from("XX"),
                replace_range: Some(2..4),
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let buf = app.world.entity(e).get::<TextBuffer>().unwrap();
        assert_eq!(buf.to_string(), "abXXef");
    }

    #[test]
    fn single_line_strips_newlines_on_insert() {
        let mut app = build_app();
        let buf = TextBuffer::single_line("hi");
        let p = TextPos::from_byte("hi", 2);
        let e = app
            .world
            .spawn((
                TextContent(String::from("hi")),
                TextInput {
                    placeholder: String::new(),
                    cursor: 2,
                    selection_anchor: None,
                    multiline: false,
                },
                buf,
                TextCursor {
                    head: p,
                    anchor: p,
                    affinity: Affinity::default(),
                    goal_x: None,
                },
                UndoStack::default(),
            ))
            .id();
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Insert {
                entity: e,
                at: p,
                text: Arc::from("a\nb"),
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        let buf = app.world.entity(e).get::<TextBuffer>().unwrap();
        assert_eq!(buf.to_string(), "hia b");
    }

    #[test]
    fn extend_selection_keeps_anchor() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "hello world");
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        // Set the caret to byte 7, then extend to byte 2: the anchor
        // must stay at 7 (Select would have sorted it to the low end).
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::SetCursor {
                entity: e,
                pos: TextPos::from_byte("hello world", 7),
            });
        sched.run(&mut app.world);
        app.world.resource_mut::<Messages<TextEditRequest>>().write(
            TextEditRequest::ExtendSelection {
                entity: e,
                pos: TextPos::from_byte("hello world", 2),
            },
        );
        sched.run(&mut app.world);
        let c = app.world.entity(e).get::<TextCursor>().unwrap();
        assert_eq!(c.anchor.byte, 7);
        assert_eq!(c.head.byte, 2);
        assert_eq!(c.selection_range(), Some(2..7));
    }

    #[test]
    fn undo_then_redo_restores_state() {
        let mut app = build_app();
        let e = spawn_editable(&mut app, "hello");
        // Insert
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Insert {
                entity: e,
                at: TextPos {
                    byte: 5,
                    grapheme: 5,
                },
                text: Arc::from("!"),
            });
        let mut sched = bevy_ecs::schedule::Schedule::default();
        sched.add_systems(text_apply_edits);
        sched.run(&mut app.world);
        assert_eq!(
            app.world.entity(e).get::<TextBuffer>().unwrap().to_string(),
            "hello!"
        );
        // Undo
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Undo { entity: e });
        sched.run(&mut app.world);
        assert_eq!(
            app.world.entity(e).get::<TextBuffer>().unwrap().to_string(),
            "hello"
        );
        // Redo
        app.world
            .resource_mut::<Messages<TextEditRequest>>()
            .write(TextEditRequest::Redo { entity: e });
        sched.run(&mut app.world);
        assert_eq!(
            app.world.entity(e).get::<TextBuffer>().unwrap().to_string(),
            "hello!"
        );
    }

    // --- D5 / D6: goal-column + visual-line motion -------------------------

    use crate::{GlyphPosition, ShapedRun, ShapedSegment, TextGeometry};

    fn vglyph(bs: u32, be: u32, x: f32, y: f32) -> GlyphPosition {
        GlyphPosition {
            id: 0,
            x,
            y,
            advance: 10.0,
            byte_start: bs,
            byte_end: be,
        }
    }

    fn geom_of(glyphs: Vec<GlyphPosition>, width: f32) -> TextGeometry {
        let seg = ShapedSegment {
            font_id: 1,
            font_data: std::sync::Arc::new(Vec::new()),
            font_index: 0,
            normalized_coords: Vec::new(),
            level: 0,
            glyphs: glyphs.clone(),
            width,
        };
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs,
            segments: vec![seg],
            width,
        };
        TextGeometry::from(&run)
    }

    /// Three visual lines: long (0..5), medium (5..8), long (8..13).
    fn three_line_geom() -> TextGeometry {
        geom_of(
            vec![
                vglyph(0, 1, 0.0, 0.0),
                vglyph(1, 2, 10.0, 0.0),
                vglyph(2, 3, 20.0, 0.0),
                vglyph(3, 4, 30.0, 0.0),
                vglyph(4, 5, 40.0, 0.0),
                vglyph(5, 6, 0.0, 19.2),
                vglyph(6, 7, 10.0, 19.2),
                vglyph(7, 8, 20.0, 19.2),
                vglyph(8, 9, 0.0, 38.4),
                vglyph(9, 10, 10.0, 38.4),
                vglyph(10, 11, 20.0, 38.4),
                vglyph(11, 12, 30.0, 38.4),
                vglyph(12, 13, 40.0, 38.4),
            ],
            50.0,
        )
    }

    /// D5: the sticky goal-x is kept across consecutive vertical moves, so a
    /// long -> medium -> long descent lands back on the original column.
    #[test]
    fn goal_x_preserved_across_vertical_moves() {
        let g = three_line_geom();
        let buf = TextBuffer::multi_line("aaaaabbbccccc"); // 13 bytes
        // Head at byte 4 (x = 40, end of line 0), no goal yet.
        let cur = TextCursor {
            head: TextPos::from_byte("aaaaabbbccccc", 4),
            ..Default::default()
        };
        let (p1, goal1) = move_cursor_visual(&g, &buf, cur, CursorMotion::LineDown);
        // Line 1 is short (max x 30): the column clamps, but the goal keeps 40.
        assert_eq!(goal1, Some(40.0));
        // Descend again with the preserved goal: land back at x = 40 (byte 12).
        let cur2 = TextCursor {
            head: p1,
            goal_x: goal1,
            ..Default::default()
        };
        let (p2, goal2) = move_cursor_visual(&g, &buf, cur2, CursorMotion::LineDown);
        assert_eq!(goal2, Some(40.0));
        assert_eq!(p2.byte, 12);
        assert_eq!(g.caret_xy(p2.byte).0, 40.0);
    }

    /// D5: with the goal cleared (as a horizontal motion / edit does in the
    /// router) the next vertical move re-seeds from the caret's CURRENT x, so
    /// it lands on a different byte than a stale goal would.
    #[test]
    fn goal_x_reset_reseeds_from_current_column() {
        let g = three_line_geom();
        let buf = TextBuffer::multi_line("aaaaabbbccccc");
        let at2 = TextPos::from_byte("aaaaabbbccccc", 2); // x = 20 on line 0
        // Cleared goal -> re-seed from x = 20 -> lands on line 1 byte 7.
        let cleared = TextCursor {
            head: at2,
            goal_x: None,
            ..Default::default()
        };
        let (p_reset, _) = move_cursor_visual(&g, &buf, cleared, CursorMotion::LineDown);
        // Stale goal of 40 -> lands on line 1 byte 8 instead.
        let stale = TextCursor {
            head: at2,
            goal_x: Some(40.0),
            ..Default::default()
        };
        let (p_stale, _) = move_cursor_visual(&g, &buf, stale, CursorMotion::LineDown);
        assert_eq!(p_reset.byte, 7);
        assert_eq!(p_stale.byte, 8);
        assert_ne!(p_reset.byte, p_stale.byte);
    }

    /// Two visual lines from ONE logical line (soft wrap, no '\n').
    fn wrapped_geom() -> TextGeometry {
        geom_of(
            vec![
                vglyph(0, 1, 0.0, 0.0),
                vglyph(1, 2, 10.0, 0.0),
                vglyph(2, 3, 20.0, 0.0),
                vglyph(3, 4, 30.0, 0.0),
                vglyph(4, 5, 40.0, 0.0),
                vglyph(5, 6, 0.0, 19.2),
                vglyph(6, 7, 10.0, 19.2),
                vglyph(7, 8, 20.0, 19.2),
                vglyph(8, 9, 30.0, 19.2),
                vglyph(9, 10, 40.0, 19.2),
            ],
            50.0,
        )
    }

    /// D6: End/Home snap to the VISUAL line bounds (soft-wrap aware), not the
    /// logical `\n` line, and Up/Down cross the soft-wrap boundary.
    #[test]
    fn visual_line_bounds_and_cross_wrap() {
        let g = wrapped_geom();
        let buf = TextBuffer::multi_line("aaaaabbbbb"); // 10 bytes, one logical line
        let text = "aaaaabbbbb";
        // End from byte 2 (visual line 0) -> byte 5, the WRAP point, not the
        // logical end (10).
        let (end0, _) = move_cursor_visual(
            &g,
            &buf,
            TextCursor {
                head: TextPos::from_byte(text, 2),
                ..Default::default()
            },
            CursorMotion::LineEnd,
        );
        assert_eq!(end0.byte, 5);
        // Home from byte 7 (visual line 1) -> byte 5, the visual line start.
        let (home1, _) = move_cursor_visual(
            &g,
            &buf,
            TextCursor {
                head: TextPos::from_byte(text, 7),
                ..Default::default()
            },
            CursorMotion::LineStart,
        );
        assert_eq!(home1.byte, 5);
        // Up from byte 7 (line 1, x = 20) crosses onto line 0 at the same
        // column (byte 2).
        let (up, _) = move_cursor_visual(
            &g,
            &buf,
            TextCursor {
                head: TextPos::from_byte(text, 7),
                ..Default::default()
            },
            CursorMotion::LineUp,
        );
        assert_eq!(g.visual_line_of_byte(up.byte), 0);
        assert_eq!(up.byte, 2);
    }
}

#[cfg(test)]
mod grapheme_motion_tests {
    //! Arrow motion, selection extension, and forward Delete step one
    //! extended grapheme cluster (Qt `previousCursorPosition` /
    //! `nextCursorPosition`, Slint `GraphemeCursor`). Backspace keeps
    //! stepping one code point, which both references also do; the
    //! asymmetry is deliberate and pinned here.
    use super::*;
    use lumen_core::text_model::{TextBuffer, TextCursor, TextPos};

    /// "e" + U+0301 COMBINING ACUTE ACCENT: 1 + 2 bytes, one cluster.
    const COMBINING: &str = "e\u{0301}";
    /// U+1F1E6 U+1F1E7 regional indicators: 4 + 4 bytes, one flag cluster.
    const FLAG: &str = "\u{1F1E6}\u{1F1E7}";
    /// Family emoji: five scalars joined by ZWJ, one cluster.
    const ZWJ: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    /// Devanagari "ki": consonant + vowel sign, one cluster.
    const DEVANAGARI: &str = "\u{0915}\u{093F}";

    fn buf_of(s: &str) -> TextBuffer {
        TextBuffer::from(s)
    }

    fn right(text: &str, from: usize) -> usize {
        move_cursor(
            &buf_of(text),
            TextPos::from_byte(text, from),
            CursorMotion::CharRight,
        )
        .byte
    }

    fn left(text: &str, from: usize) -> usize {
        move_cursor(
            &buf_of(text),
            TextPos::from_byte(text, from),
            CursorMotion::CharLeft,
        )
        .byte
    }

    #[test]
    fn arrow_right_clears_a_whole_cluster() {
        for s in [COMBINING, FLAG, ZWJ, DEVANAGARI] {
            assert_eq!(right(s, 0), s.len(), "Right must cross all of {s:?}");
        }
    }

    #[test]
    fn arrow_left_clears_a_whole_cluster() {
        for s in [COMBINING, FLAG, ZWJ, DEVANAGARI] {
            assert_eq!(left(s, s.len()), 0, "Left must cross all of {s:?}");
        }
    }

    #[test]
    fn arrow_round_trip_returns_to_the_start_byte() {
        for s in [COMBINING, FLAG, ZWJ, DEVANAGARI] {
            let text = format!("a{s}b");
            let after_a = 1;
            let crossed = right(&text, after_a);
            assert_eq!(crossed, 1 + s.len());
            assert_eq!(left(&text, crossed), after_a);
        }
    }

    #[test]
    fn arrow_never_parks_inside_a_cluster() {
        let text = format!("{ZWJ}x");
        assert_eq!(right(&text, 0), ZWJ.len());
        assert_eq!(right(&text, ZWJ.len()), text.len());
    }

    #[test]
    fn forward_delete_removes_the_whole_cluster() {
        for s in [COMBINING, FLAG, ZWJ, DEVANAGARI] {
            let text = format!("{s}z");
            let mut buf = buf_of(&text);
            let mut cur = TextCursor::default();
            let mut undo = UndoStack::default();
            // Forward Delete resolves its range through CharRight.
            let end = move_cursor(&buf, cur.head, CursorMotion::CharRight).byte;
            delete_range(&mut buf, &mut cur, &mut undo, 0..end);
            assert_eq!(buf.to_string(), "z", "Delete left a remnant of {s:?}");
        }
    }

    #[test]
    fn backspace_still_peels_one_code_point() {
        // Deliberate asymmetry: Qt's `backspace()` and Slint's
        // `PreviousCharacter` both step one code point so a combining
        // mark can be removed from its base character.
        assert_eq!(prev_code_point_boundary(COMBINING, COMBINING.len()), 1);
        assert_eq!(prev_code_point_boundary(ZWJ, ZWJ.len()), ZWJ.len() - 4);
        assert_eq!(prev_code_point_boundary(FLAG, FLAG.len()), 4);
    }

    #[test]
    fn shift_arrow_extends_by_the_same_cluster_boundaries() {
        let text = format!("{FLAG}{COMBINING}");
        let buf = buf_of(&text);
        let mut cur = TextCursor::default();
        // Shift+Right twice: the head crosses the flag, then the
        // combining pair, while the anchor stays at 0.
        for expected in [FLAG.len(), text.len()] {
            let p = move_cursor(&buf, cur.head, CursorMotion::CharRight);
            cur.move_head(p, true);
            assert_eq!(cur.head.byte, expected);
            assert_eq!(cur.anchor.byte, 0);
        }
        assert_eq!(cur.selection_range(), Some(0..text.len()));
        // Shift+Left walks the same boundaries back.
        for expected in [FLAG.len(), 0] {
            let p = move_cursor(&buf, cur.head, CursorMotion::CharLeft);
            cur.move_head(p, true);
            assert_eq!(cur.head.byte, expected);
        }
        assert_eq!(cur.selection_range(), None);
    }

    /// Plain ASCII must still step exactly one byte at a time. Cluster
    /// segmentation is only supposed to change the answer where a cluster
    /// spans more than one scalar; if it ever skipped ahead on ordinary
    /// text the caret would jump over characters.
    #[test]
    fn ascii_steps_one_byte_at_a_time() {
        let text = "hello world";
        let mut at = 0usize;
        for expected in 1..=text.len() {
            at = right(text, at);
            assert_eq!(at, expected, "Right over ASCII skipped a character");
        }
        for expected in (0..text.len()).rev() {
            at = left(text, at);
            assert_eq!(at, expected, "Left over ASCII skipped a character");
        }
    }

    /// A newline is its own cluster, so crossing a line break is one step
    /// and never swallows the first character of the next line.
    #[test]
    fn a_newline_is_one_step() {
        let text = "ab\ncd";
        assert_eq!(right(text, 2), 3, "Right at the line end skipped the \\n");
        assert_eq!(right(text, 3), 4);
        assert_eq!(left(text, 3), 2);
    }

    #[test]
    fn motion_snaps_a_mid_cluster_start_byte() {
        // A byte parked inside the ZWJ sequence still lands on a real
        // cluster edge rather than splitting a scalar.
        let b = next_grapheme_boundary(ZWJ, 4);
        assert_eq!(b, ZWJ.len());
        assert_eq!(prev_grapheme_boundary(ZWJ, 4), 0);
    }
}
