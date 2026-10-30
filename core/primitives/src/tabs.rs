//! `<tabs>`: one `<tab>` shown at a time, picked from a strip of buttons.
//!
//! A `<tabs>` holds a `.tab-strip` row ([`TabStrip`]) and then its `<tab>`
//! children ([`Tab`]). The strip holds one `.tab-btn` button
//! ([`TabStripButton`]) per tab, in the tabs' order, captioned with each
//! tab's `label` (its `name` when it has none). The parser writes the strip
//! for markup; the scene keeps it in step with the tabs however they were
//! added, so a `<tabs>` a script or a module builds, or one whose tabs move,
//! has the same strip.
//!
//! Which tab shows is a name. With `bind-value` the bound signal holds it;
//! without, the `<tabs>` element's own `value` attribute does, which a
//! script reads and writes like any other attribute. A click
//! or a key on a strip button writes it and raises the DOM `change` event
//! on the `<tabs>`, with the name as its value. A module picks a tab through
//! the [`TabPicks`] system param. [`sync_tabs`] shows the named tab, hides the rest
//! (`display: none` held over their styles, so they keep their state), and
//! marks the shown tab's button [`Selected`]; a name that matches no tab
//! shows the first tab that is not disabled. To reorder the tabs, move the
//! `<tab>` elements: the strip follows. Every tab system runs in
//! [`TabsSet`].

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use lumen_core::components::{Disabled, Display, LumenAttributes, Style};
use lumen_core::input::{
    ClickEvent, ControlInput, FocusTracker, Key, KeyPressed, MenuClicked, NamedKey,
};
use lumen_core::prelude::*;
use lumen_core::style_overrides::{Live, StyleOverrides, StyleOwner, StyleValues};

use crate::held::hold_fill;

/// On a `<tabs>`: the tab set.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct Tabs {
    /// The signal `bind-value` names, which holds the shown tab's name.
    /// `None` keeps the name in the element's `value` attribute.
    pub bind: Option<String>,
    /// The `<tab>` showing, as [`sync_tabs`] last left it.
    pub shown: Option<Entity>,
}

/// On a `<tab>`: one tab of a `<tabs>`. Its `name` attribute is what picks
/// it and its `label` attribute what its strip button says. A hidden tab
/// holds `display: none` over its styles.
#[derive(Component, Clone, Copy, Debug, Default)]
#[require(StyleOverrides)]
pub struct Tab;

impl StyleOwner for Tab {
    const RANK: u8 = 20;
}

/// On a `<tabs>`' `.tab-strip`: the row its buttons sit in.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TabStrip;

/// On a `.tab-btn` in a tab strip: the `<tab>` it shows.
#[derive(Component, Clone, Copy, Debug)]
pub struct TabStripButton {
    /// The tab. [`Entity::PLACEHOLDER`] until the scene pairs a button the
    /// parser wrote with its tab.
    pub tab: Entity,
}

/// The system set every `<tabs>` system runs in. Order a system that reads
/// [`Tabs::shown`] or a tab's visibility after it, and one that picks a tab
/// through [`TabPicks`] the set should show this tick before it.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TabsSet;

/// A tab's name: its `name` attribute.
pub fn tab_name(attrs: Option<&LumenAttributes>) -> Option<&str> {
    attrs?.get("name").filter(|n| !n.is_empty())
}

/// What a tab's strip button says: its `label` attribute, else its name.
pub fn tab_label(attrs: Option<&LumenAttributes>) -> &str {
    attrs
        .and_then(|a| a.get("label").filter(|l| !l.is_empty()))
        .or_else(|| tab_name(attrs))
        .unwrap_or("")
}

/// Picking a tab: [`TabPicks::pick`] writes the bound signal, or the
/// `<tabs>`' `value` attribute, as a script switching tabs would, and
/// [`sync_tabs`] shows the tab. A pick through this param raises no DOM
/// event; only a pick the user makes does.
#[derive(SystemParam)]
pub struct TabPicks<'w, 's> {
    parents: Query<'w, 's, &'static ChildOf>,
    sets: Query<'w, 's, (&'static Tabs, Option<&'static mut LumenAttributes>), Without<Tab>>,
    tabs: Query<'w, 's, Option<&'static LumenAttributes>, With<Tab>>,
    store: ResMut<'w, PropertyStore>,
    commands: Commands<'w, 's>,
}

