//! CSS transitions: the `transition` declarations an element carries and the
//! driver that tweens a property from where it shows to where the cascade
//! moved it.
//!
//! A running transition holds its property in
//! [`StyleOverrides`](lumen_core::style_overrides::StyleOverrides) under
//! [`TransitionHolder`], at the rank CSS gives transitions: above every
//! normal declaration and above keyframe animations. Each tick the driver
//! samples every running transition through the shared value math in
//! [`lumen_core::motion`] and holds the blend; when one ends it lets go and
//! the property shows what the cascade says now.
//!
//! [`Transition`] is the bare tween primitive, for motion that is not a CSS
//! transition (the switch thumb slide).

use bevy_ecs::prelude::*;
use bevy_ecs::world::EntityWorldMut;
use lumen_core::components::{Opacity, OutlineColor, TextStyle, Transform, Visible, Visuals};
use lumen_core::motion::{Easing, Interpolate};
use lumen_core::prelude::{Tick, TickStage};
use lumen_core::render_world::AnimationsActive;
use lumen_core::style_overrides::{Live, StyleFields, StyleOverrides, StyleOwner, StyleValues};
use lumen_core::time::{Duration, Instant};
use lumen_core::transform::ElementTransform;

/// One-shot tween between two values of an interpolable type, for motion
/// that is not a CSS transition.
///
/// [`Transition::sample`] returns the eased value at any instant;
/// [`Transition::done`] is true once `duration` has passed.
#[derive(Component, Clone, Debug)]
pub struct Transition<T: Interpolate + Clone + Send + Sync + 'static> {
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

impl<T: Interpolate + Clone + Send + Sync + 'static> Transition<T> {
    /// A transition that starts now and runs over `duration` with `easing`.
    pub fn new(from: T, to: T, duration: Duration, easing: Easing) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration,
            easing,
        }
    }

    /// Linear fraction of `duration` elapsed at `now`, clamped to `[0, 1]`.
    /// A zero-length transition reports `1` at once.
    pub fn progress(&self, now: Instant) -> f32 {
        progress(self.start, self.duration, now)
    }

    /// Eased value at `now`.
    pub fn sample(&self, now: Instant) -> T {
        let t = self.easing.apply(self.progress(now));
        self.from.interpolate(&self.to, t)
    }

    /// `true` once the transition has met or exceeded `duration`.
    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

fn progress(start: Instant, duration: Duration, now: Instant) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(start).as_secs_f32();
    (elapsed / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// A property a `transition` declaration can name.
///
/// Paint properties only: animating a layout property would run layout every
/// frame, so the CSS parser warns about one and drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitionProperty {
    /// CSS `opacity`.
    Opacity,
    /// CSS `background-color` (`background`, `bg`), a solid fill. Also
    /// times the hover and press tints in [`crate::hover`].
    BackgroundColor,
    /// CSS `color` (`text-color`).
    TextColor,
    /// CSS `border-color`.
    BorderColor,
    /// CSS `box-shadow` (`shadow`).
    BoxShadow,
    /// CSS `border-radius` (`radius`).
    BorderRadius,
    /// CSS `outline-color`: the focus outline's color.
    OutlineColor,
    /// CSS `transform`.
    Transform,
    /// CSS `translate`.
    Translate,
    /// CSS `rotate`.
    Rotate,
    /// CSS `scale`.
    Scale,
}

impl TransitionProperty {
    /// The held field the property lives in. The transform family shares
    /// one.
    pub fn field(self) -> StyleFields {
        match self {
            TransitionProperty::Opacity => StyleFields::OPACITY,
            TransitionProperty::BackgroundColor => StyleFields::FILL,
            TransitionProperty::TextColor => StyleFields::TEXT_COLOR,
            TransitionProperty::BorderColor => StyleFields::BORDER,
            TransitionProperty::BoxShadow => StyleFields::SHADOWS,
            TransitionProperty::BorderRadius => StyleFields::RADIUS,
            TransitionProperty::OutlineColor => StyleFields::OUTLINE_COLOR,
            TransitionProperty::Transform
            | TransitionProperty::Translate
            | TransitionProperty::Rotate
            | TransitionProperty::Scale => StyleFields::TRANSFORM,
        }
    }
}

/// Author-declared transition: one entry of a `transition` list.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionSpec {
    /// Which property this spec applies to.
    pub property: TransitionProperty,
    /// Length of the tween.
    pub duration: Duration,
    /// Easing curve.
    pub easing: Easing,
}

