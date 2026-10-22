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
//! messages a window backend writes and reads, the redraw state, and the
//! per-window focus memory. A backend installs it when it runs; a headless
//! run installs it alone, plus [`HeadlessWindowPlugin`], which stands in for
//! the platform windows on a virtual desktop, so systems that read window
//! state behave the same with no window open.
//!
//! [`CloseRequests`] and [`run_close_request`] are the close path every
//! driver shares: the app's `window.close()` / `app.quit()` requests, the
//! veto tick that lets an `on_close` hook keep a window open, and what an
//! unvetoed close does to the app ([`crate::window::QuitPolicy`]).

use crate::app::{App, Plugin};
use crate::backends::{BackendEntry, Backends};
use crate::input::{
    CloseReason, CloseRequest, FocusTracker, FocusVisible, Focused, WindowFocused, WindowOccluded,
};
use crate::render_world::install_extract_pipeline;
use crate::text_events::{ImeSurroundingRequested, ImeSurroundingResponse, TextEditRequest};
use crate::traits::WindowBackend;
use crate::window::{
    LastFocus, MonitorInfo, Monitors, PrimaryWindow, QuitPolicy, QuitRequest, Window,
    WindowClosing, WindowPartitions, WindowRequest, WindowRuntime, WindowViewport,
};
use crate::window_state;
use bevy_ecs::message::{MessageCursor, Messages};
use bevy_ecs::prelude::*;
use glam::{IVec2, UVec2};

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

/// Redraw pacing for the whole app: whether a frame is wanted, and whether
/// any window can show one.
///
/// The app ticks once per frame however many windows it has. Other systems
/// request a tick by writing `pending = true`; the window backend asks the
/// platform for a redraw only while some window is visible (not occluded),
/// then presents every window whose frame changed. That keeps an idle app
/// from repainting at the display rate, the way Qt gates `requestUpdate` on
/// `isExposed()` and GTK drives its frame clock. Each window's own state
/// (focused, occluded, minimized) lives in its
/// [`crate::window::WindowRuntime`]; this resource holds the roll-up.
///
/// The pump gates on visibility, not focus. On tiling window managers
/// (Hyprland, sway) an unfocused window stays fully on-screen, so it must
/// keep animating: a module's off-thread position pump, restyle tweens, and
/// scroll inertia all ride the redraw loop, and freezing them the moment
/// focus leaves would stop a slider mid-drag. Only an app whose every window
/// is occluded or minimized parks.
///
/// The window backend owns `focused`, `occluded`, and `paused`, and calls
/// [`Self::recompute_paused`] when the first two change. Other code reads
/// them and writes only `pending`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct RedrawScheduler {
    /// Set to `true` to request a paint the next time the event loop goes
    /// idle. Cleared once the paint is dispatched.
    pub pending: bool,
    /// `true` when one of the app's windows has keyboard focus. Tracked
    /// for the focus-return repaint; not part of the pump gate (see
    /// [`RedrawScheduler::compute_paused`]).
    pub focused: bool,
    /// `true` when every window is fully occluded (covered by other
    /// windows, minimized, or moved off-screen).
    pub occluded: bool,
    /// Whether redraw requests are held back: recomputed whenever
    /// `occluded` or `focused` changes, and true only while every window is
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
    /// pending and some window is not parked. Anything that wants a frame
    /// while unfocused (a worker thread's
    /// [`crate::app::EventLoopWaker`], a restyle tween, scroll inertia) sets
    /// `pending`, and this gate lets it through.
    pub fn should_forward_redraw(&self) -> bool {
        self.pending && !self.paused
    }

    /// Recompute the roll-up from every window's [`WindowRuntime`]: focused
    /// when one window is, occluded when all are. A backend calls it after
    /// it records a window's focus or visibility.
    pub fn roll_up(&mut self, windows: impl IntoIterator<Item = WindowRuntime>) {
        let mut any = false;
        let mut focused = false;
        let mut occluded = true;
        for runtime in windows {
            any = true;
            focused |= runtime.focused;
            occluded &= runtime.occluded || runtime.minimized;
        }
        if any {
            self.focused = focused;
            self.occluded = occluded;
            self.recompute_paused();
        }
    }
}

