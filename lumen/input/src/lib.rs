//! Hit-testing, hover state, and click dispatch.
//!
//! - Reads pointer messages from the main-world bus and [`PointerState`].
//! - Computes the entity under the cursor each tick.
//! - Inserts/removes [`Hovered`] on entities entering and leaving hover.
//! - Inserts [`Pressed`] on `PointerPressed` against the hovered entity; removes it and emits [`ClickEvent`] on `PointerReleased` against the same entity.
//! - Hit-tests via AABB against [`Transform`] for entities carrying [`Transform`] and an input-relevant component such as [`Visuals`] or [`TextContent`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use glam::Vec2;
use lumen_core::components::{EchoMode, TextBlockOrigin, text_baseline_in_line, text_block_top};
use lumen_core::input::{FocusTracker, FocusedKey, KeyPressed, PendingFileDrops};
use lumen_core::prelude::*;
use lumen_core::text_events::TextEditRequest;
use lumen_core::text_model::{TextBuffer, TextCursor, TextEditable, TextPos};
use lumen_text::{UndoStack, hit_test_text, select_line_at_byte, select_word_at_byte};
use std::sync::Arc;
use std::time::Instant;

/// Backwards-compatible re-export. Lives in `lumen-os-clipboard` now -
/// extracted per the W6.1 OS-integration refactor.
#[deprecated(
    since = "0.1.0-dev",
    note = "use `lumen_os_clipboard::ClipboardHost` instead"
)]
pub type ClipboardResource = lumen_os_clipboard::ClipboardHost;

/// Route [`ImeEvent`]s at the currently-focused entity:
///
/// * `Preedit { text, cursor }` - insert/update [`ImeState`] on the
///   focused entity. Empty `text` removes the component (preedit cleared).
/// * `Commit(text)` - append `text` to the focused entity's
///   [`TextContent`], clear [`ImeState`], emit [`TextInputCommitted`].
/// * `Enabled` / `Disabled` - no-op at the routing layer; consumed for
///   completeness so the message bus doesn't accumulate stale events.
///
/// If no entity has focus, all preedit/commit events are dropped (the IME
/// has nowhere to deposit text). Backends should also be gating events
/// via [`ImeRequest::allowed`], but this is a defensive check.
pub fn route_ime_events(
    mut commands: Commands,
    mut ime: MessageReader<ImeEvent>,
    tracker: Res<FocusTracker>,
    mut inputs: Query<(&mut TextContent, &mut TextInput)>,
    mut commits: MessageWriter<TextInputCommitted>,
) {
    let Some(entity) = tracker.0 else {
        ime.read().for_each(drop);
        return;
    };
    if inputs.get(entity).is_err() {
        ime.read().for_each(drop);
        return;
    }
    for ev in ime.read() {
        match ev {
            ImeEvent::Preedit { text, cursor } => {
                if text.is_empty() {
                    commands.entity(entity).remove::<ImeState>();
                } else {
                    let cur = cursor.map(|(_, end)| end).unwrap_or_else(|| text.len());
                    commands.entity(entity).insert(ImeState {
                        preedit: text.clone(),
                        cursor: cur,
                    });
                }
            }
            ImeEvent::Commit(text) => {
                if let Ok((mut tc, mut input)) = inputs.get_mut(entity) {
                    // Defensive: a hot-reload / signal-driven overwrite of
                    // TextContent can leave `cursor` past the buffer end OR
                    // in the middle of a multi-byte codepoint. `insert_str`
                    // panics on a non-char-boundary index, so clamp to len
                    // first and then snap downward to the nearest valid
                    // boundary before splicing.
                    let mut at = input.cursor.min(tc.0.len());
                    while at > 0 && !tc.0.is_char_boundary(at) {
                        at -= 1;
                    }
                    tc.0.insert_str(at, text);
                    input.cursor = at + text.len();
                }
                commands.entity(entity).remove::<ImeState>();
                commits.write(TextInputCommitted {
                    entity,
                    text: text.clone(),
                });
            }
            ImeEvent::Enabled | ImeEvent::Disabled => {}
        }
    }
}

/// True for the canonical named-key strings the window backend forwards
/// as `Key::Character` (W3C UI Events key names: "Shift", "Control",
/// "F1", "PageUp", ...). They are multi-char pure-ASCII-alphanumeric
/// words; genuine typed text is a single grapheme - one scalar, or a
/// multi-scalar cluster containing non-ASCII (composed accents, emoji).
fn is_named_key_string(s: &str) -> bool {
    s.chars().count() > 1 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Route [`FocusedKey`] events into the focused entity's text-editing
/// model. Runs after `dispatch_focused_keys` so the `FocusedKey` bus is
/// populated for this tick.
///
/// Gate: only fires on entities that carry both [`TextContent`] AND
/// [`TextInput`]. Plain labels / tiles with text are NOT editable -
/// `<input>` in markup is the only way to opt in.
///
/// ## Single source of truth (W2 text-editing core)
///
/// [`TextBuffer`] / [`TextCursor`] / `UndoStack` are the canonical
/// editing state. When `lumen_text::text_attach_buffer` has
/// attached them, every keystroke edits the buffer through
/// `lumen-text-edit`'s shared edit ops (which also record undo) and the
/// legacy `TextContent` / `TextInput{cursor, selection_anchor}` pair is
/// rewritten in lockstep before the system returns - renderer, bindings,
/// and a11y consumers keep reading the legacy shape with zero skew.
/// Without the components (embedders that never installed
/// `TextEditPlugin`) the same key handling runs against temporaries
/// seeded from the legacy pair, so behavior is identical minus
/// persistent undo history.
///
/// Shortcuts: Ctrl/Cmd+A/C/X/V (select-all / clipboard),
/// Ctrl/Cmd+Z / Ctrl/Cmd+Shift+Z / Ctrl/Cmd+Y (undo / redo), Ctrl+<-/->
/// word jumps, Ctrl+Backspace/Delete word deletes, Home/End line
/// bounds, Ctrl+Home/End document bounds, Arrow Up/Down line moves on
/// multiline inputs; Shift extends the selection on every motion.
///
/// IME composition is still preferred when active - this path is for
/// keyboards that emit direct character events (ASCII on most desktop
/// OSes when IME is idle, or systems with no IME at all).
#[allow(clippy::type_complexity)]
#[allow(deprecated)]
pub fn type_into_focused(
    tracker: Res<FocusTracker>,
    mods: Res<ModifiersState>,
    clipboard: Option<NonSendMut<ClipboardResource>>,
    mut keys: MessageReader<FocusedKey>,
    mut inputs: Query<(
        &mut TextContent,
        &mut TextInput,
        Option<&mut TextBuffer>,
        Option<&mut TextCursor>,
        Option<&mut UndoStack>,
        Option<&EchoMode>,
        Option<&lumen_text::ShapedText>,
    )>,
) {
    if keys.is_empty() {
        return;
    }
    let Some(entity) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    let Ok((mut tc, mut input, mut buf, mut cur, mut undo, echo, shaped)) = inputs.get_mut(entity)
    else {
        keys.read().for_each(drop);
        return;
    };
    // Qt `QLineEdit`: copy / cut are disabled for any non-`Normal`
    // echo mode so a password can't be lifted off the clipboard.
    let concealed = echo.is_some_and(|m| m.is_concealed());
    // Canonical state: the attached TextBuffer/TextCursor/UndoStack, or
    // temporaries seeded from the legacy pair when absent. Note the
    // borrow through `as_deref_mut` does NOT trip change detection -
    // only actual writes inside the key handler below do.
    let use_components = buf.is_some() && cur.is_some();
    let mut tmp_buf = TextBuffer::default();
    let mut tmp_cur = TextCursor::default();
    let mut tmp_undo = UndoStack::default();
    if !use_components {
        tmp_buf = seed_buffer(&tc, &input);
        tmp_cur = seed_cursor(&tc.0, &input);
    }
    let b: &mut TextBuffer = match buf.as_deref_mut() {
        Some(b) if use_components => b,
        _ => &mut tmp_buf,
    };
    let c: &mut TextCursor = match cur.as_deref_mut() {
        Some(c) if use_components => c,
        _ => &mut tmp_cur,
    };
    let u: &mut UndoStack = match undo.as_deref_mut() {
        Some(u) if use_components => u,
        _ => &mut tmp_undo,
    };
    if use_components {
        // Reconcile: an external write (signal binding this tick, legacy
        // IME commit, hot reload) may have replaced TextContent since the
        // buffer last mirrored. Re-seed like `text_reflect_content_to_buffer`
        // would, including the undo wipe (stale offsets must not replay).
        if b.to_string() != tc.0 {
            *b = seed_buffer(&tc, &input);
            *c = seed_cursor(&tc.0, &input);
            u.clear();
        } else {
            // Defensive clamp against a cursor left past the buffer end.
            let len = b.len_bytes();
            if c.head.byte > len || c.anchor.byte > len {
                let text = b.to_string();
                c.head = TextPos::from_byte(&text, c.head.byte.min(len));
                c.anchor = TextPos::from_byte(&text, c.anchor.byte.min(len));
            }
        }
    }

    let mut clipboard = clipboard;
    let multiline = input.multiline;
    // D5/D6: the shaped geometry (produced last tick) drives visual-line
    // Up/Down + Home/End and seeds the sticky goal-x. Absent (no shaper) the
    // router falls back to the byte-only `\n` motions.
    //
    // A concealed field's geometry describes the MASK run, whose bytes are
    // not buffer bytes, so the visual resolver is skipped there. Concealed
    // fields are single-line, where the byte motions are equivalent.
    let geom = if concealed {
        None
    } else {
        shaped.map(|s| &s.geometry)
    };
    for ev in keys.read() {
        if ev.entity != entity {
            continue;
        }
        apply_focused_key(
            ev,
            &mods.0,
            multiline,
            concealed,
            &mut clipboard,
            b,
            c,
            u,
            geom,
        );
    }

    // Lockstep mirror back into the legacy pair (single source of truth
    // lives in the buffer). Equality guards keep change detection quiet
    // on pure-passthrough keys.
    let s = b.to_string();
    if tc.0 != s {
        tc.0 = s;
    }
    if input.cursor != c.head.byte {
        input.cursor = c.head.byte;
    }
    let anchor = if c.is_empty() {
        None
    } else {
        Some(c.anchor.byte)
    };
    if input.selection_anchor != anchor {
        input.selection_anchor = anchor;
    }
}

/// Build a [`TextBuffer`] from the legacy `TextContent` + `TextInput`
/// pair (fallback path and reconcile path of [`type_into_focused`]).
fn seed_buffer(tc: &TextContent, input: &TextInput) -> TextBuffer {
    if input.multiline {
        TextBuffer::multi_line(&tc.0)
    } else {
        TextBuffer::single_line(&tc.0)
    }
}

/// Build a [`TextCursor`] from the legacy byte-offset fields, clamped
/// to the current text.
fn seed_cursor(text: &str, input: &TextInput) -> TextCursor {
    let head = TextPos::from_byte(text, input.cursor.min(text.len()));
    let anchor = match input.selection_anchor {
        Some(a) => TextPos::from_byte(text, a.min(text.len())),
        None => head,
    };
    TextCursor {
        head,
        anchor,
        ..TextCursor::default()
    }
}

/// Apply one focused-key event against the canonical buffer model, then
/// enforce the concealed-field undo policy.
///
/// Qt treats a concealed field's undo history as secret-bearing:
/// `QWidgetLineControl::undo()` under any non-`Normal` echo mode clears the
/// field instead of replaying. Lumen drops the history the moment the
/// buffer is empty or the value has been submitted, so Ctrl+Z can never
/// walk a password back after the field looks cleared.
#[allow(deprecated, clippy::too_many_arguments)]
fn apply_focused_key(
    ev: &FocusedKey,
    mods: &lumen_core::input::Modifiers,
    multiline: bool,
    concealed: bool,
    clipboard: &mut Option<NonSendMut<'_, ClipboardResource>>,
    b: &mut TextBuffer,
    c: &mut TextCursor,
    u: &mut UndoStack,
    geom: Option<&lumen_text::TextGeometry>,
) {
    use lumen_text::{delete_range, insert_text, move_cursor, move_cursor_visual, replace_range};
    let shift = mods.shift;
    // D5: only consecutive VERTICAL motions keep the sticky goal-x; every
    // other key path clears it. The vertical arms below set it explicitly;
    // this flag drives the post-match clear.
    let is_vertical = multiline
        && matches!(
            &ev.key,
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown)
        );
    // Cmd on macOS, Ctrl elsewhere - both produce the same edit
    // shortcuts. We can't tell them apart from winit's modifier
    // bitfield alone, so accept either.
    let cmd_or_ctrl = mods.ctrl || mods.super_;

    // Insert `text` at the caret, replacing any live selection. Strips
    // newlines for single-line buffers (Qt's QLineEdit convention),
    // matching the sanitize step of the request mutator.
    let type_text = |text: &str, b: &mut TextBuffer, c: &mut TextCursor, u: &mut UndoStack| {
        let sanitized;
        let text = if b.is_single_line() && text.contains('\n') {
            sanitized = text.replace('\n', " ");
            sanitized.as_str()
        } else {
            text
        };
        match c.selection_range() {
            Some(r) => replace_range(b, c, u, r, text),
            None => insert_text(b, c, u, c.head.byte, text),
        }
    };
    // Move the caret along `motion`; Shift keeps the anchor (extends).
    let motion_key = |motion: CursorMotion, b: &TextBuffer, c: &mut TextCursor| {
        let p = move_cursor(b, c.head, motion);
        c.move_head(p, shift);
    };

    // Shortcut chords come first; they short-circuit on letter keys
    // before the "type a character" path runs. None are vertical motions,
    // so they clear the goal-x (these arms early-return past the post-match
    // clear below).
    if cmd_or_ctrl && let Key::Character(s) = &ev.key {
        c.goal_x = None;
        let lc = s.to_ascii_lowercase();
        match lc.as_str() {
            "a" => {
                c.anchor = TextPos::ZERO;
                c.head = TextPos::from_buffer_byte(b, b.len_bytes());
                return;
            }
            "c" => {
                // Password / no-echo: copy is a no-op (Qt `QLineEdit`
                // disables it so the secret can't reach the clipboard).
                if !concealed && let Some(r) = c.selection_range() {
                    write_clipboard(clipboard.as_deref_mut(), &b.slice(r));
                }
                return;
            }
            "x" => {
                // Concealed inputs disable cut entirely - Qt's `cut()`
                // early-returns unless `echoMode() == Normal`, so the
                // selection is neither copied NOR deleted. Delete/Backspace
                // still remove the selection through their own paths.
                if !concealed && let Some(r) = c.selection_range() {
                    write_clipboard(clipboard.as_deref_mut(), &b.slice(r.clone()));
                    delete_range(b, c, u, r);
                }
                return;
            }
            "v" => {
                let paste = read_clipboard(clipboard.as_deref_mut());
                if !paste.is_empty() {
                    type_text(&paste, b, c, u);
                }
                return;
            }
            // Undo / redo: Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y. The stack
            // coalesces consecutive typed characters into word-ish
            // groups (see `UndoStack::record_insert`).
            "z" => {
                if shift {
                    u.redo(b, c);
                } else {
                    u.undo(b, c);
                }
                return;
            }
            "y" => {
                u.redo(b, c);
                return;
            }
            // Any other Ctrl/Cmd chord is a shortcut, not text - Ctrl+B
            // must not type "b" into the buffer.
            _ => return,
        }
    }

    match &ev.key {
        Key::Character(s) => {
            // The window backend forwards named keys without a typed
            // NamedKey variant (Shift, Control, F1, PageUp, ...) as
            // Key::Character(canonical_name) so scripts can bind
            // them. Canonical names are multi-char ASCII words and
            // must never insert as text; real typed graphemes are
            // either single-char or contain non-ASCII scalars.
            if is_named_key_string(s) {
                return;
            }
            type_text(s, b, c, u);
        }
        // winit maps Space to NamedKey::Space, not Character(" ").
        // Treat it as a literal space for text input.
        Key::Named(NamedKey::Space) => type_text(" ", b, c, u),
        // Multiline inputs treat bare Enter as a newline; Shift+Enter
        // still commits via `activate_focused_on_enter`. Single-line
        // inputs skip this arm and fall through to the commit handler.
        Key::Named(NamedKey::Enter) if multiline && !shift => {
            match c.selection_range() {
                Some(r) => replace_range(b, c, u, r, "\n"),
                None => insert_text(b, c, u, c.head.byte, "\n"),
            };
        }
        // Ctrl+Backspace / Ctrl+Delete: delete the previous / next word
        // instead of a single char. A live selection still takes
        // priority (same as the plain-char arms below).
        //
        // Plain Backspace peels ONE code point, not one grapheme cluster:
        // Qt's `backspace()` and Slint's `PreviousCharacter` both do this
        // so a combining mark can be removed from its base character.
        // Left / Right / Delete step whole clusters instead.
        Key::Named(NamedKey::Backspace) => {
            if let Some(r) = c.selection_range() {
                delete_range(b, c, u, r);
            } else if c.head.byte > 0 {
                let prev = if cmd_or_ctrl {
                    move_cursor(b, c.head, CursorMotion::WordLeft).byte
                } else {
                    lumen_text::prev_code_point_boundary(&b.to_string(), c.head.byte)
                };
                delete_range(b, c, u, prev..c.head.byte);
            }
        }
        Key::Named(NamedKey::Delete) => {
            if let Some(r) = c.selection_range() {
                delete_range(b, c, u, r);
            } else if c.head.byte < b.len_bytes() {
                let motion = if cmd_or_ctrl {
                    CursorMotion::WordRight
                } else {
                    CursorMotion::CharRight
                };
                let next = move_cursor(b, c.head, motion).byte;
                delete_range(b, c, u, c.head.byte..next);
            }
        }
        // Ctrl+<-/-> jump word boundaries (same Unicode segmentation the
        // double-click word select uses); Shift extends on every motion.
        Key::Named(NamedKey::ArrowLeft) => {
            let motion = if cmd_or_ctrl {
                CursorMotion::WordLeft
            } else {
                CursorMotion::CharLeft
            };
            motion_key(motion, b, c);
        }
        Key::Named(NamedKey::ArrowRight) => {
            let motion = if cmd_or_ctrl {
                CursorMotion::WordRight
            } else {
                CursorMotion::CharRight
            };
            motion_key(motion, b, c);
        }
        // D5/D6: Arrow up/down move by VISUAL line (soft-wrap aware) with a
        // sticky goal-x when shaped geometry is present; else fall back to
        // the byte-column `\n` motion. Single-line inputs ignore them,
        // matching QLineEdit.
        Key::Named(NamedKey::ArrowUp) if multiline => {
            visual_or_byte_motion(CursorMotion::LineUp, geom, shift, b, c);
        }
        Key::Named(NamedKey::ArrowDown) if multiline => {
            visual_or_byte_motion(CursorMotion::LineDown, geom, shift, b, c);
        }
        // Home/End go to line bounds (== document bounds on single-line
        // text); Ctrl+Home/End always go to document bounds. D6: on a
        // multiline input with geometry, the line bounds are VISUAL (soft-wrap
        // aware) rather than `\n`-delimited.
        Key::Named(NamedKey::Home) => {
            if cmd_or_ctrl {
                motion_key(CursorMotion::DocStart, b, c);
            } else if multiline && geom.is_some() {
                let (p, _) = move_cursor_visual(geom.unwrap(), b, *c, CursorMotion::LineStart);
                c.move_head(p, shift);
            } else {
                motion_key(CursorMotion::LineStart, b, c);
            }
        }
        Key::Named(NamedKey::End) => {
            if cmd_or_ctrl {
                motion_key(CursorMotion::DocEnd, b, c);
            } else if multiline && geom.is_some() {
                let (p, _) = move_cursor_visual(geom.unwrap(), b, *c, CursorMotion::LineEnd);
                c.move_head(p, shift);
            } else {
                motion_key(CursorMotion::LineEnd, b, c);
            }
        }
        // Enter is reserved as the "commit" signal for single-line
        // inputs (see `activate_focused_on_enter`).
        _ => {}
    }
    // D5: clear the goal-x for every non-vertical key path (the vertical
    // arms set it via `visual_or_byte_motion`). The chord block above
    // early-returns, so it clears its own goal-x.
    if !is_vertical {
        c.goal_x = None;
    }
}