/// Every [`TransitionSpec`] declared for an entity, in source order.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct TransitionSpecs(pub Vec<TransitionSpec>);

impl TransitionSpecs {
    /// The last spec naming `property`: the last declaration wins, as CSS
    /// orders a property listed twice.
    pub fn for_property(&self, property: TransitionProperty) -> Option<&TransitionSpec> {
        self.0.iter().rev().find(|s| s.property == property)
    }

    /// The spec that times a change to `field`, with a non-zero duration.
    /// For the transform family the `transform` list's own spec wins over an
    /// individual property's.
    pub fn for_field(&self, field: StyleFields) -> Option<&TransitionSpec> {
        let mut found = None;
        for s in self.0.iter().rev() {
            if s.property.field() != field || s.duration.is_zero() {
                continue;
            }
            if s.property == TransitionProperty::Transform {
                return Some(s);
            }
            found = found.or(Some(s));
        }
        found
    }
}

/// What an element entering the tree resolved to with its `@starting-style`
/// rules applied: the values its transitions start from. Set by the restyle
/// that first resolves it and taken by the commit that follows.
#[derive(Component, Clone, Debug, Default)]
pub struct StartingStyle(pub StyleValues);

/// The [`StyleOverrides`] layer running transitions hold their properties
/// in, at rank 80: above keyframe animations, which CSS orders below
/// transitions, and above every normal declaration.
pub struct TransitionHolder;

impl StyleOwner for TransitionHolder {
    const RANK: u8 = 80;
}

/// The rank below which a holder is not a motion but a runtime system's own
/// state (a tint, a checked fill): a property such a holder covers is that
/// system's to animate, and a transition on it is dropped.
pub const MOTION_RANK: u8 = 70;

/// One property group a running transition moves.
#[derive(Clone, Debug)]
pub struct RunningTransition {
    /// The field it holds.
    pub field: StyleFields,
    /// Where it started: what showed when the cascade moved the property.
    pub from: StyleValues,
    /// Where it ends: what the cascade says.
    pub to: StyleValues,
    /// When it started.
    pub start: Instant,
    /// How long it runs.
    pub duration: Duration,
    /// Its timing function.
    pub easing: Easing,
}

impl RunningTransition {
    /// The value at `now`, for an element whose border box is `size`.
    pub fn sample(&self, now: Instant, size: glam::Vec2) -> StyleValues {
        let t = self.easing.apply(progress(self.start, self.duration, now));
        self.from.interpolate(&self.to, t, size)
    }

    /// Whether it has run its course at `now`.
    pub fn done(&self, now: Instant) -> bool {
        progress(self.start, self.duration, now) >= 1.0
    }
}

/// The transitions running on an element. Present only while one runs.
#[derive(Component, Clone, Debug, Default)]
pub struct ActiveTransitions(pub Vec<RunningTransition>);

impl ActiveTransitions {
    /// The running transition on `field`, if any.
    pub fn on(&self, field: StyleFields) -> Option<&RunningTransition> {
        self.0.iter().find(|r| r.field == field)
    }
}

/// The value a transform transition starts from: `from`, with the parts of
/// the transform `spec` does not name already at `to`'s, since only the
/// named property tweens and a change to the others snaps.
fn transform_start(spec: TransitionProperty, from: &StyleValues, to: &StyleValues) -> StyleValues {
    let (Some(from_t), Some(to_t)) = (&from.transform, &to.transform) else {
        return from.clone();
    };
    let none = ElementTransform::default();
    let mut start = from_t.clone().unwrap_or_default();
    let target = to_t.as_ref().unwrap_or(&none);
    if spec != TransitionProperty::Transform {
        start.functions = target.functions.clone();
    }
    if spec != TransitionProperty::Translate {
        start.translate = target.translate;
    }
    if spec != TransitionProperty::Rotate {
        start.rotate = target.rotate;
    }
    if spec != TransitionProperty::Scale {
        start.scale = target.scale;
    }
    start.origin = target.origin;
    StyleValues {
        transform: Some(Some(start)),
        ..from.clone()
    }
}

