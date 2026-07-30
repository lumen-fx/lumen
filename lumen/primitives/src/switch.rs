//! `<switch>` behavior: an on/off toggle rendered as a pill track with a
//! thumb that slides between the two ends, built on the same shared
//! [`Toggleable`] machinery as `<checkbox>`.
//!
//! The markup parser spawns `<switch>` as a track element (tag `switch`,
//! carrying [`Toggleable`] + [`SwitchStyle`]) with a single absolute-
//! positioned child - the `.switch-thumb` tile (carrying [`SwitchThumb`]).
//! Every visual is design-token / CSS reachable through the skins:
//!
//! ```css
//! switch            { width: 52; height: 28; bg: ...; }  /* track (off) */
//! switch:checked    { bg: var(--lumen-accent); }        /* track (on) */
//! .switch-thumb     { knob-color: ... }                   /* thumb fill */
//! ```
//!
//! Behavior reuse - identical to `<checkbox>`, nothing bespoke:
//! - Click anywhere on the track (or the thumb) toggles -
//!   [`crate::controls::flip_toggle_on_click`] resolves a child hit to the
//!   ancestor [`Toggleable`].
//! - Space / Enter on the focused switch toggles -
//!   `lumen_input::activate_focused_on_enter`'s press-and-release FSM emits
//!   the same `ClickEvent` (mirrors Slint `SwitchBase`'s
//!   `key-pressed: if event.text == " " || "\n" { toggle-checked() }` and
//!   Qt `QAbstractButton`'s space-bar activation).
//! - `bind-checked` two-way signal binding comes with [`Toggleable`]
//!   (`apply_checked_bindings` / `push_toggle_to_signal`) - mirrors Slint's
//!   `in-out property <bool> checked` two-way binding.
//! - `disabled` inserts the generic `Disabled` marker; interaction is
//!   ejected by [`crate::state_style::eject_interaction_on_disable`].
//!
//! What IS bespoke: the thumb slides between off and on with an implicit
//! animation driven by the shared transition primitive
//! ([`crate::transition::Transition`]) rather than snapping - mirroring
//! Slint's `animate ... x { duration: 75ms }` on the switch handle and Qt
//! QML `Switch`'s `Behavior on position { NumberAnimation }`. The tween is
//! reactive: [`step_switch_thumb`] samples it and pings
//! [`AnimationsActive`] only while a slide is in flight, then retires the
//! component - no per-frame loop.
//!
//! Accessibility: `<switch>` carries an explicit
//! [`lumen_core::components::A11yRole::Switch`], so the a11y tree exposes
//! it as `Role::Switch` with the `Toggled` state derived from
//! [`Toggleable::checked`] - the `Role::Switch` analogue of how
//! `<checkbox>` surfaces `Role::CheckBox`.

use std::time::Duration;

use bevy_ecs::prelude::*;
use lumen_core::components::{Color, Fill, Visible, Visuals};
use lumen_core::prelude::*;
use lumen_core::render_world::AnimationsActive;

use crate::controls::{KNOB_INSET, TOGGLE_CHECKED_BG, TOGGLE_UNCHECKED_BG};
use crate::hover::{HoverBaseColor, PressBaseColor};
use crate::transition::{Easing, Transition};

/// Duration of the thumb slide when the switch flips. 140 ms sits between
/// Slint's 75 ms handle tween and the 200 ms Qt QML `Switch` default -
/// long enough to read as motion, short enough to feel instant.
pub const SWITCH_SLIDE_MS: u64 = 140;

/// Easing for the thumb slide. Ease-out (fast start, gentle settle) is the
/// Cocoa / Material short-transition curve; the same curve `<checkbox>` and
/// the hover tints reach for.
pub const SWITCH_SLIDE_EASING: Easing = Easing::EaseOut;

/// Per-switch track fills, resolved at spawn from markup / CSS.
/// [`sync_switch_visuals`] swaps [`Visuals::fill`] between the two on every
/// `checked` flip - the track equivalent of [`crate::controls::ToggleStyle`].
#[derive(Component, Clone, Copy, Debug)]
pub struct SwitchStyle {
    /// Track fill while checked (`switch:checked { bg }` or the default
    /// accent).
    pub checked_bg: Color,
    /// Track fill while unchecked (author `bg` or the default gray).
    pub unchecked_bg: Color,
}

impl Default for SwitchStyle {
    fn default() -> Self {
        Self {
            checked_bg: TOGGLE_CHECKED_BG,
            unchecked_bg: TOGGLE_UNCHECKED_BG,
        }
    }
}

