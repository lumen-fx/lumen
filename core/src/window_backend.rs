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
//! window entity, the messages a window backend writes and reads, and the
//! redraw state. A backend installs it when it runs; a headless run installs
//! it alone, plus [`HeadlessWindowPlugin`], which stands in for the platform
//! window, so systems that read window state behave the same with no window
//! open.
//!
//! [`CloseRequests`] and [`run_close_request`] are the close path every
//! driver shares: the app's `window.close()` / `app.quit()` requests, and the
//! veto tick that lets an `on_close` hook keep the window open.

use crate::app::{App, Plugin};
use crate::backends::{BackendEntry, Backends};
use crate::input::{CloseReason, CloseRequest, WindowFocused, WindowOccluded};
use crate::render_world::{FrameDirty, Viewport, install_extract_pipeline};
use crate::text_events::{ImeSurroundingRequested, ImeSurroundingResponse, TextEditRequest};
use crate::traits::WindowBackend;
use crate::window::{PrimaryWindow, QuitRequest, Window, WindowRequest};
use crate::window_state;
use bevy_ecs::message::{MessageCursor, Messages};
use bevy_ecs::prelude::*;

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
/// Spawns the primary window entity ([`Window`] + [`PrimaryWindow`]), sized
/// from the main world's [`Viewport`] and titled from the app's
/// [`crate::components::A11yRootLabel`] when it has one; the driver that
/// opens the window seeds both with what it opened through
/// [`seed_primary_window`]. Registers the messages a window backend writes
/// ([`WindowFocused`], [`WindowOccluded`], the IME surrounding-text pair, and
/// [`TextEditRequest`] for IME commits) and the requests it obeys
/// ([`WindowRequest`], [`QuitRequest`]), inserts a default
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
        app.add_message::<WindowRequest>();
        app.add_message::<QuitRequest>();
        app.world.insert_resource(RedrawScheduler::default());
        if crate::window::primary_window(&mut app.world).is_none() {
            let size = app
                .world
                .get_resource::<Viewport>()
                .map(|v| v.size)
                .unwrap_or_default();
            let title = app
                .world
                .get_resource::<crate::components::A11yRootLabel>()
                .map(|l| l.0.clone())
                .unwrap_or_default();
            let window = app
                .world
                .spawn((Window { title, size }, PrimaryWindow))
                .id();
            window_state::publish_primary(Some(window));
        }
    }
}

/// Record what the driver opened as the primary window's state: the title
/// and logical size the platform window (or the headless stand-in) has. The
/// write is the platform reporting, not the app asking, so nothing applies
/// it back. Publishes the title for the `window.title()` getter.
pub fn seed_primary_window(world: &mut World, title: &str, size: glam::Vec2) {
    let Some(entity) = crate::window::primary_window(world) else {
        return;
    };
    if let Some(mut window) = world.get_mut::<Window>(entity) {
        let window = window.bypass_change_detection();
        window.title = title.to_string();
        window.size = size;
    }
    window_state::publish_title(title);
}

/// The window stand-in for a run with no display.
///
/// With no platform window between the app and its viewport, a change to
/// the primary [`Window`] applies at once: a new size becomes the viewport
/// (both worlds, so an offscreen renderer draws at it) and lays out on the
/// same tick, and a new title is what `window.title()` reports. Installed by
/// the headless runners beside [`WindowCorePlugin`]; a window backend never
/// installs it, since there the platform decides what the window gets.
pub struct HeadlessWindowPlugin;

impl Plugin for HeadlessWindowPlugin {
    fn build(self, app: &mut App) {
        app.add_systems(
            crate::tick::TickStage::Systems,
            apply_window_headless.in_set(WindowApply),
        );
        app.add_extract_fn(mirror_viewport_to_render);
    }
}

/// The systems that apply the app's [`Window`] changes to a window. A system
/// that writes [`Window`] in [`crate::tick::TickStage::Systems`] runs before
/// this set, so its change applies on the same tick.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct WindowApply;

