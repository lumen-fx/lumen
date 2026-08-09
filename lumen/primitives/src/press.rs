//! Press primitive: long-press detection + double-click recognizer.
//!
//! Both signals are derived from existing low-level state:
//!
//! * Long-press = entity carrying [`Pressed`] continuously for more than
//!   `long_press_threshold` (default 500 ms). Fires [`LongPressEvent`] once
//!   per press cycle. The marker [`PressStartedAt`] is auto-attached on
//!   [`Pressed`] insertion and removed on [`Pressed`] removal so a fresh
//!   press always starts a fresh timer.
//! * Double-click = two consecutive [`ClickEvent`]s on the same entity
//!   within `double_click_threshold` (default 300 ms). Tracked in the
//!   [`LastClick`] resource.
//!
//! Apps consume [`LongPressEvent`] / [`DoubleClickEvent`] just like any
//! other Message - `MessageReader<LongPressEvent>` / `MessageReader<DoubleClickEvent>`.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_core::render_world::AnimationsActive;
use std::time::{Duration, Instant};

/// Default long-press threshold. Matches the W3C touch UI guideline.
pub const DEFAULT_LONG_PRESS_MS: u64 = 500;
/// Default double-click window. Matches GTK / Windows defaults (~300ms).
pub const DEFAULT_DOUBLE_CLICK_MS: u64 = 300;
/// Maximum pointer travel between two clicks for them to still count as a
/// double-click. Matches the GTK / Qt double-click slop (~5px): two
/// clicks at opposite corners of one large entity are two singles, not a
/// double.
pub const DOUBLE_CLICK_RADIUS_PX: f32 = 5.0;

/// Plugin: registers press-recognizer systems.
pub struct PressPlugin {
    /// Threshold above which a continuous press fires [`LongPressEvent`].
    pub long_press: Duration,
    /// Maximum gap between two clicks on the same entity to count as a
    /// [`DoubleClickEvent`].
    pub double_click: Duration,
}

impl Default for PressPlugin {
    fn default() -> Self {
        Self {
            long_press: Duration::from_millis(DEFAULT_LONG_PRESS_MS),
            double_click: Duration::from_millis(DEFAULT_DOUBLE_CLICK_MS),
        }
    }
}

impl Plugin for PressPlugin {
    fn build(self, app: &mut App) {
        app.world.insert_resource(PressConfig {
            long_press: self.long_press,
            double_click: self.double_click,
        });
        app.world.insert_resource(LastClick::default());
        // attach_press_timer must run AFTER lumen-input::dispatch_clicks
        // for the same Commands-deferred reason as the drag primitive.
        app.add_systems(
            TickStage::Systems,
            attach_press_timer.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            detect_long_press.after(attach_press_timer),
        );
        app.add_systems(
            TickStage::Systems,
            detect_double_click.after(attach_press_timer),
        );
    }
}

/// Tunables. Re-read every frame so apps can mutate at runtime.
#[derive(Resource, Clone, Copy, Debug)]
pub struct PressConfig {
    /// See [`PressPlugin::long_press`].
    pub long_press: Duration,
    /// See [`PressPlugin::double_click`].
    pub double_click: Duration,
}

/// Auto-attached to every entity the moment it acquires [`Pressed`].
/// Carries the [`Instant`] the press began. Removed once the entity
/// releases. Also marks whether a `LongPressEvent` has already fired for
/// this press cycle so we don't spam.
#[derive(Component, Clone, Copy, Debug)]
pub struct PressStartedAt {
    /// When the [`Pressed`] marker was first observed on this entity.
    pub at: Instant,
    /// Set once a [`LongPressEvent`] fires for this press cycle.
    pub long_fired: bool,
}

/// Last accepted click - drives the double-click detector.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct LastClick {
    /// Most recent click target + timestamp + position. `None` until first click.
    pub last: Option<(Entity, Instant, glam::Vec2)>,
}

/// Maintain [`PressStartedAt`]: attach on first frame an entity carries
/// [`Pressed`] without a timer, remove when [`Pressed`] disappears.
pub fn attach_press_timer(
    mut commands: Commands,
    just_pressed: Query<Entity, (With<Pressed>, Without<PressStartedAt>)>,
    just_released: Query<Entity, (With<PressStartedAt>, Without<Pressed>)>,
) {
    let now = Instant::now();
    for e in &just_pressed {
        commands.entity(e).insert(PressStartedAt {
            at: now,
            long_fired: false,
        });
    }
    for e in &just_released {
        commands.entity(e).remove::<PressStartedAt>();
    }
}

