//! The window backends a binary carries, and the window-free state every
//! window backend shares with the app.
//!
//! Each backend registers a [`WindowBackendEntry`] into the app's
//! [`WindowBackends`] from its own crate, and an interactive launch runs
//! the app through the highest-priority one's
//! [`crate::traits::WindowBackend::run`]. The core and the runtime name no
//! backend.
//!
//! [`WindowCorePlugin`] is the half of a window that needs no display: the
//! messages a window backend writes and the redraw state it reads. A
//! backend installs it when it runs; a headless run installs it alone, so
//! systems that read window state behave the same with no window open.

use crate::app::{App, Plugin};
use crate::backends::{BackendEntry, Backends};
use crate::input::{WindowFocused, WindowOccluded};
use crate::render_world::install_extract_pipeline;
use crate::text_events::{ImeSurroundingRequested, ImeSurroundingResponse, TextEditRequest};
use crate::traits::WindowBackend;
use bevy_ecs::prelude::Resource;

/// One window backend: its name, its rank, and how to make it.
#[derive(Clone, Copy, Debug)]
pub struct WindowBackendEntry {
    /// The name the backend is registered under.
    pub name: &'static str,
    /// Preference: the highest-priority backend is the one a launch runs.
    pub priority: i32,
    /// The backend, not yet running. Construction does no work;
    /// [`WindowBackend::run`] opens the window.
    pub backend: fn() -> Box<dyn WindowBackend>,
}

impl BackendEntry for WindowBackendEntry {
    const KIND: &'static str = "window";

    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }
}

/// Main-world registry of the window backends this binary carries. A
/// backend's capability install adds itself with
/// [`crate::backends::register_backend`].
pub type WindowBackends = Backends<WindowBackendEntry>;

/// Redraw pacing: whether a frame is wanted, and whether the window can
/// show one.
///
/// Other systems request a paint by writing `pending = true`; the window
/// backend asks the platform for a redraw only while the window is
/// visible (not occluded). That keeps an idle app from repainting at the
/// display rate, the way Qt gates `requestUpdate` on `isExposed()` and GTK
/// drives its frame clock.
///
/// The pump gates on visibility, not focus. On tiling window managers
/// (Hyprland, sway) an unfocused window stays fully on-screen, so it must
/// keep animating: a module's off-thread position pump, restyle tweens, and
/// scroll inertia all ride the redraw loop, and freezing them the moment
/// focus leaves would stop a slider mid-drag. Only an occluded or minimized
/// window parks.
///
/// The window backend owns `focused`, `occluded`, and `paused`, and calls
/// [`Self::recompute_paused`] when the first two change. Other code reads
/// them and writes only `pending`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct RedrawScheduler {
    /// Set to `true` to request a paint the next time the event loop goes
    /// idle. Cleared once the paint is dispatched.
    pub pending: bool,
    /// `true` when the window has keyboard focus. Tracked for the
    /// [`WindowFocused`] message and the focus-return repaint; not part of
    /// the pump gate (see [`RedrawScheduler::compute_paused`]).
    pub focused: bool,
    /// `true` when the window is fully occluded (covered by another window
    /// or moved off-screen).
    pub occluded: bool,
    /// Whether redraw requests are held back: recomputed whenever
    /// `occluded` or `focused` changes, and true only while the window is
    /// occluded.
    pub paused: bool,
}

impl Default for RedrawScheduler {
    fn default() -> Self {
        // Start focused and visible so the first frame paints. The platform
        // reports focus and visibility shortly after the window opens; until
        // then the app should render rather than show an empty surface.
        Self {
            pending: true,
            focused: true,
            occluded: false,
            paused: false,
        }
    }
}

impl RedrawScheduler {
    /// Recompute [`Self::paused`] after a change to `focused` or
    /// `occluded`.
    pub fn recompute_paused(&mut self) {
        self.paused = Self::compute_paused(self.focused, self.occluded);
    }

    /// The pump gate: should the redraw loop park?
    ///
    /// Only occlusion parks the loop. `focused` is accepted and ignored, so
    /// a visible but unfocused window (the tiling window manager case) keeps
    /// animating.
    pub fn compute_paused(_focused: bool, occluded: bool) -> bool {
        occluded
    }

    /// Whether the backend should ask the platform for a redraw: a paint is
    /// pending and the window is not parked. Anything that wants a frame
    /// while unfocused (a worker thread's
    /// [`crate::app::EventLoopWaker`], a restyle tween, scroll inertia) sets
    /// `pending`, and this gate lets it through.
    pub fn should_forward_redraw(&self) -> bool {
        self.pending && !self.paused
    }
}

