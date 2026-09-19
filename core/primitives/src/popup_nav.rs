//! Keyboard interaction for popup widgets: `<dropdown>` (combobox) and
//! `<menu>` panels. Matches the Qt contract:
//!
//! **Dropdown, closed + focused header**
//! ([`closed_dropdown_keys`]):
//! - `ArrowUp` / `ArrowDown` step the committed value directly through
//!   the enabled options (no wrap - `QComboBox` clamps at the ends).
//! - `Alt+ArrowDown` opens the popup. `Space` / `Enter` open it through
//!   the existing focused-activation -> click -> header-toggle path.
//! - Type-ahead: printable characters jump (and commit) the selection
//!   to the next option whose label starts with the typed prefix,
//!   wrapping, with a reset timer ([`PopupNavConfig`]).
//!
//! **Open popup** ([`popup_nav_lifecycle`] + [`popup_nav_keys`] +
//! [`follow_hover_highlight`]):
//! - Opening moves keyboard focus into the panel: onto the option row
//!   matching the committed value (dropdown - Qt highlights the current
//!   item) or onto the panel itself (menu - no initial highlight, like
//!   a fresh `QMenu`). The row highlight *is* the [`Focused`] marker,
//!   so `.dropdown-option:hover`-style CSS (routed through
//!   [`crate::hover::Interaction::hover_tint`], which treats `Focused`
//!   like `Hovered`) and `:focus` rules both style it - highlight
//!   styling stays CSS-reachable with zero new style plumbing.
//! - `ArrowUp` / `ArrowDown` move the highlight without committing
//!   (dropdown clamps like `QListView`; menus wrap like `QMenu`),
//!   skipping disabled rows and separators. `Home` / `End` jump.
//! - `Enter` / `Space` activate the highlighted row through the
//!   existing focused-activation path (`activate_focused_on_enter`
//!   synthesizes the click; the option / menu-item dispatcher commits
//!   and closes) - so commit semantics live in exactly one place.
//! - `Escape` closes without committing (the highlight never wrote the
//!   value, so the pre-open selection is intact - Qt's revert
//!   contract). `lumenc`'s `close_dialogs_on_escape` flips the open
//!   signal; the lifecycle system here restores focus to the trigger.
//! - `Alt+ArrowUp` also closes a dropdown popup (Qt).
//! - Hovering a row moves the highlight to it; a keyboard move strips
//!   the hover highlight - keyboard and hover highlight are one state,
//!   last input wins (Qt).
//! - Highlight movement scrolls the panel's internal scroller (skins
//!   cap `.dropdown-panel` height via `--lumen-dropdown-max-height` +
//!   `overflow-y: scroll`) so the highlighted row stays visible.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::{Disabled, DocumentOrder};
use lumen_core::input::{FocusVisible, Key, KeyPressed, NamedKey};
use lumen_core::prelude::*;
use lumen_core::time::{Duration, Instant};

use crate::popup::PopupPanel;
use crate::tabs::{DropdownButton, DropdownOptionButton, MenuItemButton};

/// Interaction timings for popup keyboard navigation. This `Default`
/// is the single Rust-side fallback (blank-no-css contract); embedders
/// can override the resource before the first tick.
#[derive(Resource, Clone, Copy, Debug)]
pub struct PopupNavConfig {
    /// Idle gap after which the type-ahead prefix resets. Qt's
    /// `QAbstractItemView` keyboard-search interval (~1 s).
    pub type_ahead_reset: Duration,
}

impl Default for PopupNavConfig {
    fn default() -> Self {
        Self {
            type_ahead_reset: Duration::from_secs(1),
        }
    }
}

/// The one live popup keyboard session (dropdown or menu panel). At
/// most one popup owns the keyboard at a time - nested menus are a
/// later wave; today's `<menu>` is a flat panel.
#[derive(Clone, Debug)]
pub struct PopupNavSession {
    /// Open-state signal the session is keyed on
    /// (`__dropdown_open:*` / `__menu_open:*`).
    pub open_signal: String,
    /// The [`PopupPanel`] entity.
    pub panel: Entity,
    /// Focus holder at open time, restored when a *menu* closes.
    /// Dropdowns restore to their header instead (Qt returns focus to
    /// the combobox).
    pub prev_focus: Option<Entity>,
    /// Whether the pre-open focus carried [`FocusVisible`] (keyboard
    /// focus) - the restored focus mirrors it.
    pub prev_focus_visible: bool,
    /// Initial scroll-into-view still pending: the panel mounts lazily
    /// on first open, so the highlighted row has no measured
    /// [`Transform`] until a layout pass has run. Cleared once applied.
    pub needs_scroll: bool,
}

/// Resource slot for the live [`PopupNavSession`].
#[derive(Resource, Clone, Debug, Default)]
pub struct ActivePopupNav(pub Option<PopupNavSession>);