/// D5/D6: resolve an Up/Down motion through the visual resolver when shaped
/// [`lumen_text::TextGeometry`] is present (seeding / keeping the sticky
/// goal-x), else fall back to the byte-column [`move_cursor`] motion. Shift
/// keeps the anchor (extends the selection).
fn visual_or_byte_motion(
    motion: CursorMotion,
    geom: Option<&lumen_text::TextGeometry>,
    shift: bool,
    b: &TextBuffer,
    c: &mut TextCursor,
) {
    match geom {
        Some(g) => {
            let (p, goal) = lumen_text::move_cursor_visual(g, b, *c, motion);
            c.move_head(p, shift);
            c.goal_x = goal;
        }
        None => {
            let p = lumen_text::move_cursor(b, c.head, motion);
            c.move_head(p, shift);
            // No geometry to seed a pixel goal-x; leave it cleared.
            c.goal_x = None;
        }
    }
}

// `ClipboardResource` (the previous in-crate struct) now lives in
// `lumen-os-clipboard` as `ClipboardHost`; the deprecated type alias at
// the top of this file preserves the old name for one minor version.
//
// The text editor still routes copy / cut / paste through the same
// `NonSend` resource; the helpers below adapt that API to the new
// `ClipboardHost::{read_text, write_text}` methods.

#[allow(deprecated)]
fn write_clipboard(res: Option<&mut ClipboardResource>, text: &str) {
    let Some(res) = res else { return };
    let _ = res.write_text(text);
}

#[allow(deprecated)]
fn read_clipboard(res: Option<&mut ClipboardResource>) -> String {
    let Some(res) = res else { return String::new() };
    res.read_text()
}

/// Drain [`PendingFileDrops`] populated by the window backend, hit-test
/// each `(path, position)` against every entity carrying both
/// [`Transform`] and [`DropTarget`], and emit a [`FileDropped`] message
/// at the deepest match (scroll-aware via [`ancestor_scroll`]). Drops
/// outside any DropTarget are discarded silently - apps that want a
/// fallback can read [`FileHovered`] / [`FileHoverCancelled`] directly.
pub fn dispatch_file_drops(
    mut drops: ResMut<PendingFileDrops>,
    candidates: Query<(Entity, &Transform), With<DropTarget>>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    mut out: MessageWriter<FileDropped>,
) {
    let queue: Vec<(std::path::PathBuf, Vec2)> = std::mem::take(&mut drops.drops);
    for (path, pos) in queue {
        let mut best: Option<(u32, Entity)> = None;
        for (e, t) in &candidates {
            let off = ancestor_scroll(e, &parents, &scrolls);
            let origin = t.absolute - off;
            if !(pos.x >= origin.x
                && pos.y >= origin.y
                && pos.x < origin.x + t.size.x
                && pos.y < origin.y + t.size.y)
            {
                continue;
            }
            let mut depth = 0u32;
            let mut cur = e;
            while let Ok(co) = parents.get(cur) {
                depth += 1;
                cur = co.parent();
            }
            let candidate = (depth, e);
            match best {
                None => best = Some(candidate),
                Some(b) if candidate > b => best = Some(candidate),
                _ => {}
            }
        }
        if let Some((_, entity)) = best {
            out.write(FileDropped {
                entity,
                path,
                position: pos,
            });
        }
    }
}

/// Standard keyboard activation (desktop-convention FSM):
///
/// * **Enter** on a non-[`TextInput`] focused entity synthesizes a
///   primary [`ClickEvent`] immediately on keydown (desktop convention:
///   Enter activates without a pressed phase). OS key auto-repeat is
///   ignored - holding Enter fires exactly one click.
/// * **Space** on a non-[`TextInput`] focused entity is
///   press-and-release: keydown inserts [`Pressed`] (the pressed visual
///   shows via the normal press-tint path), keyup removes it and emits
///   the [`ClickEvent`]. Auto-repeat keydowns are ignored. An Escape
///   between keydown and keyup cancels the press
///   ([`cancel_press_on_escape`] strips `Pressed`), so the keyup
///   activates nothing.
/// * For [`TextInput`] focused entities, Enter emits a
///   [`TextInputCommitted`] carrying the input's *full* current text -
///   the canonical "submit" signal scripts use to react to a finished
///   single-line edit.
/// * Focused **sliders** are exempt: a slider is a positional control,
///   and a synthetic click can't carry a meaningful pointer position -
///   the `position: Vec2::ZERO` placeholder made `set_slider_on_click`
///   compute `frac = 0` and reset the value to `min` on every
///   Space/Enter press. Keyboard interaction for sliders is
///   `lumen_primitives::controls::move_slider_on_keys` (arrows /
///   Home / End / PageUp / PageDown). Exempting here, at the source,
///   is cleaner than teaching every downstream click consumer to
///   recognize synthetic zero-position clicks (which would also
///   swallow a genuine click at the window origin).
/// * [`Disabled`](lumen_core::components::Disabled) focused entities
///   neither press nor click (belt-and-braces - focus is normally
///   ejected the moment an entity becomes disabled).
#[allow(clippy::too_many_arguments)]
pub fn activate_focused_on_enter(
    mut commands: Commands,
    tracker: Res<FocusTracker>,
    mods: Res<ModifiersState>,
    mut keys: MessageReader<FocusedKey>,
    mut releases: MessageReader<lumen_core::input::KeyReleased>,
    inputs: Query<(&TextContent, &TextInput)>,
    sliders: Query<(), With<lumen_core::components::SliderValue>>,
    pressed: Query<(), With<Pressed>>,
    disabled: Query<(), With<lumen_core::components::Disabled>>,
    mut space_held: Local<Option<Entity>>,
    mut clicks: MessageWriter<ClickEvent>,
    mut commits: MessageWriter<TextInputCommitted>,
) {
    // `Pressed` inserted by a keydown THIS run is deferred (Commands) and
    // invisible to the `pressed` query until the next sync point; track it
    // locally so a same-tick keydown+keyup (simulated input) still
    // activates.
    let mut pressed_this_run: Option<Entity> = None;
    if let Some(entity) = tracker.0 {
        let input_state = inputs
            .get(entity)
            .ok()
            .map(|(tc, ti)| (tc.0.clone(), ti.multiline));
        for ev in keys.read() {
            if ev.entity != entity {
                continue;
            }
            let is_enter = matches!(&ev.key, Key::Named(NamedKey::Enter));
            let is_space = matches!(&ev.key, Key::Named(NamedKey::Space));
            if let Some((text, multiline)) = &input_state {
                if is_enter {
                    // Multiline inputs commit only on Shift+Enter; bare Enter inserts a newline (handled in `type_into_focused`).
                    if !*multiline || mods.0.shift {
                        commits.write(TextInputCommitted {
                            entity,
                            text: text.clone(),
                        });
                    }
                }
            } else if !ev.repeat && !sliders.contains(entity) && !disabled.contains(entity) {
                if is_enter {
                    clicks.write(ClickEvent {
                        entity,
                        position: glam::Vec2::ZERO,
                        button: PointerButton::Primary,
                    });
                } else if is_space {
                    commands.entity(entity).insert(Pressed);
                    *space_held = Some(entity);
                    pressed_this_run = Some(entity);
                }
            }
        }
    } else {
        keys.read().for_each(drop);
    }

    for rel in releases.read() {
        if !matches!(&rel.key, Key::Named(NamedKey::Space)) {
            continue;
        }
        let Some(e) = space_held.take() else {
            continue;
        };
        // A press cancelled mid-hold (Escape stripped `Pressed`, or the
        // entity became disabled and was ejected) releases without a
        // click.
        let live = pressed.contains(e) || pressed_this_run == Some(e);
        commands.entity(e).remove::<Pressed>();
        if live && !disabled.contains(e) {
            clicks.write(ClickEvent {
                entity: e,
                position: glam::Vec2::ZERO,
                button: PointerButton::Primary,
            });
        }
    }
}

/// Logical width of the IME caret-area rect (Qt's thin `ImCursorRectangle`).
const IME_CARET_WIDTH_PX: f32 = 2.0;

