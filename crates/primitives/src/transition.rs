//! Generic per-property tween primitive.
//!
//! `Transition<T>` carries a start value, end value, start instant,
//! duration, and easing. Querying [`Transition::sample`] returns the
//! interpolated value at any wall-clock instant; once
//! [`Transition::done`] is true the transition can be retired.
//!
//! The crate ships the primitive plus a [`Lerp`] impl for [`f32`] and
//! [`Color`] - concrete property bridges (CSS `transition: opacity` ->
//! `Transition<f32>` driving [`Opacity`], CSS `transition: bg` ->
//! `Transition<Color>` driving [`Visuals.fill`]) live in the CSS
//! application layer, not here.
//!
//! Designed for the markup-driven CSS `transition:` shorthand
//! (Transitions Level 1 spec). Hover/press tints in [`crate::hover`]
//! keep their bespoke state machine - their bidirectional snap-on-state-
//! flip semantics don't fit a one-shot transition.

use bevy_ecs::prelude::*;
use lumen_core::components::{Color, Fill, Opacity, Visuals};
use lumen_core::prelude::{Tick, TickStage};
use lumen_core::time::{Duration, Instant};

/// Tween-able value. Implementations should return a linear interpolation
/// between `self` and `other` at fraction `t` in `[0, 1]`. Implementations
/// are free to assume `t` is already easing-adjusted by [`Easing::apply`].
pub trait Lerp: Copy + PartialEq {
    /// Linear interpolation between `self` (at `t = 0`) and `other` (at
    /// `t = 1`).
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Color {
    fn lerp(self, other: Self, t: f32) -> Self {
        Color {
            r: self.r.lerp(other.r, t),
            g: self.g.lerp(other.g, t),
            b: self.b.lerp(other.b, t),
            a: self.a.lerp(other.a, t),
        }
    }
}

/// Easing curve sampled by [`Easing::apply`]. The curves match common
/// CSS / native-control timings:
///
/// - `Linear` - uniform speed; not common in UI but useful for tests.
/// - `EaseIn` - cubic ease-in (slow start).
/// - `EaseOut` - cubic ease-out (fast start, slow settle); matches the
///   Cocoa AppKit / Material 3 short-transition curve.
/// - `EaseInOut` - cubic ease-in-out.
/// - `CubicBezier(p1x, p1y, p2x, p2y)` - CSS `cubic-bezier(...)` form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    /// `f(t) = t`.
    Linear,
    /// `f(t) = t^3`.
    EaseIn,
    /// `f(t) = 1 - (1 - t)^3`.
    EaseOut,
    /// `f(t) = 0.5*(2t)^3` for `t < 0.5`, mirrored above.
    EaseInOut,
    /// CSS `cubic-bezier(p1x, p1y, p2x, p2y)`. Anchors are implicit
    /// `(0, 0)` and `(1, 1)`.
    CubicBezier(f32, f32, f32, f32),
}

impl Easing {
    /// Map a linear progress `t` in `[0, 1]` through the curve.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t * t,
            Easing::EaseOut => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            Easing::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - inv * inv * inv * 0.5
                }
            }
            Easing::CubicBezier(p1x, p1y, p2x, p2y) => cubic_bezier(p1x, p1y, p2x, p2y, t),
        }
    }
}

/// One-shot transition between two values of a tween-able type.
///
/// Use [`Transition::new`] to construct or `From<(T, T, Duration)>` for
/// the common linear case. [`Transition::sample`] returns the eased value
/// at any instant; [`Transition::done`] returns `true` once elapsed time
/// has met or exceeded `duration` so the caller can despawn the component
/// and stop tweening.
#[derive(Component, Clone, Copy, Debug)]
pub struct Transition<T: Lerp + Send + Sync + 'static> {
    /// Value at `start`.
    pub from: T,
    /// Value at `start + duration`.
    pub to: T,
    /// Wall-clock instant the transition began.
    pub start: Instant,
    /// Total length of the transition.
    pub duration: Duration,
    /// Curve applied to the linear progress fraction.
    pub easing: Easing,
}

