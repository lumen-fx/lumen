//! State-driven text-color / opacity / box-shadow / background swaps.
//!
//! The CSS parser routes `:hover` / `:focus` / `:active` / `:disabled`
//! declarations for `text-color`, `opacity`, and `box-shadow` (plus
//! `bg` for `:disabled`) into a [`StateVisuals`] component (one
//! [`StatePatch`] per state); this module's [`apply_state_visuals`]
//! system swaps the live [`TextStyle`] / [`Opacity`] /
//! [`Visuals::shadows`] / [`Visuals::fill`] values as the interaction
//! markers come and go, restoring the captured idle baseline when every
//! state ends. Swaps snap (no tween), matching CSS state rules without
//! a `transition` - exactly like the border swap in
//! [`crate::hover::apply_state_borders`].
//!
//! `:disabled` reacts to the
//! [`Disabled`](lumen_core::components::Disabled) marker at runtime
//! (Wave 3: `bind-disabled` can add / remove it live). Statically
//! disabled markup without a binding still takes the spawn-time fast
//! path in `lumenc::spawn`; the patch here drives every dynamic case.
//! [`eject_interaction_on_disable`] strips `Hovered` / `Pressed` /
//! focus the moment an entity becomes disabled (moving keyboard focus
//! to the next focusable) so no interaction state lingers on a
//! greyed-out widget.

use bevy_ecs::prelude::*;
use lumen_core::components::{
    Color, Disabled, DocumentOrder, DropHovered, Fill, Opacity, ShadowSpec, TextStyle, Visuals,
};
use lumen_core::input::FocusVisible;
use lumen_core::prelude::*;

/// One state's property patch. Only authored fields swap; the rest keep
/// the idle value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatePatch {
    /// `:state { text-color: ... }`.
    pub text_color: Option<Color>,
    /// `:state { opacity: ... }`.
    pub opacity: Option<f32>,
    /// `:state { box-shadow: ... }` (full stack replacement).
    pub shadows: Option<Vec<ShadowSpec>>,
    /// `:state { bg: ... }` (solid fill replacement). Routed for
    /// `:disabled` today - hover / press backgrounds keep going through
    /// the tweened [`crate::hover::Interaction`] path.
    pub bg: Option<Color>,
}

impl StatePatch {
    /// `true` when no field is authored.
    pub fn is_empty(&self) -> bool {
        self.text_color.is_none()
            && self.opacity.is_none()
            && self.shadows.is_none()
            && self.bg.is_none()
    }
}

/// Author-set per-state style patches. Pressed wins over focused wins
/// over hovered on a per-field basis (mirrors the cascade order skins
/// author these rules in); disabled stamps over everything.
#[derive(Component, Clone, Debug, Default)]
pub struct StateVisuals {
    /// `:hover` patch.
    pub hover: StatePatch,
    /// `:focus` patch (any focus source).
    pub focus: StatePatch,
    /// `:focus-visible` patch - applies only while the `FocusVisible`
    /// marker is present (keyboard-driven focus). Wins over
    /// [`Self::focus`] per field.
    pub focus_visible: StatePatch,
    /// `:active` patch.
    pub active: StatePatch,
    /// `:drag-over` patch - applies while the entity carries the
    /// [`DropHovered`] marker (an acceptable in-app drag is hovering this
    /// drop target). HTML5 `dragover` parity.
    pub drag_over: StatePatch,
    /// `:disabled` patch - applies while the entity carries the
    /// [`Disabled`] marker. Wins over every other state (a disabled
    /// widget has no live interaction states anyway; see
    /// [`eject_interaction_on_disable`]).
    pub disabled: StatePatch,
}

impl StateVisuals {
    /// `true` when every state patch is empty (component not needed).
    pub fn is_empty(&self) -> bool {
        self.hover.is_empty()
            && self.focus.is_empty()
            && self.focus_visible.is_empty()
            && self.active.is_empty()
            && self.drag_over.is_empty()
            && self.disabled.is_empty()
    }

