//! `<tabs>` strip-button runtime.
//!
//! The markup-level `<tabs bind-value="active"> <tab name="general"
//! label="General">...</tab> ... </tabs>` parser pass synthesises one
//! `<button class="tab-btn">` per tab and tags each with a
//! [`TabStripButton`] component. This plugin's tiny dispatcher
//! watches `ClickEvent`s, looks up the matching button, and writes
//! `Signals[signal_name] = tab_value` - no author-side script
//! required for the basic "switch active tab" flow.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::input::{ClickEvent, FocusTracker, Key, KeyPressed, MenuClicked, NamedKey};
use lumen_core::prelude::*;

/// Strip-button marker. `(signal_name, tab_value)` - clicking writes
/// `Signals[signal_name] = tab_value`. Authored only by the
/// `<tabs>` parser pass; not exposed to markup directly.
#[derive(Component, Clone, Debug)]
pub struct TabStripButton {
    /// Signal name the `<tabs bind-value>` attribute pointed at.
    pub signal_name: String,
    /// `name=` of the `<tab>` this button represents.
    pub value: String,
}

/// Default fill for a `.tab-btn:selected` when no author / skin
/// `:selected { bg: ... }` rule matched. Mirrors the default skin's
/// `--lumen-accent` (`#33c7ce`) so a bare `<tabs>` with no CSS still
/// shows which tab is active.
pub const TAB_SELECTED_BG: Color = Color::rgb(0.20, 0.78, 0.81);

/// Per-tab-button track fills, resolved at spawn from markup / CSS.
/// [`sync_tab_button_visuals`] swaps [`Visuals::fill`] between the two
/// as the [`Selected`] marker is added to / removed from the button -
/// same pattern as [`crate::controls::ToggleStyle`] /
/// [`crate::controls::sync_toggle_visuals`] for `<toggle>`.
#[derive(Component, Clone, Copy, Debug)]
pub struct TabButtonStyle {
    /// Fill shown while this button carries [`Selected`] (`:selected {
    /// bg: ... }` or [`TAB_SELECTED_BG`]).
    pub selected_bg: Color,
    /// Fill shown while unselected (author `bg` / class default, or
    /// fully transparent when neither supplied one).
    pub unselected_bg: Color,
}

impl Default for TabButtonStyle {
    fn default() -> Self {
        Self {
            selected_bg: TAB_SELECTED_BG,
            unselected_bg: Color::rgba(0.0, 0.0, 0.0, 0.0),
        }
    }
}

/// Plugin: registers the click dispatcher in `TickStage::Systems`
/// after `lumen-input`'s `dispatch_clicks` so we observe the same
/// frame's clicks.
pub struct TabsPlugin;