impl<T: Lerp + Send + Sync + 'static> Transition<T> {
    /// Construct a transition that starts at `now()` and runs over
    /// `duration` with `easing`.
    pub fn new(from: T, to: T, duration: Duration, easing: Easing) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration,
            easing,
        }
    }

    /// Linear fraction of total `duration` elapsed at `now`, clamped to
    /// `[0, 1]`. Zero-duration transitions report `1` immediately so they
    /// surface their `to` value without dividing by zero.
    pub fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Eased value at `now`.
    pub fn sample(&self, now: Instant) -> T {
        let t = self.easing.apply(self.progress(now));
        self.from.lerp(self.to, t)
    }

    /// `true` once the transition has met or exceeded `duration`.
    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

impl<T: Lerp + Send + Sync + 'static> From<(T, T, Duration)> for Transition<T> {
    /// Convenience: `(from, to, duration).into()` builds a
    /// [`Easing::Linear`] transition starting at `now()`.
    fn from((from, to, duration): (T, T, Duration)) -> Self {
        Self::new(from, to, duration, Easing::Linear)
    }
}

/// Property identifier for a [`TransitionSpec`].
///
/// The v1 animatable set is colors + opacity - geometry-free visual
/// properties only. Layout properties (`width`, `height`, padding, ...)
/// are deliberately not transitionable: animating them would re-run
/// layout every frame; the CSS parser warns and drops them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitionProperty {
    /// CSS `opacity`; driven by [`step_opacity_transitions`] against the [`Opacity`] component.
    Opacity,
    /// CSS `background-color`; driven by [`step_background_transitions`]
    /// against the solid [`Visuals::fill`]. Also parameterises the
    /// hover / press tint tweens in [`crate::hover`] (duration + easing).
    BackgroundColor,
    /// CSS `color` / `text-color`; driven by
    /// [`step_text_color_transitions`] against `TextStyle::color`.
    TextColor,
    /// CSS `border-color`; driven by [`step_border_color_transitions`]
    /// against `Visuals::border.color`.
    BorderColor,
}

impl TransitionProperty {
    /// Parse a CSS property name into a recognised [`TransitionProperty`].
    /// Unknown names return `None`; callers should warn and ignore.
    pub fn from_css(name: &str) -> Option<Self> {
        match name {
            "opacity" => Some(Self::Opacity),
            "background-color" | "background" | "bg" => Some(Self::BackgroundColor),
            "color" | "text-color" => Some(Self::TextColor),
            "border-color" => Some(Self::BorderColor),
            _ => None,
        }
    }
}

/// Author-declared transition. CSS shorthand
/// `transition: opacity 200ms ease-out` lands as one of these per
/// comma-separated entry. The runtime stores all specs for an entity in
/// a [`TransitionSpecs`] component and consults them when a relevant
/// property changes (e.g. on a class flip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionSpec {
    /// Which property this spec applies to.
    pub property: TransitionProperty,
    /// Length of the tween.
    pub duration: Duration,
    /// Easing curve.
    pub easing: Easing,
}

/// Every [`TransitionSpec`] declared for an entity, in source order.
#[derive(Component, Clone, Debug, Default)]
pub struct TransitionSpecs(pub Vec<TransitionSpec>);

impl TransitionSpecs {
    /// Look up the last spec matching `property`. Earlier duplicates are
    /// overridden - CSS Transitions Level 1 / Cascade 5 mandate
    /// last-declaration-wins per property when authors list the same
    /// property twice in a `transition:` shorthand or across multiple
    /// rule blocks of equal specificity.
    pub fn for_property(&self, property: TransitionProperty) -> Option<&TransitionSpec> {
        self.0.iter().rev().find(|s| s.property == property)
    }
}

/// Active [`Transition<f32>`] driving an entity's [`Opacity`]. Spawned
/// when a class flip changes the resolved opacity and the entity carries
/// a matching [`TransitionSpec`]. The driver removes this component
/// when the transition completes.
#[derive(Component, Clone, Copy, Debug)]
pub struct OpacityTransition(pub Transition<f32>);