/// Accumulated type-ahead prefix, shared by the open-popup and
/// closed-header paths (only one can be focused at a time).
#[derive(Resource, Clone, Debug, Default)]
pub struct TypeAheadBuffer {
    /// Lower-cased prefix typed so far.
    pub prefix: String,
    /// Instant of the last accepted character; `None` = empty buffer.
    pub last: Option<Instant>,
}

impl TypeAheadBuffer {
    /// Fold `ch` into the prefix, resetting first when the reset
    /// interval elapsed. Returns whether this press *extended* an
    /// existing prefix (multi-char search keeps the current item
    /// eligible, Qt behaviour).
    fn push(&mut self, ch: &str, now: Instant, reset: Duration) -> bool {
        let stale = self
            .last
            .map(|t| now.duration_since(t) > reset)
            .unwrap_or(true);
        if stale {
            self.prefix.clear();
        }
        let extended = !self.prefix.is_empty();
        self.prefix.push_str(&ch.to_lowercase());
        self.last = Some(now);
        extended
    }
}

/// One highlightable row of an open panel, in visual (markup) order.
struct NavRow {
    entity: Entity,
    label: String,
    disabled: bool,
}

/// `true` when `key` is a printable type-ahead character: a single
/// non-control char with no chording modifier. Multi-char
/// `Key::Character` strings ("PageUp"-style forwarded names) never
/// match.
fn type_ahead_char<'k>(key: &'k Key, modifiers: &lumen_core::input::Modifiers) -> Option<&'k str> {
    if modifiers.ctrl || modifiers.alt || modifiers.super_ {
        return None;
    }
    let Key::Character(s) = key else {
        return None;
    };
    let mut chars = s.chars();
    let first = chars.next()?;
    if chars.next().is_some() || first.is_control() {
        return None;
    }
    Some(s.as_str())
}