/// Maintain [`ImeRequest`]: enable IME whenever a [`TextInput`] entity is
/// focused, and point its cursor area at the CARET (Qt `ImCursorRectangle`),
/// not the whole box (D8). Window backends poll the resource each frame and
/// forward the rect to the OS IME so the candidate window docks under the
/// caret.
///
/// The caret rect comes from the main-world [`lumen_text::ShapedText`]
/// geometry (`byte_to_caret`) placed into window coords with the same
/// baseline / padding / edit-scroll math the renderer uses. Falls back to the
/// whole-box rect when the geometry or cursor is missing (no shaper wired,
/// e.g. a headless test that skipped the producer).
///
/// A concealed [`EchoMode`] keeps IME off entirely. Qt sets
/// `Qt::ImhHiddenText` in `QLineEdit::setEchoMode` and
/// `QInputMethodPrivate::objectAcceptsInputMethod()` disables input methods
/// for it, so a password field gets no candidate window and no predictive
/// text.
#[allow(clippy::type_complexity)]
pub fn update_ime_request(
    mut req: ResMut<ImeRequest>,
    tracker: Res<FocusTracker>,
    inputs: Query<(
        &Transform,
        &TextInput,
        Option<&TextCursor>,
        Option<&lumen_text::ShapedText>,
        Option<&TextStyle>,
        Option<&lumen_core::components::Style>,
        Option<&lumen_core::components::TextInputScroll>,
        Option<&EchoMode>,
        Option<&TextBlockOrigin>,
        Option<&TextBuffer>,
    )>,
) {
    let Some((t, input, cursor, shaped, ts, style, escroll, echo, block, buf)) =
        tracker.0.and_then(|e| inputs.get(e).ok())
    else {
        req.allowed = false;
        req.cursor_area = None;
        return;
    };
    let echo = echo.copied().unwrap_or_default();
    req.allowed = !echo.is_concealed();
    req.cursor_area = Some(match (cursor, shaped) {
        (Some(cur), Some(shaped)) => {
            let size_px = ts.map(|s| s.size_px).unwrap_or(16.0);
            let (pad_l, pad_t, pad_b) = style
                .map(|s| (s.padding.left, s.padding.top, s.padding.bottom))
                .unwrap_or((0.0, 0.0, 0.0));
            let esc = escroll.map(|s| s.offset).unwrap_or(Vec2::ZERO);
            // Same vertical origin as `extract_text` and the pointer hit
            // test, so the IME rect and the drawn caret agree.
            let plain = buf.map(|b| b.to_string()).unwrap_or_default();
            let display = echo.display_string(&plain);
            let geom = &shaped.geometry;
            let block_top = block_top_of(
                block,
                Some(geom),
                &display,
                input.multiline,
                t.size.y,
                pad_t,
                pad_b,
                size_px,
            );
            let baseline_y = t.absolute.y + pad_t + block_top + text_baseline_in_line(size_px);
            // The geometry describes the DISPLAYED run, so the caret byte
            // crosses into display coordinates first.
            let caret = geom.byte_to_caret(echo.display_offset(&plain, cur.head.byte));
            let pos = Vec2::new(
                t.absolute.x + pad_l + caret.x - esc.x,
                baseline_y + caret.top - esc.y,
            );
            (pos, Vec2::new(IME_CARET_WIDTH_PX, caret.height))
        }
        // No geometry / cursor: keep the whole-box rect.
        _ => (t.absolute, t.size),
    });
}

/// Walk the ChildOf chain starting at `entity`, sum every ancestor's
/// [`ScrollOffset`] (excluding the entity's own offset). Used by both
/// hit-test and the scroll-aware extracts so visual + logical positions
/// agree.
pub fn ancestor_scroll(
    entity: Entity,
    parents: &Query<&ChildOf>,
    scrolls: &Query<&ScrollOffset>,
) -> Vec2 {
    let mut total = Vec2::ZERO;
    let mut cur = entity;
    while let Ok(child_of) = parents.get(cur) {
        let parent = child_of.parent();
        if let Ok(off) = scrolls.get(parent) {
            total += off.0;
        }
        cur = parent;
    }
    total
}