/// Active [`Transition<Color>`] driving an entity's solid
/// [`Visuals::fill`] (CSS `transition: background-color`). Started by
/// the restyle path (theme / class flips) when the computed background
/// changes; hover / press pseudo-class tints keep their own FSM in
/// [`crate::hover`] (which reads the same [`TransitionSpecs`] for
/// duration + easing).
#[derive(Component, Clone, Copy, Debug)]
pub struct BackgroundTransition(pub Transition<Color>);

/// Active [`Transition<Color>`] driving `TextStyle::color`.
#[derive(Component, Clone, Copy, Debug)]
pub struct TextColorTransition(pub Transition<Color>);

/// Active [`Transition<Color>`] driving `Visuals::border.color`.
#[derive(Component, Clone, Copy, Debug)]
pub struct BorderColorTransition(pub Transition<Color>);

/// CSS-retarget rule: when a transition re-triggers mid-flight, the new
/// tween starts from the CURRENT interpolated value (the live component
/// value the previous driver tick wrote), never the old endpoint. This
/// helper builds that tween; equal endpoints return `None` (equal-value
/// writes are no-ops, no zombie transitions).
pub fn retarget<T: Lerp + Send + Sync + 'static>(
    current: T,
    to: T,
    spec: &TransitionSpec,
) -> Option<Transition<T>> {
    if current == to {
        return None;
    }
    Some(Transition::new(current, to, spec.duration, spec.easing))
}

/// Sample every active [`OpacityTransition`] and write the result into
/// the entity's [`Opacity`]. When the transition is done, snap to the
/// final value and despawn the transition component.
///
/// Hidden entities (`Visible(false)`) tick no animations (CSS: a
/// `display: none` subtree has no transitions): the tween completes
/// instantly - snap to the target value, drop the component, no
/// self-scheduled frame.
pub fn step_opacity_transitions(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut q: Query<(
        Entity,
        &OpacityTransition,
        &mut Opacity,
        Option<&lumen_core::components::Visible>,
    )>,
) {
    let now = tick.now;
    for (entity, tween, mut opacity, visible) in &mut q {
        let hidden = visible.is_some_and(|v| !v.0);
        let next = if hidden {
            tween.0.to
        } else {
            tween.0.sample(now)
        };
        if opacity.0 != next {
            opacity.0 = next;
        }
        if hidden || tween.0.done(now) {
            commands.entity(entity).remove::<OpacityTransition>();
        } else {
            // Transition still running - keep the loop awake so it advances
            // without waiting for an unrelated OS event.
            anim.request();
        }
    }
}

/// Sample every active [`BackgroundTransition`] into the entity's solid
/// [`Visuals::fill`]. Gradient fills are never animated (the component
/// is dropped on sight). Skips entities whose fill is currently owned by
/// the hover / press tint FSM (their snapshot components are present) -
/// the last writer would flicker otherwise.
#[allow(clippy::type_complexity)]
pub fn step_background_transitions(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut q: Query<(
        Entity,
        &BackgroundTransition,
        &mut Visuals,
        Option<&lumen_core::components::Visible>,
        Option<&crate::hover::HoverBaseColor>,
        Option<&crate::hover::PressBaseColor>,
    )>,
) {
    let now = tick.now;
    for (entity, tween, mut vis, visible, hover_base, press_base) in &mut q {
        if hover_base.is_some() || press_base.is_some() {
            // Hover/press FSM owns the fill right now; retire the
            // restyle tween rather than fight over the color.
            commands.entity(entity).remove::<BackgroundTransition>();
            continue;
        }
        let hidden = visible.is_some_and(|v| !v.0);
        let next = if hidden {
            tween.0.to
        } else {
            tween.0.sample(now)
        };
        match vis.fill.as_mut() {
            Some(Fill::Solid(slot)) => {
                if *slot != next {
                    *slot = next;
                }
            }
            _ => {
                commands.entity(entity).remove::<BackgroundTransition>();
                continue;
            }
        }
        if hidden || tween.0.done(now) {
            commands.entity(entity).remove::<BackgroundTransition>();
        } else {
            anim.request();
        }
    }
}

