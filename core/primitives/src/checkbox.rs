//! `<checkbox>` behavior: box + label control on top of the shared
//! [`Toggleable`] machinery, with a tri-state `indeterminate` mode.
//!
//! The markup parser desugars `<checkbox label="...">` into a row (tag
//! `checkbox`, carrying [`Toggleable`] + [`CheckboxStyle`]) with two
//! real child elements - a `.checkbox-box` tile (carrying
//! [`CheckboxBox`]) and a `.checkbox-label` label - so every visual is
//! CSS-reachable through the skins:
//!
//! ```css
//! .checkbox-box     { width: 18; height: 18; bg: ...; border: ...; }
//! checkbox:checked  { bg: var(--lumen-accent); }  /* box fill when on */
//! checkbox:focus    { outline: 2 var(--lumen-accent); }
//! ```
//!
//! Behavior reuse:
//! - Click anywhere on the row (box, label, gap) toggles -
//!   [`crate::controls::flip_toggle_on_click`] resolves child hits to
//!   the ancestor [`Toggleable`], exactly like `<toggle>`.
//! - Space on the focused checkbox toggles -
//!   `lumen_input::activate_focused_on_enter`'s press-and-release FSM
//!   emits the same `ClickEvent`.
//! - `bind-checked` / `on_toggle(id, checked)` come with `Toggleable`.
//!
//! Tri-state: `indeterminate="true"` inserts the [`Indeterminate`]
//! marker. The box renders a dash while it's present; the first user
//! toggle (click / Space - anything that fires
//! [`crate::controls::ToggleChanged`]) clears it, mirroring the web's
//! `indeterminate` IDL attribute and Qt's `PartiallyChecked`. Script
//! `bind-checked` writes do not clear it.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::{Color, Fill, TextContent, Visuals};
use lumen_core::prelude::*;

use crate::controls::ToggleChanged;

/// Glyph shown in the box while checked.
const MARK_CHECKED: &str = "\u{2713}"; // check mark
/// Glyph shown in the box while indeterminate.
const MARK_INDETERMINATE: &str = "\u{2013}"; // en dash

/// Per-checkbox fill for the checked / indeterminate box state,
/// resolved at spawn from `checkbox:checked { bg: ... }` (routed through
/// `Attributes::checked_bg`) with the accent fallback.
#[derive(Component, Clone, Copy, Debug)]
pub struct CheckboxStyle {
    /// Box fill while checked or indeterminate.
    pub checked_bg: Color,
}

impl Default for CheckboxStyle {
    fn default() -> Self {
        Self {
            checked_bg: crate::controls::TOGGLE_CHECKED_BG,
        }
    }
}

/// Marker on the `.checkbox-box` child tile.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CheckboxBox;

/// Tri-state marker on the checkbox ROOT: renders a dash regardless of
/// `checked` until the first user toggle removes it.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Indeterminate;

/// Resting box fill captured on first sync, restored when the box
/// leaves the checked / indeterminate state - the resting fill comes
/// from CSS (`.checkbox-box { bg: ... }`), which the runtime can only
/// observe after spawn.
#[derive(Component, Clone, Copy, Debug)]
pub struct CheckboxBaseFill(pub Option<Color>);

/// Plugin: registers the visual sync + indeterminate-clear systems.
/// The toggle behavior itself ships with [`crate::ControlsPlugin`].
pub struct CheckboxPlugin;

impl Plugin for CheckboxPlugin {
    fn build(self, app: &mut App) {
        // Clear runs after the click consumer that emits ToggleChanged
        // so a same-tick user toggle clears the dash immediately; the
        // visual sync runs last.
        app.add_systems(
            TickStage::Systems,
            clear_indeterminate_on_user_toggle.after(crate::controls::flip_toggle_on_click),
        );
        app.add_systems(
            TickStage::Systems,
            sync_checkbox_visuals.after(clear_indeterminate_on_user_toggle),
        );
    }
}

/// First user toggle clears [`Indeterminate`] (web/Qt tri-state
/// contract). Listens to [`ToggleChanged`] - fired only by the user
/// input path, not by `bind-checked` signal pulls.
pub fn clear_indeterminate_on_user_toggle(
    mut commands: Commands,
    mut changes: MessageReader<ToggleChanged>,
    indeterminate: Query<(), With<Indeterminate>>,
) {
    for ev in changes.read() {
        if indeterminate.contains(ev.entity) {
            commands.entity(ev.entity).remove::<Indeterminate>();
        }
    }
}