/// Marker for the thumb child spawned inside every `<switch>`.
/// Absolute-positioned; [`sync_switch_visuals`] slides it between the two
/// track ends, animating flips through [`SwitchThumbSlide`].
///
/// `placed` records the `checked` state the thumb was last positioned for.
/// `None` until the first sync after layout, so the initial placement snaps
/// (no slide-in from the wrong end on spawn); a later mismatch means the
/// state flipped and starts an animated slide.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SwitchThumb {
    /// Last `checked` value the thumb was positioned for; `None` = unplaced.
    pub placed: Option<bool>,
}

/// Active thumb-slide tween driving a [`SwitchThumb`]'s horizontal inset.
/// Reuses the shared [`Transition<f32>`] primitive; [`step_switch_thumb`]
/// samples it and removes the component when the slide completes.
#[derive(Component, Clone, Copy, Debug)]
pub struct SwitchThumbSlide(pub Transition<f32>);

/// Plugin registering the switch visual-sync + thumb-slide systems.
///
/// The toggle behavior itself (click / keyboard / bindings) ships with
/// [`crate::ControlsPlugin`] via the shared [`Toggleable`] path, exactly as
/// `<checkbox>` reuses it; [`crate::ControlsPlugin`] also wires these two
/// systems so a host that installs `ControlsPlugin` gets `<switch>` for
/// free. This standalone plugin exists for tests / hosts that want the
/// switch systems in isolation.
pub struct SwitchPlugin;

impl Plugin for SwitchPlugin {
    fn build(self, app: &mut App) {
        register_switch_systems(app);
    }
}

/// Register [`sync_switch_visuals`] + [`step_switch_thumb`] in
/// `TickStage::Systems`. Called by both [`SwitchPlugin`] and
/// [`crate::ControlsPlugin`]; system de-duplication makes a double install
/// harmless.
pub fn register_switch_systems(app: &mut App) {
    app.add_systems(
        TickStage::Systems,
        sync_switch_visuals.after(crate::controls::flip_toggle_on_click),
    );
    app.add_systems(
        TickStage::Systems,
        step_switch_thumb.after(sync_switch_visuals),
    );
}

/// Keep the `<switch>` visuals in step with [`Toggleable::checked`]:
///
/// - track: swap [`Visuals::fill`] between [`SwitchStyle::checked_bg`] /
///   [`SwitchStyle::unchecked_bg`] on every flip (gated on
///   `Changed<Toggleable>`, rebasing any captured hover/press tint so a
///   tint release doesn't restore the stale pre-flip color - same rule as
///   [`crate::controls::sync_toggle_visuals`]);
/// - thumb: size + corner radius from the laid-out track height, and the
///   horizontal inset toward the checked/unchecked end. The first placement
///   snaps; a subsequent `checked` flip starts a [`SwitchThumbSlide`] from
///   the thumb's current position so it glides.
#[allow(clippy::type_complexity)]
pub fn sync_switch_visuals(
    mut commands: Commands,
    mut switches: Query<
        (
            &Toggleable,
            &SwitchStyle,
            &mut Visuals,
            Option<&mut HoverBaseColor>,
            Option<&mut PressBaseColor>,
        ),
        (Changed<Toggleable>, Without<SwitchThumb>),
    >,
    parents: Query<(&Toggleable, &Transform)>,
    mut thumbs: Query<
        (
            Entity,
            &ChildOf,
            &mut SwitchThumb,
            &mut Style,
            &mut Visuals,
            Option<&SwitchThumbSlide>,
        ),
        (With<SwitchThumb>, Without<Toggleable>),
    >,
) {
    // Track fill swap.
    for (t, style, mut vis, hover_base, press_base) in &mut switches {
        let target = if t.checked {
            style.checked_bg
        } else {
            style.unchecked_bg
        };
        if vis.fill.as_ref().and_then(Fill::as_solid) != Some(target) {
            vis.fill = Some(Fill::Solid(target));
        }
        if let Some(mut base) = hover_base {
            base.0 = target;
        }
        if let Some(mut base) = press_base {
            base.0 = target;
        }
    }

    // Thumb geometry + animated slide.
    for (thumb_e, child_of, mut thumb, mut style, mut vis, slide) in &mut thumbs {
        let Ok((t, tr)) = parents.get(child_of.parent()) else {
            continue;
        };
        if tr.size.x <= 0.0 || tr.size.y <= 0.0 {
            continue;
        }
        let knob = (tr.size.y - 2.0 * KNOB_INSET).max(2.0);
        let target_left = if t.checked {
            (tr.size.x - knob - KNOB_INSET).max(KNOB_INSET)
        } else {
            KNOB_INSET
        };
        let radius = knob / 2.0;
        if vis.radius != radius {
            vis.radius = radius;
        }
        let size = Length::Px(knob);
        let mut dirty = false;
        if style.width != size || style.height != size || style.inset.top != KNOB_INSET {
            style.width = size;
            style.height = size;
            style.inset.top = KNOB_INSET;
            dirty = true;
        }
        match thumb.placed {
            // First placement after layout: snap, no slide.
            None => {
                style.inset.left = target_left;
                thumb.placed = Some(t.checked);
                dirty = true;
            }
            // State flipped: glide from the current rest position.
            Some(prev) if prev != t.checked => {
                let from = style.inset.left;
                thumb.placed = Some(t.checked);
                if (from - target_left).abs() > f32::EPSILON {
                    commands
                        .entity(thumb_e)
                        .insert(SwitchThumbSlide(Transition::new(
                            from,
                            target_left,
                            Duration::from_millis(SWITCH_SLIDE_MS),
                            SWITCH_SLIDE_EASING,
                        )));
                } else {
                    style.inset.left = target_left;
                    dirty = true;
                }
            }
            // No flip: park at the target when idle (handles track resize)
            // without fighting an in-flight slide.
            Some(_) => {
                if slide.is_none() && (style.inset.left - target_left).abs() > 0.25 {
                    style.inset.left = target_left;
                    dirty = true;
                }
            }
        }
        if dirty {
            commands.entity(thumb_e).insert(DirtyLayout);
        }
    }
}