/// Plugin: registers hit-test + dispatch systems in the Systems stage.
///
/// Runs after CommandDrain (so any deferred component mutations from the
/// previous tick have been applied) and before LayoutSync (so Transform is
/// stable for the test - Transform changes propagate to ExtractedRect next
/// tick anyway).
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(self, app: &mut App) {
        #[allow(deprecated)]
        if let Some(cb) = ClipboardResource::try_new() {
            app.world.insert_non_send_resource(cb);
        }
        app.add_systems(TickStage::Systems, hit_test);
        app.add_systems(TickStage::Systems, dispatch_clicks.after(hit_test));
        // Escape press-cancel runs in the Input stage so its `Pressed`
        // removals are flushed before this tick's Systems stage (the
        // release in `dispatch_clicks` then finds nothing to click) and
        // so downstream Input-stage Escape consumers (dialog close) can
        // observe the consumed flag on the same keystroke.
        app.world
            .init_resource::<lumen_core::input::EscapePressCancel>();
        app.add_systems(TickStage::Input, cancel_press_on_escape);
        // Frameless-window drag: pressing a `<title-bar drag>` region
        // requests a native window drag via the backend. Requires the
        // `WindowDragRequest` resource - install it here so apps that
        // never use frameless mode still get a stable schedule.
        app.world
            .init_resource::<lumen_core::components::WindowDragRequest>();
        app.add_systems(
            TickStage::Systems,
            request_window_drag_on_titlebar_press.after(dispatch_clicks),
        );
        // Tiles / buttons stay keyboard-only (clicking them shouldn't
        // steal focus from a textbox the user just typed into). Inputs
        // are the exception - click-to-focus is the universal text-edit
        // gesture and worth opting in by default.
        app.add_systems(
            TickStage::Systems,
            focus_input_on_click.after(dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            cycle_focus_on_tab
                .in_set(lumen_core::text_events::TextEditSet::Producers)
                .after(dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            dispatch_focused_keys.after(cycle_focus_on_tab),
        );
        app.add_systems(
            TickStage::Systems,
            type_into_focused
                .in_set(lumen_core::text_events::TextEditSet::Producers)
                .after(dispatch_focused_keys),
        );
        // Standard a11y / keyboard pattern: Enter or Space on a focused,
        // non-TextInput entity synthesizes a primary ClickEvent so apps
        // don't have to handle the key directly.
        app.add_systems(
            TickStage::Systems,
            activate_focused_on_enter.after(dispatch_focused_keys),
        );
        // IME routing: enable IME when a TextContent-bearing entity is
        // focused, route preedit / commit at it, update ImeRequest cursor
        // area from the focused entity's transform.
        app.add_systems(
            TickStage::Systems,
            route_ime_events
                .in_set(lumen_core::text_events::TextEditSet::Producers)
                .after(cycle_focus_on_tab),
        );
        app.add_systems(
            TickStage::Systems,
            update_ime_request.after(route_ime_events),
        );
        // File drop routing: drain PendingFileDrops, hit-test against
        // DropTarget entities, emit FileDropped messages at the
        // top-most match. Runs after layout sync so transforms are
        // current.
        app.add_systems(TickStage::Systems, dispatch_file_drops);

        // W3.4: pointer -> caret / word / line selection. Registered
        // here (input layer) because it consumes pointer presses/moves.
        // Emits TextEditRequest at the W3 model -
        // lumen_text::TextEditPlugin is the consumer; the shared
        // `TextEditSet::Producers` label lets its mutator schedule
        // itself after these on the same tick.
        app.world.init_resource::<LastTextClick>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<TextEditRequest>>();
        app.add_systems(
            TickStage::Systems,
            text_pointer_to_caret
                .in_set(lumen_core::text_events::TextEditSet::Producers)
                .after(dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            text_pointer_drag_select
                .in_set(lumen_core::text_events::TextEditSet::Producers)
                .after(text_pointer_to_caret),
        );
    }
}

/// AABB hit-test based on `PointerState.position`. Inserts/removes [`Hovered`].
///
/// Scroll-aware: walks the [`ChildOf`] chain for each candidate, accumulating
/// ancestor [`ScrollOffset`] values, and shifts the candidate's logical AABB
/// by `-cumulative_offset` so it matches the visual position the renderer
/// draws at.
///
/// Candidate set: anything with a [`Transform`] **and** at least one of
/// [`Visuals`] (visual button-like surfaces) or [`Scroll`] (transparent
/// scroll containers - still need to receive wheel events).
///
/// Hidden entities never hit (spec section 17.4): an entity is skipped when it
/// (or any ancestor) carries `Visible(false)` (render-gate hide, the
/// keep-space variant) or `Style.display: None` (space-releasing hide).
/// Without this, a closed `<if mode="hide">` dialog kept stealing
/// hover/clicks over its full stale rect.
///
/// Clipped content never hits either (spec section 15/section 16: clipped = not
/// hittable): the pointer position must also fall inside the visual
/// rect of every `overflow: hidden` / `overflow: scroll` ancestor -
/// see [`point_clipped_by_ancestors`]. Without this, rows scrolled out
/// of a fixed-height `<scroll>` (painted nowhere) kept hit-shadowing
/// the widgets laid out below the clip box.
///
/// Disabled entities never hit (spec section 0): an entity is skipped when it
/// (or any ancestor) carries
/// [`Disabled`](lumen_core::components::Disabled). No `Hovered` means
/// no hover tint and no tooltip on a greyed-out widget; clicks were
/// already swallowed in [`dispatch_clicks`], this closes the hover
/// half.
///
/// Pointer capture (spec section 0 rules 3-4): while the primary button is
/// held and a press target is live (an entity carries [`Pressed`]),
/// the press target owns the pointer. `Hovered` may only sit on the
/// captured entity (pointer over it -> pressed visual) or on nothing
/// (dragged off -> un-pressed visual while capture is retained; see
/// `lumen_primitives::apply_press_tint`). It never migrates to a
/// neighbouring widget mid-press, so a drag-across paints no hover
/// tint on other buttons. Scrollbar / slider drags keep their own
/// capture FSMs: the bar path below resolves to the pressed scroll
/// container anyway, and slider drags run off `DragState`, not
/// `Hovered`.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn hit_test(
    mut commands: Commands,
    pointer: Res<PointerState>,
    candidates: Query<
        (Entity, &Transform),
        Or<(With<lumen_core::components::Visuals>, With<Scroll>)>,
    >,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    currently_hovered: Query<Entity, With<Hovered>>,
    visibles: Query<&lumen_core::components::Visible>,
    styles: Query<&lumen_core::components::Style>,
    disableds: Query<(), With<lumen_core::components::Disabled>>,
    pressed_entities: Query<Entity, With<Pressed>>,
    transforms: Query<&Transform>,
    scrollbar_ix: Option<Res<lumen_core::input::ScrollbarInteraction>>,
    overlay_roots: Query<Entity, With<lumen_core::render_world::OverlayLayer>>,
    overlay_order: Option<Res<lumen_core::render_world::OverlayOpenOrder>>,
    mut current: Local<Option<Entity>>,
) {
    // Top-layer hit ordering (mirrors the paint bands in
    // `lumen_core::render_world`): an entity inside an [`OverlayLayer`]
    // subtree (dialog / dropdown / menu panel / tooltip) hit-tests ABOVE
    // all normal content, exactly as it paints above it - otherwise a
    // deeply-nested widget BEHIND a shallow dialog wins the raw-`depth`
    // tiebreak and clicks fall through the modal (dialog inputs and
    // dropdowns went dead this way). Among overlays the later-opened one
    // wins, via the same `OverlayOpenOrder` stamps the extract uses, so a
    // dropdown panel opened inside a dialog sits above the dialog body.
    let overlay_set: std::collections::HashSet<Entity> = overlay_roots.iter().collect();
    // Overlay scrollbars sit ABOVE all content (spec section 16.2): while the
    // pointer is over a visible bar - or a thumb drag is in flight
    // (pointer capture; the pointer may be anywhere) - the hit resolves
    // to the scroll container itself. Content under the bar can't be
    // hovered/clicked through it, while wheel events still route through
    // the container's normal nested-scroll chain (bars never steal the
    // wheel). `update_scrollbars` (lumen-primitives) writes the resource
    // strictly before this system each tick.
    let bar_target = scrollbar_ix.as_ref().and_then(|ix| {
        ix.drag
            .map(|d| d.entity)
            .or_else(|| ix.hover.map(|(e, ..)| e))
    });
    let hit = match (bar_target, pointer.position) {
        (Some(target), _) => Some(target),
        (None, None) => None,
        (None, Some(p)) => {
            // Winner key: `(overlay_band, overlay_stamp, depth, entity)`,
            // higher wins - the painter's order restated for hit-testing.
            let mut best: Option<((u32, u64, u32, u64), Entity)> = None;
            for (e, t) in &candidates {
                let off = ancestor_scroll(e, &parents, &scrolls);
                let origin = t.absolute - off;
                if !(p.x >= origin.x
                    && p.y >= origin.y
                    && p.x < origin.x + t.size.x
                    && p.y < origin.y + t.size.y)
                {
                    continue;
                }
                if hidden_via_ancestors(e, &parents, &visibles, &styles) {
                    continue;
                }
                if disabled_via_ancestors(e, &parents, &disableds) {
                    continue;
                }
                if point_clipped_by_ancestors(p, e, &parents, &styles, &transforms, &scrolls) {
                    continue;
                }
                // Depth: count ChildOf hops up to a root. Along the way,
                // capture the NEAREST [`OverlayLayer`] ancestor (or self)
                // so overlay content banks above normal content.
                let mut depth = 0u32;
                let mut cur = e;
                let mut overlay_root = if overlay_set.contains(&e) {
                    Some(e)
                } else {
                    None
                };
                while let Ok(co) = parents.get(cur) {
                    depth += 1;
                    let parent = co.parent();
                    if overlay_root.is_none() && overlay_set.contains(&parent) {
                        overlay_root = Some(parent);
                    }
                    cur = parent;
                }
                let band = overlay_root.is_some() as u32;
                let stamp = overlay_root
                    .and_then(|r| {
                        overlay_order
                            .as_ref()
                            .and_then(|oo| oo.stamps.get(&r).copied())
                    })
                    .unwrap_or(0);
                let candidate = ((band, stamp, depth, e.to_bits()), e);
                match best {
                    None => best = Some(candidate),
                    Some(b) if candidate.0 > b.0 => best = Some(candidate),
                    _ => {}
                }
            }
            best.map(|(_, e)| e)
        }
    };

    // Pointer-capture gate: mid-press, the hover marker is confined to
    // the pressed entity (over it) or nothing (dragged off). The gate is
    // inactive on the press tick itself - `Pressed` lands later that
    // tick via `dispatch_clicks` - so the press still resolves against a
    // freshly-computed hover. Keyboard presses (Space FSM) leave
    // `primary_down` false and never confine the pointer.
    let hit = if pointer.primary_down && !pressed_entities.is_empty() {
        hit.filter(|h| pressed_entities.contains(*h))
    } else {
        hit
    };

    // Reconcile the `Hovered` marker against the actual component world, not
    // just the private `current` cache. A stationary pointer over an entity
    // that lost `Hovered` mid-hover - another system disabled/hid it, or reset
    // its state - would hit the `hit == *current` fast-path and never re-assert
    // the marker, leaving it un-hovered until the pointer left and returned.
    // Trusting `currently_hovered` (the live query) closes that desync while
    // staying idempotent: we only touch the ECS when it actually disagrees with
    // the desired hit, so a truly-still pointer emits no redundant change ticks.
    if let Some(prev) = *current {
        if hit != Some(prev) && currently_hovered.contains(prev) {
            commands.entity(prev).remove::<Hovered>();
        }
    }
    if let Some(next) = hit {
        if !currently_hovered.contains(next) {
            commands.entity(next).insert(Hovered);
        }
    }
    *current = hit;
}

/// Disabled-check for the pointer path (spec section 0: disabled widgets take
/// no pointer interaction at all). True when `entity` or any ancestor
/// carries [`Disabled`](lumen_core::components::Disabled) - a disabled
/// container disables its whole subtree, mirroring Qt's
/// `QWidget::setEnabled(false)` propagation.
pub fn disabled_via_ancestors(
    entity: Entity,
    parents: &Query<&ChildOf>,
    disableds: &Query<(), With<lumen_core::components::Disabled>>,
) -> bool {
    let mut cur = entity;
    loop {
        if disableds.contains(cur) {
            return true;
        }
        match parents.get(cur) {
            Ok(co) => cur = co.parent(),
            Err(_) => return false,
        }
    }
}

/// Escape cancels an in-flight press (spec section 0): every entity carrying
/// [`Pressed`] loses the marker immediately - the pressed visual
/// un-presses this tick, and the eventual pointer release emits no
/// [`ClickEvent`] ([`dispatch_clicks`]'s release path only clicks
/// entities still carrying `Pressed`). Applies to pointer presses and
/// keyboard (Space FSM) presses alike; drag state attached to the
/// pressed entity unwinds through the normal
/// `lumen_primitives::drag::release_drag_on_unpress` path.
///
/// Writes [`EscapePressCancel`](lumen_core::input::EscapePressCancel)
/// so downstream Escape consumers (`close_dialogs_on_escape` in
/// `lumenc`) treat the keystroke as consumed - cancelling a press never
/// also closes the dialog under it. Runs in `TickStage::Input`, before
/// those consumers.
pub fn cancel_press_on_escape(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    pressed: Query<Entity, With<Pressed>>,
    mut flag: ResMut<lumen_core::input::EscapePressCancel>,
) {
    let escape = keys
        .read()
        .any(|k| matches!(k.key, Key::Named(NamedKey::Escape)));
    let cancelled = escape && !pressed.is_empty();
    if cancelled {
        for e in &pressed {
            commands.entity(e).remove::<Pressed>();
        }
    }
    if flag.0 != cancelled {
        flag.0 = cancelled;
    }
}

/// Clip-aware pointer test (spec section 15/section 16: visually clipped content is
/// not hittable). Walks `entity`'s ancestor chain; an ancestor clips
/// exactly when the renderer emits an `ExtractedClipBox` for it - it
/// carries a `Scroll` component (detected here via its paired
/// [`ScrollOffset`]; clips both axes) or its [`Style`] sets
/// `overflow: hidden` on an axis. For every such ancestor the pointer
/// position `p` must fall inside the ancestor's visual rect (its
/// layout rect shifted by the scroll offsets of *its* ancestors - an
/// ancestor's own scroll offset moves its content, not its own box) on
/// the clipped axes. Returns `true` when `p` lands outside any clip
/// rect, i.e. the candidate is visually clipped away at `p` and must
/// not hit. Mirroring the renderer's rule keeps "hittable" identical
/// to "painted": rows escaping a fixed-height `<scroll>` used to
/// hit-shadow the button laid out below the clip box.
pub fn point_clipped_by_ancestors(
    p: Vec2,
    entity: Entity,
    parents: &Query<&ChildOf>,
    styles: &Query<&lumen_core::components::Style>,
    transforms: &Query<&Transform>,
    scrolls: &Query<&ScrollOffset>,
) -> bool {
    use lumen_core::components::Overflow;
    let mut cur = entity;
    while let Ok(co) = parents.get(cur) {
        let anc = co.parent();
        if let Ok(t) = transforms.get(anc) {
            let is_scroller = scrolls.contains(anc);
            let (style_clip_x, style_clip_y) = styles
                .get(anc)
                .map(|s| {
                    (
                        matches!(s.overflow_x, Overflow::Hidden),
                        matches!(s.overflow_y, Overflow::Hidden),
                    )
                })
                .unwrap_or((false, false));
            let clip_x = is_scroller || style_clip_x;
            let clip_y = is_scroller || style_clip_y;
            if clip_x || clip_y {
                let origin = t.absolute - ancestor_scroll(anc, parents, scrolls);
                if clip_x && !(p.x >= origin.x && p.x < origin.x + t.size.x) {
                    return true;
                }
                if clip_y && !(p.y >= origin.y && p.y < origin.y + t.size.y) {
                    return true;
                }
            }
        }
        cur = anc;
    }
    false
}

/// Press-to-focus: pressing a [`TextInput`] entity focuses it on the
/// press (Qt timing - the caret must land and start blinking before the
/// button is released, and a press-drag selection needs focus even when
/// the release happens outside the field); pressing anywhere else clears
/// input focus. Without the clear, the placeholder stays hidden after
/// users tap "outside" an input.
pub fn focus_input_on_click(
    mut commands: Commands,
    mut presses: MessageReader<PointerPressed>,
    hovered: Query<Entity, With<Hovered>>,
    mut tracker: ResMut<FocusTracker>,
    inputs: Query<(), With<TextInput>>,
) {
    for press in presses.read() {
        if !matches!(press.button, PointerButton::Primary) {
            continue;
        }
        let target = hovered.single().ok();
        match target.filter(|e| inputs.contains(*e)) {
            Some(e) => {
                if let Some(prev) = tracker.0
                    && prev != e
                {
                    commands
                        .entity(prev)
                        .remove::<(Focused, lumen_core::input::FocusVisible)>();
                }
                // Pointer-driven focus: `Focused` without `FocusVisible`
                // (CSS `:focus-visible` = keyboard-only focus).
                commands
                    .entity(e)
                    .insert(Focused)
                    .remove::<lumen_core::input::FocusVisible>();
                tracker.0 = Some(e);
            }
            None => {
                if let Some(prev) = tracker.0
                    && inputs.contains(prev)
                {
                    // Only clear focus when the *previously* focused
                    // entity was an input. Buttons / tiles can hold
                    // focus via Tab and shouldn't lose it on every
                    // stray press.
                    commands
                        .entity(prev)
                        .remove::<(Focused, lumen_core::input::FocusVisible)>();
                    tracker.0 = None;
                }
            }
        }
    }
}

/// On every [`ClickEvent`], move focus to the clicked entity - or, when
/// the click landed on a non-focusable CHILD (a button's text child, a
/// control's knob/dot), to the nearest [`TabIndex`]-bearing ancestor
/// (insert [`Focused`], remove from previous holder, update
/// [`FocusTracker`]).
///
/// W5 hit-shadowing fix: matching only `click.entity` meant clicking a
/// `<button>` never focused it once buttons grew hit-testable text
/// children - which silently broke every downstream focus consumer
/// (dialog focus save/restore recorded the wrong previous holder). Same
/// ancestor-walk contract as `lumen_primitives`' control dispatchers.
pub fn focus_on_click(
    mut commands: Commands,
    mut clicks: MessageReader<ClickEvent>,
    mut tracker: ResMut<FocusTracker>,
    focusables: Query<&TabIndex, Without<lumen_core::components::Disabled>>,
    parents: Query<&ChildOf>,
) {
    for click in clicks.read() {
        // Nearest focusable: the entity itself, else the first
        // TabIndex-bearing (enabled) ancestor.
        let mut cur = Some(click.entity);
        let target = loop {
            let Some(e) = cur else { break None };
            if focusables.contains(e) {
                break Some(e);
            }
            cur = parents.get(e).ok().map(|c| c.parent());
        };
        let Some(target) = target else {
            continue;
        };
        if let Some(prev) = tracker.0
            && prev != target
        {
            commands
                .entity(prev)
                .remove::<(Focused, lumen_core::input::FocusVisible)>();
        }
        // Pointer focus never carries the keyboard-only marker.
        commands
            .entity(target)
            .insert(Focused)
            .remove::<lumen_core::input::FocusVisible>();
        tracker.0 = Some(target);
    }
}

/// Tab / Shift-Tab cycles focus through entities with [`TabIndex`] >= 0,
/// sorted ascending by tab index then by
/// [`DocumentOrder`](lumen_core::components::DocumentOrder). Consumes the
/// Tab key from [`KeyPressed`] so [`dispatch_focused_keys`] doesn't also
/// forward it. [`Disabled`](lumen_core::components::Disabled) entities
/// are skipped.
///
/// The `DocumentOrder` tiebreak matters because `bevy_ecs` 0.18's
/// `Entity: Ord` compares a niche-optimized row index, not spawn order -
/// sorting ties by raw `Entity` silently reverses the on-screen order for
/// same-`TabIndex` siblings whenever a freed row gets recycled underneath
/// a later spawn. `lumenc::spawn` assigns `DocumentOrder` from a
/// monotonic counter as it walks the parsed tree, so it always matches
/// markup order; entities without it (hand-built ECS fixtures, mainly)
/// sort after everything that has one, then fall back to `Entity` so the
/// cycle is still fully deterministic.
///
/// When any [`lumen_core::components::FocusBoundary`] entity is currently
/// visible (`Visible(true)` or no `Visible` component), Tab cycling is
/// restricted to that boundary's descendants - this is how `<dialog>`
/// traps focus inside the modal. Multiple boundaries: any visible one
/// counts.
#[allow(clippy::too_many_arguments)]
pub fn cycle_focus_on_tab(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    mut tracker: ResMut<FocusTracker>,
    focusables: Query<
        (
            Entity,
            &TabIndex,
            Option<&lumen_core::components::DocumentOrder>,
        ),
        Without<lumen_core::components::Disabled>,
    >,
    parents: Query<&ChildOf>,
    boundaries: Query<
        (Entity, Option<&lumen_core::components::Visible>),
        bevy_ecs::prelude::With<lumen_core::components::FocusBoundary>,
    >,
    visibles: Query<&lumen_core::components::Visible>,
    styles: Query<&lumen_core::components::Style>,
    mut select_inputs: Query<(&TextContent, &mut TextInput, Option<&mut TextCursor>)>,
) {
    let trap_roots: Vec<Entity> = boundaries
        .iter()
        .filter(|(_, vis)| vis.map(|v| v.0).unwrap_or(true))
        .map(|(e, _)| e)
        .collect();
    let in_trap = |e: Entity| -> bool {
        if trap_roots.is_empty() {
            return true;
        }
        let mut cur = e;
        loop {
            if trap_roots.contains(&cur) {
                return true;
            }
            match parents.get(cur) {
                Ok(p) => cur = p.parent(),
                Err(_) => return false,
            }
        }
    };
    for ev in keys.read() {
        if !matches!(ev.key, Key::Named(NamedKey::Tab)) {
            continue;
        }
        let mut sorted: Vec<(Entity, i32, u32)> = focusables
            .iter()
            .filter(|(e, t, _)| {
                t.0 >= 0
                    && in_trap(*e)
                    // section 17.4: hidden widgets leave the tab chain - same
                    // hidden test the pointer path uses.
                    && !hidden_via_ancestors(*e, &parents, &visibles, &styles)
            })
            .map(|(e, t, doc)| (e, t.0, doc.map(|d| d.0).unwrap_or(u32::MAX)))
            .collect();
        if sorted.is_empty() {
            continue;
        }
        // (TabIndex, DocumentOrder, Entity): TabIndex is the author's
        // explicit ordering, DocumentOrder breaks ties in markup order,
        // and Entity is the final deterministic fallback for entities
        // that never got a DocumentOrder (missing values all sort as
        // `u32::MAX`, so they fall back to `Entity` amongst themselves -
        // same as before this fix).
        sorted.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
        let backward = ev.modifiers.shift;
        let next = match tracker.0 {
            None => {
                if backward {
                    sorted.last().copied()
                } else {
                    sorted.first().copied()
                }
            }
            Some(cur) => {
                let idx = sorted.iter().position(|(e, _, _)| *e == cur);
                match idx {
                    None => sorted.first().copied(),
                    Some(i) => {
                        let n = sorted.len();
                        let next_i = if backward {
                            (i + n - 1) % n
                        } else {
                            (i + 1) % n
                        };
                        Some(sorted[next_i])
                    }
                }
            }
        };
        if let Some((next_e, _, _)) = next {
            if let Some(prev) = tracker.0 {
                commands
                    .entity(prev)
                    .remove::<(Focused, lumen_core::input::FocusVisible)>();
            }
            // Keyboard-driven focus: mark for `:focus-visible` styling.
            commands
                .entity(next_e)
                .insert((Focused, lumen_core::input::FocusVisible));
            tracker.0 = Some(next_e);
            // Tab-focus lands on a text input => select all with the
            // caret at the end (Qt): the next keystroke replaces the
            // whole value, click-focus places the caret instead
            // (`text_pointer_to_caret`). Both the canonical TextCursor
            // and the legacy byte fields are written in lockstep.
            if let Ok((tc, mut ti, cur)) = select_inputs.get_mut(next_e) {
                let len = tc.0.len();
                let anchor = if len > 0 { Some(0) } else { None };
                if ti.cursor != len || ti.selection_anchor != anchor {
                    ti.cursor = len;
                    ti.selection_anchor = anchor;
                }
                if let Some(mut cur) = cur {
                    cur.anchor = TextPos::ZERO;
                    cur.head = TextPos::from_byte(&tc.0, len);
                }
            }
        }
    }
}

/// For every non-Tab [`KeyPressed`], emit a [`FocusedKey`] addressed to the
/// currently-focused entity. If no entity is focused, no-op.
pub fn dispatch_focused_keys(
    tracker: Res<FocusTracker>,
    mut keys: MessageReader<KeyPressed>,
    mut focused_keys: MessageWriter<FocusedKey>,
) {
    let Some(entity) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    for ev in keys.read() {
        if matches!(ev.key, Key::Named(NamedKey::Tab)) {
            continue;
        }
        focused_keys.write(FocusedKey {
            entity,
            key: ev.key.clone(),
            modifiers: ev.modifiers,
            repeat: ev.repeat,
        });
    }
}

/// Convert pointer presses/releases on the hovered entity into [`ClickEvent`].
/// [`Disabled`](lumen_core::components::Disabled) entities neither press
/// nor click.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_clicks(
    mut commands: Commands,
    pointer: Res<PointerState>,
    hovered: Query<Entity, With<Hovered>>,
    pressed: Query<Entity, With<Pressed>>,
    disabled: Query<(), With<lumen_core::components::Disabled>>,
    mut presses: MessageReader<PointerPressed>,
    mut releases: MessageReader<PointerReleased>,
    mut clicks: MessageWriter<ClickEvent>,
) {
    // Entities that received a primary press during THIS system run.
    //
    // `Commands` are deferred: the `Pressed` inserted below is not visible
    // to the `pressed` query until the next sync point, which is AFTER this
    // system returns. When a press and its release land on the same tick -
    // the MCP simulate path injects both from one `drain_simulate_queue`,
    // and fast synthetic / batched OS events can too - the release loop
    // would find an empty `pressed` query, swallow the click, and leak a
    // never-removed `Pressed` marker onto the entity. Tracking the
    // press targets locally lets a same-tick release resolve against them.
    let mut pressed_this_tick: Vec<Entity> = Vec::new();
    for press in presses.read() {
        if !matches!(press.button, PointerButton::Primary) {
            continue;
        }
        if let Ok(e) = hovered.single()
            && !disabled.contains(e)
        {
            commands.entity(e).insert(Pressed);
            if !pressed_this_tick.contains(&e) {
                pressed_this_tick.push(e);
            }
        }
    }

    for release in releases.read() {
        if !matches!(release.button, PointerButton::Primary) {
            continue;
        }
        // Click counts if the same entity that was pressed is still hovered.
        let still_hovered = hovered.single().ok();
        // Multi-tick path: entities carrying `Pressed` from a previous tick.
        for pressed_e in &pressed {
            commands.entity(pressed_e).remove::<Pressed>();
            if Some(pressed_e) == still_hovered && !disabled.contains(pressed_e) {
                clicks.write(ClickEvent {
                    entity: pressed_e,
                    position: release.position,
                    button: release.button,
                });
            }
        }
        // Same-tick path: entities pressed earlier in this run whose
        // `Pressed` insert has not been flushed yet, so the query above
        // can't see them. Skip any already handled there (an entity can
        // only be in both sets if a prior tick leaked `Pressed`, which
        // this same code now prevents).
        for &pressed_e in &pressed_this_tick {
            if pressed.contains(pressed_e) {
                continue;
            }
            commands.entity(pressed_e).remove::<Pressed>();
            if Some(pressed_e) == still_hovered && !disabled.contains(pressed_e) {
                clicks.write(ClickEvent {
                    entity: pressed_e,
                    position: release.position,
                    button: release.button,
                });
            }
        }
        let _ = pointer.position;
    }
}