impl Plugin for TabsPlugin {
    fn build(self, app: &mut App) {
        app.add_systems(
            TickStage::Systems,
            dispatch_tab_clicks.after(lumen_input::dispatch_clicks),
        );
        // Mirror the bound signal onto the `Selected` marker so the
        // active tab button is queryable / stylable regardless of
        // whether the activation came from a click, keyboard nav, or a
        // script write.
        app.add_systems(
            TickStage::Systems,
            sync_tab_selected.after(dispatch_tab_clicks),
        );
        // Paint the marker: swap the button's fill the same tick
        // `Selected` moves, so a click doesn't show a one-frame stale
        // highlight on the old tab.
        app.add_systems(
            TickStage::Systems,
            sync_tab_button_visuals.after(sync_tab_selected),
        );
        app.add_systems(
            TickStage::Systems,
            dispatch_dropdown_clicks.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            dispatch_menu_item_clicks.after(lumen_input::dispatch_clicks),
        );
        // Dismiss an open dropdown / menu panel when a primary press
        // lands outside it (and outside its trigger). Runs after
        // `dispatch_clicks` so it observes the same tick's hit-test.
        app.add_systems(
            TickStage::Systems,
            crate::popup::dismiss_popups_on_outside_press.after(lumen_input::dispatch_clicks),
        );
        // Viewport edge-flip: anchor open dropdown panels below their
        // trigger, flipping above near the viewport bottom.
        app.add_systems(TickStage::Systems, crate::popup::flip_open_dropdown_panels);
        // Roving-tabindex keyboard nav for the focused tab button:
        // ArrowRight / ArrowLeft cycle siblings, Home / End jump to
        // ends, Enter / Space activate (same effect as click). Reads
        // `KeyPressed` directly so it observes the raw key bus before
        // `dispatch_focused_keys` routes events through the focused
        // entity - that path strips Tab specifically but otherwise
        // preserves keys, so reading either bus works; the global bus
        // is the simpler hookup because it doesn't require us to also
        // be a TextInput.
        app.add_systems(TickStage::Systems, dispatch_tab_keys);
        // Popup keyboard nav (Wave 4 Qt polish): closed-combobox value
        // stepping + type-ahead, open-popup highlight movement, hover <->
        // keyboard highlight unification, and open/close focus
        // hand-off. Lifecycle runs last so it observes the same tick's
        // toggle clicks, outside-press dismissals, and Alt+Arrow
        // open/close writes.
        app.world
            .init_resource::<crate::popup_nav::ActivePopupNav>();
        app.world
            .init_resource::<crate::popup_nav::PopupNavConfig>();
        app.world
            .init_resource::<crate::popup_nav::TypeAheadBuffer>();
        app.add_systems(
            TickStage::Systems,
            crate::popup_nav::closed_dropdown_keys.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            crate::popup_nav::popup_nav_keys.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            crate::popup_nav::follow_hover_highlight.after(lumen_input::hit_test),
        );
        app.add_systems(
            TickStage::Systems,
            crate::popup_nav::popup_nav_lifecycle
                .after(crate::popup_nav::popup_nav_keys)
                .after(crate::popup_nav::closed_dropdown_keys)
                .after(crate::popup_nav::follow_hover_highlight)
                .after(dispatch_dropdown_clicks)
                .after(dispatch_menu_item_clicks)
                .after(crate::popup::dismiss_popups_on_outside_press),
        );
        // AT-driven Expand / Collapse -> dropdown open signal. window-winit's accesskit handler
        // flips A11yState::EXPANDED in response to assistive-tech `Action::Expand` /
        // `Action::Collapse`; this system observes the bit flip and writes it through to the
        // author-facing `__dropdown_open:<bind>` signal so screen-reader users can open / close
        // the panel without faking a click event.
        app.add_systems(TickStage::Systems, apply_a11y_expand_to_dropdown);
    }
}

/// Switch tabs on **press**, not release - `QTabBar::mousePressEvent`
/// changes the current tab the moment the button goes down, and Lumen
/// matches that feel. The press path observes the [`Pressed`] capture
/// marker `lumen_input::dispatch_clicks` inserts (pointer) or the Space
/// FSM inserts (keyboard); the explicit `.after(dispatch_clicks)` edge
/// in [`TabsPlugin`] flushes the marker command first, so the switch
/// lands on the press tick. Disabled tabs never gain `Pressed`
/// (`hit_test` + `dispatch_clicks` both skip them), so they can't
/// switch.
///
/// The `ClickEvent` path is kept for keyboard-synthesized clicks
/// (Enter via `activate_focused_on_enter`) and stays idempotent - a
/// release on the already-switched tab re-writes the same value, which
/// `PropertyStore::set` drops as a no-op.
fn dispatch_tab_clicks(
    mut clicks: MessageReader<ClickEvent>,
    buttons: Query<&TabStripButton>,
    newly_pressed: Query<Entity, Added<Pressed>>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
) {
    // Hit-shadowing (W5): the press / click can land on a hit-testable
    // CHILD of the tab button (its text child) rather than the button
    // entity itself - resolve up the ancestor chain like
    // `controls::resolve_control` does for the slider thumb.
    for pressed in &newly_pressed {
        if let Some(target) =
            crate::controls::resolve_control(pressed, &parents, |e| buttons.contains(e))
            && let Ok(btn) = buttons.get(target)
        {
            store.set_global_str(&btn.signal_name, btn.value.as_str());
        }
    }
    for ev in clicks.read() {
        if let Some(target) =
            crate::controls::resolve_control(ev.entity, &parents, |e| buttons.contains(e))
            && let Ok(btn) = buttons.get(target)
        {
            store.set_global_str(&btn.signal_name, btn.value.as_str());
        }
    }
}

/// Maintain the [`Selected`] marker across every tab strip: the button
/// whose `value` matches its bound signal's current value carries it;
/// all its siblings shed it. Idempotent per tick - insert / remove only
/// fire on actual transitions, so archetypes stay put on idle frames.
pub fn sync_tab_selected(
    mut commands: Commands,
    store: Res<PropertyStore>,
    buttons: Query<(Entity, &TabStripButton, Option<&Selected>)>,
) {
    for (entity, btn, selected) in &buttons {
        let active = store.get_global_str(&btn.signal_name).as_deref() == Some(btn.value.as_str());
        match (active, selected.is_some()) {
            (true, false) => {
                commands.entity(entity).insert(Selected);
            }
            (false, true) => {
                commands.entity(entity).remove::<Selected>();
            }
            _ => {}
        }
    }
}