/// Collect the highlightable rows bound to `open_signal`, sorted by
/// [`DocumentOrder`] (markup order - the tab-order the spawner stamps)
/// then `Entity` as the deterministic fallback for hand-built worlds.
/// Separators are spacers, not row components, so they're naturally
/// absent; disabled rows are kept (a highlight must be able to *skip*
/// them knowingly).
#[allow(clippy::type_complexity)]
fn collect_rows(
    open_signal: &str,
    options: &Query<(
        Entity,
        &DropdownOptionButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
    items: &Query<(
        Entity,
        &MenuItemButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
) -> Vec<NavRow> {
    let mut rows: Vec<(u32, Entity, NavRow)> = Vec::new();
    for (e, opt, text, disabled, order) in options.iter() {
        if opt.open_signal == open_signal {
            rows.push((
                order.map(|d| d.0).unwrap_or(u32::MAX),
                e,
                NavRow {
                    entity: e,
                    label: text.map(|t| t.0.clone()).unwrap_or_default(),
                    disabled,
                },
            ));
        }
    }
    for (e, item, text, disabled, order) in items.iter() {
        if item.open_signal == open_signal {
            rows.push((
                order.map(|d| d.0).unwrap_or(u32::MAX),
                e,
                NavRow {
                    entity: e,
                    label: text.map(|t| t.0.clone()).unwrap_or_default(),
                    disabled,
                },
            ));
        }
    }
    rows.sort_by_key(|(order, e, _)| (*order, *e));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

/// Move the keyboard focus marker (and [`FocusTracker`]) from `from`
/// to `to`. `visible` controls the [`FocusVisible`] marker on the new
/// holder (keyboard moves mark it; hover-following doesn't).
fn move_focus(
    commands: &mut Commands,
    tracker: &mut FocusTracker,
    from: Option<Entity>,
    to: Entity,
    visible: bool,
) {
    if let Some(prev) = from
        && prev != to
    {
        commands.entity(prev).remove::<(Focused, FocusVisible)>();
    }
    if visible {
        commands.entity(to).insert((Focused, FocusVisible));
    } else {
        commands.entity(to).insert(Focused).remove::<FocusVisible>();
    }
    tracker.0 = Some(to);
}

/// Adjust the nearest scroll-bearing ancestor of `row` so the row's
/// rect sits inside the ancestor's viewport (same math as the
/// AT-driven `apply_a11y_scroll_into_view`). Returns `false` when the
/// row has no measured layout yet, so callers can retry next tick.
fn scroll_row_into_view(
    row: Entity,
    parents: &Query<&ChildOf>,
    transforms: &Query<&Transform>,
    scrolls: &mut Query<(&Transform, &mut Scroll, &mut ScrollOffset)>,
) -> bool {
    let Ok(row_tf) = transforms.get(row) else {
        return false;
    };
    if row_tf.size.y <= 0.0 {
        return false;
    }
    // Nearest Scroll ancestor.
    let mut cur = parents.get(row).ok().map(|c| c.parent());
    let mut container = None;
    while let Some(e) = cur {
        if scrolls.contains(e) {
            container = Some(e);
            break;
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    let Some(container) = container else {
        // No internal scroller (few options) - nothing to do.
        return true;
    };
    let Ok((c_tf, mut scroll, mut offset)) = scrolls.get_mut(container) else {
        return true;
    };
    // Row `absolute` is layout-space (pre-scroll); its visible band is
    // `[row.y - offset, row.y + h - offset]` relative to layout space,
    // so the target offset window keeping it fully inside the viewport
    // is `[row_bottom - viewport_bottom, row_top - viewport_top]`.
    let top = row_tf.absolute.y - c_tf.absolute.y;
    let bottom = top + row_tf.size.y;
    let lo = (bottom - c_tf.size.y).max(0.0);
    let hi = top.max(0.0);
    // `lo > hi` = the row is taller than the viewport (or the panel is
    // mid-layout with a degenerate height): align the row's top edge -
    // `f32::clamp` would panic on the inverted range.
    let new_y = if lo <= hi {
        offset.0.y.clamp(lo, hi)
    } else {
        hi
    };
    if (new_y - offset.0.y).abs() > f32::EPSILON {
        offset.0.y = new_y;
        scroll.velocity.y = 0.0;
    }
    true
}

/// `true` when `e` is `panel`, one of its rows, or any descendant.
fn in_panel_scope(e: Entity, panel: Entity, parents: &Query<&ChildOf>) -> bool {
    let mut cur = Some(e);
    while let Some(c) = cur {
        if c == panel {
            return true;
        }
        cur = parents.get(c).ok().map(|co| co.parent());
    }
    false
}

/// Open/close bookkeeping for popup keyboard sessions.
///
/// - A popup whose open signal flips `true` starts a session: focus
///   moves into the panel (selected row for dropdowns, the panel
///   itself for menus) and the highlight is scrolled into view once
///   layout has measured the freshly-mounted panel.
/// - A session whose signal flips `false` (Escape, commit click,
///   outside-press dismissal, script write) ends: focus returns to the
///   dropdown header / the menu's pre-open holder - but only when
///   focus still sits inside the panel, so a close caused by focusing
///   another widget doesn't yank focus back.
/// - Focus escaping the panel while open (Tab, click-to-focus on an
///   input) closes the popup - Qt popups dismiss on focus-out.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn popup_nav_lifecycle(
    mut commands: Commands,
    mut nav: ResMut<ActivePopupNav>,
    mut tracker: ResMut<FocusTracker>,
    mut store: ResMut<PropertyStore>,
    panels: Query<(Entity, &PopupPanel)>,
    headers: Query<(Entity, &DropdownButton)>,
    options: Query<(
        Entity,
        &DropdownOptionButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
    items: Query<(
        Entity,
        &MenuItemButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
    focus_visibles: Query<(), With<FocusVisible>>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut scrolls: Query<(&Transform, &mut Scroll, &mut ScrollOffset)>,
) {
    // 1. Tend the live session. Taken out of the slot so ending it is
    // a plain drop; a still-live session is put back at the end.
    if let Some(mut session) = nav.0.take() {
        let still_open = store.get_global_bool(&session.open_signal) == Some(true);
        let from = tracker.0;
        let focus_inside = from
            .map(|e| in_panel_scope(e, session.panel, &parents))
            .unwrap_or(false);
        if !still_open {
            // Closed by Escape / commit / dismissal: hand focus back.
            if focus_inside {
                let target = headers
                    .iter()
                    .find(|(_, h)| h.open_signal == session.open_signal)
                    .map(|(e, _)| e)
                    .or(session.prev_focus);
                if let Some(target) = target {
                    move_focus(
                        &mut commands,
                        &mut tracker,
                        from,
                        target,
                        session.prev_focus_visible,
                    );
                } else if let Some(prev) = from {
                    commands.entity(prev).remove::<(Focused, FocusVisible)>();
                    tracker.0 = None;
                }
            }
        } else if !focus_inside {
            // Focus left the panel while open (Tab / click-to-focus
            // elsewhere): dismiss without stealing focus back.
            store.set_global_bool(&session.open_signal, false);
        } else {
            if session.needs_scroll
                && let Some(row) = from.filter(|&e| e != session.panel)
                && scroll_row_into_view(row, &parents, &transforms, &mut scrolls)
            {
                session.needs_scroll = false;
            }
            nav.0 = Some(session);
        }
    }

    // 2. Start a session for a newly-open panel.
    if nav.0.is_some() {
        return;
    }
    let mut open_panels: Vec<(Entity, &PopupPanel)> = panels
        .iter()
        .filter(|(_, p)| store.get_global_bool(&p.open_signal) == Some(true))
        .collect();
    open_panels.sort_by_key(|(e, _)| *e);
    let Some((panel_e, panel)) = open_panels.into_iter().next() else {
        return;
    };
    let prev_focus = tracker.0;
    let prev_focus_visible = prev_focus
        .map(|e| focus_visibles.contains(e))
        .unwrap_or(false);
    // Dropdown: highlight the committed option (Qt shows the current
    // item highlighted + scrolled into view); fall back to the first
    // enabled row. Menu: focus the panel itself - no initial highlight.
    let header = headers
        .iter()
        .find(|(_, h)| h.open_signal == panel.open_signal);
    let rows = collect_rows(&panel.open_signal, &options, &items);
    let initial = if let Some((_, h)) = header {
        let committed = store.get_global_str(&h.value_signal);
        let committed = committed.as_deref();
        options
            .iter()
            .find(|(_, o, ..)| {
                o.open_signal == panel.open_signal && Some(o.value.as_str()) == committed
            })
            .map(|(e, ..)| e)
            .or_else(|| rows.iter().find(|r| !r.disabled).map(|r| r.entity))
            .unwrap_or(panel_e)
    } else {
        panel_e
    };
    move_focus(
        &mut commands,
        &mut tracker,
        prev_focus,
        initial,
        prev_focus_visible,
    );
    nav.0 = Some(PopupNavSession {
        open_signal: panel.open_signal.clone(),
        panel: panel_e,
        prev_focus,
        prev_focus_visible,
        needs_scroll: initial != panel_e,
    });
}

/// Keyboard handling while a popup session is live: highlight
/// movement, type-ahead, and `Alt+ArrowUp` close. Commit keys
/// (`Enter` / `Space`) are deliberately not handled here - they flow
/// through `lumen_input::activate_focused_on_enter` -> `ClickEvent` ->
/// the option / menu-item click dispatchers, so keyboard and pointer
/// commits share one code path.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn popup_nav_keys(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    tick: Res<Tick>,
    config: Res<PopupNavConfig>,
    mut type_ahead: ResMut<TypeAheadBuffer>,
    mut nav: ResMut<ActivePopupNav>,
    mut tracker: ResMut<FocusTracker>,
    mut store: ResMut<PropertyStore>,
    options: Query<(
        Entity,
        &DropdownOptionButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
    items: Query<(
        Entity,
        &MenuItemButton,
        Option<&TextContent>,
        Has<Disabled>,
        Option<&DocumentOrder>,
    )>,
    hovered_rows: Query<
        Entity,
        (
            With<Hovered>,
            Or<(With<DropdownOptionButton>, With<MenuItemButton>)>,
        ),
    >,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut scrolls: Query<(&Transform, &mut Scroll, &mut ScrollOffset)>,
) {
    let Some(session) = nav.0.as_mut() else {
        return;
    };
    if store.get_global_bool(&session.open_signal) != Some(true) {
        return;
    }
    // Menus wrap on arrows (QMenu); dropdown popups clamp (QListView).
    let is_menu = session.open_signal.starts_with("__menu_open:");
    let rows = collect_rows(&session.open_signal, &options, &items);
    if rows.is_empty() {
        return;
    }
    let current: Option<usize> = tracker
        .0
        .and_then(|f| rows.iter().position(|r| r.entity == f));
    let mut target: Option<usize> = None;

    // Next enabled index from `from` (exclusive) toward `dir`,
    // wrapping when `wrap`.
    let step = |from: Option<usize>, dir: i64, wrap: bool| -> Option<usize> {
        let n = rows.len() as i64;
        let start = match from {
            Some(i) => i as i64,
            // No highlight yet (fresh menu): Down enters at the top,
            // Up at the bottom.
            None => {
                if dir > 0 {
                    -1
                } else {
                    n
                }
            }
        };
        let mut i = start + dir;
        let mut hops = 0;
        while hops < n {
            if wrap {
                i = i.rem_euclid(n);
            } else if i < 0 || i >= n {
                return None;
            }
            if !rows[i as usize].disabled {
                return Some(i as usize);
            }
            i += dir;
            hops += 1;
        }
        None
    };

    for ev in keys.read() {
        match &ev.key {
            Key::Named(NamedKey::ArrowDown) if !ev.modifiers.alt => {
                if let Some(idx) = step(target.or(current), 1, is_menu) {
                    target = Some(idx);
                }
            }
            Key::Named(NamedKey::ArrowUp) if !ev.modifiers.alt => {
                if let Some(idx) = step(target.or(current), -1, is_menu) {
                    target = Some(idx);
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                // Alt+ArrowUp closes an open dropdown popup (Qt).
                if !is_menu {
                    store.set_global_bool(&session.open_signal, false);
                    return;
                }
            }
            Key::Named(NamedKey::Home) => {
                target = rows.iter().position(|r| !r.disabled);
            }
            Key::Named(NamedKey::End) => {
                target = rows.iter().rposition(|r| !r.disabled);
            }
            key => {
                let Some(ch) = type_ahead_char(key, &ev.modifiers) else {
                    continue;
                };
                let extended = type_ahead.push(ch, tick.now, config.type_ahead_reset);
                // Multi-char prefixes keep the current row eligible;
                // single chars search strictly after it (Qt).
                let anchor = target.or(current);
                let start = match (anchor, extended) {
                    (Some(i), true) => i,
                    (Some(i), false) => (i + 1) % rows.len(),
                    (None, _) => 0,
                };
                let n = rows.len();
                let hit = (0..n).map(|k| (start + k) % n).find(|&i| {
                    !rows[i].disabled
                        && rows[i].label.to_lowercase().starts_with(&type_ahead.prefix)
                });
                if let Some(idx) = hit {
                    target = Some(idx);
                }
            }
        }
    }

    if let Some(idx) = target
        && current != Some(idx)
    {
        // Last input wins: a keyboard move strips the pointer-hover
        // highlight so exactly one row presents highlighted (Qt).
        for hovered in &hovered_rows {
            if hovered != rows[idx].entity {
                commands.entity(hovered).remove::<Hovered>();
            }
        }
        let from = tracker.0;
        move_focus(&mut commands, &mut tracker, from, rows[idx].entity, true);
        if !scroll_row_into_view(rows[idx].entity, &parents, &transforms, &mut scrolls) {
            session.needs_scroll = true;
        }
    }
}

/// Pointer half of "keyboard highlight and hover highlight are the
/// same state": hovering a row of the open panel moves the focus
/// highlight onto it (no [`FocusVisible`] - pointer-driven), so the
/// next `Enter` activates what the pointer indicated and arrow keys
/// continue from it. Runs after `hit_test` so this tick's hover is
/// visible.
#[allow(clippy::type_complexity)]
pub fn follow_hover_highlight(
    mut commands: Commands,
    nav: Res<ActivePopupNav>,
    mut tracker: ResMut<FocusTracker>,
    newly_hovered: Query<
        (
            Entity,
            Option<&DropdownOptionButton>,
            Option<&MenuItemButton>,
        ),
        Added<Hovered>,
    >,
) {
    let Some(session) = nav.0.as_ref() else {
        return;
    };
    for (e, opt, item) in &newly_hovered {
        let signal = opt
            .map(|o| o.open_signal.as_str())
            .or(item.map(|i| i.open_signal.as_str()));
        if signal != Some(session.open_signal.as_str()) {
            continue;
        }
        let from = tracker.0;
        if from != Some(e) {
            move_focus(&mut commands, &mut tracker, from, e, false);
        }
    }
}

/// Closed-combobox keyboard interaction on the focused dropdown
/// header. Uses the option metadata mirrored onto [`DropdownButton`]
/// (the panel body may never have mounted). See the module docs for
/// the exact Qt contract.
pub fn closed_dropdown_keys(
    mut keys: MessageReader<KeyPressed>,
    tick: Res<Tick>,
    config: Res<PopupNavConfig>,
    mut type_ahead: ResMut<TypeAheadBuffer>,
    tracker: Res<FocusTracker>,
    headers: Query<&DropdownButton>,
    mut store: ResMut<PropertyStore>,
) {
    let Some(focused) = tracker.0 else {
        return;
    };
    let Ok(header) = headers.get(focused) else {
        return;
    };
    if store.get_global_bool(&header.open_signal) == Some(true) {
        return;
    }
    if header.options.is_empty() {
        return;
    }
    let enabled = |i: usize| !header.options[i].disabled;
    for ev in keys.read() {
        let current = {
            let committed = store.get_global_str(&header.value_signal);
            let committed = committed.as_deref();
            header
                .options
                .iter()
                .position(|o| Some(o.value.as_str()) == committed)
        };
        let target: Option<usize> = match &ev.key {
            Key::Named(NamedKey::ArrowDown) if ev.modifiers.alt => {
                // Alt+ArrowDown opens the popup (Qt).
                store.set_global_bool(&header.open_signal, true);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                // Step to the next enabled option, clamping at the end
                // (QComboBox does not wrap on closed-state stepping).
                match current {
                    Some(i) => ((i + 1)..header.options.len()).find(|&k| enabled(k)),
                    None => (0..header.options.len()).find(|&k| enabled(k)),
                }
            }
            Key::Named(NamedKey::ArrowUp) => match current {
                Some(i) => (0..i).rev().find(|&k| enabled(k)),
                None => (0..header.options.len()).find(|&k| enabled(k)),
            },
            key => {
                let Some(ch) = type_ahead_char(key, &ev.modifiers) else {
                    continue;
                };
                let extended = type_ahead.push(ch, tick.now, config.type_ahead_reset);
                let n = header.options.len();
                let start = match (current, extended) {
                    (Some(i), true) => i,
                    (Some(i), false) => (i + 1) % n,
                    (None, _) => 0,
                };
                (0..n).map(|k| (start + k) % n).find(|&i| {
                    enabled(i)
                        && header.options[i]
                            .label
                            .to_lowercase()
                            .starts_with(&type_ahead.prefix)
                })
            }
        };
        if let Some(idx) = target {
            // Closed-state movement commits directly (Qt: the closed
            // combobox has no separate highlight - arrows change the
            // value).
            store.set_global_str(&header.value_signal, header.options[idx].value.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::popup::PopupPanel;
    use crate::tabs::DropdownOptionSpec;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use bevy_ecs::world::World;
    use glam::Vec2;
    use lumen_core::input::Modifiers;

    const OPEN: &str = "__dropdown_open:fruit";
    const MENU_OPEN: &str = "__menu_open:actions";

    fn base_world() -> World {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.insert_resource(PropertyStore::default());
        world.insert_resource(FocusTracker::default());
        world.init_resource::<ActivePopupNav>();
        world.init_resource::<PopupNavConfig>();
        world.init_resource::<TypeAheadBuffer>();
        world.insert_resource(Tick::default());
        world
    }

    fn press(world: &mut World, key: Key) {
        press_mod(world, key, Modifiers::default());
    }

    fn press_mod(world: &mut World, key: Key, modifiers: Modifiers) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key,
                modifiers,
                repeat: false,
            });
    }

    fn clear_keys(world: &mut World) {
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    fn focused(world: &World) -> Option<Entity> {
        world.resource::<FocusTracker>().0
    }

    /// Dropdown fixture: header + panel + three option rows
    /// ("Light", "Medium", "Heavy"), medium committed.
    /// Returns (header, panel, [rows]).
    fn spawn_dropdown(world: &mut World) -> (Entity, Entity, [Entity; 3]) {
        let header = world
            .spawn(DropdownButton {
                open_signal: OPEN.into(),
                value_signal: "weight".into(),
                options: vec![
                    DropdownOptionSpec {
                        value: "light".into(),
                        label: "Light".into(),
                        disabled: false,
                    },
                    DropdownOptionSpec {
                        value: "medium".into(),
                        label: "Medium".into(),
                        disabled: false,
                    },
                    DropdownOptionSpec {
                        value: "heavy".into(),
                        label: "Heavy".into(),
                        disabled: false,
                    },
                ],
            })
            .id();
        let panel = world
            .spawn(PopupPanel {
                open_signal: OPEN.into(),
                default_top: 40.0,
                default_bottom: -1.0,
                positioned: false,
            })
            .id();
        let mut rows = [Entity::PLACEHOLDER; 3];
        for (i, (value, label)) in [("light", "Light"), ("medium", "Medium"), ("heavy", "Heavy")]
            .iter()
            .enumerate()
        {
            rows[i] = world
                .spawn((
                    DropdownOptionButton {
                        value_signal: "weight".into(),
                        value: (*value).into(),
                        open_signal: OPEN.into(),
                    },
                    TextContent((*label).into()),
                    DocumentOrder(i as u32),
                    ChildOf(panel),
                ))
                .id();
        }
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("weight", "medium");
        (header, panel, rows)
    }

    fn run_lifecycle(world: &mut World) {
        world.run_system_once(popup_nav_lifecycle).unwrap();
    }

    fn run_keys(world: &mut World) {
        world.run_system_once(popup_nav_keys).unwrap();
        clear_keys(world);
    }

    #[test]
    fn open_moves_highlight_to_committed_option() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        assert_eq!(
            focused(&world),
            Some(rows[1]),
            "committed option (medium) gets the highlight on open"
        );
        assert!(
            world.get::<Focused>(rows[1]).is_some(),
            "highlight is the Focused marker"
        );
    }

    #[test]
    fn arrows_move_highlight_without_committing_and_clamp() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);

        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[2]), "down: medium -> heavy");
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(rows[2]),
            "dropdown highlight clamps at the last row (no wrap)"
        );
        press(&mut world, Key::Named(NamedKey::Home));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[0]), "Home -> first enabled");
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("medium"),
            "highlight movement never commits"
        );
    }

    #[test]
    fn escape_close_reverts_and_restores_header_focus() {
        let mut world = base_world();
        let (header, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[2]));

        // Escape path: lumenc's close_dialogs_on_escape flips the
        // signal; the lifecycle system observes the close.
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, false);
        run_lifecycle(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("medium"),
            "close without commit leaves the pre-open selection"
        );
        assert_eq!(
            focused(&world),
            Some(header),
            "focus returns to the dropdown header"
        );
        assert!(
            world.resource::<ActivePopupNav>().0.is_none(),
            "session ended"
        );
    }

    #[test]
    fn disabled_rows_are_skipped_by_highlight_movement() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world.entity_mut(rows[2]).insert(Disabled);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        assert_eq!(focused(&world), Some(rows[1]));
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(rows[1]),
            "the only row below is disabled -> highlight stays"
        );
        press(&mut world, Key::Named(NamedKey::End));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(rows[1]),
            "End lands on the last ENABLED row"
        );
    }

    #[test]
    fn type_ahead_moves_highlight_and_wraps() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        assert_eq!(focused(&world), Some(rows[1]), "start at medium");

        press(&mut world, Key::Character("h".into()));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[2]), "'h' -> Heavy");

        // "l" wraps past the end back to Light.
        world.resource_mut::<TypeAheadBuffer>().last = None; // expire prefix
        press(&mut world, Key::Character("l".into()));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[0]), "'l' wraps to Light");
    }

    #[test]
    fn multi_char_prefix_keeps_current_row_eligible() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);

        press(&mut world, Key::Character("h".into()));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[2]), "'h' -> Heavy");
        press(&mut world, Key::Character("e".into()));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(rows[2]),
            "'he' still matches Heavy - extending the prefix keeps the row"
        );
    }

    #[test]
    fn alt_up_closes_open_dropdown_without_commit() {
        let mut world = base_world();
        let (_, _, _rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        press_mod(
            &mut world,
            Key::Named(NamedKey::ArrowUp),
            Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        run_keys(&mut world);
        let store = world.resource::<PropertyStore>();
        assert_eq!(store.get_global_bool(OPEN), Some(false), "popup closed");
        assert_eq!(
            store.get_global_str("weight").as_deref(),
            Some("medium"),
            "no commit"
        );
    }

    #[test]
    fn focus_leaving_panel_closes_popup() {
        let mut world = base_world();
        let (_, _, _rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        // Simulate Tab / click-to-focus moving focus to an unrelated
        // widget.
        let other = world.spawn_empty().id();
        world.resource_mut::<FocusTracker>().0 = Some(other);
        run_lifecycle(&mut world);
        assert_eq!(
            world.resource::<PropertyStore>().get_global_bool(OPEN),
            Some(false),
            "popup dismissed on focus-out"
        );
        assert_eq!(
            focused(&world),
            Some(other),
            "focus is NOT yanked back to the header"
        );
    }

    #[test]
    fn keyboard_move_strips_hover_highlight() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        // Pointer rests on row 0 (hover highlight).
        world.entity_mut(rows[0]).insert(Hovered);
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(rows[2]), "keyboard moved highlight");
        assert!(
            world.get::<Hovered>(rows[0]).is_none(),
            "keyboard move strips the stale hover highlight (last input wins)"
        );
    }

    #[test]
    fn hover_moves_highlight_to_row() {
        let mut world = base_world();
        let (_, _, rows) = spawn_dropdown(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        assert_eq!(focused(&world), Some(rows[1]));
        world.entity_mut(rows[0]).insert(Hovered);
        world.run_system_once(follow_hover_highlight).unwrap();
        assert_eq!(
            focused(&world),
            Some(rows[0]),
            "hovering a row moves the shared highlight onto it"
        );
        assert!(
            world.get::<FocusVisible>(rows[0]).is_none(),
            "pointer-driven highlight carries no :focus-visible"
        );
    }

    #[test]
    fn highlight_movement_scrolls_row_into_view() {
        let mut world = base_world();
        let (_, panel, rows) = spawn_dropdown(&mut world);
        // Panel is a 100x64 scroller; rows are 32 px tall, stacked.
        world.entity_mut(panel).insert((
            Transform::new(Vec2::new(0.0, 0.0), Vec2::new(100.0, 64.0)),
            Scroll::vertical(),
            ScrollOffset::default(),
        ));
        for (i, row) in rows.iter().enumerate() {
            world.entity_mut(*row).insert(Transform::new(
                Vec2::new(0.0, i as f32 * 32.0),
                Vec2::new(100.0, 32.0),
            ));
        }
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(OPEN, true);
        run_lifecycle(&mut world);
        press(&mut world, Key::Named(NamedKey::End));
        run_keys(&mut world);
        let off = world.get::<ScrollOffset>(panel).unwrap().0.y;
        assert_eq!(
            off, 32.0,
            "row 2 (y=64..96) needs offset 32 to sit inside the 64-tall viewport"
        );
    }

    /// Menu fixture: panel + items Rename / Duplicate / Delete with
    /// Duplicate disabled. Returns (panel, [items]).
    fn spawn_menu(world: &mut World) -> (Entity, [Entity; 3]) {
        let panel = world
            .spawn(PopupPanel {
                open_signal: MENU_OPEN.into(),
                default_top: 0.0,
                default_bottom: f32::NAN,
                positioned: false,
            })
            .id();
        let mut items = [Entity::PLACEHOLDER; 3];
        for (i, label) in ["Rename", "Duplicate", "Delete"].iter().enumerate() {
            let mut e = world.spawn((
                MenuItemButton {
                    open_signal: MENU_OPEN.into(),
                    item_id: label.to_lowercase(),
                },
                TextContent((*label).into()),
                DocumentOrder(i as u32),
                ChildOf(panel),
            ));
            if i == 1 {
                e.insert(Disabled);
            }
            items[i] = e.id();
        }
        (panel, items)
    }

    #[test]
    fn menu_opens_with_no_highlight_and_arrows_skip_disabled_with_wrap() {
        let mut world = base_world();
        let (panel, items) = spawn_menu(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(MENU_OPEN, true);
        run_lifecycle(&mut world);
        assert_eq!(
            focused(&world),
            Some(panel),
            "fresh menu: no item highlighted (focus parks on the panel)"
        );

        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(items[0]), "first Down -> first item");

        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(items[2]),
            "Duplicate is disabled -> skipped"
        );

        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(items[0]), "menu wraps past the end");

        press(&mut world, Key::Named(NamedKey::ArrowUp));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(items[2]), "wraps backwards too");
    }

    #[test]
    fn menu_up_from_no_highlight_enters_at_bottom() {
        let mut world = base_world();
        let (_, items) = spawn_menu(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(MENU_OPEN, true);
        run_lifecycle(&mut world);
        press(&mut world, Key::Named(NamedKey::ArrowUp));
        run_keys(&mut world);
        assert_eq!(
            focused(&world),
            Some(items[2]),
            "Up with no highlight enters at the last enabled item"
        );
    }

    #[test]
    fn menu_close_restores_previous_focus() {
        let mut world = base_world();
        let prev = world.spawn_empty().id();
        world.resource_mut::<FocusTracker>().0 = Some(prev);
        let (_, items) = spawn_menu(&mut world);
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(MENU_OPEN, true);
        run_lifecycle(&mut world);
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_keys(&mut world);
        assert_eq!(focused(&world), Some(items[0]));

        world
            .resource_mut::<PropertyStore>()
            .set_global_bool(MENU_OPEN, false);
        run_lifecycle(&mut world);
        assert_eq!(
            focused(&world),
            Some(prev),
            "menu close hands focus back to the pre-open holder"
        );
    }

    // -- Closed-combobox keyboard interaction ------------------------

    fn run_closed(world: &mut World) {
        world.run_system_once(closed_dropdown_keys).unwrap();
        clear_keys(world);
    }

    #[test]
    fn closed_arrows_step_value_directly_and_clamp() {
        let mut world = base_world();
        let (header, ..) = spawn_dropdown(&mut world);
        world.resource_mut::<FocusTracker>().0 = Some(header);

        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_closed(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("heavy"),
            "closed Down commits the next option"
        );
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_closed(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("heavy"),
            "clamps at the last option (no wrap)"
        );
        press(&mut world, Key::Named(NamedKey::ArrowUp));
        press(&mut world, Key::Named(NamedKey::ArrowUp));
        run_closed(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("light"),
            "two Ups walk back to the first option"
        );
    }

    #[test]
    fn closed_arrows_skip_disabled_options() {
        let mut world = base_world();
        let (header, ..) = spawn_dropdown(&mut world);
        // Disable "heavy" in the header metadata.
        world.get_mut::<DropdownButton>(header).unwrap().options[2].disabled = true;
        world.resource_mut::<FocusTracker>().0 = Some(header);
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_closed(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("medium"),
            "the only option below is disabled -> value unchanged"
        );
    }

    #[test]
    fn closed_alt_down_opens_popup_instead_of_stepping() {
        let mut world = base_world();
        let (header, ..) = spawn_dropdown(&mut world);
        world.resource_mut::<FocusTracker>().0 = Some(header);
        press_mod(
            &mut world,
            Key::Named(NamedKey::ArrowDown),
            Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        run_closed(&mut world);
        let store = world.resource::<PropertyStore>();
        assert_eq!(store.get_global_bool(OPEN), Some(true), "popup opened");
        assert_eq!(
            store.get_global_str("weight").as_deref(),
            Some("medium"),
            "value untouched"
        );
    }

    #[test]
    fn closed_type_ahead_commits_matching_option() {
        let mut world = base_world();
        let (header, ..) = spawn_dropdown(&mut world);
        world.resource_mut::<FocusTracker>().0 = Some(header);
        press(&mut world, Key::Character("l".into()));
        run_closed(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("weight")
                .as_deref(),
            Some("light"),
            "'l' jumps (and commits) to Light"
        );
    }

    #[test]
    fn type_ahead_prefix_resets_after_interval() {
        let mut buf = TypeAheadBuffer::default();
        let t0 = Instant::now();
        let reset = Duration::from_secs(1);
        assert!(!buf.push("h", t0, reset), "first char starts a prefix");
        assert!(
            buf.push("e", t0 + Duration::from_millis(300), reset),
            "fast follow-up extends"
        );
        assert_eq!(buf.prefix, "he");
        assert!(
            !buf.push("l", t0 + Duration::from_millis(1500), reset),
            "past the reset interval the prefix restarts"
        );
        assert_eq!(buf.prefix, "l");
    }

    #[test]
    fn type_ahead_char_rejects_chords_named_forwards_and_controls() {
        let none = Modifiers::default();
        assert_eq!(
            type_ahead_char(&Key::Character("h".into()), &none),
            Some("h")
        );
        assert_eq!(
            type_ahead_char(&Key::Character("PageUp".into()), &none),
            None,
            "multi-char forwarded key names never type-ahead"
        );
        assert_eq!(type_ahead_char(&Key::Named(NamedKey::Enter), &none), None);
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            type_ahead_char(&Key::Character("h".into()), &ctrl),
            None,
            "chords are not type-ahead"
        );
    }
}