    /// `true` when any patch authors a background - only then does
    /// [`apply_state_visuals`] manage [`Visuals::fill`] (so it never
    /// fights the tweened hover / press tint path on entities that
    /// don't swap backgrounds by state).
    pub fn manages_fill(&self) -> bool {
        self.hover.bg.is_some()
            || self.focus.bg.is_some()
            || self.focus_visible.bg.is_some()
            || self.active.bg.is_some()
            || self.drag_over.bg.is_some()
            || self.disabled.bg.is_some()
    }
}

/// Idle baseline captured before the first swap so every field can be
/// restored exactly when all interaction states end.
#[derive(Component, Clone, Debug)]
pub struct StateStyleBase {
    /// Idle `TextStyle.color` (when the entity has a `TextStyle`).
    pub text_color: Option<Color>,
    /// Idle `Opacity` value; `None` = the component was absent.
    pub opacity: Option<f32>,
    /// Idle `Visuals.shadows` stack.
    pub shadows: Vec<ShadowSpec>,
    /// Idle `Visuals.fill`; only restored when the entity's
    /// [`StateVisuals::manages_fill`].
    pub fill: Option<Fill>,
}

/// Plugin: registers [`eject_interaction_on_disable`] then
/// [`apply_state_visuals`].
pub struct StateStylePlugin;

impl Plugin for StateStylePlugin {
    fn build(self, app: &mut App) {
        // Ejection runs first (with a sync point in between) so the
        // baseline `apply_state_visuals` captures on the disable tick is
        // the true idle style, not a half-faded hover tint.
        app.add_systems(
            TickStage::Systems,
            eject_interaction_on_disable.before(apply_state_visuals),
        );
        // `.after(dispatch_clicks)` (=> after `hit_test` too): flush the
        // Hovered / Pressed marker commands before the swap so `:hover`
        // / `:active` / `:disabled` styling reflects this tick's input.
        // No-op when `lumen-input` isn't installed.
        app.add_systems(
            TickStage::Systems,
            apply_state_visuals.after(lumen_input::dispatch_clicks),
        );
    }
}

