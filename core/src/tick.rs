//! Main-world tick stages and the [`Tick`] resource.
//!
//! - Each stage is a `bevy_ecs` [`SystemSet`].
//! - Ordering is enforced by `.chain()` in [`crate::app::App::new`].
//! - The render schedule runs after the main schedule and the extract step; see [`crate::render_world`].

use crate::plugin_events::plugin_events_pending;
use crate::property_store::external_properties_pending;
use crate::render_world::{AnimationsActive, FrameDirty};
use crate::time::{Duration, Instant};
use bevy_ecs::prelude::*;

/// The five ordered main-world stages of a Lumen tick.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TickStage {
    /// Ingests OS events (keyboard, mouse, IME, window). Window backend writes here.
    Input,
    /// Drains the bounded [`crate::command::CommandQueue`] and applies deferred mutations.
    CommandDrain,
    /// Runs application systems: state mutation, animations, scripts.
    Systems,
    /// Runs the layout engine: dirty flush, taffy round-trip, absolute-coord write-back.
    LayoutSync,
    /// Computes the accessibility-tree diff and pushes it to the OS.
    A11ySync,
}

/// Per-tick frame clock resource.
///
/// - [`Self::now`] is captured at the start of each [`crate::app::App::tick`] before the [`TickStage::Input`] systems run.
/// - [`Self::dt`] is `now - previous_now` (zero on the first tick).
/// - [`Self::frame`] is a monotonic counter incremented once per tick (starts at 0; reaches 1 on the first tick).
///
/// Wave 1 migrates the animation primitives off [`Instant::now`] to read this resource so headless tests can
/// drive deterministic frame clocks; foundation only installs and updates the resource.
#[derive(Resource, Clone, Copy, Debug)]
pub struct Tick {
    /// Wall-clock instant captured at the start of the current tick.
    pub now: Instant,
    /// Elapsed time since the previous tick's [`Self::now`]. Zero on the first tick.
    pub dt: Duration,
    /// Monotonic tick counter; 0 before the first tick, 1 after, and so on.
    pub frame: u64,
}

impl Default for Tick {
    fn default() -> Self {
        Self {
            now: Instant::now(),
            dt: Duration::ZERO,
            frame: 0,
        }
    }
}

impl Tick {
    /// Advances the clock by capturing a fresh `Instant::now()` and bumping [`Self::frame`].
    /// Called by [`crate::app::App::tick`] at the top of each tick, before the main schedule runs.
    pub fn advance(&mut self) {
        let now = Instant::now();
        self.dt = now.saturating_duration_since(self.now);
        self.now = now;
        self.frame = self.frame.wrapping_add(1);
    }
}

/// Whether the tick that just ran left work behind, so a driver that only
/// wakes on events has to schedule another frame.
///
/// Five sources, each of which reaches `false` on its own once the system
/// settles, so a caller that loops on this can never spin forever:
///
/// 1. The external typed-property bus still holds undrained writes, from a
///    cross-thread producer or a main-thread script write that landed after
///    this tick's drain. It empties once drained.
/// 2. The plugin-event bus still holds events a portable plugin pushed (see
///    [`crate::plugin_events`]). It likewise empties once drained.
/// 3. A driver reported unfinished work this tick through
///    [`AnimationsActive`]: a hover or press tween, an opacity transition,
///    scroll inertia, an element still waiting on content that has not
///    arrived. The flag is cleared at the top of every tick and re-raised
///    only while the thing it is keyed on is still outstanding, and every
///    driver keys its claim on something that settles or leaves the tree.
/// 4. [`FrameDirty`] is still set, which a system dirtying state after the
///    encode leaves behind. The next present clears it.
/// 5. A [`WakeDeadline`] requested this tick has already passed. A driver
///    that reaches a deadline hands its work out on the tick that follows,
///    so the claim settles with it.
///
/// This is a frame predicate, not a state predicate: an app with a permanent
/// animation raises the third source forever. A caller that needs to know
/// when an app's *state* stopped moving compares the state itself, as the
/// prerenderer does.
pub fn work_pending(world: &World) -> bool {
    external_properties_pending()
        || plugin_events_pending()
        || world
            .get_resource::<AnimationsActive>()
            .is_some_and(|a| a.get())
        || world.get_resource::<FrameDirty>().is_some_and(|f| f.dirty)
        || wake_deadline(world).is_some_and(|at| at <= Instant::now())
}

/// The earliest instant a driver asked for another tick at, if any.
///
/// The companion of [`work_pending`] for work that is due later rather than
/// now: a driver that parks on events alone sleeps until this instant (winit's
/// `ControlFlow::WaitUntil`, a timed park) instead of polling, and ticks when
/// it passes. `None` means nothing is scheduled and the loop may wait for an
/// event indefinitely.
pub fn wake_deadline(world: &World) -> Option<Instant> {
    world
        .get_resource::<WakeDeadline>()
        .and_then(WakeDeadline::get)
}

/// Per-tick "tick again no later than this" request, the timed counterpart of
/// [`AnimationsActive`].
///
/// A system whose work comes due at a known instant with nothing else to wake
/// the loop (a script timer, a delayed transition) calls [`Self::request`]
/// with that instant. Several requests keep the earliest. Like
/// [`AnimationsActive`], [`reset_wake_deadline`] clears it at the top of every
/// tick and each driver re-requests only while its deadline is still
/// outstanding, so a loop that settles has nothing left to wake for.
///
/// Interior-mutable so drivers can request through a shared `Res`.
#[derive(Resource, Debug, Default)]
pub struct WakeDeadline(std::sync::Mutex<Option<Instant>>);

impl WakeDeadline {
    /// Ask for a tick no later than `at`. Keeps the earliest of all requests
    /// made since the last [`Self::clear`].
    pub fn request(&self, at: Instant) {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *slot = Some(slot.map_or(at, |cur| cur.min(at)));
    }

    /// The earliest instant requested since the last [`Self::clear`].
    pub fn get(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop every request. Called by [`reset_wake_deadline`] at tick start.
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Clears [`WakeDeadline`] at the top of every tick (registered in
/// [`TickStage::Input`], chained before the `Systems` stage where drivers
/// request), so a deadline nobody renews stops waking the loop.
pub fn reset_wake_deadline(deadline: Res<WakeDeadline>) {
    deadline.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_earliest_request_wins_until_cleared() {
        let deadline = WakeDeadline::default();
        assert_eq!(deadline.get(), None);
        let now = Instant::now();
        deadline.request(now + Duration::from_secs(5));
        deadline.request(now + Duration::from_secs(2));
        deadline.request(now + Duration::from_secs(9));
        assert_eq!(deadline.get(), Some(now + Duration::from_secs(2)));
        deadline.clear();
        assert_eq!(deadline.get(), None);
    }

    #[test]
    fn only_a_passed_deadline_is_pending_work() {
        let mut world = World::new();
        world.init_resource::<WakeDeadline>();
        assert!(!work_pending(&world));
        let later = Instant::now() + Duration::from_secs(60);
        world.resource::<WakeDeadline>().request(later);
        assert!(!work_pending(&world), "a future deadline is not due yet");
        assert_eq!(wake_deadline(&world), Some(later));
        world.resource::<WakeDeadline>().request(Instant::now());
        assert!(work_pending(&world), "a passed deadline is due");
    }
}