/// Sample every active [`TextColorTransition`] into `TextStyle::color`.
pub fn step_text_color_transitions(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut q: Query<(
        Entity,
        &TextColorTransition,
        &mut lumen_core::components::TextStyle,
        Option<&lumen_core::components::Visible>,
    )>,
) {
    let now = tick.now;
    for (entity, tween, mut style, visible) in &mut q {
        let hidden = visible.is_some_and(|v| !v.0);
        let next = if hidden {
            tween.0.to
        } else {
            tween.0.sample(now)
        };
        if style.color != next {
            style.color = next;
        }
        if hidden || tween.0.done(now) {
            commands.entity(entity).remove::<TextColorTransition>();
        } else {
            anim.request();
        }
    }
}

/// Sample every active [`BorderColorTransition`] into
/// `Visuals::border.color`. A border removed mid-flight retires the
/// tween (nothing left to paint).
pub fn step_border_color_transitions(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut q: Query<(
        Entity,
        &BorderColorTransition,
        &mut Visuals,
        Option<&lumen_core::components::Visible>,
    )>,
) {
    let now = tick.now;
    for (entity, tween, mut vis, visible) in &mut q {
        let hidden = visible.is_some_and(|v| !v.0);
        let next = if hidden {
            tween.0.to
        } else {
            tween.0.sample(now)
        };
        match vis.border.as_mut() {
            Some(border) => {
                if border.color != next {
                    border.color = next;
                }
            }
            None => {
                commands.entity(entity).remove::<BorderColorTransition>();
                continue;
            }
        }
        if hidden || tween.0.done(now) {
            commands.entity(entity).remove::<BorderColorTransition>();
        } else {
            anim.request();
        }
    }
}

/// Plugin: registers every per-property transition driver in
/// `TickStage::Systems`. Add via `App::add_plugin(TransitionPlugin)` once;
/// subsequent additions are no-ops as long as the same instance is reused.
pub struct TransitionPlugin;

impl lumen_core::prelude::Plugin for TransitionPlugin {
    fn build(self, app: &mut lumen_core::prelude::App) {
        app.add_systems(TickStage::Systems, step_opacity_transitions);
        // Background tween runs after the hover/press tint systems so
        // its owns-the-fill check observes this tick's snapshots.
        app.add_systems(
            TickStage::Systems,
            step_background_transitions.after(crate::hover::apply_press_tint),
        );
        app.add_systems(TickStage::Systems, step_text_color_transitions);
        app.add_systems(TickStage::Systems, step_border_color_transitions);
    }
}