/// Keep a tab-strip button's visuals in step with whether it currently
/// carries [`Selected`]: swap [`Visuals::fill`] between
/// [`TabButtonStyle::selected_bg`] / [`TabButtonStyle::unselected_bg`],
/// and rebase any captured [`crate::hover::HoverBaseColor`] /
/// [`crate::hover::PressBaseColor`] snapshot onto the new fill so a
/// tint release doesn't restore the stale pre-swap color - mirrors
/// [`crate::controls::sync_toggle_visuals`]'s track-fill handling.
///
/// Runs every tick rather than gating on a change filter: unlike
/// `Toggleable::checked` (a `bool` that only ever changes via an
/// explicit flip), `Selected` is a marker that's inserted *and removed*
/// as it moves between siblings, and there's no single component whose
/// `Changed` filter would fire on both transitions. The write-if-target-
/// differs guard below keeps idle frames from dirtying `Visuals` (and
/// therefore triggering a repaint) even though the query has no filter -
/// same technique [`sync_slider_thumb`](crate::controls::sync_slider_thumb)
/// and the [`crate::controls::sync_toggle_visuals`] knob pass use.
#[allow(clippy::type_complexity)]
pub fn sync_tab_button_visuals(
    mut buttons: Query<
        (
            &TabButtonStyle,
            Option<&Selected>,
            &mut Visuals,
            Option<&mut crate::hover::HoverBaseColor>,
            Option<&mut crate::hover::PressBaseColor>,
        ),
        With<TabStripButton>,
    >,
) {
    for (style, selected, mut vis, hover_base, press_base) in &mut buttons {
        let target = if selected.is_some() {
            style.selected_bg
        } else {
            style.unselected_bg
        };
        if vis.fill.as_ref().and_then(Fill::as_solid) != Some(target) {
            vis.fill = Some(Fill::Solid(target));
        }
        if let Some(mut base) = hover_base
            && base.0 != target
        {
            base.0 = target;
        }
        if let Some(mut base) = press_base
            && base.0 != target
        {
            base.0 = target;
        }
    }
}

/// One `<option>`'s metadata mirrored onto the dropdown header (markup
/// order). Carried on [`DropdownButton`] so the *closed* combobox can
/// run keyboard interaction - Up/Down value stepping and type-ahead -
/// before the panel body (and its [`DropdownOptionButton`] entities)
/// has ever mounted (`<if>` bodies spawn lazily on first open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropdownOptionSpec {
    /// `value=` attribute - what gets written to the bound signal.
    pub value: String,
    /// `label=` attribute (falls back to `value` in the parser) - what
    /// type-ahead matches against, mirroring Qt's text-based matching.
    pub label: String,
    /// `disabled="true"` on the source `<option>` - skipped by value
    /// stepping and type-ahead.
    pub disabled: bool,
}

/// `<dropdown>` toggle marker attached by the parser to the dropdown header button.
/// Clicks flip `Signals[open_signal]` between `"true"` and `"false"`.
#[derive(Component, Clone, Debug)]
pub struct DropdownButton {
    /// Internal "is panel open?" signal name (synthesised by the
    /// parser as `__dropdown_open:<bind-value>`).
    pub open_signal: String,
    /// Author-bound value signal (`<dropdown bind-value>`), written by
    /// closed-state Up/Down stepping and type-ahead.
    pub value_signal: String,
    /// Ordered option metadata (see [`DropdownOptionSpec`]).
    pub options: Vec<DropdownOptionSpec>,
}

impl DropdownButton {
    /// Header with an open signal only - no closed-state keyboard
    /// metadata. Test fixtures and minimal embedders use this; `lumenc`
    /// always supplies the full option list.
    pub fn new(open_signal: impl Into<String>) -> Self {
        Self {
            open_signal: open_signal.into(),
            value_signal: String::new(),
            options: Vec::new(),
        }
    }
}

/// `<option>` button inside a `<dropdown>` panel. Click writes `Signals[value_signal] = value` and closes the panel by writing `Signals[open_signal] = "false"`.
#[derive(Component, Clone, Debug)]
pub struct DropdownOptionButton {
    /// Author-bound signal (matches the `<dropdown bind-value>` attr).
    pub value_signal: String,
    /// `value=` attribute on the source `<option>`.
    pub value: String,
    /// Internal "is panel open?" signal name.
    pub open_signal: String,
}