/// Watch primary-mouse-button presses; when one lands on an entity
/// that is, or descends from, a [`TitleBarDraggable`] entity, set
/// [`WindowDragRequest`] to `true` so the window backend can call
/// `winit::Window::drag_window()` after this tick completes.
///
/// Cleared once the request is consumed by the backend; this system
/// only sets, never clears.
pub fn request_window_drag_on_titlebar_press(
    hovered: Query<Entity, With<Hovered>>,
    titlebars: Query<(), With<lumen_core::components::TitleBarDraggable>>,
    parents: Query<&ChildOf>,
    mut presses: MessageReader<PointerPressed>,
    mut request: ResMut<lumen_core::components::WindowDragRequest>,
) {
    for press in presses.read() {
        if !matches!(press.button, PointerButton::Primary) {
            continue;
        }
        let Ok(start) = hovered.single() else {
            continue;
        };
        let mut cur = Some(start);
        while let Some(e) = cur {
            if titlebars.get(e).is_ok() {
                request.0 = true;
                break;
            }
            cur = parents.get(e).ok().map(|p| p.parent());
        }
    }
}

// --- W3.4: pointer -> caret, double / triple click selection -----------------

/// Tracks the most recent click on a text input so we can detect
/// double / triple click sequences. Single-system multi-click counter,
/// independent of `lumen-primitives` so the text-edit path can ship
/// without taking that crate as a dep.
#[derive(Resource, Default, Debug)]
pub struct LastTextClick {
    /// Entity that received the most recent click.
    pub entity: Option<Entity>,
    /// When the most recent click landed.
    pub at: Option<Instant>,
    /// Position in window coords.
    pub position: Vec2,
    /// Consecutive-click count (1 = single, 2 = double, 3 = triple).
    pub count: u32,
}

/// Window in which consecutive clicks coalesce into double / triple.
const MULTI_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(450);
const MULTI_CLICK_RADIUS_PX: f32 = 4.0;

/// Estimated line height matching the cosmic-text shaper's metrics
/// (`Metrics::new(size, size * 1.2)`).
const LINE_HEIGHT_FACTOR: f32 = 1.2;

/// Map a pointer position to a byte offset inside the plaintext `text`
/// drawn at `origin` (box top-left) with `pad` insets and `block_top`
/// vertical origin, honoring the per-input caret-keep-visible scroll offset.
///
/// The hit test runs against the DISPLAYED run, which is the mask glyphs
/// under a concealed [`EchoMode`], and maps the result back to a plaintext
/// byte. Bullet advances differ from letter advances, so resolving against
/// the plaintext would land on the wrong character.
///
/// D4: when the entity's main-world [`lumen_text::TextGeometry`] is present
/// (the layout producer shaped it last tick), the byte comes from a real
/// glyph hit-test (`x_to_byte`) -- correct in proportional fonts and
/// soft-wrapped text. Absent (no shaper wired, e.g. a headless test that
/// skipped the producer), it falls back (O4) to the uniform per-grapheme
/// advance estimate (`avg_advance = size_px * 0.55`): multiline text resolves
/// the logical line from the pointer's y first, then hit-tests x inside it.
///
/// `block_top` is the shared vertical origin (`TextBlockOrigin`); both the
/// drawn baseline and this hit test measure from it, so a tall multiline
/// box resolves the same line the user clicked on.
#[allow(clippy::too_many_arguments)]
fn pointer_to_byte(
    geom: Option<&lumen_text::TextGeometry>,
    text: &str,
    echo: EchoMode,
    origin: Vec2,
    pad: Vec2,
    block_top: f32,
    edit_scroll: Vec2,
    pointer: Vec2,
    size_px: f32,
) -> usize {
    let display = echo.display_string(text);
    let content_x = pointer.x - (origin.x + pad.x) + edit_scroll.x;
    let content_y = pointer.y - (origin.y + pad.y + block_top) + edit_scroll.y;
    let display_byte = display_hit(geom, &display, content_x, content_y, size_px);
    echo.plain_offset(text, display_byte)
}

/// Byte offset into the displayed run for a run-local `(x, y)`.
fn display_hit(
    geom: Option<&lumen_text::TextGeometry>,
    display: &str,
    content_x: f32,
    content_y: f32,
    size_px: f32,
) -> usize {
    if let Some(g) = geom {
        return g.x_to_byte(content_x, content_y);
    }
    let avg_advance = size_px * 0.55;
    if !display.contains('\n') {
        return hit_test_text(display, 0.0, content_x, avg_advance);
    }
    let line_height = size_px * LINE_HEIGHT_FACTOR;
    let line_count = display.split('\n').count();
    let line_idx = if line_height > 0.0 {
        ((content_y / line_height).floor().max(0.0) as usize).min(line_count.saturating_sub(1))
    } else {
        0
    };
    let mut start = 0usize;
    for (i, line) in display.split('\n').enumerate() {
        if i == line_idx {
            return start + hit_test_text(line, 0.0, content_x, avg_advance);
        }
        start += line.len() + 1; // +1 for the '\n'
    }
    display.len()
}

/// Vertical origin of an entity's text block: the producer's published
/// [`TextBlockOrigin`], or the same rule evaluated locally when the
/// producer has not run.
#[allow(clippy::too_many_arguments)]
fn block_top_of(
    block: Option<&TextBlockOrigin>,
    geom: Option<&lumen_text::TextGeometry>,
    display: &str,
    multiline: bool,
    box_h: f32,
    pad_t: f32,
    pad_b: f32,
    size_px: f32,
) -> f32 {
    match block {
        Some(b) => b.top,
        None => {
            let wrapped = geom
                .map(|g| g.line_count() > 1)
                .unwrap_or_else(|| display.contains('\n'));
            let inner_h = (box_h - pad_t - pad_b).max(size_px);
            text_block_top(inner_h, size_px, multiline || wrapped)
        }
    }
}

/// W3.4 / W2 Qt-polish: pointer press on a `TextEditable` entity emits
/// one of:
/// - `SetCursor` for a single press - caret lands where the user
///   pressed (Qt places the caret on press, not release, so a
///   press-drag selection starts from the pressed position).
/// - `ExtendSelection` when Shift is held - the existing anchor stays
///   fixed and the head moves to the pressed position.
/// - `Select` (word range) for double press - uses
///   [`select_word_at_byte`].
/// - `Select` (line range) for triple press - uses
///   [`select_line_at_byte`].
///
/// Runs after `dispatch_clicks` (and alongside `focus_input_on_click`,
/// which focuses the input on the same press).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn text_pointer_to_caret(
    mut presses: MessageReader<PointerPressed>,
    hovered: Query<Entity, With<Hovered>>,
    mods: Res<ModifiersState>,
    mut requests: MessageWriter<TextEditRequest>,
    editables: Query<
        (
            &Transform,
            &TextBuffer,
            Option<&TextStyle>,
            Option<&lumen_core::components::Style>,
            Option<&lumen_core::components::TextInputScroll>,
            Option<&lumen_text::ShapedText>,
            Option<&EchoMode>,
            Option<&TextBlockOrigin>,
        ),
        With<TextEditable>,
    >,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    mut last: ResMut<LastTextClick>,
) {
    let now = Instant::now();
    for press in presses.read() {
        if !matches!(press.button, PointerButton::Primary) {
            continue;
        }
        let Ok(entity) = hovered.single() else {
            continue;
        };
        let Ok((t, buf, style, box_style, edit_scroll, shaped, echo, block)) =
            editables.get(entity)
        else {
            continue;
        };
        let off = ancestor_scroll(entity, &parents, &scrolls);
        let origin = t.absolute - off;
        let size_px = style.map(|s| s.size_px).unwrap_or(16.0);
        let (pad_b, pad) = box_style
            .map(|s| (s.padding.bottom, Vec2::new(s.padding.left, s.padding.top)))
            .unwrap_or((0.0, Vec2::ZERO));
        let escroll = edit_scroll.map(|s| s.offset).unwrap_or(Vec2::ZERO);
        let text = buf.to_string();
        let echo = echo.copied().unwrap_or_default();
        let geom = shaped.map(|s| &s.geometry);
        let block_top = block_top_of(
            block,
            geom,
            &echo.display_string(&text),
            !buf.is_single_line(),
            t.size.y,
            pad.y,
            pad_b,
            size_px,
        );
        let byte = pointer_to_byte(
            geom,
            &text,
            echo,
            origin,
            pad,
            block_top,
            escroll,
            press.position,
            size_px,
        );

        // Shift+press extends the current selection from its anchor and
        // never participates in double/triple coalescing.
        if mods.0.shift {
            last.entity = Some(entity);
            last.at = Some(now);
            last.position = press.position;
            last.count = 1;
            requests.write(TextEditRequest::ExtendSelection {
                entity,
                pos: TextPos::from_byte(&text, byte),
            });
            continue;
        }

        // Multi-click coalesce.
        let near_last = last
            .at
            .map(|t| now.saturating_duration_since(t) < MULTI_CLICK_WINDOW)
            .unwrap_or(false)
            && last.entity == Some(entity)
            && (last.position - press.position).length() <= MULTI_CLICK_RADIUS_PX;
        let new_count = if near_last {
            (last.count + 1).min(3)
        } else {
            1
        };
        last.entity = Some(entity);
        last.at = Some(now);
        last.position = press.position;
        last.count = new_count;

        let req = match new_count {
            1 => TextEditRequest::SetCursor {
                entity,
                pos: TextPos::from_byte(&text, byte),
            },
            2 => {
                let (s, e) = select_word_at_byte(&text, byte);
                TextEditRequest::Select {
                    entity,
                    range: s..e,
                }
            }
            _ => {
                let (s, e) = select_line_at_byte(&text, byte);
                TextEditRequest::Select {
                    entity,
                    range: s..e,
                }
            }
        };
        requests.write(req);
    }
}

/// W3.4: pointer-moved while pressed on a `TextEditable` extends the
/// selection. The drag emits `ExtendSelection` so the mutator keeps the
/// anchor set by the press (`SetCursor`) fixed, whichever side of it the
/// pointer ends up on.
#[allow(clippy::type_complexity)]
pub fn text_pointer_drag_select(
    mut moves: MessageReader<PointerMoved>,
    mut presses: MessageReader<PointerPressed>,
    pressed: Query<Entity, (With<Pressed>, With<TextEditable>)>,
    editables: Query<(
        &Transform,
        &TextBuffer,
        &TextCursor,
        Option<&TextStyle>,
        Option<&lumen_core::components::Style>,
        Option<&lumen_core::components::TextInputScroll>,
        Option<&lumen_text::ShapedText>,
        Option<&EchoMode>,
        Option<&TextBlockOrigin>,
    )>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    mut requests: MessageWriter<TextEditRequest>,
) {
    // A press this tick means `text_pointer_to_caret` just placed the
    // caret / selection from the same pointer position - treating the
    // accompanying PointerMoved as a drag would immediately clobber a
    // double/triple-click selection with a char-level extend.
    if presses.read().count() > 0 {
        moves.read().for_each(drop);
        return;
    }
    let Ok(entity) = pressed.single() else {
        moves.read().for_each(drop);
        return;
    };
    let Ok((t, buf, cur, style, box_style, edit_scroll, shaped, echo, block)) =
        editables.get(entity)
    else {
        moves.read().for_each(drop);
        return;
    };
    let off = ancestor_scroll(entity, &parents, &scrolls);
    let origin = t.absolute - off;
    let size_px = style.map(|s| s.size_px).unwrap_or(16.0);
    let (pad_b, pad) = box_style
        .map(|s| (s.padding.bottom, Vec2::new(s.padding.left, s.padding.top)))
        .unwrap_or((0.0, Vec2::ZERO));
    let escroll = edit_scroll.map(|s| s.offset).unwrap_or(Vec2::ZERO);
    let text = buf.to_string();
    let echo = echo.copied().unwrap_or_default();
    let geom = shaped.map(|s| &s.geometry);
    let block_top = block_top_of(
        block,
        geom,
        &echo.display_string(&text),
        !buf.is_single_line(),
        t.size.y,
        pad.y,
        pad_b,
        size_px,
    );
    let Some(last_pos) = moves.read().last().map(|m| m.position) else {
        return;
    };
    let head_byte = pointer_to_byte(
        geom, &text, echo, origin, pad, block_top, escroll, last_pos, size_px,
    );
    if head_byte == cur.head.byte {
        return;
    }
    requests.write(TextEditRequest::ExtendSelection {
        entity,
        pos: TextPos::from_byte(&text, head_byte),
    });
}

