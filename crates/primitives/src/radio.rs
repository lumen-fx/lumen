//! `<radio>` behavior: name-grouped exclusive selection with roving
//! tab index and wrapping arrow-key navigation (Qt `QRadioButton` in
//! an auto-exclusive group / WAI-ARIA radiogroup).
//!
//! The markup parser desugars `<radio group="g" value="v" label="...">`
//! into a row (tag `radio`, carrying [`RadioButton`]) with a
//! `.radio-dot` tile child ([`RadioDot`]) and a `.radio-label` label,
//! so every visual is CSS-reachable:
//!
//! ```css
//! .radio-dot     { width: 18; height: 18; radius: 9; bg: ...; border: ...; }
//! radio:selected { bg: var(--lumen-accent); }   /* dot fill when on */
//! radio:focus    { outline: 2 var(--lumen-accent); }
//! ```
//!
//! Contract:
//! - The GROUP is the set of `<radio>` elements sharing a `group`
//!   string; the group's selected value lives in the `PropertyStore`
//!   global of that name (bindable / script-readable like any signal).
//! - Exactly one selected per group: [`sync_radio_selected`] seeds the
//!   first enabled member when the signal is unset.
//! - Click (row, dot, or label) and Space select. Arrow keys move
//!   selection to the next / previous enabled member, wrapping at the
//!   ends and skipping disabled members (web radiogroup behavior;
//!   selection follows focus, as in Qt/GTK).
//! - Roving tabindex: only the selected (else first enabled) member is
//!   in the Tab chain, so Tab enters and leaves the GROUP, not each
//!   item ([`sync_radio_tab_index`]).

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::{Color, DocumentOrder, Fill, Selected, Visuals};
use lumen_core::prelude::*;
use lumen_core::property_store::PropertyStore;

/// One member of a radio group.
#[derive(Component, Clone, Debug)]
pub struct RadioButton {
    /// Group signal name (`group="..."`): the PropertyStore global that
    /// holds the group's selected value.
    pub group: String,
    /// This member's value, written to the group signal on select.
    pub value: String,
}

/// Marker on the `.radio-dot` child tile.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RadioDot;

/// Per-radio fill for the selected dot state, resolved at spawn from
/// `radio:selected { bg: ... }` (routed through
/// `Attributes::selected_bg`) with the accent fallback.
#[derive(Component, Clone, Copy, Debug)]
pub struct RadioStyle {
    /// Dot fill while this member is selected.
    pub selected_bg: Color,
}

impl Default for RadioStyle {
    fn default() -> Self {
        Self {
            selected_bg: crate::controls::TOGGLE_CHECKED_BG,
        }
    }
}

/// Resting dot fill captured on first sync (CSS-authored), restored on
/// deselect - same capture pattern as
/// [`crate::checkbox::CheckboxBaseFill`].
#[derive(Component, Clone, Copy, Debug)]
pub struct RadioBaseFill(pub Option<Color>);

/// Plugin: registers selection dispatch, marker/visual sync, roving
/// tab index, and arrow-key navigation.
pub struct RadioPlugin;

impl Plugin for RadioPlugin {
    fn build(self, app: &mut App) {
        // Click consumer after the producer so a click selects on the
        // same tick (no-op edge when the host never registers it).
        app.add_systems(
            TickStage::Systems,
            dispatch_radio_clicks.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            radio_group_keys.after(lumen_input::dispatch_focused_keys),
        );
        // Marker + visuals + roving tabindex settle after both input
        // paths so this tick's selection is reflected this tick.
        app.add_systems(
            TickStage::Systems,
            sync_radio_selected
                .after(dispatch_radio_clicks)
                .after(radio_group_keys),
        );
        app.add_systems(
            TickStage::Systems,
            sync_radio_tab_index.after(sync_radio_selected),
        );
        app.add_systems(
            TickStage::Systems,
            sync_radio_visuals.after(sync_radio_selected),
        );
    }
}