/// The window-free half of every window backend.
///
/// Registers the messages a window backend writes ([`WindowFocused`],
/// [`WindowOccluded`], the IME surrounding-text pair, and
/// [`TextEditRequest`] for IME commits) and the requests it obeys
/// ([`WindowRequest`], [`QuitRequest`], [`WindowClosing`]), inserts a
/// default [`RedrawScheduler`] and [`Monitors`], installs the extract
/// pipeline a window presents from, and keeps each window's
/// [`LastFocus`]: focus leaving a window remembers the element it was on,
/// focus coming back restores it, and focusing an element in a window
/// that does not have focus asks the backend to raise that window. A window
/// backend installs it before it opens a window unless the app already has
/// it; a headless run installs it alone.
///
/// The primary window entity itself comes from [`App::new`], so an app
/// that never shows a window still has the viewport it lays out against.
/// `CloseRequest` is registered there too, so close hooks work headless,
/// and registering a message twice cycles its buffer twice per tick,
/// dropping writes made before the tick.
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
        app.add_message::<WindowClosing>();
        app.world.insert_resource(RedrawScheduler::default());
        app.world.init_resource::<Monitors>();
        app.world.init_resource::<QuitPolicy>();
        // The window bookkeeping: one system, run only while a surface root
        // exists or one was just added or removed, so an app with one window
        // never dispatches it. The observers flag a surface root coming or
        // going.
        app.world.add_observer(
            |_: On<Add, crate::window::WindowOf>, partitions: ResMut<WindowPartitions>| {
                crate::window::mark_partitions_stale(partitions)
            },
        );
        app.world.add_observer(
            |_: On<Remove, crate::window::WindowOf>, partitions: ResMut<WindowPartitions>| {
                crate::window::mark_partitions_stale(partitions)
            },
        );
        app.add_systems(
            crate::tick::TickStage::LayoutSync,
            sync_windows
                .before(crate::layout_backend::LayoutSolve)
                .run_if(windows_in_play),
        );
    }
}

/// Run condition for [`sync_windows`]: some element shows in a window other
/// than the primary one, or a surface root was just added or removed.
pub fn windows_in_play(partitions: Option<Res<WindowPartitions>>) -> bool {
    partitions.is_some_and(|p| p.in_play())
}

/// What [`sync_windows`] needs for focus: the focus events of each window,
/// the focused element, each window's remembered element, and the requests
/// it makes of the window backend.
#[derive(bevy_ecs::system::SystemParam)]
pub struct WindowFocusBook<'w, 's> {
    commands: Commands<'w, 's>,
    events: MessageReader<'w, 's, WindowFocused>,
    tracker: ResMut<'w, FocusTracker>,
    last: Query<'w, 's, &'static LastFocus>,
    alive: Query<'w, 's, (), Without<bevy_ecs::resource::IsResource>>,
    runtimes: Query<'w, 's, &'static WindowRuntime>,
    requests: MessageWriter<'w, WindowRequest>,
    raised_for: Local<'s, Option<Entity>>,
}