/// Start a transition of `field` on an element: from `from`, what shows
/// now, to `to`, what the cascade says, timed by `spec` and starting at
/// `now`. Holds `from` at once, so the frame the cascade moved shows no
/// jump. A transition already running on the field is replaced.
pub fn start_transition(
    ent: &mut EntityWorldMut<'_>,
    field: StyleFields,
    from: StyleValues,
    to: StyleValues,
    spec: &TransitionSpec,
    now: Instant,
) {
    let from = if field == StyleFields::TRANSFORM {
        transform_start(spec.property, &from, &to)
    } else {
        from
    }
    .restricted(field);
    let running = RunningTransition {
        field,
        from: from.clone(),
        to: to.restricted(field),
        start: now,
        duration: spec.duration,
        easing: spec.easing.clone(),
    };
    let mut active = ent.take::<ActiveTransitions>().unwrap_or_default();
    active.0.retain(|r| r.field != field);
    let others = active
        .0
        .iter()
        .fold(StyleFields::empty(), |f, r| f | r.field);
    active.0.push(running);
    ent.insert(active);
    // The other running transitions keep the values they hold; the new one
    // joins at its start.
    let mut held = ent
        .get::<StyleOverrides>()
        .and_then(|o| o.held_by::<TransitionHolder>())
        .cloned()
        .unwrap_or_default()
        .restricted(others);
    held.overlay(&from);
    lumen_core::style_overrides::hold_on::<TransitionHolder>(ent, held);
}

/// Stop the transition of `field` on an element, if one runs, and let the
/// property show what the cascade says.
pub fn cancel_transition(ent: &mut EntityWorldMut<'_>, field: StyleFields) {
    let Some(mut active) = ent.get_mut::<ActiveTransitions>() else {
        return;
    };
    let before = active.0.len();
    active.0.retain(|r| r.field != field);
    if active.0.len() == before {
        return;
    }
    if active.0.is_empty() {
        ent.remove::<ActiveTransitions>();
        lumen_core::style_overrides::release_on::<TransitionHolder>(ent);
        return;
    }
    let left = active
        .0
        .iter()
        .fold(StyleFields::empty(), |f, r| f | r.field);
    let kept = ent
        .get::<StyleOverrides>()
        .and_then(|o| o.held_by::<TransitionHolder>())
        .cloned()
        .unwrap_or_default()
        .restricted(left);
    lumen_core::style_overrides::hold_on::<TransitionHolder>(ent, kept);
}

/// The components a transition writes, read and written in place.
type Written = (
    Option<&'static mut Visuals>,
    Option<&'static mut TextStyle>,
    Option<&'static mut Opacity>,
    Option<&'static mut ElementTransform>,
    Option<&'static mut OutlineColor>,
);

type WrittenItem<'w, 's> = <Written as bevy_ecs::query::QueryData>::Item<'w, 's>;

/// Lay `v` onto the components in `w`, inserting or removing through
/// `commands` the ones a value adds or takes away. Shared by every holder
/// that drives the motion components from a system.
pub fn write_motion_values(
    commands: &mut Commands,
    entity: Entity,
    w: &mut WrittenItem<'_, '_>,
    v: &StyleValues,
) {
    if let Some(vis) = w.0.as_mut() {
        v.write_visuals(vis);
    }
    if let Some(text) = w.1.as_mut() {
        v.write_text(text);
    }
    match (v.opacity, w.2.as_mut()) {
        (Some(Some(o)), Some(cur)) => {
            if cur.0 != o {
                cur.0 = o;
            }
        }
        (Some(Some(o)), None) => {
            commands.entity(entity).try_insert(Opacity(o));
        }
        (Some(None), Some(_)) => {
            commands.entity(entity).try_remove::<Opacity>();
        }
        _ => {}
    }
    match (&v.transform, w.3.as_mut()) {
        (Some(Some(x)), Some(cur)) => {
            if **cur != *x {
                **cur = x.clone();
            }
        }
        (Some(Some(x)), None) => {
            commands.entity(entity).try_insert(x.clone());
        }
        (Some(None), Some(_)) => {
            commands.entity(entity).try_remove::<ElementTransform>();
        }
        _ => {}
    }
    match (v.outline_color, w.4.as_mut()) {
        (Some(Some(c)), Some(cur)) => {
            if cur.0 != c {
                cur.0 = c;
            }
        }
        (Some(Some(c)), None) => {
            commands.entity(entity).try_insert(OutlineColor(c));
        }
        (Some(None), Some(_)) => {
            commands.entity(entity).try_remove::<OutlineColor>();
        }
        _ => {}
    }
}

/// The live values in `w`, for a holder taking a field nobody held yet.
pub fn live_of<'a>(w: &'a WrittenItem<'_, '_>) -> Live<'a> {
    Live {
        style: None,
        visuals: w.0.as_deref(),
        text: w.1.as_deref(),
        opacity: w.2.as_deref(),
        transform: w.3.as_deref(),
        outline_color: w.4.as_deref(),
    }
}