// Bring `Arc` into the use namespace for cross-system needs.
const _: fn() = || {
    let _ = Arc::<str>::from("");
};

#[cfg(test)]
mod hidden_hit_tests {
    //! D2: hidden entities - by `Visible(false)` on themselves or an
    //! ancestor, or by `Style.display: None` - must not receive pointer
    //! hits. A closed `<if mode="hide">` dialog used to keep stealing
    //! hover/clicks over its full stale rect.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::{Display, Style, Transform, Visible, Visuals};

    fn world_with_pointer(x: f32, y: f32) -> World {
        let mut world = World::new();
        world.insert_resource(PointerState {
            position: Some(Vec2::new(x, y)),
            primary_down: false,
        });
        world
    }

    fn full_rect() -> Transform {
        Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0))
    }

    #[test]
    fn hidden_dialog_does_not_steal_clicks() {
        let mut world = world_with_pointer(10.0, 10.0);
        // A button underneath (depth 0)...
        let button = world.spawn((full_rect(), Visuals::default())).id();
        // ...and a closed dialog overlay on top (deeper in the tree, so it
        // would win the depth tiebreak if it were still hittable).
        let overlay_root = world.spawn((full_rect(), Visuals::default())).id();
        let dialog = world
            .spawn((full_rect(), Visuals::default(), ChildOf(overlay_root)))
            .id();
        world.entity_mut(overlay_root).insert(Visible(false));

        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(button).is_some(),
            "the visible button must win once the hidden overlay is skipped"
        );
        assert!(
            world.get::<Hovered>(dialog).is_none(),
            "a child of a Visible(false) ancestor must not hit"
        );
    }

    /// Top-layer hit ordering: an entity inside an [`OverlayLayer`]
    /// subtree must win the hit over NORMAL content painted behind it,
    /// even when that normal content is nested DEEPER in the tree (and so
    /// would win the raw-`depth` tiebreak). This is the kanban regression:
    /// a board card button behind the modal was stealing every click on
    /// the dialog's inputs / dropdown headers, so the dialog went dead.
    #[test]
    fn overlay_content_wins_over_deeper_normal_content() {
        use lumen_core::render_world::OverlayLayer;
        let mut world = world_with_pointer(10.0, 10.0);
        // Normal content nested deep - would win on raw depth alone.
        let n0 = world.spawn((full_rect(), Visuals::default())).id();
        let n1 = world
            .spawn((full_rect(), Visuals::default(), ChildOf(n0)))
            .id();
        let n2 = world
            .spawn((full_rect(), Visuals::default(), ChildOf(n1)))
            .id();
        let deep = world
            .spawn((full_rect(), Visuals::default(), ChildOf(n2)))
            .id();
        // A SHALLOW dialog overlay painted on top, with one field child.
        let dialog = world
            .spawn((full_rect(), Visuals::default(), OverlayLayer))
            .id();
        let field = world
            .spawn((full_rect(), Visuals::default(), ChildOf(dialog)))
            .id();

        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(field).is_some(),
            "overlay content must hit above deeper normal content"
        );
        assert!(
            world.get::<Hovered>(deep).is_none(),
            "content behind the modal must not steal the hit"
        );
    }

    /// Among concurrently-open overlays, the later-opened one hit-tests on
    /// top - same `OverlayOpenOrder` stamps the extract paints by. Guards
    /// a dropdown panel opened inside a dialog resolving above the dialog
    /// body, and stacked popups generally.
    #[test]
    fn later_opened_overlay_wins_over_earlier() {
        use lumen_core::render_world::{OverlayLayer, OverlayOpenOrder};
        let mut world = world_with_pointer(10.0, 10.0);
        let early = world
            .spawn((full_rect(), Visuals::default(), OverlayLayer))
            .id();
        let late = world
            .spawn((full_rect(), Visuals::default(), OverlayLayer))
            .id();
        let mut oo = OverlayOpenOrder::default();
        oo.stamps.insert(early, 1);
        oo.stamps.insert(late, 2);
        world.insert_resource(oo);

        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(late).is_some(),
            "later-opened overlay wins the hit"
        );
        assert!(world.get::<Hovered>(early).is_none());
    }

    #[test]
    fn display_none_subtree_does_not_hit() {
        let mut world = world_with_pointer(10.0, 10.0);
        let button = world.spawn((full_rect(), Visuals::default())).id();
        let hidden_parent = world
            .spawn((
                full_rect(),
                Style {
                    display: Display::None,
                    ..Style::default()
                },
            ))
            .id();
        let child = world
            .spawn((full_rect(), Visuals::default(), ChildOf(hidden_parent)))
            .id();

        world.run_system_once(hit_test).unwrap();
        assert!(world.get::<Hovered>(button).is_some());
        assert!(
            world.get::<Hovered>(child).is_none(),
            "descendants of display:none must not hit"
        );
    }

    #[test]
    fn visible_true_still_hits() {
        let mut world = world_with_pointer(10.0, 10.0);
        let e = world
            .spawn((full_rect(), Visuals::default(), Visible(true)))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(world.get::<Hovered>(e).is_some());
    }
}

#[cfg(test)]
mod clip_hit_tests {
    //! Spec section 15/section 16: content visually clipped by an `overflow: hidden`
    //! / `overflow: scroll` ancestor is not hittable. Rows escaping a
    //! fixed-height scroll clip used to hit-shadow widgets laid out
    //! below the clip box.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::{Overflow, Style, Transform, Visuals};
    use lumen_core::input::{PointerState, ScrollOffset};

    fn world_with_pointer(x: f32, y: f32) -> World {
        let mut world = World::new();
        world.insert_resource(PointerState {
            position: Some(Vec2::new(x, y)),
            primary_down: false,
        });
        world
    }

    fn clip_style() -> Style {
        Style {
            overflow_y: Overflow::Scroll,
            ..Style::default()
        }
    }

    /// A 100-tall scroll clip whose row overflows to y=150; a button
    /// sits at y=150 below the clip. Pointer at the button.
    #[test]
    fn row_escaping_scroll_clip_does_not_shadow_button_below() {
        let mut world = world_with_pointer(10.0, 160.0);
        let scroll_box = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(200.0, 100.0)),
                clip_style(),
                Scroll::vertical(),
                ScrollOffset::default(),
            ))
            .id();
        // Row laid out past the clip bottom (deeper in the tree, so it
        // would win the depth tiebreak if clipping didn't exclude it).
        let row = world
            .spawn((
                Transform::new(Vec2::new(0.0, 150.0), Vec2::new(200.0, 30.0)),
                Visuals::default(),
                ChildOf(scroll_box),
            ))
            .id();
        // Button below the scroll area, overlapping the escaped row.
        let button = world
            .spawn((
                Transform::new(Vec2::new(0.0, 150.0), Vec2::new(200.0, 30.0)),
                Visuals::default(),
            ))
            .id();

        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(row).is_none(),
            "row clipped by its scroll ancestor must not hit"
        );
        assert!(
            world.get::<Hovered>(button).is_some(),
            "the button under the pointer wins instead"
        );
    }

    /// Content scrolled INTO view must still hit: the candidate's AABB
    /// is shifted by the ancestor scroll offset, and the clip rect test
    /// uses the ancestor's own (unshifted) box.
    #[test]
    fn row_scrolled_into_view_still_hits() {
        let mut world = world_with_pointer(10.0, 50.0);
        let scroll_box = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(200.0, 100.0)),
                clip_style(),
                Scroll::vertical(),
                ScrollOffset(Vec2::new(0.0, 100.0)),
            ))
            .id();
        // Laid out at y=150 but scrolled up by 100 -> visually at y=50.
        let row = world
            .spawn((
                Transform::new(Vec2::new(0.0, 150.0), Vec2::new(200.0, 30.0)),
                Visuals::default(),
                ChildOf(scroll_box),
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(row).is_some(),
            "row scrolled into the clip box must hit at its visual position"
        );
    }

    /// `overflow: hidden` (non-scroll) clips the same way.
    #[test]
    fn overflow_hidden_child_outside_parent_rect_not_hittable() {
        let mut world = world_with_pointer(150.0, 10.0);
        let clipper = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Style {
                    overflow_x: Overflow::Hidden,
                    ..Style::default()
                },
            ))
            .id();
        let child = world
            .spawn((
                Transform::new(Vec2::new(120.0, 0.0), Vec2::new(60.0, 30.0)),
                Visuals::default(),
                ChildOf(clipper),
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(child).is_none(),
            "child outside an overflow:hidden parent's x-range must not hit"
        );
    }

    /// `overflow: visible` ancestors do not clip hits.
    #[test]
    fn overflow_visible_ancestor_does_not_clip_hits() {
        let mut world = world_with_pointer(150.0, 10.0);
        let parent = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Style::default(),
            ))
            .id();
        let child = world
            .spawn((
                Transform::new(Vec2::new(120.0, 0.0), Vec2::new(60.0, 30.0)),
                Visuals::default(),
                ChildOf(parent),
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(child).is_some(),
            "no clipping ancestor -> overflowing child still hits"
        );
    }
}

#[cfg(test)]
mod typing_tests {
    //! Named keys forwarded as `Key::Character("Shift")`/`"F1"`/... and
    //! Ctrl-chorded letters must never insert text into a focused input.
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::{TextContent, TextInput};
    use lumen_core::input::{FocusTracker, Modifiers, ModifiersState};

    fn press(world: &mut World, key: Key, modifiers: Modifiers) {
        let entity = world.resource::<FocusTracker>().0.unwrap();
        world
            .resource_mut::<Messages<FocusedKey>>()
            .write(FocusedKey {
                entity,
                key,
                modifiers,
                repeat: false,
            });
        world.resource_mut::<ModifiersState>().0 = modifiers;
        world.run_system_once(type_into_focused).unwrap();
        // run_system_once builds a fresh MessageReader each call, which
        // would re-read this press on the next call - drain it.
        world.resource_mut::<Messages<FocusedKey>>().clear();
    }

    fn input_world(initial: &str) -> World {
        let mut world = World::new();
        world.init_resource::<Messages<FocusedKey>>();
        world.init_resource::<ModifiersState>();
        let e = world
            .spawn((
                TextContent(initial.to_string()),
                TextInput {
                    cursor: initial.len(),
                    ..Default::default()
                },
            ))
            .id();
        world.insert_resource(FocusTracker(Some(e)));
        world
    }

    fn text(world: &mut World) -> String {
        let e = world.resource::<FocusTracker>().0.unwrap();
        world.get::<TextContent>(e).unwrap().0.clone()
    }

    #[test]
    fn named_key_strings_do_not_type() {
        let mut world = input_world("ab");
        for name in ["Shift", "Control", "Alt", "Meta", "F1", "PageUp"] {
            press(
                &mut world,
                Key::Character(name.into()),
                Modifiers::default(),
            );
        }
        assert_eq!(text(&mut world), "ab", "named keys must not insert text");
    }

    #[test]
    fn ctrl_chord_does_not_type_letter() {
        let mut world = input_world("ab");
        press(
            &mut world,
            Key::Character("b".into()),
            Modifiers {
                ctrl: true,
                ..Default::default()
            },
        );
        assert_eq!(text(&mut world), "ab", "Ctrl+B is a shortcut, not text");
    }

    #[test]
    fn plain_and_composed_characters_still_type() {
        let mut world = input_world("");
        press(&mut world, Key::Character("x".into()), Modifiers::default());
        press(
            &mut world,
            Key::Character("\u{e9}".into()),
            Modifiers::default(),
        );
        press(
            &mut world,
            Key::Character("e\u{0301}".into()),
            Modifiers::default(),
        );
        assert_eq!(text(&mut world), "x\u{e9}e\u{0301}");
    }

    /// Build a focused input with the whole value selected (anchor 0,
    /// caret at end) and an optional [`EchoMode`].
    fn selected_input_world(initial: &str, echo: Option<EchoMode>) -> World {
        let mut world = World::new();
        world.init_resource::<Messages<FocusedKey>>();
        world.init_resource::<ModifiersState>();
        let mut e = world.spawn((
            TextContent(initial.to_string()),
            TextInput {
                cursor: initial.len(),
                selection_anchor: Some(0),
                ..Default::default()
            },
        ));
        if let Some(mode) = echo {
            e.insert(mode);
        }
        let e = e.id();
        world.insert_resource(FocusTracker(Some(e)));
        world
    }

    fn ctrl(key: &str) -> (Key, Modifiers) {
        (
            Key::Character(key.into()),
            Modifiers {
                ctrl: true,
                ..Default::default()
            },
        )
    }

    /// Qt `QLineEdit`: cut is disabled under a password echo mode - the
    /// selection is neither copied nor deleted (`cut()` early-returns
    /// unless `echoMode() == Normal`).
    #[test]
    fn password_cut_does_not_delete_selection() {
        let mut world = selected_input_world("secret", Some(EchoMode::Password));
        let (k, m) = ctrl("x");
        press(&mut world, k, m);
        assert_eq!(
            text(&mut world),
            "secret",
            "cut in password mode must not delete the selection"
        );
    }

    /// The same chord under the default (`Normal`) echo mode DOES cut -
    /// proves the block is echo-mode-gated, not a blanket disable.
    #[test]
    fn normal_cut_deletes_selection() {
        let mut world = selected_input_world("secret", None);
        let (k, m) = ctrl("x");
        press(&mut world, k, m);
        assert_eq!(
            text(&mut world),
            "",
            "cut in Normal mode removes the selection"
        );
    }

    /// No-echo mode blocks cut just like password mode.
    #[test]
    fn no_echo_cut_does_not_delete_selection() {
        let mut world = selected_input_world("secret", Some(EchoMode::NoEcho));
        let (k, m) = ctrl("x");
        press(&mut world, k, m);
        assert_eq!(text(&mut world), "secret");
    }
}

#[cfg(test)]
mod keyboard_activation_tests {
    //! `activate_focused_on_enter` - the keyboard activation FSM:
    //!
    //! * Enter clicks immediately on keydown; auto-repeat ignored.
    //! * Space is press-and-release: keydown inserts `Pressed` (visual),
    //!   keyup removes it and clicks; auto-repeat ignored; an Escape
    //!   cancel between the two suppresses the click.
    //! * Sliders are fully exempt: the synthetic click's
    //!   `position: Vec2::ZERO` placeholder would make the slider's
    //!   click-to-position handler reset the value to `min`. Keyboard
    //!   slider control lives in `move_slider_on_keys` instead.
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::schedule::Schedule;
    use lumen_core::components::SliderValue;
    use lumen_core::input::KeyReleased;