/// The window bookkeeping of an app showing content in more than one
/// window, in one system so an app with one window pays for none of it:
///
/// - refreshes [`WindowPartitions`], which window each element shows in;
/// - focus leaving a window records the element it was on as the window's
///   [`LastFocus`], and focus coming back puts it on that element again when
///   the element still exists and still shows in that window;
/// - focusing an element in a window that does not have keyboard focus,
///   while another of the app's windows does, asks the window backend to
///   raise the element's window (`focus()` on an element in another window);
/// - a window other than the primary one clears its frame to its surface
///   root's background colour, so the area the root leaves uncovered matches
///   it.
///
/// Runs before layout, so a surface root a system added this tick lays out
/// in its window on the same tick.
pub fn sync_windows(
    mut partitions: ResMut<WindowPartitions>,
    mut tree: crate::window::PartitionTree,
    mut focus: WindowFocusBook,
    (visuals, mut viewports): (
        Query<&crate::components::Visuals>,
        Query<&mut WindowViewport>,
    ),
) {
    partitions.refresh(&mut tree);
    let partitions = &*partitions;
    let focus = &mut focus;
    for event in focus.events.read() {
        let in_window = |e: Entity| partitions.in_window(e, event.window);
        if !event.focused {
            if let Some(focused) = focus.tracker.0.filter(|e| in_window(*e))
                && let Ok(mut window) = focus.commands.get_entity(event.window)
            {
                window.insert(LastFocus(Some(focused)));
            }
            continue;
        }
        let Some(restore) = focus.last.get(event.window).ok().and_then(|l| l.0) else {
            continue;
        };
        if focus.tracker.0 == Some(restore) || !focus.alive.contains(restore) || !in_window(restore)
        {
            continue;
        }
        if let Some(prev) = focus.tracker.0
            && let Ok(mut e) = focus.commands.get_entity(prev)
        {
            e.remove::<(Focused, FocusVisible)>();
        }
        focus.commands.entity(restore).insert(Focused);
        focus.tracker.0 = Some(restore);
    }
    if *focus.raised_for != focus.tracker.0 {
        *focus.raised_for = focus.tracker.0;
        if let Some(window) = focus.tracker.0.and_then(|e| partitions.window_of(e)) {
            let window_focused = focus.runtimes.get(window).is_ok_and(|r| r.focused);
            let app_focused = focus.runtimes.iter().any(|r| r.focused);
            if app_focused && !window_focused {
                focus.requests.write(WindowRequest::Focus(window));
            }
        }
    }
    for &(root, window) in partitions.surface_roots() {
        let Some(crate::components::Fill::Solid(color)) =
            visuals.get(root).ok().and_then(|v| v.fill.as_ref())
        else {
            continue;
        };
        if let Ok(mut viewport) = viewports.get_mut(window)
            && viewport.clear != *color
        {
            viewport.clear = *color;
        }
    }
}

/// Record what the driver opened as `window`'s state: the title and logical
/// size the platform window (or the headless stand-in) has. The write is the
/// platform reporting, not the app asking, so nothing applies it back.
/// Publishes the title for the `window.title()` getter.
pub fn seed_window(world: &mut World, window: Entity, title: &str, size: glam::Vec2) {
    if let Some(mut wanted) = world.get_mut::<Window>(window) {
        let wanted = wanted.bypass_change_detection();
        wanted.title = title.to_string();
        wanted.size = size;
    }
    window_state::publish_title(Some(window), title);
}

/// The headless stand-in's desktop: whether it honours window positions the
/// way a desktop that places windows does, or ignores them the way Wayland
/// does.
#[derive(Resource, Clone, Copy, Debug)]
pub struct HeadlessDesktop {
    /// Apply [`Window::position`]; off, windows are placed by cascading
    /// from the primary monitor's corner, as a compositor would.
    pub positioning: bool,
}

impl Default for HeadlessDesktop {
    fn default() -> Self {
        Self { positioning: true }
    }
}

/// The monitor a headless run has when it is given none: one 1920x1080
/// screen at `scale`.
pub fn default_virtual_monitor(scale: f32) -> MonitorInfo {
    MonitorInfo {
        name: "virtual-1".into(),
        position: IVec2::ZERO,
        size: UVec2::new(1920, 1080),
        work_area: (IVec2::ZERO, UVec2::new(1920, 1080)),
        scale,
    }
}

/// The window stand-in for a run with no display.
///
/// Every window entity gets an offscreen frame of its own on a virtual
/// desktop ([`Monitors`], one 1920x1080 screen unless the run gives others).
/// With no platform window between the app and its viewports, a change to a
/// [`Window`] applies at once: a new size becomes the viewport and lays out
/// on the same tick, a new title is what `window.title()` reports, a new
/// position moves the window and takes the scale of the monitor it lands on.
/// A new window opens focused. Installed by the headless runners beside
/// [`WindowCorePlugin`]; a window backend never installs it, since there the
/// platform decides what each window gets.
pub struct HeadlessWindowPlugin;