/// The query data [`write_motion_values`] writes through.
pub type MotionComponents = Written;

/// Sample every running transition and hold the blend.
///
/// A transition whose property a runtime system's own state now covers (a
/// hover tint, a checked fill) is dropped: that system animates it. A
/// hidden element (`Visible(false)` on itself) runs no transitions, as CSS
/// runs none in a `display: none` subtree: they finish at once. While one
/// runs the loop is kept awake.
#[allow(clippy::type_complexity)]
pub fn step_transitions(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<AnimationsActive>,
    mut q: Query<(
        Entity,
        &mut ActiveTransitions,
        &mut StyleOverrides,
        Option<&Transform>,
        Option<&Visible>,
        Written,
    )>,
) {
    let now = tick.now;
    for (entity, mut active, mut overrides, layout, visible, mut written) in &mut q {
        let hidden = visible.is_some_and(|v| !v.0);
        let covered = overrides.held_below(MOTION_RANK);
        active
            .0
            .retain(|r| !hidden && !r.done(now) && !covered.intersects(r.field));
        if active.0.is_empty() {
            let out = overrides.release::<TransitionHolder>();
            write_motion_values(&mut commands, entity, &mut written, &out);
            commands.entity(entity).try_remove::<ActiveTransitions>();
            continue;
        }
        let size = layout.map_or(glam::Vec2::ZERO, |t| t.size);
        let mut held = StyleValues::default();
        for r in &active.0 {
            held.overlay(&r.sample(now, size));
        }
        let out = overrides.hold::<TransitionHolder>(held, &live_of(&written));
        write_motion_values(&mut commands, entity, &mut written, &out);
        anim.request();
    }
}

/// Plugin: the transition driver, in `TickStage::Systems`.
pub struct TransitionPlugin;

impl lumen_core::prelude::Plugin for TransitionPlugin {
    fn build(self, app: &mut lumen_core::prelude::App) {
        // After the hover and press tints, so a tint that took a property
        // this tick drops the transition on it before it paints.
        app.add_systems(
            TickStage::Systems,
            step_transitions.after(crate::hover::apply_press_tint),
        );
    }
}

/// The instant the tick being run started at, which every motion samples
/// against; the wall clock when no tick runs.
pub fn tick_now(ent: &EntityWorldMut<'_>) -> Instant {
    ent.world()
        .get_resource::<Tick>()
        .map_or_else(Instant::now, |t| t.now)
}