/// [`HeadlessWindowPlugin`]'s apply step.
///
/// A [`Window`] the app wrote since it was spawned becomes the viewport and
/// the published title. A viewport written from outside (a runner seeding
/// it, a test sizing it) is the size the stand-in window has, so it is
/// written back to [`Window`] the way a backend reports a resize. Telling
/// the two apart by the change ticks keeps the first tick, when everything
/// reads as changed, from undoing a viewport set after the window spawned.
fn apply_window_headless(
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    viewport: Option<ResMut<Viewport>>,
    frame: Option<ResMut<FrameDirty>>,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let asked = window.is_changed() && window.last_changed() != window.added();
    if asked {
        window_state::publish_title(&window.title);
    }
    let Some(mut viewport) = viewport else {
        return;
    };
    if asked {
        if is_window_size(window.size.x, window.size.y) && viewport.size != window.size {
            viewport.size = window.size;
            if let Some(mut frame) = frame {
                frame.dirty = true;
            }
        }
    } else if viewport.is_changed() && viewport.size != window.size {
        window.bypass_change_detection().size = viewport.size;
    }
}

/// Carry the main world's viewport size and scale to the render world's, which
/// a headless run has no platform window to keep current.
fn mirror_viewport_to_render(main: &mut World, render: &mut World) {
    let Some((size, scale)) = main
        .get_resource::<Viewport>()
        .map(|v| (v.size, v.scale_factor))
    else {
        return;
    };
    if let Some(mut viewport) = render.get_resource_mut::<Viewport>()
        && (viewport.size != size || viewport.scale_factor != scale)
    {
        viewport.size = size;
        viewport.scale_factor = scale;
    }
}

/// Whether `width` x `height` is a size a window can have: both sides finite
/// and positive.
pub fn is_window_size(width: f32, height: f32) -> bool {
    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0
}

/// The app's close and quit requests, read once each.
///
/// A driver keeps one and calls [`Self::take`] after every tick; a request
/// means it runs [`run_close_request`] with the reason returned.
#[derive(Default)]
pub struct CloseRequests {
    close: MessageCursor<WindowRequest>,
    quit: MessageCursor<QuitRequest>,
}

impl CloseRequests {
    /// The close the app asked for since the last call, if any. A quit wins
    /// over a window close made in the same tick.
    pub fn take(&mut self, world: &World) -> Option<CloseReason> {
        let quit = world
            .get_resource::<Messages<QuitRequest>>()
            .is_some_and(|msgs| self.quit.read(msgs).count() > 0);
        let close = world
            .get_resource::<Messages<WindowRequest>>()
            .is_some_and(|msgs| {
                self.close
                    .read(msgs)
                    .filter(|r| matches!(r, WindowRequest::Close))
                    .count()
                    > 0
            });
        if quit {
            Some(CloseReason::AppQuit)
        } else if close {
            Some(CloseReason::WindowClose)
        } else {
            None
        }
    }
}

/// Run the close veto path: write `CloseRequest { reason, vetoed: false }`,
/// run one tick so every close hook (a script's `on_close`, the C ABI's
/// `lumen_app_on_close`, an app system reading [`CloseRequest`]) sees it, and
/// report whether the close goes ahead. A hook keeps the window open by
/// writing `CloseRequest { vetoed: true, .. }` during that tick.
///
/// `requests` is drained after the veto tick: a hook that asks for a close
/// while answering one does not start a second round.
pub fn run_close_request(app: &mut App, requests: &mut CloseRequests, reason: CloseReason) -> bool {
    if let Some(mut msgs) = app.world.get_resource_mut::<Messages<CloseRequest>>() {
        msgs.write(CloseRequest {
            reason,
            vetoed: false,
        });
    }
    app.tick();
    let _ = requests.take(&app.world);
    let vetoed = app
        .world
        .get_resource::<Messages<CloseRequest>>()
        .map(|msgs| msgs.iter_current_update_messages().any(|m| m.vetoed))
        .unwrap_or(false);
    !vetoed
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