    /// World + persistent schedule so the system's `Local` FSM state and
    /// message cursors survive across ticks (`run_system_once` would
    /// rebuild both every call).
    struct Fixture {
        world: World,
        schedule: Schedule,
    }

    impl Fixture {
        fn new() -> Self {
            let mut world = World::new();
            world.init_resource::<Messages<FocusedKey>>();
            world.init_resource::<Messages<KeyReleased>>();
            world.init_resource::<Messages<ClickEvent>>();
            world.init_resource::<Messages<TextInputCommitted>>();
            world.init_resource::<ModifiersState>();
            let mut schedule = Schedule::default();
            schedule.add_systems(activate_focused_on_enter);
            Self { world, schedule }
        }

        fn keydown(&mut self, key: Key, repeat: bool) {
            let entity = self.world.resource::<FocusTracker>().0.unwrap();
            self.world
                .resource_mut::<Messages<FocusedKey>>()
                .write(FocusedKey {
                    entity,
                    key,
                    modifiers: Modifiers::default(),
                    repeat,
                });
        }

        fn keyup(&mut self, key: Key) {
            self.world
                .resource_mut::<Messages<KeyReleased>>()
                .write(KeyReleased {
                    key,
                    modifiers: Modifiers::default(),
                });
        }

        fn tick(&mut self) {
            self.schedule.run(&mut self.world);
        }

        fn clicks(&self) -> usize {
            self.world
                .resource::<Messages<ClickEvent>>()
                .iter_current_update_messages()
                .count()
        }
    }

    #[test]
    fn space_and_enter_on_focused_slider_emit_no_click() {
        let mut f = Fixture::new();
        let slider = f
            .world
            .spawn(SliderValue {
                value: 42.0,
                min: 0.0,
                max: 100.0,
                step: None,
            })
            .id();
        f.world.insert_resource(FocusTracker(Some(slider)));
        f.keydown(Key::Named(NamedKey::Space), false);
        f.keyup(Key::Named(NamedKey::Space));
        f.keydown(Key::Named(NamedKey::Enter), false);
        f.tick();
        assert_eq!(
            f.clicks(),
            0,
            "keyboard must not click-activate a slider (a zero-position \
             synthetic click would reset its value to min)"
        );
        assert!(
            f.world.get::<Pressed>(slider).is_none(),
            "Space must not press a slider either"
        );
        assert_eq!(
            f.world.get::<SliderValue>(slider).unwrap().value,
            42.0,
            "focused slider value unchanged by Space/Enter"
        );
    }

    #[test]
    fn enter_clicks_on_keydown_and_ignores_repeat() {
        let mut f = Fixture::new();
        let button = f.world.spawn(TabIndex(0)).id();
        f.world.insert_resource(FocusTracker(Some(button)));
        f.keydown(Key::Named(NamedKey::Enter), false);
        f.tick();
        assert_eq!(f.clicks(), 1, "Enter activates immediately on keydown");
        // Held Enter: the OS auto-repeat stream must not spam clicks.
        for _ in 0..5 {
            f.keydown(Key::Named(NamedKey::Enter), true);
        }
        f.tick();
        assert_eq!(f.clicks(), 1, "auto-repeat Enter fires no further clicks");
    }

    #[test]
    fn space_presses_on_keydown_and_activates_on_keyup() {
        let mut f = Fixture::new();
        let button = f.world.spawn(TabIndex(0)).id();
        f.world.insert_resource(FocusTracker(Some(button)));

        // Keydown: pressed visual, no click yet.
        f.keydown(Key::Named(NamedKey::Space), false);
        f.tick();
        assert!(
            f.world.get::<Pressed>(button).is_some(),
            "Space keydown shows the pressed visual"
        );
        assert_eq!(f.clicks(), 0, "no click until keyup");

        // Auto-repeat while held: ignored entirely.
        for _ in 0..5 {
            f.keydown(Key::Named(NamedKey::Space), true);
        }
        f.tick();
        assert_eq!(f.clicks(), 0, "auto-repeat Space fires no clicks");

        // Keyup: activates exactly once, pressed visual clears.
        f.keyup(Key::Named(NamedKey::Space));
        f.tick();
        assert_eq!(f.clicks(), 1, "Space keyup activates");
        assert!(
            f.world.get::<Pressed>(button).is_none(),
            "Pressed removed on keyup"
        );
    }

    #[test]
    fn same_tick_space_press_and_release_still_activates() {
        // Simulated input can land keydown + keyup on one tick; the
        // deferred `Pressed` insert must not swallow the activation.
        let mut f = Fixture::new();
        let button = f.world.spawn(TabIndex(0)).id();
        f.world.insert_resource(FocusTracker(Some(button)));
        f.keydown(Key::Named(NamedKey::Space), false);
        f.keyup(Key::Named(NamedKey::Space));
        f.tick();
        assert_eq!(f.clicks(), 1, "same-tick press+release clicks once");
        assert!(f.world.get::<Pressed>(button).is_none());
    }

    #[test]
    fn escape_between_space_keydown_and_keyup_cancels_activation() {
        let mut f = Fixture::new();
        let button = f.world.spawn(TabIndex(0)).id();
        f.world.insert_resource(FocusTracker(Some(button)));
        f.keydown(Key::Named(NamedKey::Space), false);
        f.tick();
        assert!(f.world.get::<Pressed>(button).is_some());

        // Escape mid-hold: `cancel_press_on_escape` strips `Pressed`.
        f.world.init_resource::<Messages<KeyPressed>>();
        f.world
            .init_resource::<lumen_core::input::EscapePressCancel>();
        f.world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Escape),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        use bevy_ecs::system::RunSystemOnce;
        f.world.run_system_once(cancel_press_on_escape).unwrap();
        assert!(
            f.world.get::<Pressed>(button).is_none(),
            "Escape un-presses immediately"
        );
        assert!(
            f.world.resource::<lumen_core::input::EscapePressCancel>().0,
            "the Escape is flagged as consumed by the press cancel"
        );

        // The eventual keyup must not click.
        f.keyup(Key::Named(NamedKey::Space));
        f.tick();
        assert_eq!(f.clicks(), 0, "cancelled press does not activate on keyup");
    }
}

#[cfg(test)]
mod press_capture_tests {
    //! Spec section 0 rules 3-4: pointer capture during a press. While the
    //! primary button is held with a live `Pressed` entity, `Hovered`
    //! must never migrate to another widget (no hover tint on neighbors
    //! during a drag-across); dragging off the pressed widget clears
    //! `Hovered` entirely (un-pressed visual, capture retained) and
    //! re-entering restores it.
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use glam::Vec2;
    use lumen_core::components::{Transform, Visuals};

    /// Persistent schedule so `hit_test`'s `Local` hover tracking
    /// survives across ticks (`run_system_once` would reset it and leak
    /// stale `Hovered` markers).
    fn hit_schedule() -> Schedule {
        let mut s = Schedule::default();
        s.add_systems(hit_test);
        s
    }

    fn two_buttons(world: &mut World) -> (Entity, Entity) {
        // Side by side: a at x 0..100, b at x 100..200.
        let a = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 50.0)),
                Visuals::default(),
            ))
            .id();
        let b = world
            .spawn((
                Transform::new(Vec2::new(100.0, 0.0), Vec2::new(100.0, 50.0)),
                Visuals::default(),
            ))
            .id();
        (a, b)
    }

    fn set_pointer(world: &mut World, x: f32, y: f32, down: bool) {
        world.insert_resource(PointerState {
            position: Some(Vec2::new(x, y)),
            primary_down: down,
        });
    }

    #[test]
    fn hover_does_not_migrate_to_neighbor_mid_press() {
        let mut world = World::new();
        let mut sched = hit_schedule();
        let (a, b) = two_buttons(&mut world);
        // Press on a: hover lands on a, then the press marks it.
        set_pointer(&mut world, 50.0, 25.0, false);
        sched.run(&mut world);
        assert!(world.get::<Hovered>(a).is_some());
        world.entity_mut(a).insert(Pressed);

        // Drag across onto b while held: b must NOT gain hover; a keeps
        // nothing either (pointer is off it) - capture shows un-pressed.
        set_pointer(&mut world, 150.0, 25.0, true);
        sched.run(&mut world);
        assert!(
            world.get::<Hovered>(b).is_none(),
            "neighbor must not hover-tint during a drag-across"
        );
        assert!(
            world.get::<Hovered>(a).is_none(),
            "dragged-off pressed widget is not hovered (un-pressed visual)"
        );
        assert!(
            world.get::<Pressed>(a).is_some(),
            "capture is retained while dragged off"
        );

        // Re-entering the pressed widget restores its hover (pressed
        // visual returns).
        set_pointer(&mut world, 50.0, 25.0, true);
        sched.run(&mut world);
        assert!(
            world.get::<Hovered>(a).is_some(),
            "re-entering the captured widget re-hovers it"
        );
    }

    #[test]
    fn hover_migrates_freely_when_button_up() {
        let mut world = World::new();
        let mut sched = hit_schedule();
        let (a, b) = two_buttons(&mut world);
        set_pointer(&mut world, 50.0, 25.0, false);
        sched.run(&mut world);
        assert!(world.get::<Hovered>(a).is_some());
        set_pointer(&mut world, 150.0, 25.0, false);
        sched.run(&mut world);
        assert!(world.get::<Hovered>(a).is_none());
        assert!(
            world.get::<Hovered>(b).is_some(),
            "no press -> normal hover"
        );
    }

    #[test]
    fn keyboard_press_does_not_confine_pointer_hover() {
        // A Space-FSM press (primary button up) must not gate hover.
        let mut world = World::new();
        let mut sched = hit_schedule();
        let (a, b) = two_buttons(&mut world);
        world.entity_mut(a).insert(Pressed);
        set_pointer(&mut world, 150.0, 25.0, false);
        sched.run(&mut world);
        assert!(
            world.get::<Hovered>(b).is_some(),
            "keyboard press leaves pointer hover unconfined"
        );
    }
}

#[cfg(test)]
mod disabled_hit_tests {
    //! Spec section 0: disabled widgets take no hover - no hover tint and no
    //! tooltip trigger (`record_hover_started` keys on `Added<Hovered>`).
    //! A disabled ancestor disables the whole subtree.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::{Disabled, Transform, Visuals};

    #[test]
    fn disabled_entity_never_hovers() {
        let mut world = World::new();
        world.insert_resource(PointerState {
            position: Some(Vec2::new(10.0, 10.0)),
            primary_down: false,
        });
        let e = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Visuals::default(),
                Disabled,
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(e).is_none(),
            "disabled widgets must not gain Hovered (no tint, no tooltip)"
        );
    }

    #[test]
    fn child_of_disabled_ancestor_never_hovers() {
        let mut world = World::new();
        world.insert_resource(PointerState {
            position: Some(Vec2::new(10.0, 10.0)),
            primary_down: false,
        });
        let parent = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Disabled,
            ))
            .id();
        let child = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Visuals::default(),
                ChildOf(parent),
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(
            world.get::<Hovered>(child).is_none(),
            "a disabled container disables its subtree"
        );
    }

    #[test]
    fn hover_moves_to_sibling_under_disabled_overlay() {
        let mut world = World::new();
        world.insert_resource(PointerState {
            position: Some(Vec2::new(10.0, 10.0)),
            primary_down: false,
        });
        let under = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Visuals::default(),
            ))
            .id();
        let root = world
            .spawn(Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)))
            .id();
        // Deeper (would win the depth tiebreak) but disabled.
        let over = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Visuals::default(),
                Disabled,
                ChildOf(root),
            ))
            .id();
        world.run_system_once(hit_test).unwrap();
        assert!(world.get::<Hovered>(over).is_none());
        assert!(
            world.get::<Hovered>(under).is_some(),
            "the enabled widget under the pointer hovers instead"
        );
    }
}

#[cfg(test)]
mod focus_visible_tests {
    //! CSS `:focus-visible` semantics: keyboard-driven focus (Tab /
    //! Shift-Tab) carries the `FocusVisible` marker; pointer-driven
    //! focus carries `Focused` alone. Skins key keyboard-only focus
    //! rings on the marker.
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::DocumentOrder;
    use lumen_core::input::FocusVisible;

    #[test]
    fn tab_focus_carries_focus_visible_click_focus_does_not() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<ClickEvent>>();
        world.insert_resource(FocusTracker(None));
        let a = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        let b = world.spawn((TabIndex(0), DocumentOrder(1))).id();

        // Tab -> first focusable gains Focused + FocusVisible.
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Tab),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        world.run_system_once(cycle_focus_on_tab).unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
        assert!(world.get::<Focused>(a).is_some(), "Tab focuses a");
        assert!(
            world.get::<FocusVisible>(a).is_some(),
            "keyboard focus must carry FocusVisible"
        );

        // Click b -> b gains Focused WITHOUT FocusVisible; a loses both.
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: b,
                position: Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world.run_system_once(focus_on_click).unwrap();
        assert!(world.get::<Focused>(b).is_some(), "click focuses b");
        assert!(
            world.get::<FocusVisible>(b).is_none(),
            "pointer focus must NOT carry FocusVisible"
        );
        assert!(world.get::<Focused>(a).is_none(), "a lost focus");
        assert!(world.get::<FocusVisible>(a).is_none(), "a lost the marker");
    }

    /// W5 hit-shadowing regression: a click that lands on a button's
    /// TEXT CHILD (no TabIndex) must still focus the button - the same
    /// ancestor-resolve contract the control dispatchers use. Without
    /// it, dialog focus save/restore recorded the wrong previous
    /// holder because opener buttons never actually took focus.
    #[test]
    fn click_on_text_child_focuses_button_ancestor() {
        let mut world = World::new();
        world.init_resource::<Messages<ClickEvent>>();
        world.insert_resource(FocusTracker(None));
        let button = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        let text_child = world.spawn(ChildOf(button)).id();
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: text_child,
                position: Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world.run_system_once(focus_on_click).unwrap();
        assert!(
            world.get::<Focused>(button).is_some(),
            "focus resolves to the TabIndex-bearing ancestor"
        );
        assert_eq!(world.resource::<FocusTracker>().0, Some(button));
        assert!(
            world.get::<Focused>(text_child).is_none(),
            "the child itself is not focused"
        );
    }

    /// A click on a child of a DISABLED focusable must not focus it.
    #[test]
    fn click_on_child_of_disabled_button_does_not_focus() {
        let mut world = World::new();
        world.init_resource::<Messages<ClickEvent>>();
        world.insert_resource(FocusTracker(None));
        let button = world
            .spawn((
                TabIndex(0),
                DocumentOrder(0),
                lumen_core::components::Disabled,
            ))
            .id();
        let text_child = world.spawn(ChildOf(button)).id();
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: text_child,
                position: Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world.run_system_once(focus_on_click).unwrap();
        assert_eq!(world.resource::<FocusTracker>().0, None);
        assert!(world.get::<Focused>(button).is_none());
    }
}