/// Emit [`LongPressEvent`] for entities pressed past the threshold. Once
/// per press cycle (gated by `PressStartedAt::long_fired`).
pub fn detect_long_press(
    cfg: Res<PressConfig>,
    anim: Option<Res<AnimationsActive>>,
    mut q: Query<(Entity, &mut PressStartedAt), With<Pressed>>,
    mut events: MessageWriter<LongPressEvent>,
) {
    let now = Instant::now();
    for (entity, mut state) in &mut q {
        if state.long_fired {
            continue;
        }
        if now.duration_since(state.at) >= cfg.long_press {
            state.long_fired = true;
            events.write(LongPressEvent { entity });
        } else if let Some(anim) = &anim {
            // Threshold not yet reached: keep the frame loop awake so the
            // timer is re-evaluated on the next tick. A held press on a
            // static screen produces no OS events; without this ping the
            // loop parks once the press-tint tween settles (~120ms) and
            // the 500ms long-press fires late or never.
            anim.request();
        }
    }
}

/// Detect double-click by tracking the last-seen [`ClickEvent`] target +
/// time. Two clicks on the same entity within `cfg.double_click` fire a
/// [`DoubleClickEvent`]; the cache resets so a third click within the
/// window is a *new* first click.
pub fn detect_double_click(
    cfg: Res<PressConfig>,
    mut last: ResMut<LastClick>,
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageWriter<DoubleClickEvent>,
) {
    let now = Instant::now();
    for click in clicks.read() {
        if let Some((prev_e, prev_t, prev_pos)) = last.last
            && prev_e == click.entity
            && now.duration_since(prev_t) <= cfg.double_click
            && (click.position - prev_pos).length() <= DOUBLE_CLICK_RADIUS_PX
        {
            doubles.write(DoubleClickEvent {
                entity: click.entity,
                position: click.position,
            });
            last.last = None;
            continue;
        }
        last.last = Some((click.entity, now, click.position));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    fn world_with_press(at: Instant, long_fired: bool) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(PressConfig {
            long_press: Duration::from_millis(500),
            double_click: Duration::from_millis(300),
        });
        world.insert_resource(AnimationsActive::default());
        world.init_resource::<Messages<LongPressEvent>>();
        let e = world
            .spawn((Pressed, PressStartedAt { at, long_fired }))
            .id();
        world.resource::<AnimationsActive>().clear();
        (world, e)
    }

    /// A held press below the threshold must ping [`AnimationsActive`] so
    /// the frame loop keeps ticking and actually re-evaluates the timer on
    /// an otherwise static screen - without it the long-press fires late
    /// or never.
    #[test]
    fn pending_long_press_keeps_loop_awake() {
        let (mut world, _e) = world_with_press(Instant::now(), false);
        world.run_system_once(detect_long_press).unwrap();
        assert!(
            world.resource::<AnimationsActive>().get(),
            "a pending long-press must request an animation frame"
        );
        assert!(
            world.resource::<Messages<LongPressEvent>>().is_empty(),
            "the threshold has not elapsed yet, so no event fires"
        );
    }

    /// Once the threshold elapses the event fires and the press is marked
    /// fired; it must stop requesting frames from then on.
    #[test]
    fn elapsed_long_press_fires_then_stops_requesting() {
        let past = Instant::now() - Duration::from_millis(600);
        let (mut world, e) = world_with_press(past, false);
        world.run_system_once(detect_long_press).unwrap();
        assert!(
            !world.resource::<Messages<LongPressEvent>>().is_empty(),
            "past-threshold press fires LongPressEvent"
        );
        assert!(
            world.get::<PressStartedAt>(e).unwrap().long_fired,
            "the press is marked fired so it won't spam"
        );

        // Second pass: already fired -> must not keep the loop awake.
        world.resource::<AnimationsActive>().clear();
        world.run_system_once(detect_long_press).unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "an already-fired long-press must stop requesting frames"
        );
    }
}