/// Start the opacity transition an element mounting into the tree takes
/// when it declares `transition: opacity`: from fully transparent to its own
/// opacity. What `<if>` and `<for>` give an element they mount.
pub fn fade_in_on_mount(ent: &mut EntityWorldMut<'_>) {
    // A page's browser fades its own elements in, from the stylesheet.
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let now = tick_now(ent);
    // An opacity its starting style gives it already sets where the fade
    // starts.
    if ent
        .get::<ActiveTransitions>()
        .is_some_and(|a| a.on(StyleFields::OPACITY).is_some())
    {
        return;
    }
    let Some(spec) = ent
        .get::<TransitionSpecs>()
        .and_then(|s| s.for_field(StyleFields::OPACITY))
        .cloned()
    else {
        return;
    };
    let target = ent.get::<Opacity>().map(|o| o.0);
    if target.unwrap_or(1.0) <= 0.0 {
        return;
    }
    start_transition(
        ent,
        StyleFields::OPACITY,
        StyleValues {
            opacity: Some(Some(0.0)),
            ..StyleValues::default()
        },
        StyleValues {
            opacity: Some(target),
            ..StyleValues::default()
        },
        &spec,
        now,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::{Color, Fill};

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn transition_samples_linearly_and_clamps() {
        let t = Transition::<f32>::new(0.0, 10.0, Duration::from_millis(100), Easing::Linear);
        assert!(approx(t.sample(t.start + Duration::from_millis(50)), 5.0));
        assert!(approx(t.sample(t.start + Duration::from_millis(200)), 10.0));
        assert!(t.done(t.start + Duration::from_millis(100)));
        let zero = Transition::<f32>::new(0.0, 1.0, Duration::ZERO, Easing::Linear);
        assert!(approx(zero.sample(zero.start), 1.0));
    }

    #[test]
    fn for_property_returns_last_declaration() {
        let specs = TransitionSpecs(vec![
            TransitionSpec {
                property: TransitionProperty::Opacity,
                duration: Duration::from_millis(100),
                easing: Easing::Linear,
            },
            TransitionSpec {
                property: TransitionProperty::Opacity,
                duration: Duration::from_millis(500),
                easing: Easing::EASE_OUT,
            },
        ]);
        let chosen = specs.for_property(TransitionProperty::Opacity).unwrap();
        assert_eq!(chosen.duration, Duration::from_millis(500));
        assert_eq!(chosen.easing, Easing::EASE_OUT);
    }

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(Tick::default());
        world.insert_resource(AnimationsActive::default());
        world
    }

    fn spec(property: TransitionProperty, ms: u64) -> TransitionSpec {
        TransitionSpec {
            property,
            duration: Duration::from_millis(ms),
            easing: Easing::Linear,
        }
    }

    fn fill(c: Color) -> StyleValues {
        StyleValues {
            fill: Some(Some(Fill::Solid(c))),
            ..StyleValues::default()
        }
    }

    fn solid(world: &World, e: Entity) -> Color {
        world
            .get::<Visuals>(e)
            .unwrap()
            .fill
            .as_ref()
            .and_then(Fill::as_solid)
            .unwrap()
    }

    #[test]
    fn a_transition_holds_its_blend_and_lets_go_to_the_cascade() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = world();
        let black = Color::rgb(0.0, 0.0, 0.0);
        let white = Color::rgb(1.0, 1.0, 1.0);
        let e = world
            .spawn(Visuals {
                fill: Some(Fill::Solid(black)),
                ..Visuals::default()
            })
            .id();
        let start = world.resource::<Tick>().now;
        start_transition(
            &mut world.entity_mut(e),
            StyleFields::FILL,
            fill(black),
            fill(white),
            &spec(TransitionProperty::BackgroundColor, 100),
            start,
        );
        // The cascade moved to white while the transition holds black.
        let shown = world
            .get_mut::<StyleOverrides>(e)
            .unwrap()
            .rebase(&fill(white));
        assert_eq!(shown.fill, Some(Some(Fill::Solid(black))));
        assert_eq!(solid(&world, e), black, "starts where it showed");

        world.resource_mut::<Tick>().now = start + Duration::from_millis(50);
        world.run_system_once(step_transitions).unwrap();
        world.flush();
        let mid = solid(&world, e);
        assert!(approx(mid.r, 0.5), "halfway (r={})", mid.r);
        assert!(world.resource::<AnimationsActive>().get());

        world.resource_mut::<Tick>().now = start + Duration::from_millis(150);
        world.run_system_once(step_transitions).unwrap();
        world.flush();
        assert_eq!(solid(&world, e), white, "lands on the cascade's value");
        assert!(world.get::<ActiveTransitions>(e).is_none());
        assert!(world.get::<StyleOverrides>(e).unwrap().is_empty());
    }

    #[test]
    fn a_hidden_element_finishes_its_transitions_at_once() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = world();
        let e = world.spawn((Opacity(1.0), Visible(false))).id();
        let now = world.resource::<Tick>().now;
        start_transition(
            &mut world.entity_mut(e),
            StyleFields::OPACITY,
            StyleValues {
                opacity: Some(Some(0.0)),
                ..StyleValues::default()
            },
            StyleValues {
                opacity: Some(Some(1.0)),
                ..StyleValues::default()
            },
            &spec(TransitionProperty::Opacity, 500),
            now,
        );
        assert_eq!(world.get::<Opacity>(e), Some(&Opacity(0.0)));
        world.run_system_once(step_transitions).unwrap();
        world.flush();
        assert_eq!(world.get::<Opacity>(e), Some(&Opacity(1.0)));
        assert!(world.get::<ActiveTransitions>(e).is_none());
        assert!(!world.resource::<AnimationsActive>().get());
    }

    #[test]
    fn only_the_named_transform_property_tweens() {
        use lumen_core::transform::LengthPct;
        let from = StyleValues {
            transform: Some(None),
            ..StyleValues::default()
        };
        let to = StyleValues {
            transform: Some(Some(ElementTransform {
                rotate: 90.0,
                translate: (LengthPct::px(10.0), LengthPct::default()),
                ..ElementTransform::default()
            })),
            ..StyleValues::default()
        };
        let start = transform_start(TransitionProperty::Rotate, &from, &to);
        let x = start.transform.unwrap().unwrap();
        assert_eq!(x.rotate, 0.0, "rotate tweens from where it was");
        assert_eq!(x.translate.0, LengthPct::px(10.0), "translate snaps");
    }
}