/// Advance every active [`SwitchThumbSlide`], writing the eased offset into
/// the thumb's [`Style::inset`]`.left` and marking it dirty so layout
/// repositions it. Pings [`AnimationsActive`] while running so the loop
/// keeps ticking without an unrelated OS event; snaps to the endpoint and
/// retires the component when done (or immediately if the thumb is hidden -
/// a `display: none` subtree runs no animation, matching the CSS transition
/// drivers in [`crate::transition`]).
pub fn step_switch_thumb(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<AnimationsActive>,
    mut q: Query<(Entity, &SwitchThumbSlide, &mut Style, Option<&Visible>)>,
) {
    let now = tick.now;
    for (entity, slide, mut style, visible) in &mut q {
        let hidden = visible.is_some_and(|v| !v.0);
        let next = if hidden {
            slide.0.to
        } else {
            slide.0.sample(now)
        };
        if style.inset.left != next {
            style.inset.left = next;
            commands.entity(entity).insert(DirtyLayout);
        }
        if hidden || slide.0.done(now) {
            if style.inset.left != slide.0.to {
                style.inset.left = slide.0.to;
                commands.entity(entity).insert(DirtyLayout);
            }
            commands.entity(entity).remove::<SwitchThumbSlide>();
        } else {
            anim.request();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;

    /// Spawn a laid-out `<switch>` track + thumb child. Track is 52x28 so
    /// the thumb math is `knob = 28 - 8 = 20`, `off = 4`, `on = 52-20-4 = 28`.
    fn spawn_switch(world: &mut World, checked: bool) -> (Entity, Entity) {
        let track = world
            .spawn((
                Toggleable { checked },
                SwitchStyle::default(),
                Visuals {
                    fill: Some(Fill::Solid(SwitchStyle::default().unchecked_bg)),
                    ..Default::default()
                },
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(52.0, 28.0),
                    baseline_y: None,
                },
            ))
            .id();
        let thumb = world
            .spawn((
                SwitchThumb::default(),
                Style::default(),
                Visuals::default(),
                ChildOf(track),
            ))
            .id();
        (track, thumb)
    }

    fn thumb_left(world: &World, thumb: Entity) -> f32 {
        world.get::<Style>(thumb).unwrap().inset.left
    }

    fn track_fill(world: &World, track: Entity) -> Option<Color> {
        world
            .get::<Visuals>(track)
            .and_then(|v| v.fill.as_ref().and_then(Fill::as_solid))
    }

    #[test]
    fn unchecked_parks_thumb_left_and_paints_off_track() {
        let mut world = World::new();
        let (track, thumb) = spawn_switch(&mut world, false);
        world.run_system_once(sync_switch_visuals).unwrap();
        assert_eq!(thumb_left(&world, thumb), KNOB_INSET);
        assert_eq!(
            track_fill(&world, track),
            Some(SwitchStyle::default().unchecked_bg)
        );
        // First placement snaps - no slide component.
        assert!(world.get::<SwitchThumbSlide>(thumb).is_none());
    }

    #[test]
    fn checked_on_spawn_snaps_thumb_right_and_paints_on_track() {
        let mut world = World::new();
        let (track, thumb) = spawn_switch(&mut world, true);
        world.run_system_once(sync_switch_visuals).unwrap();
        // on = 52 - 20 - 4 = 28.
        assert_eq!(thumb_left(&world, thumb), 28.0);
        assert_eq!(
            track_fill(&world, track),
            Some(SwitchStyle::default().checked_bg)
        );
        // A switch spawned already-on must not animate in from the off end.
        assert!(world.get::<SwitchThumbSlide>(thumb).is_none());
    }

    #[test]
    fn thumb_size_and_radius_follow_track_height() {
        let mut world = World::new();
        let (_, thumb) = spawn_switch(&mut world, false);
        world.run_system_once(sync_switch_visuals).unwrap();
        let style = world.get::<Style>(thumb).unwrap();
        assert_eq!(style.width, Length::Px(20.0));
        assert_eq!(style.height, Length::Px(20.0));
        assert_eq!(world.get::<Visuals>(thumb).unwrap().radius, 10.0);
    }

    #[test]
    fn flip_starts_a_slide_from_the_current_position() {
        let mut world = World::new();
        let (track, thumb) = spawn_switch(&mut world, false);
        // First sync: place at off (snap).
        world.run_system_once(sync_switch_visuals).unwrap();
        assert_eq!(thumb_left(&world, thumb), KNOB_INSET);
        // Flip to on and re-sync: a slide toward the on end must start,
        // beginning from the current (off) position - the animated
        // equivalent of Slint's `animate x` on the handle.
        world.get_mut::<Toggleable>(track).unwrap().checked = true;
        world.run_system_once(sync_switch_visuals).unwrap();
        let slide = world
            .get::<SwitchThumbSlide>(thumb)
            .expect("flip must start a thumb slide");
        assert_eq!(slide.0.from, KNOB_INSET, "slide starts at the old rest pos");
        assert_eq!(slide.0.to, 28.0, "slide targets the on end");
    }

    #[test]
    fn step_advances_and_retires_the_slide() {
        let mut world = World::new();
        world.insert_resource(Tick::default());
        world.insert_resource(AnimationsActive::default());
        let (_, thumb) = spawn_switch(&mut world, false);
        // Install a slide directly: off (4) -> on (28) over 140 ms.
        let tween = Transition::new(
            KNOB_INSET,
            28.0,
            Duration::from_millis(SWITCH_SLIDE_MS),
            Easing::Linear,
        );
        world.entity_mut(thumb).insert(SwitchThumbSlide(tween));
        // Mid-flight: back-date the start so sampling lands halfway.
        world.get_mut::<SwitchThumbSlide>(thumb).unwrap().0.start -=
            Duration::from_millis(SWITCH_SLIDE_MS / 2);
        {
            let now = world.get::<SwitchThumbSlide>(thumb).unwrap().0.start
                + Duration::from_millis(SWITCH_SLIDE_MS / 2);
            world.resource_mut::<Tick>().now = now;
        }
        world.run_system_once(step_switch_thumb).unwrap();
        let mid = thumb_left(&world, thumb);
        assert!(
            mid > KNOB_INSET + 4.0 && mid < 28.0,
            "thumb mid-slide between the ends (left = {mid})"
        );
        assert!(
            world.get::<SwitchThumbSlide>(thumb).is_some(),
            "slide still active mid-flight"
        );
        assert!(
            world.resource::<AnimationsActive>().get(),
            "an in-flight slide keeps the loop awake"
        );
        // Past the end: snap to the on position and retire.
        {
            let now = world.get::<SwitchThumbSlide>(thumb).unwrap().0.start
                + Duration::from_millis(SWITCH_SLIDE_MS * 4);
            world.resource_mut::<Tick>().now = now;
        }
        world.run_system_once(step_switch_thumb).unwrap();
        assert_eq!(thumb_left(&world, thumb), 28.0);
        assert!(world.get::<SwitchThumbSlide>(thumb).is_none());
    }

    #[test]
    fn hidden_switch_snaps_slide_without_waking() {
        let mut world = World::new();
        world.insert_resource(Tick::default());
        world.insert_resource(AnimationsActive::default());
        let (_, thumb) = spawn_switch(&mut world, false);
        world.entity_mut(thumb).insert((
            SwitchThumbSlide(Transition::new(
                KNOB_INSET,
                28.0,
                Duration::from_millis(SWITCH_SLIDE_MS),
                Easing::Linear,
            )),
            Visible(false),
        ));
        world.run_system_once(step_switch_thumb).unwrap();
        assert_eq!(thumb_left(&world, thumb), 28.0, "hidden slide jumps to end");
        assert!(world.get::<SwitchThumbSlide>(thumb).is_none());
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "hidden slide must not keep the loop awake"
        );
    }
}
