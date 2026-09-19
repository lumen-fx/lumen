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
/// Four sources, each of which reaches `false` on its own once the system
/// settles, so a caller that loops on this can never spin forever:
///
/// 1. The external typed-property bus still holds undrained writes, from a
///    cross-thread producer or a main-thread script write that landed after
///    this tick's drain. It empties once drained.
/// 2. The plugin-event bus still holds events a portable plugin pushed (see
///    [`crate::plugin_events`]). It likewise empties once drained.
/// 3. An animation driver (a hover or press tween, an opacity transition,
///    scroll inertia) reported motion this tick through [`AnimationsActive`],
///    which is cleared at the top of every tick and re-raised only while a
///    value is mid-flight.
/// 4. [`FrameDirty`] is still set, which a system dirtying state after the
///    encode leaves behind. The next present clears it.
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
}