/// D4 / D8: exercise the shaped-geometry hit-test and IME caret rect with a
/// REAL `CosmicShaper`, proving pointer->byte is proportional-font correct
/// and that the IME cursor area is a thin caret rect, not the whole box.
#[cfg(test)]
mod shaped_geometry_tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::{Style, TextStyle, Transform};
    use lumen_core::input::{FocusTracker, ImeRequest};
    use lumen_core::text_model::{TextCursor, TextPos};
    use lumen_text::{ShapeOptions, build_shaped_text};
    use lumen_text_cosmic::CosmicShaper;

    /// D4: with real shaped geometry, `pointer_to_byte` hit-tests by true
    /// glyph widths (proportional), disagreeing with the uniform 0.55-advance
    /// estimate the cold-cache fallback uses.
    #[test]
    fn pointer_to_byte_uses_proportional_glyph_widths() {
        let mut shaper = CosmicShaper::new();
        let st = build_shaped_text(&mut shaper, "Willi", 16.0, ShapeOptions::default(), 0)
            .expect("shaped");
        let g = &st.geometry;
        // 'W' is much wider than 'i': the caret steps are non-uniform.
        let w_adv = g.caret_xy(1).0 - g.caret_xy(0).0;
        let i_adv = g.caret_xy(2).0 - g.caret_xy(1).0;
        assert!(
            w_adv > i_adv * 1.5,
            "expected 'W' wider than 'i' (w={w_adv}, i={i_adv})"
        );
        // The input hit-test routes through the geometry when present.
        let origin = Vec2::ZERO;
        let x_past_w = g.caret_xy(1).0 + 1.0;
        let byte = pointer_to_byte(
            Some(g),
            "Willi",
            EchoMode::Normal,
            origin,
            Vec2::ZERO,
            0.0,
            Vec2::ZERO,
            Vec2::new(x_past_w, 5.0),
            16.0,
        );
        assert_eq!(byte, 1, "click just past 'W' lands after 'W'");
        // Somewhere across the run the proportional hit-test disagrees with
        // the uniform-advance fallback (which assumes every glyph is 0.55em).
        let end_x = g.caret_xy(5).0;
        let mut diverged = false;
        for k in 0..=20 {
            let px = end_x * (k as f32) / 20.0;
            let geo = pointer_to_byte(
                Some(g),
                "Willi",
                EchoMode::Normal,
                origin,
                Vec2::ZERO,
                0.0,
                Vec2::ZERO,
                Vec2::new(px, 5.0),
                16.0,
            );
            let uni = pointer_to_byte(
                None,
                "Willi",
                EchoMode::Normal,
                origin,
                Vec2::ZERO,
                0.0,
                Vec2::ZERO,
                Vec2::new(px, 5.0),
                16.0,
            );
            if geo != uni {
                diverged = true;
            }
        }
        assert!(
            diverged,
            "geometry hit-test must differ from the uniform estimate somewhere"
        );
    }

    /// D8: `update_ime_request` reports a thin caret-height rect at the caret
    /// x, and it tracks when the caret moves right.
    #[test]
    fn ime_request_is_caret_rect_and_tracks() {
        let mut shaper = CosmicShaper::new();
        let st = build_shaped_text(&mut shaper, "hello", 16.0, ShapeOptions::default(), 0)
            .expect("shaped");

        let mut world = World::new();
        world.insert_resource(ImeRequest::default());
        let e = world
            .spawn((
                Transform::new(Vec2::new(10.0, 20.0), Vec2::new(200.0, 30.0)),
                TextInput {
                    placeholder: String::new(),
                    cursor: 1,
                    selection_anchor: None,
                    multiline: false,
                },
                TextStyle::default(),
                Style::default(),
                TextCursor {
                    head: TextPos::from_byte("hello", 1),
                    anchor: TextPos::from_byte("hello", 1),
                    ..Default::default()
                },
                st.clone(),
            ))
            .id();
        world.insert_resource(FocusTracker(Some(e)));

        world.run_system_once(update_ime_request).unwrap();
        let (pos1, size1) = world.resource::<ImeRequest>().cursor_area.expect("area");
        // Thin caret rect, NOT the 200x30 box.
        assert_eq!(size1.x, IME_CARET_WIDTH_PX);
        assert!(
            (size1.y - 16.0 * 1.05).abs() < 1e-3,
            "caret height ~= 1.05*size ({})",
            size1.y
        );
        assert!(size1.y < 30.0, "caret rect is shorter than the box");

        // Move the caret to byte 3 -> the rect x moves right.
        world.get_mut::<TextCursor>(e).unwrap().head = TextPos::from_byte("hello", 3);
        world.run_system_once(update_ime_request).unwrap();
        let (pos2, _) = world.resource::<ImeRequest>().cursor_area.expect("area");
        assert!(
            pos2.x > pos1.x,
            "IME caret x tracks the cursor ({} -> {})",
            pos1.x,
            pos2.x
        );
    }

    /// Spawn a focused single-line input carrying `text` and an optional
    /// echo mode, with shaped geometry for whatever it displays.
    fn ime_world(text: &str, echo: Option<EchoMode>) -> World {
        let mode = echo.unwrap_or_default();
        let mut shaper = CosmicShaper::new();
        let st = build_shaped_text(
            &mut shaper,
            &mode.display_string(text),
            16.0,
            ShapeOptions::default(),
            0,
        )
        .expect("shaped");
        let mut world = World::new();
        world.insert_resource(ImeRequest::default());
        let mut ent = world.spawn((
            Transform::new(Vec2::new(10.0, 20.0), Vec2::new(200.0, 30.0)),
            TextInput {
                cursor: text.len(),
                ..Default::default()
            },
            TextStyle::default(),
            Style::default(),
            TextBuffer::single_line(text),
            TextCursor {
                head: TextPos::from_byte(text, text.len()),
                anchor: TextPos::from_byte(text, text.len()),
                ..Default::default()
            },
            st,
        ));
        if let Some(mode) = echo {
            ent.insert(mode);
        }
        let e = ent.id();
        world.insert_resource(FocusTracker(Some(e)));
        world
    }

    /// Qt sets `Qt::ImhHiddenText` for a non-`Normal` echo mode and
    /// `objectAcceptsInputMethod()` then disables input methods outright,
    /// so a password field gets no candidate window or predictive text.
    #[test]
    fn ime_is_disabled_on_a_concealed_field() {
        for mode in [EchoMode::Password, EchoMode::NoEcho] {
            let mut world = ime_world("secret", Some(mode));
            world.run_system_once(update_ime_request).unwrap();
            assert!(
                !world.resource::<ImeRequest>().allowed,
                "IME must stay off under {mode:?}"
            );
        }
    }

    /// The same field under `Normal` still enables IME, so the gate is
    /// echo-mode-driven rather than a blanket disable.
    #[test]
    fn ime_stays_enabled_on_a_normal_field() {
        let mut world = ime_world("secret", None);
        world.run_system_once(update_ime_request).unwrap();
        assert!(world.resource::<ImeRequest>().allowed);
    }

    /// The hit test resolves against the DISPLAYED run. Mask glyphs are
    /// much narrower than 'W', so resolving the same x against the
    /// plaintext geometry lands on a different character.
    #[test]
    fn password_hit_test_resolves_against_the_mask_glyphs() {
        use lumen_core::components::PASSWORD_MASK_CHAR;
        let plain = "WWWWW";
        let mut shaper = CosmicShaper::new();
        let masked = EchoMode::Password.display_string(plain);
        let masked_geom = build_shaped_text(&mut shaper, &masked, 16.0, ShapeOptions::default(), 0)
            .expect("shaped")
            .geometry;
        let plain_geom = build_shaped_text(&mut shaper, plain, 16.0, ShapeOptions::default(), 1)
            .expect("shaped")
            .geometry;
        // x of the caret after two mask glyphs.
        let x = masked_geom.caret_xy(2 * PASSWORD_MASK_CHAR.len_utf8()).0;
        let byte = pointer_to_byte(
            Some(&masked_geom),
            plain,
            EchoMode::Password,
            Vec2::ZERO,
            Vec2::ZERO,
            0.0,
            Vec2::ZERO,
            Vec2::new(x, 5.0),
            16.0,
        );
        assert_eq!(byte, 2, "click past two mask glyphs lands after two chars");
        assert_ne!(
            plain_geom.x_to_byte(x, 5.0),
            2,
            "plaintext geometry would resolve the same x elsewhere"
        );
    }

    /// Nothing is drawn under `NoEcho`, so every click collapses the
    /// caret to the origin, as `QLineEdit` does with an empty display run.
    #[test]
    fn no_echo_hit_test_collapses_to_the_origin() {
        let plain = "secret";
        let mut shaper = CosmicShaper::new();
        let geom = build_shaped_text(
            &mut shaper,
            &EchoMode::NoEcho.display_string(plain),
            16.0,
            ShapeOptions::default(),
            0,
        )
        .expect("shaped")
        .geometry;
        let byte = pointer_to_byte(
            Some(&geom),
            plain,
            EchoMode::NoEcho,
            Vec2::ZERO,
            Vec2::ZERO,
            0.0,
            Vec2::ZERO,
            Vec2::new(200.0, 5.0),
            16.0,
        );
        assert_eq!(byte, 0);
    }
}

/// The vertical origin the drawn baseline and the pointer hit test share.
///
/// The older pointer tests run without the layout plugin, so no
/// `ShapedText` exists and only the fallback path is exercised. These
/// build the geometry the producer would publish for a tall multiline box
/// and assert both directions agree on which visual line a pointer y is in.
#[cfg(test)]
mod text_block_origin_tests {
    use super::*;
    use glam::Vec2;
    use lumen_text::{GlyphPosition, ShapedRun, ShapedSegment, TextGeometry};

    const SIZE_PX: f32 = 16.0;
    const LINE_H: f32 = SIZE_PX * 1.2;
    const ADVANCE: f32 = 10.0;
    /// Three logical lines, three bytes each plus the newline.
    const TEXT: &str = "abc\ndef\nghi";
    /// Tall pane, as a `grow="1"` textarea gets.
    const BOX_H: f32 = 400.0;
    const PAD: f32 = 8.0;

    /// Geometry as the producer would shape `TEXT`: one glyph per byte,
    /// uniform advance, line `i` on baseline `i * LINE_H`.
    fn three_line_geometry() -> TextGeometry {
        let mut glyphs = Vec::new();
        for (line, s) in TEXT.split('\n').enumerate() {
            let start = TEXT
                .split('\n')
                .take(line)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            for (i, _) in s.char_indices() {
                let b = (start + i) as u32;
                glyphs.push(GlyphPosition {
                    id: 0,
                    x: i as f32 * ADVANCE,
                    y: line as f32 * LINE_H,
                    advance: ADVANCE,
                    byte_start: b,
                    byte_end: b + 1,
                });
            }
        }
        let run = ShapedRun {
            font_data: std::sync::Arc::new(Vec::new()),
            font_index: 0,
            glyphs: glyphs.clone(),
            segments: vec![ShapedSegment {
                font_id: 1,
                font_data: std::sync::Arc::new(Vec::new()),
                font_index: 0,
                level: 0,
                glyphs,
                width: 3.0 * ADVANCE,
            }],
            width: 3.0 * ADVANCE,
        };
        TextGeometry::from(&run).with_size(SIZE_PX)
    }

    /// The origin the producer publishes for this box.
    fn block_top() -> f32 {
        let inner_h = (BOX_H - PAD - PAD).max(SIZE_PX);
        text_block_top(inner_h, SIZE_PX, three_line_geometry().line_count() > 1)
    }

    /// Byte the hit test resolves for a pointer at the vertical center of
    /// visual line `line`, one advance into it.
    fn byte_at_line(line: usize) -> usize {
        let y = PAD + block_top() + (line as f32 + 0.5) * LINE_H;
        pointer_to_byte(
            Some(&three_line_geometry()),
            TEXT,
            EchoMode::Normal,
            Vec2::ZERO,
            Vec2::splat(PAD),
            block_top(),
            Vec2::ZERO,
            Vec2::new(ADVANCE * 1.5, y),
            SIZE_PX,
        )
    }

    #[test]
    fn multiline_click_lands_on_the_clicked_line() {
        let geom = three_line_geometry();
        assert_eq!(geom.line_count(), 3);
        for line in 0..3 {
            let byte = byte_at_line(line);
            assert_eq!(
                geom.visual_line_of_byte(byte),
                line,
                "pointer on line {line} resolved to byte {byte}"
            );
        }
    }

    #[test]
    fn multiline_click_resolves_the_byte_under_the_pointer() {
        // Line 1 is "def" at bytes 4..7; x = 1.5 advances is inside 'e',
        // whose nearest edge is byte 5.
        assert_eq!(byte_at_line(1), 5);
    }

    /// The drawn baseline (what `extract_text` computes) and the hit test
    /// read the same origin, so the first baseline falls inside line 0's
    /// hit band. Before the fix `extract_text` centered the first baseline
    /// in a 400px pane while the hit test measured from the padding top,
    /// putting the drawn text many lines below where clicks resolved.
    #[test]
    fn drawn_baseline_sits_inside_the_hit_band_of_its_own_line() {
        let top = block_top();
        assert_eq!(top, 0.0, "a multiline block starts at the inner box top");
        let baseline = top + text_baseline_in_line(SIZE_PX);
        for line in 0..3 {
            let line_baseline = baseline + line as f32 * LINE_H;
            let band = ((line_baseline - top) / LINE_H).floor() as usize;
            assert_eq!(band, line, "baseline of line {line} is in band {band}");
        }
    }

    /// A single line still centers in its box, which is what `QLineEdit`
    /// does, and the hit test measures from the same centered origin.
    #[test]
    fn single_line_stays_centered_in_a_tall_box() {
        let inner_h = BOX_H - PAD - PAD;
        let top = text_block_top(inner_h, SIZE_PX, false);
        assert!(top > 0.0, "a lone line is pushed down to center");
        let baseline = top + text_baseline_in_line(SIZE_PX);
        // Same value the pre-existing extract formula produced.
        assert!((baseline - (inner_h + SIZE_PX * 0.72) / 2.0).abs() < 1e-3);
    }
}