/// Swap text color / opacity / shadows / background per interaction
/// state and restore the captured baseline when idle.
#[allow(clippy::type_complexity)]
pub fn apply_state_visuals(
    mut commands: Commands,
    pointer: Option<Res<PointerState>>,
    mut active: Query<
        (
            Entity,
            &StateVisuals,
            Option<&mut TextStyle>,
            Option<&mut Visuals>,
            Option<&Opacity>,
            Option<&StateStyleBase>,
            Option<&Hovered>,
            Option<&Focused>,
            Option<&lumen_core::input::FocusVisible>,
            Option<&Pressed>,
            Has<Disabled>,
            Has<DropHovered>,
        ),
        Or<(
            With<Hovered>,
            With<Focused>,
            With<Pressed>,
            With<Disabled>,
            With<DropHovered>,
        )>,
    >,
    mut idle: Query<
        (
            Entity,
            &StateVisuals,
            &StateStyleBase,
            Option<&mut TextStyle>,
            Option<&mut Visuals>,
            Option<&Opacity>,
        ),
        (
            Without<Hovered>,
            Without<Focused>,
            Without<Pressed>,
            Without<Disabled>,
            Without<DropHovered>,
        ),
    >,
) {
    let primary_down = pointer.map(|p| p.primary_down).unwrap_or(false);
    for (
        entity,
        sv,
        text,
        vis,
        opacity,
        base,
        hovered,
        focused,
        focus_visible,
        pressed,
        disabled,
        drag_over,
    ) in &mut active
    {
        // `:active` mirrors the press-tint contract (spec section 0 rule 3):
        // a pointer press dragged off the captured widget presents
        // un-pressed even though `Pressed` (the capture marker) stays.
        // Keyboard presses (pointer up) present pressed without hover.
        let active_on = pressed.is_some() && (hovered.is_some() || !primary_down);
        // Field-wise merge: hover < focus < focus-visible < active <
        // disabled (disabled stamps last).
        let mut patch = StatePatch::default();
        for (marker_on, p) in [
            (hovered.is_some(), &sv.hover),
            (focused.is_some(), &sv.focus),
            (
                focused.is_some() && focus_visible.is_some(),
                &sv.focus_visible,
            ),
            (active_on, &sv.active),
            (drag_over, &sv.drag_over),
            (disabled, &sv.disabled),
        ] {
            if !marker_on {
                continue;
            }
            if p.text_color.is_some() {
                patch.text_color = p.text_color;
            }
            if p.opacity.is_some() {
                patch.opacity = p.opacity;
            }
            if p.shadows.is_some() {
                patch.shadows = p.shadows.clone();
            }
            if p.bg.is_some() {
                patch.bg = p.bg;
            }
        }
        if patch.is_empty() && base.is_none() {
            // Nothing to apply and nothing previously applied (an active
            // marker whose patches are all authored for other states -
            // e.g. `Disabled` present but only `:hover` styled).
            continue;
        }
        // Capture the idle baseline once, before the first swap.
        let base = match base {
            Some(b) => b.clone(),
            None => {
                let b = StateStyleBase {
                    text_color: text.as_ref().map(|t| t.color),
                    opacity: opacity.map(|o| o.0),
                    shadows: vis.as_ref().map(|v| v.shadows.clone()).unwrap_or_default(),
                    fill: vis.as_ref().and_then(|v| v.fill.clone()),
                };
                commands.entity(entity).insert(b.clone());
                b
            }
        };
        if let Some(mut t) = text {
            let want = patch.text_color.or(base.text_color).unwrap_or(t.color);
            if t.color != want {
                t.color = want;
            }
        }
        match patch.opacity.or(base.opacity) {
            Some(want) => {
                if opacity.map(|o| o.0) != Some(want) {
                    commands.entity(entity).insert(Opacity(want));
                }
            }
            None => {
                if opacity.is_some() {
                    commands.entity(entity).remove::<Opacity>();
                }
            }
        }
        if let Some(mut v) = vis {
            let want = patch.shadows.unwrap_or_else(|| base.shadows.clone());
            if v.shadows != want {
                v.shadows = want;
            }
            if sv.manages_fill() {
                let want = patch.bg.map(Fill::Solid).or_else(|| base.fill.clone());
                if v.fill != want {
                    v.fill = want;
                }
            }
        }
    }
    for (entity, sv, base, text, vis, opacity) in &mut idle {
        if let (Some(mut t), Some(idle_color)) = (text, base.text_color)
            && t.color != idle_color
        {
            t.color = idle_color;
        }
        match base.opacity {
            Some(v) => {
                if opacity.map(|o| o.0) != Some(v) {
                    commands.entity(entity).insert(Opacity(v));
                }
            }
            None => {
                if opacity.is_some() {
                    commands.entity(entity).remove::<Opacity>();
                }
            }
        }
        if let Some(mut v) = vis {
            if v.shadows != base.shadows {
                v.shadows = base.shadows.clone();
            }
            if sv.manages_fill() && v.fill != base.fill {
                v.fill = base.fill.clone();
            }
        }
        commands.entity(entity).remove::<StateStyleBase>();
    }
}