impl Plugin for HeadlessWindowPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<HeadlessDesktop>();
        if app
            .world
            .get_resource::<Monitors>()
            .is_none_or(|m| m.0.is_empty())
        {
            let scale = crate::window::primary_viewport(&app.world).map_or(1.0, |v| v.scale_factor);
            app.world
                .insert_resource(Monitors(vec![default_virtual_monitor(scale)]));
        }
        app.add_systems(
            crate::tick::TickStage::Systems,
            apply_windows_headless.in_set(WindowApply),
        );
    }
}

/// The systems that apply the app's [`Window`] changes to a window. A system
/// that writes [`Window`] in [`crate::tick::TickStage::Systems`] runs before
/// this set, so its change applies on the same tick.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct WindowApply;

/// Where the headless desktop puts the `n`th window it places with no
/// position of its own: cascaded from the corner of the primary monitor's
/// work area.
fn cascade(monitors: &Monitors, n: i32) -> IVec2 {
    let origin = monitors.0.first().map_or(IVec2::ZERO, |m| m.work_area.0);
    origin + IVec2::splat(32 * n)
}

/// [`HeadlessWindowPlugin`]'s apply step.
///
/// A [`Window`] the app wrote since it was spawned becomes its viewport, its
/// published title and its place on the desktop. A viewport written from
/// outside (a runner seeding it, a test sizing it) is the size the stand-in
/// window has, so it is written back to [`Window`] the way a backend reports
/// a resize. Telling the two apart by the change ticks keeps the first tick,
/// when everything reads as changed, from undoing a viewport set after the
/// window spawned.
#[allow(clippy::type_complexity)]
fn apply_windows_headless(
    mut windows: Query<(
        Entity,
        &mut Window,
        &mut WindowViewport,
        &mut WindowRuntime,
        Has<PrimaryWindow>,
    )>,
    monitors: Res<Monitors>,
    desktop: Res<HeadlessDesktop>,
    mut focus: MessageWriter<WindowFocused>,
    mut placed: Local<i32>,
) {
    let mut opened: Option<Entity> = None;
    for (entity, mut window, mut viewport, mut runtime, primary) in &mut windows {
        let added = window.is_added();
        let asked = window.is_changed() && window.last_changed() != window.added();
        if added || asked {
            window_state::publish_title(Some(entity), &window.title);
        }
        if asked {
            if crate::window_backend::is_window_size(window.size.x, window.size.y)
                && viewport.size != window.size
            {
                viewport.size = window.size;
            }
        } else if added && !primary {
            // A secondary window opens at the size it asks for; the primary
            // one at the size the runner seeded its viewport with.
            if crate::window_backend::is_window_size(window.size.x, window.size.y)
                && viewport.size != window.size
            {
                viewport.size = window.size;
            }
        } else if viewport.is_changed() && viewport.size != window.size {
            window.bypass_change_detection().size = viewport.size;
        }
        // Placement: a new window lands where it asked (when the desktop
        // positions windows) or cascades; a moved one follows its request.
        let wants = window.position.filter(|_| desktop.positioning);
        let place = match (runtime.position, wants) {
            (None, Some(p)) => Some(p),
            (None, None) => {
                let p = if primary {
                    cascade(&monitors, 0)
                } else {
                    *placed += 1;
                    cascade(&monitors, *placed)
                };
                Some(p)
            }
            (Some(at), Some(p)) if asked && at != p => Some(p),
            _ => None,
        };
        if let Some(p) = place {
            runtime.position = Some(p);
            let center = p + (window.size * viewport.scale_factor / 2.0).as_ivec2();
            if let Some(monitor) = monitors.at(center).or_else(|| monitors.0.first()) {
                if runtime.monitor.as_deref() != Some(monitor.name.as_str()) {
                    runtime.monitor = Some(monitor.name.clone());
                }
                if viewport.scale_factor != monitor.scale && !(primary && added) {
                    viewport.scale_factor = monitor.scale;
                }
            }
        }
        if added && !primary {
            opened = Some(entity);
        } else if added && primary && !runtime.focused {
            runtime.focused = true;
        }
    }
    // A new window takes focus from whichever window had it, as a desktop
    // raises the window it just opened.
    if let Some(new) = opened {
        for (entity, _, _, mut runtime, _) in &mut windows {
            let want = entity == new;
            if runtime.focused != want {
                runtime.focused = want;
                focus.write(WindowFocused {
                    window: entity,
                    focused: want,
                });
            }
        }
    }
}