/// Sample a CSS-style cubic Bezier `(0, 0) - (p1x, p1y) - (p2x, p2y) -
/// (1, 1)` at fraction `t`. The Bezier parametric form is sampled by
/// Newton-Raphson - three iterations + a bisection fallback - to invert
/// `x(s) -> s` then evaluate `y(s)`. Matches CSS Transitions Level 1.
fn cubic_bezier(p1x: f32, p1y: f32, p2x: f32, p2y: f32, t: f32) -> f32 {
    let ax = 3.0 * p1x - 3.0 * p2x + 1.0;
    let bx = -6.0 * p1x + 3.0 * p2x;
    let cx = 3.0 * p1x;
    let ay = 3.0 * p1y - 3.0 * p2y + 1.0;
    let by = -6.0 * p1y + 3.0 * p2y;
    let cy = 3.0 * p1y;
    let bx_t = |s: f32| ((ax * s + bx) * s + cx) * s;
    let dbx_t = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
    let by_t = |s: f32| ((ay * s + by) * s + cy) * s;
    let mut s = t;
    for _ in 0..3 {
        let x = bx_t(s) - t;
        let dx = dbx_t(s);
        if dx.abs() < 1e-6 {
            break;
        }
        s -= x / dx;
        s = s.clamp(0.0, 1.0);
    }
    // Bisection if Newton drifted; rare in practice for the standard CSS
    // ease curves which are well-conditioned.
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..16 {
        let x = bx_t(s) - t;
        if x.abs() < 1e-4 {
            break;
        }
        if x < 0.0 {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    by_t(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn linear_easing_is_identity() {
        assert!(approx(Easing::Linear.apply(0.0), 0.0));
        assert!(approx(Easing::Linear.apply(0.5), 0.5));
        assert!(approx(Easing::Linear.apply(1.0), 1.0));
    }

    #[test]
    fn ease_in_starts_slow_ends_fast() {
        let mid = Easing::EaseIn.apply(0.5);
        // t^3 at 0.5 = 0.125 - well below the linear midpoint.
        assert!(mid < 0.2);
        assert!(approx(Easing::EaseIn.apply(0.0), 0.0));
        assert!(approx(Easing::EaseIn.apply(1.0), 1.0));
    }

    #[test]
    fn ease_out_starts_fast_ends_slow() {
        let mid = Easing::EaseOut.apply(0.5);
        // 1 - (1 - 0.5)^3 = 0.875 - well above the linear midpoint.
        assert!(mid > 0.8);
        assert!(approx(Easing::EaseOut.apply(0.0), 0.0));
        assert!(approx(Easing::EaseOut.apply(1.0), 1.0));
    }

    #[test]
    fn ease_in_out_passes_through_midpoint() {
        assert!(approx(Easing::EaseInOut.apply(0.5), 0.5));
        // First half resembles ease-in.
        assert!(Easing::EaseInOut.apply(0.25) < 0.25);
        // Second half resembles ease-out.
        assert!(Easing::EaseInOut.apply(0.75) > 0.75);
    }

    #[test]
    fn cubic_bezier_matches_linear_anchors() {
        // (0.25, 0.1, 0.25, 1.0) is the classic CSS ease curve. Endpoints
        // must still pin at 0 and 1.
        assert!(approx(
            Easing::CubicBezier(0.25, 0.1, 0.25, 1.0).apply(0.0),
            0.0
        ));
        assert!(approx(
            Easing::CubicBezier(0.25, 0.1, 0.25, 1.0).apply(1.0),
            1.0
        ));
        // Midpoint of CSS ease is above 0.5.
        let mid = Easing::CubicBezier(0.25, 0.1, 0.25, 1.0).apply(0.5);
        assert!(mid > 0.5 && mid < 1.0);
    }

    #[test]
    fn transition_samples_linearly() {
        let t = Transition::<f32>::new(0.0, 10.0, Duration::from_millis(100), Easing::Linear);
        let halfway = t.start + Duration::from_millis(50);
        assert!(approx(t.sample(halfway), 5.0));
        let done = t.start + Duration::from_millis(100);
        assert!(approx(t.sample(done), 10.0));
        assert!(t.done(done));
    }

    #[test]
    fn transition_clamps_after_duration() {
        let t = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(100), Easing::Linear);
        let after = t.start + Duration::from_millis(200);
        assert!(approx(t.sample(after), 1.0));
        assert!(t.done(after));
    }

    #[test]
    fn transition_zero_duration_is_instant() {
        // Zero-duration transitions must not divide-by-zero; they jump
        // straight to `to`.
        let t = Transition::<f32>::new(0.0, 1.0, Duration::ZERO, Easing::Linear);
        assert!(approx(t.sample(t.start), 1.0));
        assert!(t.done(t.start));
    }

    #[test]
    fn from_tuple_builds_linear_transition() {
        let t: Transition<f32> = (0.0, 10.0, Duration::from_millis(100)).into();
        assert_eq!(t.easing, Easing::Linear);
        assert!(approx(t.sample(t.start + Duration::from_millis(50)), 5.0));
    }

    #[test]
    fn for_property_returns_last_declaration() {
        // CSS cascade-5: last declaration wins for a duplicated property.
        let specs = TransitionSpecs(vec![
            TransitionSpec {
                property: TransitionProperty::Opacity,
                duration: Duration::from_millis(100),
                easing: Easing::Linear,
            },
            TransitionSpec {
                property: TransitionProperty::Opacity,
                duration: Duration::from_millis(500),
                easing: Easing::EaseOut,
            },
        ]);
        let chosen = specs.for_property(TransitionProperty::Opacity).unwrap();
        assert_eq!(chosen.duration, Duration::from_millis(500));
        assert_eq!(chosen.easing, Easing::EaseOut);
    }

    #[test]
    fn retarget_starts_from_current_value_and_skips_noops() {
        let spec = TransitionSpec {
            property: TransitionProperty::BackgroundColor,
            duration: Duration::from_millis(200),
            easing: Easing::Linear,
        };
        let red = Color::rgb(1.0, 0.0, 0.0);
        let blue = Color::rgb(0.0, 0.0, 1.0);
        // Mid-flight current value (e.g. half-way between old endpoints).
        let mid = red.lerp(blue, 0.5);
        let t = retarget(mid, blue, &spec).expect("differing values start a tween");
        // CSS retarget rule: the new tween starts from the CURRENT
        // interpolated value, not either old endpoint.
        assert_eq!(t.sample(t.start), mid);
        assert_eq!(t.sample(t.start + Duration::from_millis(200)), blue);
        // Equal-value writes are no-ops - no zombie transitions.
        assert!(retarget(blue, blue, &spec).is_none());
    }

    #[test]
    fn background_driver_writes_solid_fill_and_retires() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(Tick::default());
        world.insert_resource(lumen_core::render_world::AnimationsActive::default());
        let from = Color::rgb(0.0, 0.0, 0.0);
        let to = Color::rgb(1.0, 1.0, 1.0);
        let e = world
            .spawn((
                Visuals {
                    fill: Some(Fill::Solid(from)),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
                BackgroundTransition(Transition::new(
                    from,
                    to,
                    Duration::from_millis(100),
                    Easing::Linear,
                )),
            ))
            .id();
        // Half-way: Tick.now is seeded at system run; shift the tween's
        // start back 50 ms so sampling lands mid-flight.
        world.get_mut::<BackgroundTransition>(e).unwrap().0.start -= Duration::from_millis(50);
        {
            let now =
                world.get::<BackgroundTransition>(e).unwrap().0.start + Duration::from_millis(50);
            world.resource_mut::<Tick>().now = now;
        }
        world.run_system_once(step_background_transitions).unwrap();
        let mid = world
            .get::<Visuals>(e)
            .unwrap()
            .fill
            .as_ref()
            .and_then(Fill::as_solid)
            .unwrap();
        assert!((mid.r - 0.5).abs() < 0.05, "mid-flight fill (r={})", mid.r);
        assert!(
            world.get::<BackgroundTransition>(e).is_some(),
            "tween still active mid-flight"
        );
        // Past the end: snaps to target and retires the component.
        {
            let now =
                world.get::<BackgroundTransition>(e).unwrap().0.start + Duration::from_millis(500);
            world.resource_mut::<Tick>().now = now;
        }
        world.run_system_once(step_background_transitions).unwrap();
        let done = world
            .get::<Visuals>(e)
            .unwrap()
            .fill
            .as_ref()
            .and_then(Fill::as_solid)
            .unwrap();
        assert_eq!(done, to);
        assert!(world.get::<BackgroundTransition>(e).is_none());
    }

    #[test]
    fn hidden_entity_ticks_no_animation() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(Tick::default());
        world.insert_resource(lumen_core::render_world::AnimationsActive::default());
        let e = world
            .spawn((
                Opacity(0.0),
                OpacityTransition(Transition::new(
                    0.0,
                    1.0,
                    Duration::from_millis(500),
                    Easing::Linear,
                )),
                lumen_core::components::Visible(false),
            ))
            .id();
        world.run_system_once(step_opacity_transitions).unwrap();
        // Hidden: snap to target, retire, no self-scheduled frame.
        assert_eq!(world.get::<Opacity>(e).unwrap().0, 1.0);
        assert!(world.get::<OpacityTransition>(e).is_none());
        assert!(
            !world
                .resource::<lumen_core::render_world::AnimationsActive>()
                .get(),
            "hidden transitions must not keep the loop awake"
        );
    }

    #[test]
    fn lerp_color() {
        let a = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        let b = Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        let mid = a.lerp(b, 0.5);
        assert!(approx(mid.r, 0.5));
        assert!(approx(mid.g, 0.5));
        assert!(approx(mid.b, 0.5));
        assert!(approx(mid.a, 0.5));
    }
}