/// `<dropdown>` click dispatcher. Toggles the open-panel signal on header clicks; writes the selected value and closes the panel on option clicks.
///
/// Open / close state goes through [`PropertyStore::set_global_bool`] /
/// [`PropertyStore::get_global_bool`] so any typo-prone literal-string variants
/// (`"True"`, `"TRUE"`, `"yes"`) are funnelled into the canonical
/// `"true"` / `"false"` pair the compiler's `<if eq="true">` body
/// comparator recognises. The on-the-wire signal repr is unchanged.
pub fn dispatch_dropdown_clicks(
    mut clicks: MessageReader<ClickEvent>,
    headers: Query<&DropdownButton>,
    options: Query<&DropdownOptionButton>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
) {
    for ev in clicks.read() {
        // Hit-shadowing (W5): pointer clicks resolve to the header's /
        // option's hit-testable TEXT CHILD, not the button entity -
        // walk the ancestor chain to the owning control (same fix as
        // the slider thumb / toggle knob in `controls::resolve_control`).
        // Options resolve first: an option is never an ancestor of a
        // header, and a click inside the panel must not toggle the
        // header.
        if let Some(target) =
            crate::controls::resolve_control(ev.entity, &parents, |e| options.contains(e))
            && let Ok(opt) = options.get(target)
        {
            store.set_global_str(&opt.value_signal, opt.value.as_str());
            store.set_global_bool(&opt.open_signal, false);
            continue;
        }
        if let Some(target) =
            crate::controls::resolve_control(ev.entity, &parents, |e| headers.contains(e))
            && let Ok(btn) = headers.get(target)
        {
            // Default to closed (false) when the signal has never been
            // written or carries a non-boolean value; otherwise flip
            // the current state. Matches the previous behaviour where
            // any non-`"true"` value collapsed to closed.
            let next = !store.get_global_bool(&btn.open_signal).unwrap_or(false);
            store.set_global_bool(&btn.open_signal, next);
        }
    }
}

/// `<menu>` item marker. Clicks emit [`MenuClicked`] with the item id and close the menu by writing the open signal to `"false"`.
#[derive(Component, Clone, Debug)]
pub struct MenuItemButton {
    /// Menu's open-state signal (`__menu_open:<menu-id>`).
    pub open_signal: String,
    /// `id=` on the source `<menuitem>` - relayed to `on_menu(id)`.
    pub item_id: String,
}

/// `<menu>` item click dispatcher. Emits [`MenuClicked`] and closes the menu panel in the same tick.
pub fn dispatch_menu_item_clicks(
    mut clicks: MessageReader<ClickEvent>,
    items: Query<&MenuItemButton>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
    mut menu_out: MessageWriter<MenuClicked>,
) {
    for ev in clicks.read() {
        // Hit-shadowing (W5): resolve text-child hits up to the owning
        // menu item - see `dispatch_dropdown_clicks`.
        if let Some(target) =
            crate::controls::resolve_control(ev.entity, &parents, |e| items.contains(e))
            && let Ok(item) = items.get(target)
        {
            menu_out.write(MenuClicked {
                id: item.item_id.clone(),
            });
            store.set_global_bool(&item.open_signal, false);
        }
    }
}

