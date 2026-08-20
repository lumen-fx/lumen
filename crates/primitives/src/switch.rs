//! `<switch>` behavior: an on/off toggle rendered as a pill track with a
//! thumb that slides between the two ends, built on the same shared
//! [`Toggleable`] machinery as `<checkbox>`.
//!
//! The markup parser spawns `<switch>` as a track element (tag `switch`,
//! carrying [`Toggleable`] + [`crate::controls::TrackStyle`]) with a single absolute-
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
//! The slide's duration + easing come from a `transition: bg <duration>
//! <easing>` declaration on the track (see [`slide_tween_params`]) - the
//! same mechanism [`crate::hover`] uses for its hover / press tint tween -
//! falling back to [`SWITCH_SLIDE_MS`] / [`SWITCH_SLIDE_EASING`] when
//! none is authored. Track / thumb geometry (the inset gap and the
//! thumb's diameter) come from [`crate::controls::KnobGeometry`],
//! populated from the CSS `knob-inset` property, with the same
//! today's-appearance fallback.
//!
//! Accessibility: `<switch>` carries an explicit
//! [`lumen_core::components::A11yRole::Switch`], so the a11y tree exposes
//! it as `Role::Switch` with the `Toggled` state derived from
//! [`Toggleable::checked`] - the `Role::Switch` analogue of how
//! `<checkbox>` surfaces `Role::CheckBox`.

use std::time::Duration;

use bevy_ecs::prelude::*;
use lumen_core::components::{Visible, Visuals};
use lumen_core::prelude::*;
use lumen_core::render_world::AnimationsActive;

use crate::controls::KnobGeometry;
use crate::transition::{Easing, Transition, TransitionProperty, TransitionSpecs};

/// Fallback duration of the thumb slide when the switch flips, used when
/// the track carries no `transition: bg ...` [`TransitionSpecs`] (see
/// [`slide_tween_params`]). 140 ms sits between Slint's 75 ms handle
/// tween and the 200 ms Qt QML `Switch` default - long enough to read as
/// motion, short enough to feel instant.
pub const SWITCH_SLIDE_MS: u64 = 140;

/// Fallback easing for the thumb slide, used under the same condition as
/// [`SWITCH_SLIDE_MS`]. Ease-out (fast start, gentle settle) is the
/// Cocoa / Material short-transition curve; the same curve `<checkbox>` and
/// the hover tints reach for.
pub const SWITCH_SLIDE_EASING: Easing = Easing::EaseOut;