impl TabPicks<'_, '_> {
    /// Make `tab` the one its `<tabs>` shows. Returns the `<tabs>` and the
    /// tab's name when the choice changed.
    pub fn pick(&mut self, tab: Entity) -> Option<(Entity, String)> {
        let name = tab_name(self.tabs.get(tab).ok()?)?.to_string();
        let set = self.parents.get(tab).ok()?.parent();
        let (tabs, attrs) = self.sets.get_mut(set).ok()?;
        let changed = match &tabs.bind {
            Some(signal) => {
                let before = self.store.get_global_str(signal);
                self.store.set_global_str(signal, name.as_str());
                before.as_deref() != Some(name.as_str())
            }
            None => match attrs {
                Some(mut attrs) => {
                    let changed = attrs.get("value") != Some(name.as_str());
                    if changed {
                        attrs.set("value", name.clone());
                    }
                    changed
                }
                None => {
                    let mut attrs = LumenAttributes::default();
                    attrs.set("value", name.clone());
                    self.commands.entity(set).insert(attrs);
                    true
                }
            },
        };
        changed.then_some((set, name))
    }
}

/// A pick the user made: write it, and raise the DOM `change` event on the
/// `<tabs>` when it changed which tab shows.
fn user_pick(picks: &mut TabPicks, changes: &mut MessageWriter<ControlInput>, tab: Entity) {
    if let Some((set, name)) = picks.pick(tab) {
        changes.write(ControlInput {
            entity: set,
            value: name,
            committed: true,
        });
    }
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
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[require(StyleOverrides)]
pub struct TabButtonStyle {
    /// Fill shown while this button carries [`Selected`] (`:selected {
    /// bg: ... }` or [`TAB_SELECTED_BG`]).
    pub selected_bg: Color,
    /// Fill shown while unselected (author `bg` / class default, or
    /// fully transparent when neither supplied one).
    pub unselected_bg: Color,
}

impl StyleOwner for TabButtonStyle {
    const RANK: u8 = 20;
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
        // A pick from a click or a key, then the shown tab, its
        // button's `Selected` marker and, the same tick the marker moves,
        // the button's fill, so a click doesn't show a one-frame stale
        // highlight on the old tab. Whether the pick came from a click,
        // keyboard nav, or a script write, the marker follows it.
        app.add_systems(
            TickStage::Systems,
            (
                dispatch_tab_clicks.after(lumen_input::dispatch_clicks),
                sync_tabs,
                sync_tab_button_visuals,
            )
                .chain()
                .in_set(TabsSet)
                .run_if(any_with_component::<Tabs>),
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
        // Placing an open panel against the layout and the in-window menu
        // bar are a window's: `ControlsPlugin` registers them.
        //
        // A color field's `bind-value`, on every target: a page's browser
        // picks the color, and the signal still has to follow it. It runs
        // only in a document that has one, so an app without one pays a
        // condition check for it and no system run.
        app.add_systems(
            TickStage::Systems,
            crate::color::sync_color_bindings
                .run_if(any_with_component::<crate::color::ColorInput>),
        );
        // Roving-tabindex keyboard nav for the focused tab button:
        // ArrowRight / ArrowLeft cycle siblings, Home / End jump to
        // ends, Enter / Space activate (same effect as click). Reads
        // `KeyPressed` directly so it observes the raw key bus before
        // `dispatch_focused_keys` routes events through the focused
        // entity - that path strips Tab specifically but otherwise
        // preserves keys, so reading either bus works; the global bus
        // is the simpler hookup because it doesn't require us to also
        // be a TextInput.
        app.add_systems(
            TickStage::Systems,
            dispatch_tab_keys
                .before(sync_tabs)
                .in_set(TabsSet)
                .run_if(any_with_component::<Tabs>),
        );
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
/// release on the already-switched tab picks it again, which changes
/// nothing and raises no `change`.
pub fn dispatch_tab_clicks(
    mut clicks: MessageReader<ClickEvent>,
    buttons: Query<&TabStripButton>,
    newly_pressed: Query<Entity, Added<Pressed>>,
    mut picks: TabPicks,
    mut changes: MessageWriter<ControlInput>,
) {
    // Hit-shadowing (W5): the press / click can land on a hit-testable
    // CHILD of the tab button (its text child) rather than the button
    // entity itself - resolve up the ancestor chain like
    // `controls::resolve_control` does for the slider thumb.
    let targets: Vec<Entity> = newly_pressed
        .iter()
        .chain(clicks.read().map(|ev| ev.entity))
        .collect();
    for start in targets {
        if let Some(target) =
            crate::controls::resolve_control(start, &picks.parents, |e| buttons.contains(e))
            && let Ok(btn) = buttons.get(target)
        {
            user_pick(&mut picks, &mut changes, btn.tab);
        }
    }
}

/// Show the tab each `<tabs>` names and hide the rest, and mark the shown
/// tab's strip button [`Selected`].
///
/// The name comes from the bound signal or the `value` attribute. A name no
/// tab has shows the first tab that is not disabled; when the signal holds
/// nothing yet it is seeded with that tab's name, and the `value` attribute
/// always follows what shows. Every write happens on a change only, so an
/// idle tick touches nothing.
#[allow(clippy::type_complexity)]
pub fn sync_tabs(
    mut sets: Query<
        (
            Entity,
            &mut Tabs,
            Option<&mut LumenAttributes>,
            Option<&Children>,
        ),
        Without<Tab>,
    >,
    tabs: Query<(Option<&LumenAttributes>, Has<Disabled>, Option<&Visible>), With<Tab>>,
    mut tab_styles: Query<(&mut Style, &mut StyleOverrides), With<Tab>>,
    buttons: Query<(Entity, &TabStripButton, Has<Selected>)>,
    mut store: ResMut<PropertyStore>,
    mut commands: Commands,
) {
    let mut shown_tabs: Vec<Entity> = Vec::new();
    for (set, mut state, attrs, children) in &mut sets {
        let kids: Vec<Entity> = children
            .map(|c| c.iter().filter(|e| tabs.contains(*e)).collect())
            .unwrap_or_default();
        let name_of = |e: Entity| tabs.get(e).ok().and_then(|(a, ..)| tab_name(a));
        let wanted: Option<String> = match &state.bind {
            Some(signal) => store.get_global_str(signal).map(|v| v.to_string()),
            None => attrs
                .as_ref()
                .and_then(|a| a.get("value"))
                .map(str::to_string),
        };
        let shown = wanted
            .as_deref()
            .and_then(|w| kids.iter().copied().find(|&e| name_of(e) == Some(w)))
            .or_else(|| {
                kids.iter()
                    .copied()
                    .find(|&e| tabs.get(e).is_ok_and(|(_, disabled, _)| !disabled))
            })
            .or_else(|| kids.first().copied());
        if let Some(name) = shown.and_then(name_of) {
            match &state.bind {
                Some(signal) if wanted.is_none() => {
                    store.set_global_str(signal, name);
                }
                Some(_) => {}
                None => match attrs {
                    Some(mut attrs) if attrs.get("value") != Some(name) => {
                        attrs.set("value", name.to_string());
                    }
                    Some(_) => {}
                    None => {
                        let mut attrs = LumenAttributes::default();
                        attrs.set("value", name.to_string());
                        commands.entity(set).insert(attrs);
                    }
                },
            }
        }
        for &kid in &kids {
            let visible = Some(kid) == shown;
            let Ok((_, _, current)) = tabs.get(kid) else {
                continue;
            };
            if current.map(|v| v.0) == Some(visible) {
                continue;
            }
            commands.entity(kid).insert(Visible(visible));
            if let Ok((mut style, mut overrides)) = tab_styles.get_mut(kid) {
                let out = if visible {
                    overrides.release::<Tab>()
                } else {
                    overrides.hold::<Tab>(
                        StyleValues {
                            display: Some(Display::None),
                            ..StyleValues::default()
                        },
                        &Live {
                            style: Some(&style),
                            ..Live::default()
                        },
                    )
                };
                out.write_style(&mut style);
            }
        }
        if state.shown != shown {
            state.shown = shown;
        }
        shown_tabs.extend(shown);
    }
    for (button, btn, selected) in &buttons {
        match (shown_tabs.contains(&btn.tab), selected) {
            (true, false) => {
                commands.entity(button).insert(Selected);
            }
            (false, true) => {
                commands.entity(button).remove::<Selected>();
            }
            _ => {}
        }
    }
}

/// Keep a tab-strip button's visuals in step with whether it currently
/// carries [`Selected`]: swap [`Visuals::fill`] between
/// [`TabButtonStyle::selected_bg`] / [`TabButtonStyle::unselected_bg`].
/// The fill is held in [`StyleOverrides`] under the hover and press tints,
/// so a tint releases onto the current state colour rather than a stale
/// one - mirrors [`crate::controls::sync_track_fill`].
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
            &mut StyleOverrides,
        ),
        With<TabStripButton>,
    >,
) {
    for (style, selected, mut vis, mut overrides) in &mut buttons {
        let target = if selected.is_some() {
            style.selected_bg
        } else {
            style.unselected_bg
        };
        hold_fill::<TabButtonStyle>(&mut overrides, &mut vis, target);
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

/// Commit `value` to a dropdown's `value_signal`, raising `input` and
/// `change` on `entity` when it differs from the value held, as a `<select>`
/// does.
pub(crate) fn pick_option(
    store: &mut PropertyStore,
    changes: &mut MessageWriter<ControlInput>,
    value_signal: &str,
    value: &str,
    entity: Entity,
) {
    if store.get_global_str(value_signal).as_deref() == Some(value) {
        return;
    }
    store.set_global_str(value_signal, value);
    for committed in [false, true] {
        changes.write(ControlInput {
            entity,
            value: value.to_string(),
            committed,
        });
    }
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
    headers: Query<(Entity, &DropdownButton)>,
    options: Query<&DropdownOptionButton>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
    mut changes: MessageWriter<ControlInput>,
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
            // The events belong to the `<dropdown>` box, the header's parent.
            let dropdown = headers
                .iter()
                .find(|(_, h)| h.open_signal == opt.open_signal)
                .map(|(h, _)| parents.get(h).map_or(h, ChildOf::parent))
                .unwrap_or(target);
            pick_option(
                &mut store,
                &mut changes,
                &opt.value_signal,
                &opt.value,
                dropdown,
            );
            store.set_global_bool(&opt.open_signal, false);
            continue;
        }
        if let Some(target) =
            crate::controls::resolve_control(ev.entity, &parents, |e| headers.contains(e))
            && let Ok((_, btn)) = headers.get(target)
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

/// A `<menuitem>` element: what a script that builds an item creates. Its
/// `label`, `accel` and `id` are attributes, and the scene keeps the item's
/// text, shortcut part and [`MenuItemButton`] in step with them.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct MenuItem;

/// The part of a [`MenuItem`] that shows its keyboard shortcut, built by
/// the scene from the item's `accel` attribute.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct MenuAccel;

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

/// Keyboard navigation for tab strips, scoped to the strip the focused
/// [`TabStripButton`] sits in. Matches the `QTabBar` keyboard contract:
///
/// - `ArrowRight` / `ArrowLeft`: move focus to the next / previous
///   *enabled* button in the strip - **and select its tab immediately**
///   (Qt select-on-arrow; `QTabBar::keyPressEvent` calls
///   `setCurrentIndex` as focus moves). No wrap-around: at either end
///   the arrow is a no-op, exactly like Qt.
/// - `Home` / `End`: jump to (and select) the first / last enabled
///   button.
/// - `Enter` / `Space`: show the focused button's tab - the same effect
///   as clicking it.
/// - Disabled buttons are skipped by every movement, mirroring
///   `QTabBar`'s enabled-only traversal.
///
/// The buttons are taken in the strip's own order, which follows the
/// tabs'. Keeping to one strip means nav never bleeds into a nested or a
/// neighbouring `<tabs>`, nor into non-tab focusables like inputs or
/// buttons sitting alongside the strip.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_tab_keys(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    mut tracker: ResMut<FocusTracker>,
    buttons: Query<&TabStripButton>,
    disableds: Query<(), With<Disabled>>,
    strips: Query<&Children, With<TabStrip>>,
    mut picks: TabPicks,
    mut changes: MessageWriter<ControlInput>,
) {
    // Confirm the focused entity is a tab button before we touch any
    // keys - otherwise the nav must stay silent so non-tab focusables
    // (text inputs, generic buttons) don't have their ArrowLeft /
    // ArrowRight stolen.
    let Some(current) = tracker.0.filter(|e| buttons.contains(*e)) else {
        keys.read().for_each(drop);
        return;
    };
    let Some(siblings) = picks
        .parents
        .get(current)
        .ok()
        .and_then(|co| strips.get(co.parent()).ok())
        .map(|kids| -> Vec<(Entity, Entity, bool)> {
            kids.iter()
                .filter_map(|e| {
                    let btn = buttons.get(e).ok()?;
                    Some((e, btn.tab, disableds.contains(e)))
                })
                .collect()
        })
    else {
        keys.read().for_each(drop);
        return;
    };
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
        // Show the focused button's tab - same effect as a click.
        user_pick(&mut picks, &mut changes, siblings[current_index].1);
    }
    if let Some(idx) = focus_target
        && siblings[idx].0 != current
    {
        let (next_entity, next_tab, _) = siblings[idx];
        commands
            .entity(current)
            .remove::<(Focused, lumen_core::input::FocusVisible)>();
        // Roving-tabindex arrows are keyboard navigation - mark the new
        // holder for `:focus-visible` styling.
        commands
            .entity(next_entity)
            .insert((Focused, lumen_core::input::FocusVisible));
        tracker.0 = Some(next_entity);
        // Qt select-on-arrow: moving tab focus with the keyboard also
        // switches to that tab immediately.
        user_pick(&mut picks, &mut changes, next_tab);
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

    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    /// A `<tabs>` bound to `bind` (or keeping its own choice when `None`)
    /// with a strip and one tab per name; the third tab is disabled.
    /// Returns (world, tabs, [tab...], [button...]).
    pub(super) fn tab_world(
        bind: Option<&str>,
        names: &[&str],
    ) -> (World, Entity, Vec<Entity>, Vec<Entity>) {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<ControlInput>>();
        let set = world
            .spawn(Tabs {
                bind: bind.map(str::to_string),
                shown: None,
            })
            .id();
        let strip = world.spawn((TabStrip, ChildOf(set))).id();
        let mut tabs = Vec::new();
        let mut buttons = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let mut attrs = LumenAttributes::default();
            attrs.set("name", name.to_string());
            let tab = world
                .spawn((Tab, attrs, Style::default(), ChildOf(set)))
                .id();
            let button = world.spawn((TabStripButton { tab }, ChildOf(strip))).id();
            if i == 2 {
                world.entity_mut(tab).insert(Disabled);
                world.entity_mut(button).insert(Disabled);
            }
            tabs.push(tab);
            buttons.push(button);
        }
        (world, set, tabs, buttons)
    }

    fn shown(world: &World, tabs: &[Entity]) -> Vec<bool> {
        tabs.iter()
            .map(|t| world.get::<Visible>(*t).is_none_or(|v| v.0))
            .collect()
    }

    fn selected(world: &World, buttons: &[Entity]) -> Vec<bool> {
        buttons
            .iter()
            .map(|b| world.get::<Selected>(*b).is_some())
            .collect()
    }

    fn changes(world: &mut World) -> Vec<(Entity, String)> {
        world
            .resource_mut::<Messages<ControlInput>>()
            .drain()
            .map(|c| (c.entity, c.value))
            .collect()
    }

    /// A bound `<tabs>` with nothing in its signal seeds it with its first
    /// tab and shows that one; writing the signal moves what shows, the
    /// hidden tabs hold `display: none`, and the shown tab's button carries
    /// `Selected`.
    #[test]
    fn the_bound_signal_picks_the_tab_that_shows() {
        let (mut world, set, tabs, buttons) = tab_world(Some("active"), &["a", "b", "c"]);
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("active")
                .as_deref(),
            Some("a"),
            "an empty signal is seeded with the first tab"
        );
        assert_eq!(shown(&world, &tabs), [true, false, false]);
        assert_eq!(selected(&world, &buttons), [true, false, false]);
        assert_eq!(world.get::<Tabs>(set).unwrap().shown, Some(tabs[0]));
        assert_eq!(world.get::<Style>(tabs[1]).unwrap().display, Display::None);

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "b");
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(shown(&world, &tabs), [false, true, false]);
        assert_eq!(selected(&world, &buttons), [false, true, false]);
        assert_ne!(world.get::<Style>(tabs[1]).unwrap().display, Display::None);

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("active", "nope");
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(
            shown(&world, &tabs),
            [true, false, false],
            "a name no tab has shows the first"
        );
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("active")
                .as_deref(),
            Some("nope"),
            "a value the app wrote is left alone"
        );
    }

    /// Without `bind-value` the `<tabs>` keeps its choice in its `value`
    /// attribute; a module's pick moves it and raises no `change`.
    #[test]
    fn an_unbound_tabs_keeps_its_choice_in_its_value_attribute() {
        let (mut world, set, tabs, _buttons) = tab_world(None, &["a", "b"]);
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(
            world.get::<LumenAttributes>(set).unwrap().get("value"),
            Some("a")
        );
        let tab = tabs[1];
        world
            .run_system_once(move |mut picks: TabPicks| {
                picks.pick(tab);
            })
            .unwrap();
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(
            world.get::<LumenAttributes>(set).unwrap().get("value"),
            Some("b")
        );
        assert_eq!(shown(&world, &tabs), [false, true]);
        assert!(
            changes(&mut world).is_empty(),
            "only a user pick is a change"
        );

        world
            .get_mut::<LumenAttributes>(set)
            .unwrap()
            .set("value", "a");
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(shown(&world, &tabs), [true, false], "a script's write");
    }

    /// A click on a strip button picks its tab and raises one `change` on
    /// the `<tabs>`; clicking the tab that already shows raises none.
    #[test]
    fn a_click_picks_the_tab_and_raises_change() {
        let (mut world, set, tabs, buttons) = tab_world(Some("active"), &["a", "b"]);
        world.run_system_once(sync_tabs).unwrap();
        world.write_message(ClickEvent {
            entity: buttons[1],
            position: glam::Vec2::ZERO,
            button: lumen_core::input::PointerButton::Primary,
            local: None,
        });
        world.run_system_once(dispatch_tab_clicks).unwrap();
        assert_eq!(changes(&mut world), [(set, "b".to_string())]);
        world.run_system_once(sync_tabs).unwrap();
        assert_eq!(shown(&world, &tabs), [false, true]);

        world.resource_mut::<Messages<ClickEvent>>().clear();
        world.write_message(ClickEvent {
            entity: buttons[1],
            position: glam::Vec2::ZERO,
            button: lumen_core::input::PointerButton::Primary,
            local: None,
        });
        world.run_system_once(dispatch_tab_clicks).unwrap();
        assert!(changes(&mut world).is_empty());
    }

    #[test]
    fn sync_tab_button_visuals_repaints_on_selected_add_and_remove() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let style = TabButtonStyle {
            selected_bg: Color::rgb(1.0, 0.0, 0.0),
            unselected_bg: Color::rgb(0.0, 0.0, 1.0),
        };
        let strip = |_: &str| {
            (
                TabStripButton {
                    tab: Entity::PLACEHOLDER,
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

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_tab_button_visuals);

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

        // `general` becomes active: its fill swaps to the selected color;
        // `advanced` stays unselected.
        world.entity_mut(general).insert(Selected);
        schedule.run(&mut world);
        assert_eq!(
            fill_of(&world, general),
            style.selected_bg,
            "selected tab repaints to selected_bg"
        );
        assert_eq!(fill_of(&world, advanced), style.unselected_bg);
        // A hover tint over it fades from, and lets go onto, the selected
        // fill.
        let live_vis = world.get::<Visuals>(general).unwrap().clone();
        assert_eq!(
            world
                .get::<StyleOverrides>(general)
                .unwrap()
                .below::<crate::hover::HoverTween>(
                    lumen_core::style_overrides::StyleFields::FILL,
                    &lumen_core::style_overrides::Live {
                        visuals: Some(&live_vis),
                        ..Default::default()
                    },
                )
                .fill,
            Some(Some(Fill::Solid(style.selected_bg))),
        );

        // Activation moves to `advanced`: `general` must revert.
        world.entity_mut(general).remove::<Selected>();
        world.entity_mut(advanced).insert(Selected);
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
        use lumen_core::input::{FocusTracker, Modifiers};

        /// Strip with four tabs (c disabled), `a` shown and its button
        /// focused. Returns (world, [button a, b, c, d]).
        fn strip_world() -> (World, [Entity; 4]) {
            let (mut world, _set, _tabs, buttons) =
                super::tab_world(Some("active"), &["a", "b", "c", "d"]);
            world
                .resource_mut::<PropertyStore>()
                .set_global_str("active", "a");
            world.insert_resource(FocusTracker(Some(buttons[0])));
            world.entity_mut(buttons[0]).insert(Focused);
            (world, [buttons[0], buttons[1], buttons[2], buttons[3]])
        }

        fn press_key(world: &mut World, key: NamedKey) {
            world
                .resource_mut::<Messages<KeyPressed>>()
                .write(KeyPressed {
                    key: Key::Named(key),
                    code: lumen_core::input::KeyCode::Unidentified,
                    modifiers: Modifiers::default(),
                    repeat: false,
                    time_stamp: lumen_core::time::Instant::now(),
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
                local: None,
            });
    }

    #[test]
    fn click_on_header_text_child_opens_dropdown() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        world.init_resource::<Messages<ControlInput>>();
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
        world.init_resource::<Messages<ControlInput>>();
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

    /// Picking a new option raises `input` then `change` on the `<dropdown>`
    /// box, the header's parent; picking the one already held raises none.
    #[test]
    fn picking_an_option_raises_input_and_change_on_the_dropdown() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.init_resource::<Messages<ClickEvent>>();
        world.init_resource::<Messages<ControlInput>>();
        let open = "__dropdown_open:fruit";
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "apple");
        let dropdown = world.spawn_empty().id();
        let mut header = DropdownButton::new(open);
        header.value_signal = "fruit".to_string();
        world.spawn((header, ChildOf(dropdown)));
        let option = |world: &mut World, value: &str| {
            world
                .spawn((
                    DropdownOptionButton {
                        value_signal: "fruit".to_string(),
                        value: value.to_string(),
                        open_signal: open.to_string(),
                    },
                    ChildOf(dropdown),
                ))
                .id()
        };
        let mango = option(&mut world, "mango");
        let apple = option(&mut world, "apple");
        let raised = |world: &mut World| -> Vec<(Entity, String, bool)> {
            let out = world
                .resource::<Messages<ControlInput>>()
                .iter_current_update_messages()
                .map(|c| (c.entity, c.value.clone(), c.committed))
                .collect();
            world.resource_mut::<Messages<ControlInput>>().clear();
            out
        };

        click(&mut world, mango);
        world.run_system_once(dispatch_dropdown_clicks).unwrap();
        assert_eq!(
            raised(&mut world),
            [
                (dropdown, "mango".to_string(), false),
                (dropdown, "mango".to_string(), true)
            ]
        );
        let mango_again = option(&mut world, "mango");
        click(&mut world, mango_again);
        world.run_system_once(dispatch_dropdown_clicks).unwrap();
        assert!(raised(&mut world).is_empty(), "same value, no events");
        click(&mut world, apple);
        world.run_system_once(dispatch_dropdown_clicks).unwrap();
        assert_eq!(raised(&mut world).len(), 2);
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
        let (mut world, _set, _tabs, buttons) =
            super::tests::tab_world(Some("active"), &["a", "b"]);
        let text_child = world.spawn(ChildOf(buttons[1])).id();
        click(&mut world, text_child);
        world.run_system_once(dispatch_tab_clicks).unwrap();
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("active")
                .as_deref(),
            Some("b")
        );
    }
}