/// The window-free half of every window backend.
///
/// Registers the messages a window backend writes ([`WindowFocused`],
/// [`WindowOccluded`], the IME surrounding-text pair, and
/// [`TextEditRequest`] for IME commits), inserts a default
/// [`RedrawScheduler`], and installs the extract pipeline a window presents
/// from. A window backend installs it before it opens a window unless the
/// app already has it; a headless run installs it alone.
///
/// `CloseRequest` is not registered here: [`App::new`] registers it, so
/// close hooks work headless too, and registering a message twice cycles
/// its buffer twice per tick, dropping writes made before the tick.
pub struct WindowCorePlugin;

impl Plugin for WindowCorePlugin {
    fn build(self, app: &mut App) {
        // A window presents frames, so it is what asks for the pipeline that
        // produces them; an app that is never shown installs none of it.
        install_extract_pipeline(app);
        app.add_message::<WindowFocused>();
        app.add_message::<WindowOccluded>();
        app.add_message::<ImeSurroundingRequested>();
        app.add_message::<ImeSurroundingResponse>();
        app.add_message::<TextEditRequest>();
        app.world.insert_resource(RedrawScheduler::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::backends::{register_backend, registered};
    use crate::traits::{A11yBridgeFactory, Renderer, WindowError};
    use crate::window::WindowOptions;

    /// A backend that runs nothing and reports what it was handed.
    struct Inert;

    impl WindowBackend for Inert {
        fn run(
            self: Box<Self>,
            _app: App,
            _options: WindowOptions,
            renderers: Vec<Box<dyn Renderer>>,
            _a11y: Option<A11yBridgeFactory>,
        ) -> Result<(), WindowError> {
            if renderers.is_empty() {
                Err(WindowError::NoRenderer)
            } else {
                Ok(())
            }
        }
    }

    fn inert() -> Box<dyn WindowBackend> {
        Box::new(Inert)
    }

    /// A launch runs the backend the registry ranks first, through the
    /// trait.
    #[test]
    fn the_preferred_backend_runs_the_app() {
        let mut app = App::new();
        register_backend(
            &mut app,
            WindowBackendEntry {
                name: "inert",
                priority: 0,
                backend: inert,
            },
        );
        let entry = registered::<WindowBackendEntry>(&app)
            .preferred()
            .expect("registered");
        let err = (entry.backend)()
            .run(App::new(), WindowOptions::default(), Vec::new(), None)
            .expect_err("no renderer");
        assert!(matches!(err, WindowError::NoRenderer));
    }

    /// The window-free half gives a headless app the same window state a
    /// windowed one starts with.
    #[test]
    fn the_core_plugin_installs_the_window_state() {
        let mut app = App::new();
        app.add_plugin(WindowCorePlugin);
        assert!(app.world.resource::<RedrawScheduler>().pending);
        assert!(
            app.world
                .contains_resource::<bevy_ecs::message::Messages<WindowFocused>>()
        );
        assert!(
            app.world
                .contains_resource::<bevy_ecs::message::Messages<TextEditRequest>>()
        );
    }

    /// The pump-gate policy: only occlusion parks the loop. Focus is not a
    /// factor, so a visible-but-unfocused window (Hyprland/sway, where an
    /// unfocused window is still fully on-screen) keeps animating. This is
    /// the "slider freeze while unfocused on a tiling WM" fix.
    #[test]
    fn visibility_not_focus_gates_the_pump() {
        // Visible + focused -> run.
        assert!(!RedrawScheduler::compute_paused(true, false));
        // Visible + UNFOCUSED -> still run (the regression this fixes).
        assert!(!RedrawScheduler::compute_paused(false, false));
        // Occluded -> park, regardless of focus.
        assert!(RedrawScheduler::compute_paused(true, true));
        assert!(RedrawScheduler::compute_paused(false, true));
    }

    fn scheduler(pending: bool, focused: bool, occluded: bool) -> RedrawScheduler {
        let mut s = RedrawScheduler {
            pending,
            focused,
            occluded,
            paused: false,
        };
        s.recompute_paused();
        s
    }

    /// `about_to_wait` forwards a `request_redraw` iff `should_forward_redraw`.
    /// A pending paint raised while UNFOCUSED-but-VISIBLE (a worker thread's
    /// `EventLoopWaker`, a restyle tween, or scroll inertia all set `pending`)
    /// must forward; an occluded window must not.
    #[test]
    fn forward_redraw_wakes_unfocused_visible_but_parks_occluded() {
        // A pump or tween in flight while unfocused-but-visible: it set
        // `pending = true`; the gate must forward it.
        assert!(scheduler(true, false, false).should_forward_redraw());
        // Focused + pending: unchanged from before the fix.
        assert!(scheduler(true, true, false).should_forward_redraw());
        // Nothing pending (idle) while unfocused-visible: stay damage-driven,
        // do not busy-repaint an unchanging frame.
        assert!(!scheduler(false, false, false).should_forward_redraw());
        // Occluded / minimized: park even with work pending (battery win).
        assert!(!scheduler(true, false, true).should_forward_redraw());
        assert!(!scheduler(true, true, true).should_forward_redraw());
    }
}