/// Keep every `.checkbox-box` child in step with its parent state:
///
/// - fill: `checked_bg` while checked OR indeterminate, the captured
///   resting fill otherwise;
/// - mark glyph: `MARK_CHECKED` checked, `MARK_INDETERMINATE` indeterminate,
///   empty otherwise.
///
/// Runs every tick (marker components move without a single Changed
/// filter to hang off); writes only on actual diffs so change
/// detection stays quiet on idle frames.
#[allow(clippy::type_complexity)]
pub fn sync_checkbox_visuals(
    mut commands: Commands,
    parents_q: Query<(&Toggleable, Option<&Indeterminate>, &CheckboxStyle)>,
    mut boxes: Query<
        (
            Entity,
            &ChildOf,
            &mut Visuals,
            Option<&mut TextContent>,
            Option<&CheckboxBaseFill>,
        ),
        With<CheckboxBox>,
    >,
) {
    for (box_e, child_of, mut vis, text, base) in &mut boxes {
        let Ok((t, indeterminate, style)) = parents_q.get(child_of.parent()) else {
            continue;
        };
        let indeterminate = indeterminate.is_some();
        // Capture the resting (CSS-authored) fill once, before the first
        // swap can overwrite it (see [`capture_baseline`]); correct even
        // for a checkbox spawned already-checked.
        let base_fill = crate::baseline::capture_baseline(
            &mut commands,
            box_e,
            base,
            vis.fill.as_ref().and_then(Fill::as_solid),
            |b| b.0,
            CheckboxBaseFill,
        );
        let want_fill = if t.checked || indeterminate {
            Some(style.checked_bg)
        } else {
            base_fill
        };
        if vis.fill.as_ref().and_then(Fill::as_solid) != want_fill {
            vis.fill = want_fill.map(Fill::Solid);
        }
        let mark = if indeterminate {
            MARK_INDETERMINATE
        } else if t.checked {
            MARK_CHECKED
        } else {
            ""
        };
        match text {
            Some(mut tc) => {
                if tc.0 != mark {
                    tc.0 = mark.to_string();
                    commands.entity(box_e).insert(DirtyLayout);
                }
            }
            None => {
                if !mark.is_empty() {
                    commands
                        .entity(box_e)
                        .insert((TextContent(mark.to_string()), DirtyLayout));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    fn spawn_checkbox(world: &mut World, checked: bool, indeterminate: bool) -> (Entity, Entity) {
        let root = world
            .spawn((Toggleable { checked }, CheckboxStyle::default()))
            .id();
        if indeterminate {
            world.entity_mut(root).insert(Indeterminate);
        }
        let bx = world
            .spawn((
                CheckboxBox,
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(0.2, 0.2, 0.2))),
                    ..Default::default()
                },
                ChildOf(root),
            ))
            .id();
        (root, bx)
    }

    fn box_fill(world: &World, bx: Entity) -> Option<Color> {
        world
            .get::<Visuals>(bx)
            .and_then(|v| v.fill.as_ref().and_then(Fill::as_solid))
    }

    fn box_mark(world: &World, bx: Entity) -> String {
        world
            .get::<TextContent>(bx)
            .map(|t| t.0.clone())
            .unwrap_or_default()
    }

    #[test]
    fn checked_box_paints_accent_and_mark() {
        let mut world = World::new();
        let (_, bx) = spawn_checkbox(&mut world, true, false);
        world.run_system_once(sync_checkbox_visuals).unwrap();
        assert_eq!(
            box_fill(&world, bx),
            Some(CheckboxStyle::default().checked_bg)
        );
        assert_eq!(box_mark(&world, bx), MARK_CHECKED);
    }

    #[test]
    fn unchecked_box_keeps_resting_fill_and_no_mark() {
        let mut world = World::new();
        let (_, bx) = spawn_checkbox(&mut world, false, false);
        world.run_system_once(sync_checkbox_visuals).unwrap();
        assert_eq!(box_fill(&world, bx), Some(Color::rgb(0.2, 0.2, 0.2)));
        assert_eq!(box_mark(&world, bx), "");
    }

    #[test]
    fn indeterminate_renders_dash_even_when_unchecked() {
        let mut world = World::new();
        let (_, bx) = spawn_checkbox(&mut world, false, true);
        world.run_system_once(sync_checkbox_visuals).unwrap();
        assert_eq!(box_mark(&world, bx), MARK_INDETERMINATE);
        assert_eq!(
            box_fill(&world, bx),
            Some(CheckboxStyle::default().checked_bg),
            "indeterminate paints the checked fill behind the dash"
        );
    }

    #[test]
    fn toggle_round_trip_restores_resting_fill() {
        let mut world = World::new();
        let (root, bx) = spawn_checkbox(&mut world, false, false);
        world.run_system_once(sync_checkbox_visuals).unwrap();
        world.get_mut::<Toggleable>(root).unwrap().checked = true;
        world.run_system_once(sync_checkbox_visuals).unwrap();
        assert_eq!(box_mark(&world, bx), MARK_CHECKED);
        world.get_mut::<Toggleable>(root).unwrap().checked = false;
        world.run_system_once(sync_checkbox_visuals).unwrap();
        assert_eq!(
            box_fill(&world, bx),
            Some(Color::rgb(0.2, 0.2, 0.2)),
            "resting fill captured before the first swap is restored"
        );
        assert_eq!(box_mark(&world, bx), "");
    }

    #[test]
    fn user_toggle_clears_indeterminate() {
        let mut world = World::new();
        world.init_resource::<Messages<ToggleChanged>>();
        let (root, _) = spawn_checkbox(&mut world, false, true);
        world
            .resource_mut::<Messages<ToggleChanged>>()
            .write(ToggleChanged {
                entity: root,
                checked: true,
            });
        world
            .run_system_once(clear_indeterminate_on_user_toggle)
            .unwrap();
        assert!(
            world.get::<Indeterminate>(root).is_none(),
            "first user toggle drops the tri-state marker"
        );
    }

    #[test]
    fn signal_pull_does_not_clear_indeterminate() {
        // `apply_checked_bindings` mutates Toggleable directly without
        // emitting ToggleChanged - the dash must survive.
        let mut world = World::new();
        world.init_resource::<Messages<ToggleChanged>>();
        let (root, _) = spawn_checkbox(&mut world, false, true);
        world.get_mut::<Toggleable>(root).unwrap().checked = true;
        world
            .run_system_once(clear_indeterminate_on_user_toggle)
            .unwrap();
        assert!(
            world.get::<Indeterminate>(root).is_some(),
            "non-user writes keep the tri-state marker"
        );
    }
}