/// Whether `width` x `height` is a size a window can have: both sides finite
/// and positive.
pub fn is_window_size(width: f32, height: f32) -> bool {
    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0
}

/// What the app asked to close since the driver last looked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseAsk {
    /// `app.quit()`: every window, and the app.
    Quit,
    /// `window.close()` on these windows, in the order asked.
    Windows(Vec<Entity>),
}

/// The app's close and quit requests, read once each.
///
/// A driver keeps one and calls [`Self::take`] after every tick; a request
/// means it runs [`run_close_request`] with what was asked.
#[derive(Default)]
pub struct CloseRequests {
    close: MessageCursor<WindowRequest>,
    quit: MessageCursor<QuitRequest>,
}

impl CloseRequests {
    /// The closes the app asked for since the last call, if any. A quit wins
    /// over window closes made in the same tick.
    pub fn take(&mut self, world: &World) -> Option<CloseAsk> {
        let quit = world
            .get_resource::<Messages<QuitRequest>>()
            .is_some_and(|msgs| self.quit.read(msgs).count() > 0);
        let mut windows: Vec<Entity> = Vec::new();
        if let Some(msgs) = world.get_resource::<Messages<WindowRequest>>() {
            for request in self.close.read(msgs) {
                if let WindowRequest::Close(window) = request
                    && !windows.contains(window)
                {
                    windows.push(*window);
                }
            }
        }
        if quit {
            Some(CloseAsk::Quit)
        } else if windows.is_empty() {
            None
        } else {
            Some(CloseAsk::Windows(windows))
        }
    }
}

/// What a close that went through the veto tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// A hook vetoed it: nothing closed.
    Vetoed,
    /// A secondary window closed; the app runs on.
    Closed,
    /// The app is to end: the driver exits its loop.
    Quit,
    /// The primary window closed under [`QuitPolicy::Explicit`]: every
    /// window is hidden and the app runs on until it quits.
    Hidden,
}