/// Keyboard navigation for tab strips, scoped to children of a tab
/// list (i.e. siblings of the focused [`TabStripButton`]). Matches the
/// `QTabBar` keyboard contract:
///
/// - `ArrowRight` / `ArrowLeft`: move focus to the next / previous
///   *enabled* sibling tab button - **and select it immediately**
///   (Qt select-on-arrow; `QTabBar::keyPressEvent` calls
///   `setCurrentIndex` as focus moves). No wrap-around: at either end
///   the arrow is a no-op, exactly like Qt.
/// - `Home` / `End`: jump to (and select) the first / last enabled
///   sibling.
/// - `Enter` / `Space`: activate the focused tab - the same effect as
///   clicking it, i.e. writes `Signals[signal_name] = value`.
/// - Disabled tabs are skipped by every movement, mirroring
///   `QTabBar`'s enabled-only traversal.
///
/// Siblings are identified as all [`TabStripButton`] entities that
/// share the focused tab's parent (the synthesised `tab-strip` row)
/// and bind to the same `signal_name`. This keeps the nav strictly
/// inside one tab strip even when several strips coexist in the same
/// app, and never bleeds into non-tab focusables like inputs or
/// buttons sitting alongside the strip.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_tab_keys(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    mut tracker: ResMut<FocusTracker>,
    mut store: ResMut<PropertyStore>,
    buttons: Query<(Entity, &TabStripButton)>,
    disableds: Query<(), With<lumen_core::components::Disabled>>,
    orders: Query<&lumen_core::components::DocumentOrder>,
    parents: Query<&ChildOf>,
) {
    let Some(current) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    // Confirm the focused entity is a tab button before we touch any
    // keys - otherwise the nav must stay silent so non-tab focusables
    // (text inputs, generic buttons) don't have their ArrowLeft /
    // ArrowRight stolen.
    let Ok((_, current_btn)) = buttons.get(current) else {
        keys.read().for_each(drop);
        return;
    };
    let signal_name = current_btn.signal_name.clone();
    let current_value = current_btn.value.clone();
    let Ok(child_of_current) = parents.get(current) else {
        keys.read().for_each(drop);
        return;
    };
    let parent_id = child_of_current.parent();
    // Sibling set: every tab button whose parent matches the focused
    // tab's parent AND whose `signal_name` matches. Filtering on the
    // signal scopes nav to one strip even when several strips share a
    // common ancestor (nested tabs). Disabled siblings stay in the
    // list (so the focused tab's own index resolves) but are skipped
    // by every movement below.
    let mut siblings: Vec<(u32, Entity, String, bool)> = buttons
        .iter()
        .filter_map(|(e, btn)| {
            let Ok(co) = parents.get(e) else {
                return None;
            };
            if co.parent() != parent_id || btn.signal_name != signal_name {
                return None;
            }
            Some((
                orders.get(e).map(|d| d.0).unwrap_or(u32::MAX),
                e,
                btn.value.clone(),
                disableds.contains(e),
            ))
        })
        .collect();
    // Markup order: `DocumentOrder` is the spawner's monotonic
    // walk-order counter, so it always matches the on-screen strip
    // order. `Entity` is only the deterministic fallback for
    // hand-built worlds - bevy_ecs 0.18's `Entity: Ord` compares a
    // niche-optimized row index and does NOT track spawn order (the
    // same trap `cycle_focus_on_tab` documents).
    siblings.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let siblings: Vec<(Entity, String, bool)> = siblings
        .into_iter()
        .map(|(_, e, value, disabled)| (e, value, disabled))
        .collect();
    if siblings.is_empty() {
        keys.read().for_each(drop);
        return;
    }
    let Some(current_index) = siblings.iter().position(|(e, ..)| *e == current) else {
        keys.read().for_each(drop);
        return;
    };

    // Next enabled sibling after `from` (exclusive) in `dir` (+1 / -1);
    // `None` when the edge is reached without finding one - Qt tab bars
    // do not wrap.
    let step = |from: usize, dir: i64| -> Option<usize> {
        let mut i = from as i64 + dir;
        while i >= 0 && (i as usize) < siblings.len() {
            if !siblings[i as usize].2 {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    };

    let mut focus_target: Option<usize> = None;
    let mut should_activate = false;
    for ev in keys.read() {
        match &ev.key {
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(idx) = step(focus_target.unwrap_or(current_index), 1) {
                    focus_target = Some(idx);
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(idx) = step(focus_target.unwrap_or(current_index), -1) {
                    focus_target = Some(idx);
                }
            }
            Key::Named(NamedKey::Home) => {
                focus_target = siblings.iter().position(|(.., d)| !d);
            }
            Key::Named(NamedKey::End) => {
                focus_target = siblings.iter().rposition(|(.., d)| !d);
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                should_activate = true;
            }
            _ => {}
        }
    }

    if should_activate {
        // Activate the currently focused tab - same effect as a click.
        store.set_global_str(&signal_name, current_value.as_str());
    }
    if let Some(idx) = focus_target
        && siblings[idx].0 != current
    {
        let (next_entity, next_value, _) = &siblings[idx];
        commands
            .entity(current)
            .remove::<(Focused, lumen_core::input::FocusVisible)>();
        // Roving-tabindex arrows are keyboard navigation - mark the new
        // holder for `:focus-visible` styling.
        commands
            .entity(*next_entity)
            .insert((Focused, lumen_core::input::FocusVisible));
        tracker.0 = Some(*next_entity);
        // Qt select-on-arrow: moving tab focus with the keyboard also
        // switches to that tab immediately.
        store.set_global_str(&signal_name, next_value.as_str());
    }
}

/// Mirrors [`A11yState::EXPANDED`] onto the dropdown's open-state signal whenever the bit changes.
///
/// `window-winit`'s accesskit action handler responds to assistive-tech `Action::Expand` /
/// `Action::Collapse` by flipping `A11yState::EXPANDED` on the receiving entity (today the
/// dropdown header). Without a downstream consumer that bit flip was inert - the open signal that
/// gates the panel body's `<if eq="true">` never moved. This system closes that loop: a
/// `Changed<A11yState>` filter picks the entity up on the next tick and
/// [`PropertyStore::set_global_bool`] writes `"true"` / `"false"` to the header's `open_signal`.
/// `set_global_bool` no-ops when the value is unchanged, so click-driven updates that already
/// wrote the signal don't bounce back through the AT path.
pub fn apply_a11y_expand_to_dropdown(
    headers: Query<(&DropdownButton, &A11yState), Changed<A11yState>>,
    mut store: ResMut<PropertyStore>,
) {
    for (btn, state) in &headers {
        let expanded = state.contains(A11yState::EXPANDED);
        store.set_global_bool(&btn.open_signal, expanded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    #[test]
    fn selected_marker_follows_active_tab_signal() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let strip = |value: &str| TabStripButton {
            signal_name: "active".to_string(),
            value: value.to_string(),
        };
        let general = world.spawn(strip("general")).id();
        let advanced = world.spawn(strip("advanced")).id();

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_tab_selected);

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "general");
        schedule.run(&mut world);
        assert!(world.get::<Selected>(general).is_some(), "general active");
        assert!(world.get::<Selected>(advanced).is_none());

        // Activation moves - the marker must move with it.
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "advanced");
        schedule.run(&mut world);
        assert!(
            world.get::<Selected>(general).is_none(),
            "previous tab sheds the marker"
        );
        assert!(
            world.get::<Selected>(advanced).is_some(),
            "new tab gains it"
        );
    }

    #[test]
    fn sync_tab_button_visuals_repaints_on_selected_add_and_remove() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let style = TabButtonStyle {
            selected_bg: Color::rgb(1.0, 0.0, 0.0),
            unselected_bg: Color::rgb(0.0, 0.0, 1.0),
        };
        let strip = |value: &str| {
            (
                TabStripButton {
                    signal_name: "active".to_string(),
                    value: value.to_string(),
                },
                style,
                Visuals {
                    fill: Some(Fill::Solid(style.unselected_bg)),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
            )
        };
        let general = world.spawn(strip("general")).id();
        let advanced = world.spawn(strip("advanced")).id();
        // Attach a HoverBaseColor to `general` up front to exercise the
        // rebase path - a stale hover snapshot must track the fill swap
        // so releasing the hover doesn't restore the pre-swap color.
        world
            .entity_mut(general)
            .insert(crate::hover::HoverBaseColor(style.unselected_bg));

        let mut schedule = Schedule::default();
        schedule.add_systems((sync_tab_selected, sync_tab_button_visuals).chain());

        let fill_of = |world: &World, e: Entity| -> Color {
            world
                .get::<Visuals>(e)
                .and_then(|v| v.fill.as_ref())
                .and_then(Fill::as_solid)
                .expect("solid fill")
        };

        // Nothing active yet: both stay unselected.
        schedule.run(&mut world);
        assert_eq!(fill_of(&world, general), style.unselected_bg);
        assert_eq!(fill_of(&world, advanced), style.unselected_bg);

        // `general` becomes active: its fill (and hover-base snapshot)
        // swap to the selected color; `advanced` stays unselected.
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "general");
        schedule.run(&mut world);
        assert_eq!(
            fill_of(&world, general),
            style.selected_bg,
            "selected tab repaints to selected_bg"
        );
        assert_eq!(fill_of(&world, advanced), style.unselected_bg);
        assert_eq!(
            world
                .get::<crate::hover::HoverBaseColor>(general)
                .unwrap()
                .0,
            style.selected_bg,
            "hover-base snapshot rebases onto the new fill"
        );

        // Activation moves to `advanced`: `general` must revert.
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "advanced");
        schedule.run(&mut world);
        assert_eq!(
            fill_of(&world, general),
            style.unselected_bg,
            "deselected tab reverts to unselected_bg"
        );
        assert_eq!(fill_of(&world, advanced), style.selected_bg);
    }

    mod tab_keys {
        //! Qt keyboard/press contract for tab strips: switch on press,
        //! select-on-arrow, no wrap, disabled tabs skipped.
        use super::*;
        use bevy_ecs::message::Messages;
        use bevy_ecs::system::RunSystemOnce;
        use lumen_core::components::Disabled;
        use lumen_core::input::{FocusTracker, Modifiers};

        /// Strip with four tabs (c disabled), parent row, `a` focused.
        /// Returns (world, [a, b, c, d]).
        fn strip_world() -> (World, [Entity; 4]) {
            let mut world = World::new();
            world.init_resource::<Messages<KeyPressed>>();
            world.init_resource::<Messages<ClickEvent>>();
            world.insert_resource(PropertyStore::default());
            let parent = world.spawn_empty().id();
            let mut tabs = [Entity::PLACEHOLDER; 4];
            for (i, value) in ["a", "b", "c", "d"].iter().enumerate() {
                let mut e = world.spawn((
                    TabStripButton {
                        signal_name: "active".to_string(),
                        value: value.to_string(),
                    },
                    lumen_core::components::DocumentOrder(i as u32),
                    bevy_ecs::hierarchy::ChildOf(parent),
                ));
                if i == 2 {
                    e.insert(Disabled);
                }
                tabs[i] = e.id();
            }
            world.insert_resource(FocusTracker(Some(tabs[0])));
            world.entity_mut(tabs[0]).insert(Focused);
            world
                .resource_mut::<PropertyStore>()
                .set_global_str("active", "a");
            (world, tabs)
        }

        fn press_key(world: &mut World, key: NamedKey) {
            world
                .resource_mut::<Messages<KeyPressed>>()
                .write(KeyPressed {
                    key: Key::Named(key),
                    modifiers: Modifiers::default(),
                    repeat: false,
                });
        }

        fn run_keys(world: &mut World) {
            world.run_system_once(dispatch_tab_keys).unwrap();
            world.resource_mut::<Messages<KeyPressed>>().clear();
        }

        fn active(world: &World) -> String {
            world
                .resource::<PropertyStore>()
                .get_global_str("active")
                .as_deref()
                .unwrap_or("")
                .to_string()
        }

        #[test]
        fn arrow_moves_focus_and_selects_immediately() {
            let (mut world, tabs) = strip_world();
            press_key(&mut world, NamedKey::ArrowRight);
            run_keys(&mut world);
            assert_eq!(
                world.resource::<FocusTracker>().0,
                Some(tabs[1]),
                "focus moved to the next tab"
            );
            assert_eq!(active(&world), "b", "Qt select-on-arrow: switches too");
        }

        #[test]
        fn arrow_skips_disabled_and_clamps_at_the_end() {
            let (mut world, tabs) = strip_world();
            // a -> b.
            press_key(&mut world, NamedKey::ArrowRight);
            run_keys(&mut world);
            // b -> d (c is disabled).
            press_key(&mut world, NamedKey::ArrowRight);
            run_keys(&mut world);
            assert_eq!(
                world.resource::<FocusTracker>().0,
                Some(tabs[3]),
                "disabled tab skipped"
            );
            assert_eq!(active(&world), "d");
            // d -> no wrap.
            press_key(&mut world, NamedKey::ArrowRight);
            run_keys(&mut world);
            assert_eq!(
                world.resource::<FocusTracker>().0,
                Some(tabs[3]),
                "no wrap at the last tab (Qt)"
            );
            assert_eq!(active(&world), "d");
        }

        #[test]
        fn home_and_end_jump_to_first_and_last_enabled() {
            let (mut world, tabs) = strip_world();
            press_key(&mut world, NamedKey::End);
            run_keys(&mut world);
            assert_eq!(
                world.resource::<FocusTracker>().0,
                Some(tabs[3]),
                "End -> last enabled (c disabled, d is last)"
            );
            assert_eq!(active(&world), "d");
            press_key(&mut world, NamedKey::Home);
            run_keys(&mut world);
            assert_eq!(world.resource::<FocusTracker>().0, Some(tabs[0]));
            assert_eq!(active(&world), "a");
        }

        #[test]
        fn tab_switches_on_press_not_release() {
            let (mut world, tabs) = strip_world();
            // Pointer press: dispatch_clicks inserts `Pressed` on the
            // press tick; the switch must land the same tick, before
            // any release.
            world.entity_mut(tabs[1]).insert(Pressed);
            world.run_system_once(dispatch_tab_clicks).unwrap();
            assert_eq!(active(&world), "b", "switched on press");
        }
    }

    #[test]
    fn a11y_expanded_round_trips_to_dropdown_open_signal() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());

        let entity = world
            .spawn((
                DropdownButton::new("__dropdown_open:menu"),
                A11yState::default(),
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(apply_a11y_expand_to_dropdown);

        // Initial tick: open_signal is undefined; the Changed<A11yState> filter still picks the
        // freshly-spawned A11yState up and writes "false".
        schedule.run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_bool("__dropdown_open:menu"),
            Some(false),
            "freshly-spawned A11yState default (no EXPANDED) yields open=false"
        );

        // Programmatically flip EXPANDED on -> mirrors the AT `Action::Expand` path.
        world
            .get_mut::<A11yState>(entity)
            .expect("A11yState present")
            .insert(A11yState::EXPANDED);
        schedule.run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_bool("__dropdown_open:menu"),
            Some(true),
            "EXPANDED flip on -> open=true"
        );

        // Flip EXPANDED off -> mirrors `Action::Collapse`.
        world
            .get_mut::<A11yState>(entity)
            .expect("A11yState present")
            .remove(A11yState::EXPANDED);
        schedule.run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_bool("__dropdown_open:menu"),
            Some(false),
            "EXPANDED flip off -> open=false"
        );
    }
}