/// Resolve a click target to the radio root that owns it (the dot /
/// label children are the usual hit-test winners).
fn owning_radio(
    start: Entity,
    parents: &Query<&ChildOf>,
    radios: &Query<&RadioButton>,
) -> Option<Entity> {
    let mut cur = Some(start);
    while let Some(e) = cur {
        if radios.contains(e) {
            return Some(e);
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    None
}

/// Click / Space (via the focused-key `ClickEvent` path) on a radio -
/// or any of its children - writes the member's value to the group
/// signal AND moves focus to the radio root (Qt: clicking a radio
/// focuses it, so arrow-key navigation picks up right where the
/// pointer left off). Focus must be explicit here because the click
/// usually lands on the dot / label CHILD, which carries no
/// `TabIndex`, so `lumen_input::focus_on_click` never fires.
///
/// Disabled members are unreachable by pointer already
/// (`dispatch_clicks` filters), but the ancestor resolve could cross a
/// disabled root from an enabled child, so re-check here.
pub fn dispatch_radio_clicks(
    mut commands: Commands,
    mut clicks: MessageReader<ClickEvent>,
    mut tracker: ResMut<FocusTracker>,
    radios: Query<&RadioButton>,
    disabled: Query<(), With<lumen_core::components::Disabled>>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
) {
    for click in clicks.read() {
        let Some(root) = owning_radio(click.entity, &parents, &radios) else {
            continue;
        };
        if disabled.contains(root) {
            continue;
        }
        let Ok(radio) = radios.get(root) else {
            continue;
        };
        store.set_global_str(&radio.group, radio.value.as_str());
        if tracker.0 != Some(root) {
            if let Some(prev) = tracker.0 {
                commands
                    .entity(prev)
                    .remove::<(Focused, lumen_core::input::FocusVisible)>();
            }
            // Pointer focus never carries the keyboard-only marker.
            commands
                .entity(root)
                .insert(Focused)
                .remove::<lumen_core::input::FocusVisible>();
            tracker.0 = Some(root);
        }
    }
}

/// Mirror each group's signal onto the [`Selected`] marker, and
/// enforce exactly-one-selected: a group whose signal is unset (or
/// empty) gets its first enabled member (markup order) selected and
/// the signal seeded.
#[allow(clippy::type_complexity)]
pub fn sync_radio_selected(
    mut commands: Commands,
    mut store: ResMut<PropertyStore>,
    radios: Query<(
        Entity,
        &RadioButton,
        Option<&Selected>,
        Option<&DocumentOrder>,
        Has<lumen_core::components::Disabled>,
    )>,
) {
    // Seed pass: collect groups with no usable selection.
    let mut groups: std::collections::BTreeMap<&str, Vec<(u32, Entity, &str, bool)>> =
        std::collections::BTreeMap::new();
    for (e, r, _, doc, dis) in &radios {
        groups.entry(r.group.as_str()).or_default().push((
            doc.map(|d| d.0).unwrap_or(u32::MAX),
            e,
            r.value.as_str(),
            dis,
        ));
    }
    for (group, mut members) in groups {
        members.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let current = store.get_global_str(group);
        let value_known = current
            .as_deref()
            .map(|v| members.iter().any(|(_, _, mv, _)| *mv == v))
            .unwrap_or(false);
        if !value_known && let Some((_, _, first, _)) = members.iter().find(|(.., dis)| !dis) {
            store.set_global_str(group, *first);
        }
    }
    // Marker pass (same shape as `tabs::sync_tab_selected`).
    for (e, r, selected, ..) in &radios {
        let active = store.get_global_str(&r.group).as_deref() == Some(r.value.as_str());
        match (active, selected.is_some()) {
            (true, false) => {
                commands.entity(e).insert(Selected);
            }
            (false, true) => {
                commands.entity(e).remove::<Selected>();
            }
            _ => {}
        }
    }
}

/// Roving tabindex: exactly one member per group sits in the Tab chain:
/// the selected member, else the first enabled one. Everyone else
/// holds `TabIndex(-1)`, which `cycle_focus_on_tab` skips, so Tab
/// jumps over the group as a unit in both directions.
#[allow(clippy::type_complexity)]
pub fn sync_radio_tab_index(
    store: Res<PropertyStore>,
    mut radios: Query<(
        Entity,
        &RadioButton,
        &mut TabIndex,
        Option<&DocumentOrder>,
        Has<lumen_core::components::Disabled>,
    )>,
) {
    let mut groups: std::collections::BTreeMap<String, Vec<(u32, Entity, String, bool)>> =
        std::collections::BTreeMap::new();
    for (e, r, _, doc, dis) in &radios {
        groups.entry(r.group.clone()).or_default().push((
            doc.map(|d| d.0).unwrap_or(u32::MAX),
            e,
            r.value.clone(),
            dis,
        ));
    }
    for (group, mut members) in groups {
        members.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let current = store.get_global_str(&group);
        let holder = members
            .iter()
            .find(|(_, _, v, dis)| !dis && current.as_deref() == Some(v.as_str()))
            .or_else(|| members.iter().find(|(.., dis)| !dis))
            .map(|(_, e, ..)| *e);
        for (_, e, ..) in &members {
            if let Ok((_, _, mut ti, ..)) = radios.get_mut(*e) {
                let want = if Some(*e) == holder { 0 } else { -1 };
                if ti.0 != want {
                    ti.0 = want;
                }
            }
        }
    }
}

/// Arrow keys while a radio holds focus: move selection to the next /
/// previous enabled group member, WRAPPING at the ends (web
/// radiogroup; unlike tabs, which clamp). Selection follows focus -
/// the newly focused member is selected immediately (Qt/GTK). Also
/// moves the `Focused` + `FocusVisible` markers and the tracker.
#[allow(clippy::too_many_arguments)]
pub fn radio_group_keys(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    mut tracker: ResMut<FocusTracker>,
    mut store: ResMut<PropertyStore>,
    radios: Query<(Entity, &RadioButton)>,
    disableds: Query<(), With<lumen_core::components::Disabled>>,
    orders: Query<&DocumentOrder>,
) {
    // Drain on every early-out so stale arrow presses from a tick where
    // focus wasn't on a radio can't replay later (same discipline as
    // `tabs::dispatch_tab_keys`).
    let Some(current) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    let Ok((_, current_radio)) = radios.get(current) else {
        keys.read().for_each(drop);
        return;
    };
    let group = current_radio.group.clone();
    let mut members: Vec<(u32, Entity, String, bool)> = radios
        .iter()
        .filter(|(_, r)| r.group == group)
        .map(|(e, r)| {
            (
                orders.get(e).map(|d| d.0).unwrap_or(u32::MAX),
                e,
                r.value.clone(),
                disableds.contains(e),
            )
        })
        .collect();
    members.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let Some(current_index) = members.iter().position(|(_, e, ..)| *e == current) else {
        keys.read().for_each(drop);
        return;
    };
    // Next enabled member from `from` (exclusive) stepping `dir`,
    // wrapping - `None` only when every OTHER member is disabled.
    let step = |from: usize, dir: i64| -> Option<usize> {
        let n = members.len() as i64;
        let mut i = (from as i64 + dir).rem_euclid(n);
        while i as usize != from {
            if !members[i as usize].3 {
                return Some(i as usize);
            }
            i = (i + dir).rem_euclid(n);
        }
        None
    };
    let mut target: Option<usize> = None;
    for ev in keys.read() {
        match &ev.key {
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                if let Some(i) = step(target.unwrap_or(current_index), 1) {
                    target = Some(i);
                }
            }
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
                if let Some(i) = step(target.unwrap_or(current_index), -1) {
                    target = Some(i);
                }
            }
            _ => {}
        }
    }
    if let Some(i) = target
        && members[i].1 != current
    {
        let (_, next, value, _) = &members[i];
        commands
            .entity(current)
            .remove::<(Focused, lumen_core::input::FocusVisible)>();
        commands
            .entity(*next)
            .insert((Focused, lumen_core::input::FocusVisible));
        tracker.0 = Some(*next);
        store.set_global_str(&group, value.as_str());
    }
}