/// Resolve the thumb slide's duration + easing for one flip. A CSS
/// `transition: bg <duration> <easing>` declaration on the track entity
/// (its [`TransitionSpecs`]) wins - the same `TransitionSpecs` +
/// `TransitionProperty::BackgroundColor` lookup [`crate::hover`] consults
/// for its hover / press tint tween duration - otherwise the built-in
/// [`SWITCH_SLIDE_MS`] / [`SWITCH_SLIDE_EASING`] fallback, which
/// reproduces today's hardcoded slide exactly.
fn slide_tween_params(specs: Option<&TransitionSpecs>) -> (Duration, Easing) {
    match specs.and_then(|s| s.for_property(TransitionProperty::BackgroundColor)) {
        Some(spec) => (spec.duration, spec.easing),
        None => (Duration::from_millis(SWITCH_SLIDE_MS), SWITCH_SLIDE_EASING),
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

/// Place the [`SwitchThumb`] child: size + corner radius from the
/// laid-out track height, and the horizontal inset toward the
/// checked/unchecked end. The first placement snaps; a subsequent
/// `checked` flip starts a [`SwitchThumbSlide`] from the thumb's current
/// position so it glides.
///
/// The track fill belongs to [`crate::controls::sync_track_fill`], which
/// serves `<toggle>` and `<switch>` alike.
#[allow(clippy::type_complexity)]
pub fn sync_switch_visuals(
    mut commands: Commands,
    parents: Query<(
        &Toggleable,
        &Transform,
        Option<&KnobGeometry>,
        Option<&TransitionSpecs>,
    )>,
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
    for (thumb_e, child_of, mut thumb, mut style, mut vis, slide) in &mut thumbs {
        let Ok((t, tr, geo, specs)) = parents.get(child_of.parent()) else {
            continue;
        };
        if tr.size.x <= 0.0 || tr.size.y <= 0.0 {
            continue;
        }
        let inset = geo.copied().unwrap_or_default().inset;
        let knob = (tr.size.y - 2.0 * inset).max(2.0);
        let target_left = if t.checked {
            (tr.size.x - knob - inset).max(inset)
        } else {
            inset
        };
        let radius = knob / 2.0;
        if vis.radius != radius {
            vis.radius = radius;
        }
        let size = Length::Px(knob);
        let mut dirty = false;
        if style.width != size || style.height != size || style.inset.top != inset {
            style.width = size;
            style.height = size;
            style.inset.top = inset;
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
                    let (duration, easing) = slide_tween_params(specs);
                    commands
                        .entity(thumb_e)
                        .insert(SwitchThumbSlide(Transition::new(
                            from,
                            target_left,
                            duration,
                            easing,
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
    use crate::controls::{KNOB_INSET, THUMB_SIZE, TrackStyle, sync_track_fill};
    use crate::transition::TransitionSpec;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;
    use lumen_core::components::{Color, Fill};

    /// Spawn a laid-out `<switch>` track + thumb child, with no
    /// [`KnobGeometry`] / [`TransitionSpecs`] on the track - the
    /// no-CSS-authored path, which must reproduce today's hardcoded
    /// geometry / slide timing exactly. Track is 52x28 so the thumb math
    /// is `knob = 28 - 8 = 20`, `off = 4`, `on = 52-20-4 = 28`.
    fn spawn_switch(world: &mut World, checked: bool) -> (Entity, Entity) {
        spawn_switch_with(world, checked, None, None)
    }

    /// The component-based counterpart of [`spawn_switch`]: same track +
    /// thumb shape, but with an optional CSS-supplied [`KnobGeometry`]
    /// and/or [`TransitionSpecs`] inserted on the track, to prove they
    /// move the resolved geometry / slide timing off the defaults.
    fn spawn_switch_with(
        world: &mut World,
        checked: bool,
        geo: Option<KnobGeometry>,
        specs: Option<TransitionSpecs>,
    ) -> (Entity, Entity) {
        let mut track = world.spawn((
            Toggleable { checked },
            TrackStyle::default(),
            Visuals {
                fill: Some(Fill::Solid(TrackStyle::default().unchecked_bg)),
                ..Default::default()
            },
            Transform {
                absolute: Vec2::ZERO,
                size: Vec2::new(52.0, 28.0),
                baseline_y: None,
            },
        ));
        if let Some(geo) = geo {
            track.insert(geo);
        }
        if let Some(specs) = specs {
            track.insert(specs);
        }
        let track = track.id();
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
        world.run_system_once(sync_track_fill).unwrap();
        assert_eq!(thumb_left(&world, thumb), KNOB_INSET);
        assert_eq!(
            track_fill(&world, track),
            Some(TrackStyle::default().unchecked_bg)
        );
        // First placement snaps - no slide component.
        assert!(world.get::<SwitchThumbSlide>(thumb).is_none());
    }

    #[test]
    fn checked_on_spawn_snaps_thumb_right_and_paints_on_track() {
        let mut world = World::new();
        let (track, thumb) = spawn_switch(&mut world, true);
        world.run_system_once(sync_switch_visuals).unwrap();
        world.run_system_once(sync_track_fill).unwrap();
        // on = 52 - 20 - 4 = 28.
        assert_eq!(thumb_left(&world, thumb), 28.0);
        assert_eq!(
            track_fill(&world, track),
            Some(TrackStyle::default().checked_bg)
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

    /// A CSS-supplied [`KnobGeometry`] on the track moves the thumb's
    /// rest position (and size) off the [`KNOB_INSET`] / track-height
    /// default - the component-based counterpart of the KNOB_INSET
    /// assertions above.
    #[test]
    fn custom_knob_geometry_moves_the_thumb_off_the_default_inset() {
        let mut world = World::new();
        let geo = KnobGeometry {
            inset: 8.0,
            thumb_size: THUMB_SIZE,
        };
        let (_, thumb) = spawn_switch_with(&mut world, false, Some(geo), None);
        world.run_system_once(sync_switch_visuals).unwrap();
        assert_eq!(
            thumb_left(&world, thumb),
            8.0,
            "a CSS-supplied inset moves the resting position off KNOB_INSET (4)"
        );
        // knob = 28 - 2*8 = 12, vs. the default geometry's 20.
        let style = world.get::<Style>(thumb).unwrap();
        assert_eq!(style.width, Length::Px(12.0));
        assert_eq!(style.height, Length::Px(12.0));
    }

    /// An entity with no [`KnobGeometry`] must keep today's geometry - the
    /// explicit no-component counterpart of the custom-geometry test
    /// above (also covered implicitly by every `spawn_switch`-based test).
    #[test]
    fn no_knob_geometry_keeps_the_default_inset() {
        let mut world = World::new();
        let (_, thumb) = spawn_switch_with(&mut world, false, None, None);
        world.run_system_once(sync_switch_visuals).unwrap();
        assert_eq!(thumb_left(&world, thumb), KNOB_INSET);
    }

    /// An authored `transition: bg <duration> <easing>` on the track
    /// overrides [`SWITCH_SLIDE_MS`] / [`SWITCH_SLIDE_EASING`] for the
    /// thumb slide - the same [`TransitionSpecs`] +
    /// `TransitionProperty::BackgroundColor` lookup [`crate::hover`] uses
    /// for its tint tween duration.
    #[test]
    fn authored_transition_overrides_the_default_slide_duration_and_easing() {
        let mut world = World::new();
        let specs = TransitionSpecs(vec![TransitionSpec {
            property: TransitionProperty::BackgroundColor,
            duration: Duration::from_millis(300),
            easing: Easing::Linear,
        }]);
        let (track, thumb) = spawn_switch_with(&mut world, false, None, Some(specs));
        // First sync: snap to off (no slide yet).
        world.run_system_once(sync_switch_visuals).unwrap();
        world.get_mut::<Toggleable>(track).unwrap().checked = true;
        world.run_system_once(sync_switch_visuals).unwrap();
        let slide = world
            .get::<SwitchThumbSlide>(thumb)
            .expect("flip must start a thumb slide");
        assert_eq!(
            slide.0.duration,
            Duration::from_millis(300),
            "authored transition: bg 300ms overrides SWITCH_SLIDE_MS"
        );
        assert_eq!(
            slide.0.easing,
            Easing::Linear,
            "authored easing overrides SWITCH_SLIDE_EASING"
        );
    }

    /// No `transition:` authored on the track falls back to
    /// [`SWITCH_SLIDE_MS`] / [`SWITCH_SLIDE_EASING`] - appearance must not
    /// change for a skin that never declares `transition: bg` on `switch`.
    #[test]
    fn no_authored_transition_falls_back_to_switch_slide_defaults() {
        let mut world = World::new();
        let (track, thumb) = spawn_switch_with(&mut world, false, None, None);
        world.run_system_once(sync_switch_visuals).unwrap();
        world.get_mut::<Toggleable>(track).unwrap().checked = true;
        world.run_system_once(sync_switch_visuals).unwrap();
        let slide = world
            .get::<SwitchThumbSlide>(thumb)
            .expect("flip must start a thumb slide");
        assert_eq!(slide.0.duration, Duration::from_millis(SWITCH_SLIDE_MS));
        assert_eq!(slide.0.easing, SWITCH_SLIDE_EASING);
    }
}