/// W5 hit-shadowing regression: pointer clicks resolve to the
/// hit-testable TEXT CHILD of a control, not the control entity - every
/// dispatcher must walk the ancestor chain (`controls::resolve_control`)
/// or the control is a dead zone for real pointer input (keyboard paths
/// synthesize clicks on the focused control entity and never noticed).
#[cfg(test)]
mod hit_shadowing_tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    fn click(world: &mut World, target: Entity) {
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: target,
                position: glam::Vec2::ZERO,
                button: lumen_core::input::PointerButton::Primary,
            });
    }

    #[test]
    fn click_on_header_text_child_opens_dropdown() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        let header = world
            .spawn(DropdownButton::new("__dropdown_open:fruit"))
            .id();
        let text_child = world.spawn(ChildOf(header)).id();
        click(&mut world, text_child);
        world.run_system_once(dispatch_dropdown_clicks).unwrap();
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_bool("__dropdown_open:fruit"),
            Some(true),
            "click landing on the header's text child must open the popup"
        );
    }

    #[test]
    fn click_on_option_text_child_commits_value_and_closes() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);
        let option = world
            .spawn(DropdownOptionButton {
                value_signal: "fruit".to_string(),
                value: "mango".to_string(),
                open_signal: "__dropdown_open:fruit".to_string(),
            })
            .id();
        let text_child = world.spawn(ChildOf(option)).id();
        click(&mut world, text_child);
        world.run_system_once(dispatch_dropdown_clicks).unwrap();
        let store = world.resource::<PropertyStore>();
        assert_eq!(store.get_global_str("fruit").as_deref(), Some("mango"));
        assert_eq!(
            store.get_global_bool("__dropdown_open:fruit"),
            Some(false),
            "option child click commits AND closes"
        );
    }

    #[test]
    fn click_on_menu_item_text_child_fires_menu_clicked() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        world.init_resource::<Messages<MenuClicked>>();
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__menu_open:actions", true);
        let item = world
            .spawn(MenuItemButton {
                open_signal: "__menu_open:actions".to_string(),
                item_id: "rename".to_string(),
            })
            .id();
        let text_child = world.spawn(ChildOf(item)).id();
        click(&mut world, text_child);
        world.run_system_once(dispatch_menu_item_clicks).unwrap();
        let fired: Vec<MenuClicked> = world
            .resource_mut::<Messages<MenuClicked>>()
            .drain()
            .collect();
        assert_eq!(fired.len(), 1, "exactly one MenuClicked");
        assert_eq!(fired[0].id, "rename");
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_bool("__menu_open:actions"),
            Some(false)
        );
    }

    #[test]
    fn click_on_tab_text_child_switches_tab() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        let tab = world
            .spawn(TabStripButton {
                signal_name: "active".to_string(),
                value: "settings".to_string(),
            })
            .id();
        let text_child = world.spawn(ChildOf(tab)).id();
        click(&mut world, text_child);
        world.run_system_once(dispatch_tab_clicks).unwrap();
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("active")
                .as_deref(),
            Some("settings")
        );
    }
}