/// Keep every `.radio-dot` child in step with its parent's [`Selected`]
/// marker: `selected_bg` while selected, the captured resting fill
/// otherwise.
pub fn sync_radio_visuals(
    mut commands: Commands,
    parents_q: Query<(Has<Selected>, &RadioStyle), With<RadioButton>>,
    mut dots: Query<(Entity, &ChildOf, &mut Visuals, Option<&RadioBaseFill>), With<RadioDot>>,
) {
    for (dot_e, child_of, mut vis, base) in &mut dots {
        let Ok((selected, style)) = parents_q.get(child_of.parent()) else {
            continue;
        };
        // Capture the resting fill once, before the first swap (see
        // [`crate::baseline::capture_baseline`]).
        let base_fill = crate::baseline::capture_baseline(
            &mut commands,
            dot_e,
            base,
            vis.fill.as_ref().and_then(Fill::as_solid),
            |b| b.0,
            RadioBaseFill,
        );
        let want = if selected {
            Some(style.selected_bg)
        } else {
            base_fill
        };
        if vis.fill.as_ref().and_then(Fill::as_solid) != want {
            vis.fill = want.map(Fill::Solid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    fn radio(world: &mut World, group: &str, value: &str, order: u32) -> Entity {
        world
            .spawn((
                RadioButton {
                    group: group.into(),
                    value: value.into(),
                },
                RadioStyle::default(),
                TabIndex(-1),
                DocumentOrder(order),
            ))
            .id()
    }

    fn group_world() -> (World, [Entity; 4]) {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world.insert_resource(FocusTracker(None));
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<ClickEvent>>();
        let a = radio(&mut world, "fruit", "apple", 0);
        let b = radio(&mut world, "fruit", "banana", 1);
        let c = radio(&mut world, "fruit", "cherry", 2);
        world.entity_mut(c).insert(lumen_core::components::Disabled);
        let d = radio(&mut world, "fruit", "date", 3);
        (world, [a, b, c, d])
    }

    fn selected_value(world: &World) -> Option<String> {
        world
            .resource::<PropertyStore>()
            .get_global_str("fruit")
            .map(|s| s.to_string())
    }

    fn press(world: &mut World, key: NamedKey) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(key),
                modifiers: Modifiers::default(),
                repeat: false,
            });
    }

    fn run_keys(world: &mut World) {
        world.run_system_once(radio_group_keys).unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    #[test]
    fn unset_group_seeds_first_enabled_member() {
        let (mut world, [a, ..]) = group_world();
        world.run_system_once(sync_radio_selected).unwrap();
        assert_eq!(selected_value(&world).as_deref(), Some("apple"));
        assert!(
            world.get::<Selected>(a).is_some(),
            "marker follows the seed"
        );
    }

    #[test]
    fn exactly_one_selected_marker_per_group() {
        let (mut world, [a, b, ..]) = group_world();
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "banana");
        world.run_system_once(sync_radio_selected).unwrap();
        assert!(world.get::<Selected>(b).is_some());
        assert!(world.get::<Selected>(a).is_none());
        // Move selection: marker moves, never duplicates.
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "apple");
        world.run_system_once(sync_radio_selected).unwrap();
        assert!(world.get::<Selected>(a).is_some());
        assert!(world.get::<Selected>(b).is_none());
    }

    #[test]
    fn click_on_member_selects_its_value() {
        let (mut world, [_, b, ..]) = group_world();
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: b,
                position: glam::Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world.run_system_once(dispatch_radio_clicks).unwrap();
        assert_eq!(selected_value(&world).as_deref(), Some("banana"));
        assert_eq!(
            world.resource::<FocusTracker>().0,
            Some(b),
            "clicking a radio focuses it (Qt), so arrow nav continues from there"
        );
    }

    #[test]
    fn click_on_dot_child_resolves_to_member() {
        let (mut world, [a, ..]) = group_world();
        let dot = world.spawn((RadioDot, ChildOf(a))).id();
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: dot,
                position: glam::Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world.run_system_once(dispatch_radio_clicks).unwrap();
        assert_eq!(selected_value(&world).as_deref(), Some("apple"));
    }

    #[test]
    fn arrows_move_selection_and_skip_disabled() {
        let (mut world, [_, b, _c, d]) = group_world();
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "banana");
        world.insert_resource(FocusTracker(Some(b)));
        // banana -> (cherry disabled, skipped) -> date.
        press(&mut world, NamedKey::ArrowRight);
        run_keys(&mut world);
        assert_eq!(selected_value(&world).as_deref(), Some("date"));
        assert_eq!(
            world.resource::<FocusTracker>().0,
            Some(d),
            "selection follows focus"
        );
        assert!(world.get::<Focused>(d).is_some());
    }

    #[test]
    fn arrows_wrap_at_the_ends() {
        let (mut world, [a, _, _, d]) = group_world();
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "date");
        world.insert_resource(FocusTracker(Some(d)));
        press(&mut world, NamedKey::ArrowRight);
        run_keys(&mut world);
        assert_eq!(
            selected_value(&world).as_deref(),
            Some("apple"),
            "wraps from last to first"
        );
        assert_eq!(world.resource::<FocusTracker>().0, Some(a));
        // And back: apple <- wraps to date.
        press(&mut world, NamedKey::ArrowLeft);
        run_keys(&mut world);
        assert_eq!(selected_value(&world).as_deref(), Some("date"));
    }

    #[test]
    fn arrows_on_non_radio_focus_do_nothing() {
        let (mut world, _) = group_world();
        let other = world.spawn_empty().id();
        world.insert_resource(FocusTracker(Some(other)));
        press(&mut world, NamedKey::ArrowRight);
        run_keys(&mut world);
        assert_eq!(selected_value(&world), None);
    }

    #[test]
    fn roving_tab_index_singles_out_selected_member() {
        let (mut world, [a, b, c, d]) = group_world();
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("fruit", "banana");
        world.run_system_once(sync_radio_tab_index).unwrap();
        let ti = |world: &World, e| world.get::<TabIndex>(e).unwrap().0;
        assert_eq!(ti(&world, b), 0, "selected member is the Tab stop");
        assert_eq!(ti(&world, a), -1);
        assert_eq!(ti(&world, c), -1);
        assert_eq!(ti(&world, d), -1);
    }

    #[test]
    fn roving_tab_index_falls_back_to_first_enabled() {
        let (mut world, [a, ..]) = group_world();
        // No selection written at all.
        world.run_system_once(sync_radio_tab_index).unwrap();
        assert_eq!(world.get::<TabIndex>(a).unwrap().0, 0);
    }

    #[test]
    fn selected_dot_swaps_fill_and_restores_on_deselect() {
        let (mut world, [a, ..]) = group_world();
        let resting = Color::rgb(0.25, 0.25, 0.25);
        let dot = world
            .spawn((
                RadioDot,
                Visuals {
                    fill: Some(Fill::Solid(resting)),
                    ..Default::default()
                },
                ChildOf(a),
            ))
            .id();
        world.run_system_once(sync_radio_visuals).unwrap();
        world.entity_mut(a).insert(Selected);
        world.run_system_once(sync_radio_visuals).unwrap();
        let fill = |world: &World| {
            world
                .get::<Visuals>(dot)
                .and_then(|v| v.fill.as_ref().and_then(Fill::as_solid))
        };
        assert_eq!(fill(&world), Some(RadioStyle::default().selected_bg));
        world.entity_mut(a).remove::<Selected>();
        world.run_system_once(sync_radio_visuals).unwrap();
        assert_eq!(fill(&world), Some(resting));
    }
}
