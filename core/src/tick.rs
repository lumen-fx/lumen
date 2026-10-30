//! Main-world tick stages and the [`Tick`] resource.
//!
//! - Each stage is a `bevy_ecs` [`SystemSet`].
//! - Ordering is enforced by `.chain()` in [`crate::app::App::new`].
//! - The render schedule runs after the main schedule and the extract step; see [`crate::render_world`].

use crate::plugin_events::plugin_events_pending;
use crate::property_store::{PropertyStore, external_properties_pending};
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

/// Where an embedder's app logic runs inside [`TickStage::Systems`].
///
/// The runtime orders the set after the tick's input has been turned into
/// clicks and other element events and after every control has pushed its
/// value into its signal, and before the readers that write signals back
/// into the tree. A system in it reads this tick's events and the current
/// control values, and a signal it writes reaches the screen on the same
/// tick, with no ordering of its own against engine systems. A module whose
/// systems consume what app logic produced (a canvas encoding the draws of
/// the tick) orders itself after this set.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct AppSet;

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

/// When the last tick ran, phase by phase: what an app reads to measure its
/// own frame time.
///
/// A main-world resource [`crate::app::App::tick`] fills in as the tick goes
/// and finishes once the tick has painted, so a system reading it during a
/// tick sees the tick before: [`Self::frame`] is one less than
/// [`Tick::frame`]. A window backend presents after the tick returns and adds
/// [`Self::presented`] then, before the next tick starts.
///
/// The render-side instants come from the renderer, through the render
/// world's [`RenderTimestamps`]; a renderer that cannot tell a phase apart
/// leaves it `None`. A tick that painted nothing (its frame was not dirty)
/// has every render-side field `None`.
///
/// The instants are taken on the CPU. A GPU renderer's
/// [`Self::submitted`] is when it handed the frame to the device, not when
/// the device finished drawing it.
#[derive(Resource, Clone, Copy, Debug)]
pub struct FrameTiming {
    /// The [`Tick::frame`] this describes; 0 until the first tick finishes.
    pub frame: u64,
    /// The tick started: its [`Tick::now`].
    pub start: Instant,
    /// The main schedule finished: input, app systems, style, layout and
    /// accessibility are done.
    pub update_end: Instant,
    /// The render world holds this tick's scene.
    pub extract_end: Option<Instant>,
    /// The renderer finished building the frame's draw commands.
    pub encoded: Option<Instant>,
    /// The renderer handed the frame to the device (GPU), or finished
    /// rasterizing it (CPU).
    pub submitted: Option<Instant>,
    /// The render schedule returned.
    pub render_end: Option<Instant>,
    /// The frame went on screen; set by a window backend once it presents.
    pub presented: Option<Instant>,
}

impl Default for FrameTiming {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            frame: 0,
            start: now,
            update_end: now,
            extract_end: None,
            encoded: None,
            submitted: None,
            render_end: None,
            presented: None,
        }
    }
}

impl FrameTiming {
    /// The latest instant this records: the end of the tick as far as it got.
    pub fn end(&self) -> Instant {
        self.presented
            .or(self.render_end)
            .or(self.submitted)
            .or(self.encoded)
            .or(self.extract_end)
            .unwrap_or(self.update_end)
    }

    /// From the tick's start to [`Self::end`].
    pub fn duration(&self) -> Duration {
        self.end().saturating_duration_since(self.start)
    }

    /// Whether the tick painted a frame.
    pub fn painted(&self) -> bool {
        self.extract_end.is_some()
    }
}

/// The instants a renderer reports for the frame it is drawing, in the render
/// world. [`crate::app::App::tick`] clears them before the render schedule
/// runs and copies them into the main world's [`FrameTiming`] after; a
/// renderer stamps the ones it can tell apart with [`Self::mark_encoded`],
/// [`Self::mark_submitted`] and [`Self::mark_presented`].
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderTimestamps {
    /// See [`FrameTiming::encoded`].
    pub encoded: Option<Instant>,
    /// See [`FrameTiming::submitted`].
    pub submitted: Option<Instant>,
    /// See [`FrameTiming::presented`].
    pub presented: Option<Instant>,
}

impl RenderTimestamps {
    /// The frame's draw commands are built.
    pub fn mark_encoded(&mut self) {
        self.encoded = Some(Instant::now());
    }

    /// The frame is handed to the device, or rasterized.
    pub fn mark_submitted(&mut self) {
        self.submitted = Some(Instant::now());
    }

    /// The frame is on screen.
    pub fn mark_presented(&mut self) {
        self.presented = Some(Instant::now());
    }
}

/// Whether the tick that just ran left work behind, so a driver that only
/// wakes on events has to schedule another frame.
///
/// Six sources, each of which reaches `false` on its own once the system
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
/// 6. The [`PropertyStore`] still holds writes past the end of the tick:
///    ones that landed after the `bind-*` readers had read the queue, kept
///    for the next tick's readers (see [`PropertyStore::observe_dirty`]). The
///    next tick applies them and lets them go.
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
        || world
            .get_resource::<PropertyStore>()
            .is_some_and(|store| !store.dirty_peek().is_empty())
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

/// Per-tick "content the page shows is still arriving" flag: an image still
/// decoding on a worker thread, say.
///
/// The counterpart of a browser document that has not reached its `load`:
/// what is on screen is not yet everything the page asked for. A driver
/// whose content arrives off the main thread raises it every tick while any
/// element still waits on it, keyed on the element so the claim goes away
/// with it, and [`reset_loads_pending`] clears it at the top of every tick.
/// A run that has to show a page complete (a screenshot in a test) ticks
/// until it stays down.
///
/// Interior-mutable so drivers can raise it through a shared `Res`.
#[derive(Resource, Debug, Default)]
pub struct LoadsPending(std::sync::atomic::AtomicBool);

impl LoadsPending {
    /// Raise the flag: something the page shows is still loading.
    pub fn request(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a driver raised the flag since the last [`Self::clear`].
    pub fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lower the flag. Called by [`reset_loads_pending`] at tick start.
    pub fn clear(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Clears [`LoadsPending`] at the top of every tick (registered in
/// [`TickStage::Input`], chained before the `Systems` stage where drivers
/// raise it), so the flag says what the last tick saw.
pub fn reset_loads_pending(loads: Res<LoadsPending>) {
    loads.clear();
}

/// Whether the last tick left content still loading ([`LoadsPending`]).
pub fn loads_pending(world: &World) -> bool {
    world
        .get_resource::<LoadsPending>()
        .is_some_and(LoadsPending::get)
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