/// Run the close veto path for `window`, or for the whole app when `window`
/// is `None`.
///
/// Writes `CloseRequest { window, reason, vetoed: false }` (one per window
/// for an app-wide close), runs one tick so every close hook (a script's
/// `on_close`, the C ABI's `lumen_app_on_close`, an app system reading
/// [`CloseRequest`]) sees it, and reports what happened. A hook keeps a
/// window open by writing `CloseRequest { vetoed: true, .. }` during that
/// tick.
///
/// When nothing vetoes: a secondary window announces [`WindowClosing`], one
/// more tick runs so its content can be moved out, and the window entity is
/// despawned. The primary window, or the whole app, ends the run
/// ([`CloseOutcome::Quit`]) unless [`QuitPolicy::Explicit`] is set, in which
/// case the primary window's close hides every window instead. A quit always
/// ends the run.
///
/// `requests` is drained after the veto tick: a hook that asks for a close
/// while answering one does not start a second round.
pub fn run_close_request(
    app: &mut App,
    requests: &mut CloseRequests,
    window: Option<Entity>,
    reason: CloseReason,
) -> CloseOutcome {
    let primary = crate::window::primary_window(&mut app.world);
    let asked: Vec<Entity> = match window {
        Some(window) => vec![window],
        None => {
            // Primary first, then the rest in entity order.
            let mut all: Vec<Entity> = app
                .world
                .query_filtered::<Entity, With<Window>>()
                .iter(&app.world)
                .collect();
            all.sort_by_key(|e| (Some(*e) != primary, *e));
            all
        }
    };
    if let Some(mut msgs) = app.world.get_resource_mut::<Messages<CloseRequest>>() {
        if asked.is_empty() {
            // An app with no window entity still runs its close hooks.
            if let Some(primary) = primary.or(window) {
                msgs.write(CloseRequest {
                    window: primary,
                    reason,
                    vetoed: false,
                });
            }
        }
        for &window in &asked {
            msgs.write(CloseRequest {
                window,
                reason,
                vetoed: false,
            });
        }
    }
    app.tick();
    let _ = requests.take(&app.world);
    let vetoed = app
        .world
        .get_resource::<Messages<CloseRequest>>()
        .map(|msgs| msgs.iter_current_update_messages().any(|m| m.vetoed))
        .unwrap_or(false);
    if vetoed {
        return CloseOutcome::Vetoed;
    }
    let quits = window.is_none() || window == primary;
    if !quits {
        let Some(window) = window else {
            return CloseOutcome::Quit;
        };
        app.world.write_message(WindowClosing { window });
        app.tick();
        let _ = requests.take(&app.world);
        if let Ok(e) = app.world.get_entity_mut(window) {
            e.despawn();
        }
        return CloseOutcome::Closed;
    }
    let explicit = window.is_some()
        && reason != CloseReason::Shutdown
        && app.world.get_resource::<QuitPolicy>().copied() == Some(QuitPolicy::Explicit);
    if !explicit {
        return CloseOutcome::Quit;
    }
    let mut windows = app.world.query::<&mut Window>();
    for mut w in windows.iter_mut(&mut app.world) {
        if w.visible {
            w.visible = false;
        }
    }
    CloseOutcome::Hidden
}

/// Run what the app asked for through [`run_close_request`]: a quit as one
/// app-wide close, window closes one by one. Returns what the run as a whole
/// should do: [`CloseOutcome::Quit`] when any of it ends the app, else
/// [`CloseOutcome::Hidden`], [`CloseOutcome::Closed`] or
/// [`CloseOutcome::Vetoed`], in that order.
pub fn run_close_ask(app: &mut App, requests: &mut CloseRequests, ask: CloseAsk) -> CloseOutcome {
    match ask {
        CloseAsk::Quit => run_close_request(app, requests, None, CloseReason::AppQuit),
        CloseAsk::Windows(windows) => {
            let mut outcome = CloseOutcome::Vetoed;
            for window in windows {
                if app.world.get_entity(window).is_err() {
                    continue;
                }
                match run_close_request(app, requests, Some(window), CloseReason::WindowClose) {
                    CloseOutcome::Quit => return CloseOutcome::Quit,
                    CloseOutcome::Hidden => outcome = CloseOutcome::Hidden,
                    CloseOutcome::Closed if outcome == CloseOutcome::Vetoed => {
                        outcome = CloseOutcome::Closed;
                    }
                    _ => {}
                }
            }
            outcome
        }
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
        assert!(app.world.contains_resource::<Monitors>());
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
        assert!(scheduler(true, false, false).should_forward_redraw());
        assert!(scheduler(true, true, false).should_forward_redraw());
        assert!(!scheduler(false, false, false).should_forward_redraw());
        assert!(!scheduler(true, false, true).should_forward_redraw());
        assert!(!scheduler(true, true, true).should_forward_redraw());
    }

    /// The roll-up is focused when any window is and parks only when every
    /// window is occluded or minimized.
    #[test]
    fn the_scheduler_rolls_up_every_window() {
        let window = |focused, occluded, minimized| WindowRuntime {
            focused,
            occluded,
            minimized,
            ..WindowRuntime::default()
        };
        let mut s = RedrawScheduler::default();
        s.roll_up([window(false, true, false), window(true, false, false)]);
        assert!(s.focused && !s.paused);
        s.roll_up([window(false, true, false), window(false, false, true)]);
        assert!(!s.focused && s.paused);
        // No window at all leaves the roll-up as it was.
        s.roll_up([]);
        assert!(s.paused);
    }
}