/// The moment an entity gains [`Disabled`] (spec section 0: runtime
/// enable/disable via `bind-disabled`), strip every live interaction
/// state from it:
///
/// * `Hovered` / `Pressed` are removed (with their transient tween
///   components) - the hover tint / press tint end immediately, and a
///   pending release will not click (`dispatch_clicks` requires
///   `Pressed`). Fills mid-tween are pinned back to their captured base
///   color so the baseline the `:disabled` swap captures is the true
///   idle style.
/// * Keyboard focus moves to the next focusable in tab order (same
///   `(TabIndex, DocumentOrder)` ordering `cycle_focus_on_tab` uses,
///   wrapping; hidden and disabled entities skipped), or clears when
///   the disabled entity was the only focusable. The `FocusVisible`
///   marker travels with the focus if it was present.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn eject_interaction_on_disable(
    mut commands: Commands,
    newly_disabled: Query<Entity, Added<Disabled>>,
    mut tracker: ResMut<FocusTracker>,
    mut visuals: Query<&mut Visuals>,
    hover_bases: Query<&crate::hover::HoverBaseColor>,
    press_bases: Query<&crate::hover::PressBaseColor>,
    focus_visibles: Query<(), With<FocusVisible>>,
    focusables: Query<(Entity, &TabIndex, Option<&DocumentOrder>), Without<Disabled>>,
    tab_info: Query<(&TabIndex, Option<&DocumentOrder>)>,
    parents: Query<&ChildOf>,
    visibles: Query<&lumen_core::components::Visible>,
    styles: Query<&lumen_core::components::Style>,
) {
    for entity in &newly_disabled {
        // Pin a mid-tween fill back to its captured base before the
        // tween components disappear.
        let base = hover_bases
            .get(entity)
            .map(|b| b.0)
            .or_else(|_| press_bases.get(entity).map(|b| b.0));
        if let (Ok(mut vis), Ok(base)) = (visuals.get_mut(entity), base)
            && let Some(Fill::Solid(slot)) = vis.fill.as_mut()
            && *slot != base
        {
            *slot = base;
        }
        commands.entity(entity).remove::<(
            Hovered,
            Pressed,
            Focused,
            FocusVisible,
            crate::hover::HoverBaseColor,
            crate::hover::HoverTween,
            crate::hover::PressBaseColor,
            crate::hover::PressTween,
        )>();

        if tracker.0 != Some(entity) {
            continue;
        }
        // Focus ejection: pick the next focusable after the disabled
        // entity in (TabIndex, DocumentOrder, Entity) order, wrapping.
        let mut sorted: Vec<(i32, u32, Entity)> = focusables
            .iter()
            .filter(|(e, t, _)| t.0 >= 0 && !hidden_via_ancestors(*e, &parents, &visibles, &styles))
            .map(|(e, t, doc)| (t.0, doc.map(|d| d.0).unwrap_or(u32::MAX), e))
            .collect();
        sorted.sort();
        let next = if sorted.is_empty() {
            None
        } else {
            let own_key = tab_info
                .get(entity)
                .map(|(t, doc)| (t.0, doc.map(|d| d.0).unwrap_or(u32::MAX), entity))
                .unwrap_or((i32::MIN, 0, entity));
            sorted
                .iter()
                .find(|k| **k > own_key)
                .or_else(|| sorted.first())
                .copied()
        };
        let had_visible = focus_visibles.contains(entity);
        match next {
            Some((_, _, next_e)) => {
                if had_visible {
                    commands.entity(next_e).insert((Focused, FocusVisible));
                } else {
                    commands.entity(next_e).insert(Focused);
                }
                tracker.0 = Some(next_e);
            }
            None => {
                tracker.0 = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(blur: f32) -> ShadowSpec {
        ShadowSpec {
            offset_x: 0.0,
            offset_y: 1.0,
            blur,
            spread: 0.0,
            color: Color::rgb(0.0, 0.0, 0.0),
            inner: false,
        }
    }

    fn run(world: &mut World) {
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(apply_state_visuals);
        schedule.run(world);
        // Two passes: the first tick captures the base + queues the
        // command-applied writes.
        let mut schedule2 = bevy_ecs::schedule::Schedule::default();
        schedule2.add_systems(apply_state_visuals);
        schedule2.run(world);
    }

    /// Hover swaps text color + shadows in; un-hover restores; pressed
    /// wins over hover per field.
    #[test]
    fn state_text_color_opacity_shadow_swap_and_restore() {
        let mut world = World::new();
        let red = Color::rgb(1.0, 0.0, 0.0);
        let green = Color::rgb(0.0, 1.0, 0.0);
        let idle = Color::rgb(0.5, 0.5, 0.5);
        let e = world
            .spawn((
                StateVisuals {
                    hover: StatePatch {
                        text_color: Some(red),
                        opacity: Some(0.8),
                        shadows: Some(vec![shadow(4.0)]),
                        ..Default::default()
                    },
                    active: StatePatch {
                        text_color: Some(green),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                TextStyle {
                    color: idle,
                    ..Default::default()
                },
                Visuals::default(),
            ))
            .id();

        // Hover -> patch applied.
        world.entity_mut(e).insert(Hovered);
        run(&mut world);
        assert_eq!(world.get::<TextStyle>(e).unwrap().color, red);
        assert_eq!(world.get::<Opacity>(e).map(|o| o.0), Some(0.8));
        assert_eq!(world.get::<Visuals>(e).unwrap().shadows, vec![shadow(4.0)]);

        // Hover + press -> active text color wins; hover opacity stays.
        world.entity_mut(e).insert(Pressed);
        run(&mut world);
        assert_eq!(world.get::<TextStyle>(e).unwrap().color, green);
        assert_eq!(world.get::<Opacity>(e).map(|o| o.0), Some(0.8));

        // All states end -> baseline restored, capture removed.
        world.entity_mut(e).remove::<Hovered>();
        world.entity_mut(e).remove::<Pressed>();
        run(&mut world);
        assert_eq!(world.get::<TextStyle>(e).unwrap().color, idle);
        assert!(world.get::<Opacity>(e).is_none(), "idle Opacity was absent");
        assert!(world.get::<Visuals>(e).unwrap().shadows.is_empty());
        assert!(world.get::<StateStyleBase>(e).is_none());
    }

    /// Focus ring: an `input:focus { box-shadow: ... }` rule (routed into
    /// `StateVisuals.focus.shadows`) swaps a real ring in when the input
    /// gains `Focused` and restores the idle stack on blur. The ring
    /// color/width comes entirely from the authored CSS shadow - no
    /// hardcoded Rust color - so a skin can drive it from a
    /// `--lumen-focus-ring` token. This is deliverable J's "focus ring
    /// driven by `:focus` tokens, not a hardcoded color".
    #[test]
    fn focus_ring_box_shadow_swaps_on_focus_and_restores_on_blur() {
        let mut world = World::new();
        let ring = ShadowSpec {
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 0.0,
            spread: 2.0,
            // Token-sourced color - the CSS parser fills this from
            // `var(--lumen-focus-ring)`; the test just proves the swap.
            color: Color::rgba(0.2, 0.5, 1.0, 0.9),
            inner: false,
        };
        let e = world
            .spawn((
                StateVisuals {
                    focus: StatePatch {
                        shadows: Some(vec![ring]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                TextInput::default(),
                Visuals::default(),
            ))
            .id();

        // Focus -> ring shadow applied.
        world.entity_mut(e).insert(Focused);
        run(&mut world);
        assert_eq!(
            world.get::<Visuals>(e).unwrap().shadows,
            vec![ring],
            ":focus ring must swap in on Focused"
        );

        // Blur -> idle (empty) shadow stack restored.
        world.entity_mut(e).remove::<Focused>();
        run(&mut world);
        assert!(
            world.get::<Visuals>(e).unwrap().shadows.is_empty(),
            "focus ring must clear on blur"
        );
    }

    /// `:disabled` swaps bg + opacity in when the `Disabled` marker
    /// lands at runtime, and restores the exact idle values when the
    /// marker is removed (bind-disabled re-enable).
    #[test]
    fn disabled_patch_swaps_on_marker_add_and_restores_on_remove() {
        let mut world = World::new();
        let idle_bg = Color::rgb(0.1, 0.4, 0.9);
        let disabled_bg = Color::rgb(0.3, 0.3, 0.3);
        let e = world
            .spawn((
                StateVisuals {
                    disabled: StatePatch {
                        opacity: Some(0.5),
                        bg: Some(disabled_bg),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Visuals {
                    fill: Some(Fill::Solid(idle_bg)),
                    ..Default::default()
                },
            ))
            .id();

        // Disable -> bg + opacity swap in.
        world.entity_mut(e).insert(Disabled);
        run(&mut world);
        assert_eq!(
            world.get::<Visuals>(e).unwrap().fill,
            Some(Fill::Solid(disabled_bg)),
            ":disabled bg applied on marker add"
        );
        assert_eq!(world.get::<Opacity>(e).map(|o| o.0), Some(0.5));

        // Re-enable -> exact idle restored, capture removed.
        world.entity_mut(e).remove::<Disabled>();
        run(&mut world);
        assert_eq!(
            world.get::<Visuals>(e).unwrap().fill,
            Some(Fill::Solid(idle_bg)),
            "idle bg restored on marker remove"
        );
        assert!(world.get::<Opacity>(e).is_none());
        assert!(world.get::<StateStyleBase>(e).is_none());
    }

    /// Disabled wins over hover/press patches when markers coexist for
    /// a tick (before ejection strips them).
    #[test]
    fn disabled_patch_wins_over_hover() {
        let mut world = World::new();
        let red = Color::rgb(1.0, 0.0, 0.0);
        let grey = Color::rgb(0.4, 0.4, 0.4);
        let e = world
            .spawn((
                StateVisuals {
                    hover: StatePatch {
                        text_color: Some(red),
                        ..Default::default()
                    },
                    disabled: StatePatch {
                        text_color: Some(grey),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                TextStyle::default(),
            ))
            .id();
        world.entity_mut(e).insert((Hovered, Disabled));
        run(&mut world);
        assert_eq!(world.get::<TextStyle>(e).unwrap().color, grey);
    }
}

#[cfg(test)]
mod eject_tests {
    //! `eject_interaction_on_disable` - becoming disabled strips
    //! `Hovered` / `Pressed` / focus and moves keyboard focus to the
    //! next focusable in tab order.
    use super::*;

    fn run(world: &mut World) {
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(eject_interaction_on_disable);
        schedule.run(world);
    }

    #[test]
    fn disable_strips_hover_press_and_moves_focus_forward() {
        let mut world = World::new();
        let a = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        let b = world.spawn((TabIndex(0), DocumentOrder(1))).id();
        let c = world.spawn((TabIndex(0), DocumentOrder(2))).id();
        world.insert_resource(FocusTracker(Some(b)));
        world
            .entity_mut(b)
            .insert((Hovered, Pressed, Focused, FocusVisible, Disabled));

        run(&mut world);
        assert!(world.get::<Hovered>(b).is_none(), "hover stripped");
        assert!(world.get::<Pressed>(b).is_none(), "press stripped");
        assert!(world.get::<Focused>(b).is_none(), "focus stripped");
        assert_eq!(
            world.resource::<FocusTracker>().0,
            Some(c),
            "focus moved to the next focusable in document order"
        );
        assert!(world.get::<Focused>(c).is_some());
        assert!(
            world.get::<FocusVisible>(c).is_some(),
            "keyboard-visible focus travels with the ejection"
        );
        let _ = a;
    }

    #[test]
    fn disable_of_last_focusable_wraps_to_first() {
        let mut world = World::new();
        let a = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        let b = world.spawn((TabIndex(0), DocumentOrder(1))).id();
        world.insert_resource(FocusTracker(Some(b)));
        world.entity_mut(b).insert((Focused, Disabled));
        run(&mut world);
        assert_eq!(world.resource::<FocusTracker>().0, Some(a), "wraps");
    }

    #[test]
    fn disable_of_only_focusable_clears_focus() {
        let mut world = World::new();
        let a = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        world.insert_resource(FocusTracker(Some(a)));
        world.entity_mut(a).insert((Focused, Disabled));
        run(&mut world);
        assert_eq!(world.resource::<FocusTracker>().0, None);
    }

    #[test]
    fn disable_without_focus_leaves_tracker_alone() {
        let mut world = World::new();
        let a = world.spawn((TabIndex(0), DocumentOrder(0))).id();
        let b = world.spawn((TabIndex(0), DocumentOrder(1), Hovered)).id();
        world.insert_resource(FocusTracker(Some(a)));
        world.entity_mut(b).insert(Disabled);
        run(&mut world);
        assert_eq!(world.resource::<FocusTracker>().0, Some(a));
        assert!(world.get::<Hovered>(b).is_none());
    }
}
