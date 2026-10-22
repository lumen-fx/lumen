//! winit 0.30 on-screen window + input.
//!
//! This crate owns the window, the event loop, and the translation of
//! platform events into Lumen messages. It registers itself into the app's
//! window-backend registry as the `window-winit` capability (see
//! [`capability`]), and a launch runs the app through [`WinitBackend`]'s
//! [`WindowBackend`] implementation. It owns no pixels and no
//! accessibility tree: a [`Renderer`] presents frames and an
//! [`A11yBackend`] talks to the platform accessibility API, both behind
//! traits from `lumen-core`, so the window backend compiles without naming
//! a graphics API or an accessibility library.
//!
//! Every window entity of the app (see [`lumen_core::window`]) gets a
//! platform window of its own: the primary one opens from [`WindowOptions`]
//! when the loop starts, and each one the app spawns later opens on the next
//! pass of the loop and closes when its entity goes. One renderer presents
//! all of them on one device, and each has its own accessibility bridge.
//!
//! Lifecycle:
//!   resumed -> primary window + accessibility bridge + renderer attach
//!   Resized -> renderer resize + viewport rewrite + synchronous repaint
//!   RedrawRequested -> pump accessibility requests, app.tick() (once per
//!             batch, whichever window redraws first), present every
//!             window whose frame changed
//!   CloseRequested / SIGINT / SIGTERM / `window.close()` / `app.quit()`
//!             -> emit CloseRequest, run one veto tick (script `on_close` /
//!             `lumen_app_on_close` / app systems); a secondary window then
//!             closes, the primary window or a quit ends the app; `exiting`
//!             persists window state and detaches the renderer while the
//!             platform connection is still alive
//!   about_to_wait -> open and close platform windows for the window
//!             entities, apply the app's `Window` changes, raise the windows
//!             the app asked to raise

#![warn(missing_docs)]

pub mod capability;
mod pointer;

use bevy_ecs::message::{MessageCursor, Messages};
use lumen_core::input::{CloseReason, PendingFileDrops, WindowFocused, WindowOccluded};
use lumen_core::prelude::*;
use lumen_core::render_world::is_primary_target;
use lumen_core::text_events::ImeSurroundingResponse;
use lumen_core::text_model::TextBuffer;
use lumen_core::tick::{wake_deadline, work_pending};
use lumen_core::traits::{
    A11yBackend, A11yBridgeFactory, FrameRequest, FrameTarget, PresentMode, RenderTarget, Renderer,
    WindowBackend, WindowError,
};
use lumen_core::window::{
    MenuModel, MonitorInfo, Monitors, WindowGeometry, WindowKind, WindowOptions, WindowRuntime,
};
use lumen_core::window_backend::{
    CloseAsk, CloseOutcome, CloseRequests, RedrawScheduler, WindowCorePlugin, is_window_size,
    run_close_ask, run_close_request, seed_window,
};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, Ime as WinitIme, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState as WinitModifiers, NamedKey as WinitNamed};
use winit::window::{Window, WindowAttributes, WindowId};

/// Minimum wall interval between two consecutive animation-driven paints
/// (~60 Hz). Used by [`ApplicationHandler::about_to_wait`] to pace redraws
/// that the frame loop re-armed for itself (scroll inertia, hover/press
/// tweens, opacity transitions) against a deadline anchored at the current
/// frame's start.
///
/// On a real display the renderer's present blocks in the
/// `RedrawRequested` handler until the compositor's vsync, so by the time
/// `about_to_wait` runs the deadline has already passed and the redraw is
/// requested immediately - the pacing is a no-op and vsync stays the sole
/// clock. It only bites when `present()` does not block: a headless /
/// software / no-refresh compositor (e.g. `weston --backend=headless`),
/// where the self-re-armed loop would otherwise spin at thousands of Hz
/// pegging a core and producing meaningless sub-millisecond "frame"
/// intervals. There the deadline caps the self-driven cadence at 60 Hz,
/// mirroring the offscreen headless runner's `WORK_FRAME_INTERVAL`. A
/// genuine input event still lands its first paint immediately (the
/// anchor from the previous frame is already stale); only the animation
/// tail is paced. Off-thread screenshot (MCP `SurfaceCapture`) requests
/// bypass the pacing so introspection stays prompt.
///
/// TODO(review): this is a fixed 60 Hz. On a vsync-blocking display it is
/// inert regardless, so the only case it constrains is a *non-blocking*
/// present on a >60 Hz output - e.g. a 144 Hz panel whose compositor path
/// somehow does not throttle - where it would cap self-driven animation at
/// 60 Hz. The fully general form derives the interval from the active
/// monitor's `refresh_rate_millihertz()` (falling back to 60 Hz when the
/// platform reports none); left as a fixed constant here to keep the
/// benchmark's headless cadence predictable and the change minimal.
const ANIM_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_micros(16_667);

/// The winit window backend. What the registry builds and a launch runs.
pub struct WinitBackend;

impl WindowBackend for WinitBackend {
    fn run(
        self: Box<Self>,
        app: App,
        options: WindowOptions,
        renderers: Vec<Box<dyn Renderer>>,
        a11y: Option<A11yBridgeFactory>,
    ) -> Result<(), WindowError> {
        run(app, options, renderers, a11y)
    }
}

/// Cross-thread payload pushed by the Linux XDG color-scheme listener.
/// Translated to a mutation on [`lumen_core::components::StyleManager`]
/// by the typed-command handler [`run`] registers.
pub struct XdgColorSchemeUpdate {
    /// `true` when the desktop reports a dark preference.
    pub dark: bool,
}

/// User-event envelope for the winit loop. Everything that needs to
/// interrupt a parked loop from off the main thread posts one of these:
///
/// - A payload-less wakeup ([`UserEvent::Wake`]) backing
///   [`lumen_core::app::EventLoopWaker`]. `lumen-mcp`'s `SimulateQueue`
///   (and anything else with the same cross-thread-queue shape) calls it
///   after pushing, so the tick that drains the queue runs promptly
///   instead of waiting for an unrelated OS event. The accessibility
///   bridge uses the same handle when an assistive technology queues a
///   request from its own thread.
/// - A close request from a signal handler.
enum UserEvent {
    /// Cross-thread nudge: something was pushed onto a queue the tick
    /// loop doesn't otherwise observe until the next `RedrawRequested`.
    /// Carries no payload - the tick re-reads whatever resource changed.
    Wake,
    /// Cross-thread close request (Unix: first SIGINT / SIGTERM, posted
    /// by the `lumen-signal-watcher` thread). Runs the same graceful
    /// close path as `WindowEvent::CloseRequested`: emit
    /// [`CloseRequest`], tick so app-level close hooks (script
    /// `on_close`, C-ABI `lumen_app_on_close`, SDK systems) observe it,
    /// then exit unless a system vetoed.
    CloseRequested,
}

/// The accessibility bridge factory this backend accepts, inside the
/// [`A11yBridgeFactory`] a launch hands [`WindowBackend::run`].
///
/// The window backend does not name an accessibility library: the
/// composition point passes the factory in, and whatever it returns is
/// driven through [`A11yBackend`]. It is called once per window, after the
/// window is created and before it is first shown, with the window entity
/// whose tree the bridge publishes. The last argument wakes a parked event
/// loop, which the bridge calls when an assistive technology queues a
/// request from its own thread. A factory of any other type is ignored with
/// a warning, and the window runs without accessibility.
pub type WinitA11yFactory = Box<
    dyn Fn(
        &ActiveEventLoop,
        Arc<Window>,
        Entity,
        Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn A11yBackend>,
>;

/// The winit window, shared with the renderer as a [`RenderTarget`].
///
/// The renderer holds one of these for as long as it has a surface, so the
/// window outlives every GPU object bound to it.
struct WinitTarget(Arc<Window>);

impl HasWindowHandle for WinitTarget {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

impl HasDisplayHandle for WinitTarget {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

impl RenderTarget for WinitTarget {
    fn physical_size(&self) -> (u32, u32) {
        let size = self.0.inner_size();
        (size.width.max(1), size.height.max(1))
    }
}

/// Run the app on a real winit window. Blocks until the window closes.
/// [`WinitBackend`] is the public way in.
///
/// `renderers` are the renderers that may present the frames, in the order
/// to try them. Once the window exists the first is attached; one that
/// fails to bind ([`lumen_core::traits::RenderError::Init`]) is dropped
/// for the next, so a machine with no usable GPU can still get a picture
/// from a renderer that needs none. The one that binds presents every frame
/// and is detached before the platform connection closes.
/// `a11y` builds the accessibility bridge at the same moment, or is
/// `None` for a run without one. Both ride beside [`WindowOptions`]
/// rather than inside it: the options are pure data every launch path
/// resolves, while these are live backend objects the composition point
/// chose.
fn run(
    mut app: App,
    opts: WindowOptions,
    renderers: Vec<Box<dyn Renderer>>,
    a11y: Option<A11yBridgeFactory>,
) -> Result<(), WindowError> {
    let a11y = a11y.and_then(winit_a11y_factory);
    let mut renderers = renderers.into_iter();
    let renderer = renderers.next().ok_or(WindowError::NoRenderer)?;
    let fallbacks: Vec<Box<dyn Renderer>> = renderers.collect();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| WindowError::EventLoop(e.to_string()))?;
    let proxy = event_loop.create_proxy();

    // Expose a wakeup handle for cross-thread producers (today:
    // `lumen-mcp`'s `SimulateQueue`) so pushing work doesn't sit invisible
    // until an unrelated OS event ticks the app. `send_event` is exactly
    // winit's documented mechanism for interrupting a parked loop from
    // another thread; it only fails once the loop has already exited, and
    // there's nothing useful to do with that error here.
    let waker_proxy = proxy.clone();
    let waker = lumen_core::app::EventLoopWaker(std::sync::Arc::new(move || {
        let _ = waker_proxy.send_event(UserEvent::Wake);
    }));
    // Wire the same waker into `SurfaceCapture` so an off-thread screenshot
    // request (MCP server) interrupts a parked loop and gets serviced this
    // frame. The MCP plugin inserts `SurfaceCapture` before `run()`; its
    // `waker` field is a shared `Arc<OnceLock>`, so setting it here also
    // reaches the clone the server thread holds. No-op when MCP is disabled
    // (resource absent).
    if let Some(capture) = app
        .world
        .get_resource::<lumen_core::render_world::SurfaceCapture>()
    {
        capture.set_waker(waker.clone());
    }
    // The plugin-event bus wakes the loop too, so an event a module's worker
    // thread pushes while the loop sits parked in `Wait` is delivered on the
    // tick it triggers instead of riding the next unrelated OS event.
    lumen_core::plugin_events::set_plugin_event_waker(waker.clone());
    app.world.insert_resource(waker);

    // Install the window messages and the redraw scheduler before any
    // other system can read them - unless the app already has them. The
    // guard matters: `add_message` re-registration is not idempotent (each
    // call adds another per-tick buffer update, so a double-registered
    // message type cycles twice per tick and drops pre-tick writes before
    // Systems-stage readers run).
    if !app.is_plugin_added::<WindowCorePlugin>() {
        app.add_plugin(WindowCorePlugin);
    }
    // The (Linux-only) XDG portal listener pushes `XdgColorSchemeUpdate
    // { dark }` through [`lumen_core::command::Command::Typed`]; this
    // handler applies it on the main thread inside
    // [`TickStage::CommandDrain`]. Registered on every platform; it just
    // never fires where there is no listener.
    app.register_command::<XdgColorSchemeUpdate, _>(|world, payload| {
        let dark = payload.dark;
        world.resource_mut::<StyleManager>().set_system_dark(dark);
    });

    // Unix: route SIGINT / SIGTERM through the same graceful close path
    // as the window close button. A dedicated watcher thread (signals
    // cannot safely do this work from the handler context) posts
    // [`UserEvent::CloseRequested`] on the first signal - waking the
    // parked loop exactly like [`EventLoopWaker`] does - and force-exits
    // with the conventional `128 + signo` code on the second, so a
    // wedged or veto-looping app can always be interrupted. The thread
    // parks in `sigwait` for the process lifetime; it is intentionally
    // not joined on shutdown (process exit reaps it).
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let signal_proxy = proxy.clone();
        match signal_hook::iterator::Signals::new([SIGINT, SIGTERM]) {
            Ok(mut signals) => {
                let spawned = std::thread::Builder::new()
                    .name("lumen-signal-watcher".into())
                    .spawn(move || {
                        let mut graceful_attempted = false;
                        for signal in signals.forever() {
                            if graceful_attempted {
                                // Second signal: the graceful path is
                                // still in flight (or vetoed) - bail out
                                // immediately with the conventional
                                // signal exit code.
                                std::process::exit(128 + signal);
                            }
                            graceful_attempted = true;
                            if signal_proxy.send_event(UserEvent::CloseRequested).is_err() {
                                // Event loop already gone - shutdown is
                                // in progress; nothing to do.
                                return;
                            }
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!(
                        target: "lumen::window",
                        "failed to spawn lumen-signal-watcher: {e}; \
                         SIGINT/SIGTERM fall back to immediate termination",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "lumen::window",
                    "failed to register SIGINT/SIGTERM handlers: {e}; \
                     signals fall back to immediate termination",
                );
            }
        }
    }

    // Windows: route Ctrl+C / Ctrl+Break / console-close through the same
    // graceful close path as the window close button, mirroring the Unix
    // SIGINT/SIGTERM watcher above. `ctrlc` wraps `SetConsoleCtrlHandler`
    // and runs the closure on its own thread, so the first event forwards
    // [`UserEvent::CloseRequested`] (waking the parked loop so `on_close`
    // hooks run) and the second force-exits with the conventional
    // `128 + SIGINT` code for a wedged or veto-looping app.
    #[cfg(windows)]
    {
        let signal_proxy = proxy.clone();
        let mut graceful_attempted = false;
        let installed = ctrlc::set_handler(move || {
            if graceful_attempted {
                // Conventional code for interrupt (SIGINT == 2).
                std::process::exit(128 + 2);
            }
            graceful_attempted = true;
            // Event loop already gone => shutdown is in progress; ignore.
            let _ = signal_proxy.send_event(UserEvent::CloseRequested);
        });
        if let Err(e) = installed {
            tracing::warn!(
                target: "lumen::window",
                "failed to register console-ctrl handler: {e}; \
                 Ctrl+C falls back to immediate termination",
            );
        }
    }

    // The primary window opens at the size the options ask for; seed its
    // viewport and the app's `Window` with what is about to open.
    {
        let size = glam::Vec2::new(opts.size.0 as f32, opts.size.1 as f32);
        if let Some(primary) = lumen_core::window::primary_window(&mut app.world) {
            seed_window(&mut app.world, primary, &opts.title, size);
        }
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = size;
        vp.clear = opts.clear;
    }
    let mut handler = WinitHandler {
        app,
        opts,
        close: CloseRouting::default(),
        renderer,
        fallbacks: fallbacks.into(),
        a11y_factory: a11y,
        windows: HashMap::new(),
        ids: HashMap::new(),
        primary: None,
        proxy,
        last_ime: (None, ImeRequest::default()),
        last_cursor: None,
        custom_cursors: HashMap::new(),
        last_frame_at: None,
        pointers: pointer::PointerTracker::default(),
        focus_requests: MessageCursor::default(),
        ticked: false,
    };
    event_loop
        .run_app(&mut handler)
        .map_err(|e| WindowError::Run(e.to_string()))
}

/// The factory inside `factory` when it was built for this backend. A
/// factory of another type is reported and dropped, and the window runs
/// without accessibility.
fn winit_a11y_factory(factory: A11yBridgeFactory) -> Option<WinitA11yFactory> {
    match factory.downcast::<WinitA11yFactory>() {
        Ok(factory) => Some(factory),
        Err(_) => {
            tracing::warn!(
                target: "lumen::window",
                "the accessibility bridge was built for another window backend; \
                 running without accessibility",
            );
            None
        }
    }
}

/// Bind `renderer` to `target`, the platform window of `window`, moving on
/// to the next of `fallbacks` each time a renderer cannot initialise against
/// it. Leaves the one that bound in `renderer`. Any other failure, or running out of renderers,
/// returns the last error.
fn attach_first(
    renderer: &mut Box<dyn Renderer>,
    fallbacks: &mut std::collections::VecDeque<Box<dyn Renderer>>,
    window: Entity,
    target: Arc<dyn RenderTarget>,
) -> Result<(), lumen_core::traits::RenderError> {
    loop {
        match renderer.attach(window, FrameTarget::Window(target.clone())) {
            Ok(()) => return Ok(()),
            Err(lumen_core::traits::RenderError::Init(why)) if !fallbacks.is_empty() => {
                lumen_core::warn_line!(
                    "lumen-window-winit: renderer init failed ({why}); trying the next renderer"
                );
                *renderer = fallbacks.pop_front().expect("checked non-empty");
            }
            Err(e) => return Err(e),
        }
    }
}

/// One platform window the backend drives, and the window entity it shows.
struct OsWindow {
    /// The main-world window entity.
    entity: Entity,
    /// Accessibility bridge for this window; receives every winit
    /// `WindowEvent` of the window and hands queued assistive-technology
    /// requests to the world each frame. Dropped before the window.
    a11y: Option<Box<dyn A11yBackend>>,
    /// The platform window. Every event arm finds it through the window's
    /// id, so nothing touches it once it is dropped.
    window: Arc<Window>,
    /// What the platform window was last given.
    sync: WindowSync,
}

struct WinitHandler {
    app: App,
    opts: WindowOptions,
    /// The close path: the app's own close requests, and whether a close
    /// ended the app.
    close: CloseRouting,
    /// Presents every window's frames. Attached to the primary window in
    /// [`ApplicationHandler::resumed`] (which brings the device up) and to
    /// each later window as it opens; detached in
    /// [`ApplicationHandler::exiting`].
    renderer: Box<dyn Renderer>,
    /// Renderers to try, in order, when [`Self::renderer`] cannot bind to
    /// the primary window.
    fallbacks: std::collections::VecDeque<Box<dyn Renderer>>,
    /// Builds each window's accessibility bridge once the window exists.
    /// `None` runs without accessibility.
    a11y_factory: Option<WinitA11yFactory>,
    /// The platform windows, by winit id.
    windows: HashMap<WindowId, OsWindow>,
    /// Each window entity's platform window.
    ids: HashMap<Entity, WindowId>,
    /// The primary window entity, set once its platform window exists.
    primary: Option<Entity>,
    /// EventLoop proxy backing [`lumen_core::app::EventLoopWaker`] and the
    /// signal watchers.
    proxy: EventLoopProxy<UserEvent>,
    /// Last IME control values applied, and the window they went to.
    /// Compared against the current [`ImeRequest`] resource each frame to
    /// avoid hammering winit with redundant calls.
    last_ime: (Option<WindowId>, ImeRequest),
    /// Last cursor applied via `Window::set_cursor`, and the window it went
    /// to. Compared against the main world's
    /// [`lumen_core::input::CursorRequest`] each frame so the OS call only
    /// happens on change.
    last_cursor: Option<(WindowId, CursorRequest)>,
    /// Platform cursors made from decoded `url(...)` cursor images, so a
    /// cursor shown again is not handed to the OS a second time.
    custom_cursors: CustomCursors,
    /// Wall-clock start of the most recent `RedrawRequested` paint, used by
    /// [`ApplicationHandler::about_to_wait`] to pace self-re-armed
    /// animation frames against [`ANIM_FRAME_INTERVAL`]. `None` until the
    /// first paint. See [`ANIM_FRAME_INTERVAL`] for why this is a no-op on
    /// a vsync-blocking present path and only bites on a headless /
    /// non-blocking one.
    last_frame_at: Option<std::time::Instant>,
    /// Mouse buttons held and the live touch / pen contacts, for the pointer
    /// data each message carries.
    pointers: pointer::PointerTracker,
    /// The app's requests to raise a window.
    focus_requests: MessageCursor<lumen_core::window::WindowRequest>,
    /// Whether the app ticked since the loop last went idle: the first
    /// window to redraw in a batch ticks, the others only present.
    ticked: bool,
}

impl ApplicationHandler<UserEvent> for WinitHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        let Some(primary) = lumen_core::window::primary_window(&mut self.app.world) else {
            lumen_core::warn_line!("lumen-window-winit: the app has no primary window");
            event_loop.exit();
            return;
        };
        let mut attrs = WindowAttributes::default()
            .with_title(&self.opts.title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.opts.size.0,
                self.opts.size.1,
            ))
            .with_maximized(self.opts.maximized)
            .with_decorations(!self.opts.frameless)
            // Shown once the accessibility bridge below is bound: AccessKit
            // must attach to a window that has never been visible, and
            // on Windows its adapter panics if the window was shown first.
            .with_visible(false);
        if let Some((x, y)) = self.opts.start_position {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                lumen_core::warn_line!("lumen-window-winit: create_window failed: {e:?}");
                event_loop.exit();
                return;
            }
        };
        // Seed [`lumen_core::components::StyleManager::system_dark`] from
        // the freshly-created window. winit returns `Some(theme)` on
        // macOS/Windows; on most Linux WMs it's `None`, in which case
        // we leave the default (light) and the XDG portal listener spawned
        // below will push through the real value once the bus replies.
        if let Some(theme) = window.theme() {
            self.app
                .world
                .resource_mut::<StyleManager>()
                .set_system_dark(matches!(theme, winit::window::Theme::Dark));
        }
        // Best-effort XDG `org.freedesktop.portal.Settings` listener
        // (Linux only). Spawns once per process; tracks the system
        // color-scheme preference and pushes updates via the bounded
        // `CommandQueue` so the next tick re-runs `style_manager_to_signal`.
        // When the portal call fails (no XDG portal daemon, e.g. inside CI /
        // headless containers), the spawn returns early and the runtime
        // falls back to the winit-reported `WindowEvent::ThemeChanged` path.
        #[cfg(target_os = "linux")]
        try_spawn_xdg_color_scheme_listener(&mut self.app.world);
        refresh_monitors(&mut self.app.world, event_loop);
        // The window the platform opened is the one the app has: seed the
        // viewport (logical size at the window's scale) and the app's
        // `Window` from it.
        {
            let viewport = platform_viewport(&window);
            let mut vp = lumen_core::window::primary_viewport_mut(&mut self.app.world);
            vp.size = viewport.0;
            vp.scale_factor = viewport.1;
            vp.clear = self.opts.clear;
        }
        let title = self.opts.title.clone();
        let logical = platform_viewport(&window).0;
        seed_window(&mut self.app.world, primary, &title, logical);
        record_runtime(&mut self.app.world, primary, &window);
        // The accessibility bridge must exist before the first
        // RedrawRequested so the platform's initial-tree handshake arrives
        // in time, and before the window is first shown (#551).
        let a11y = self.bridge(event_loop, &window, primary);
        window.set_visible(true);
        // Attach the native menubar from the markup spec. On macOS and
        // Windows builds a `muda::Menu` and binds it to the app/window;
        // Linux is a stub. The muda integration lives in `lumen-os-menu`.
        if let Some(spec) = self.opts.menubar.take() {
            attach_menubar_via_os_menu(&window, &spec);
        }
        if let Err(e) = attach_first(
            &mut self.renderer,
            &mut self.fallbacks,
            primary,
            Arc::new(WinitTarget(window.clone())),
        ) {
            lumen_core::warn_line!("lumen-window-winit: renderer init failed: {e}");
            event_loop.exit();
            return;
        }
        let sync = WindowSync::opened(&title, logical);
        self.insert(primary, window, a11y, sync);
        self.primary = Some(primary);
        // Windows the app spawned before the loop started open now.
        self.sync_windows(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Persist the primary window's final geometry through whatever the
        // caller wired up (typically `lumenc::window_state::save`).
        let primary = self
            .primary
            .and_then(|e| self.ids.get(&e))
            .and_then(|id| self.windows.get(id))
            .map(|w| w.window.clone());
        if let (Some(cb), Some(window)) = (self.opts.on_close_state.take(), primary.as_ref()) {
            let inner = window.inner_size();
            let scale = window.scale_factor();
            let logical_w = (inner.width as f64 / scale).round().max(1.0) as u32;
            let logical_h = (inner.height as f64 / scale).round().max(1.0) as u32;
            let position = window.outer_position().ok().map(|p| (p.x, p.y));
            cb(WindowGeometry {
                position,
                size: (logical_w, logical_h),
                maximized: window.is_maximized(),
            });
        }
        drop(primary);
        // Stop redraw scheduling - nothing may touch a window past this
        // point (`about_to_wait` / `window_event` both find windows through
        // the map emptied below).
        if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
            sch.pending = false;
        }
        // Release every accessibility bridge before the window it wraps.
        for os in self.windows.values_mut() {
            os.a11y = None;
        }
        // Orderly renderer teardown WHILE the platform connection is still
        // alive. `event_loop.run_app` consumes the `EventLoop`, so once it
        // returns the Wayland/X11 connection is already gone - and
        // releasing a GPU surface at that point makes a GLES driver
        // call `eglTerminate` against a dead `wl_display`, which segfaults
        // (observed: exit 139 on every close under Hyprland/EGL).
        // Detaching here runs the whole release chain while the display
        // connection is still valid, and drops the renderer's handles on
        // the windows so the windows themselves go last.
        self.renderer.detach_all();
        self.windows.clear();
        self.ids.clear();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                // Cross-thread producer (e.g. `lumen-mcp`'s
                // `SimulateQueue::push`) nudged the loop after pushing
                // work the tick doesn't otherwise observe until the next
                // `RedrawRequested`. Schedule a redraw the same way every
                // other pending-work source does; `about_to_wait` forwards
                // to `Window::request_redraw` only when some window isn't
                // paused (occluded), so this still can't spin a covered /
                // minimized app - but a visible-but-unfocused window
                // (tiling WM) does wake, which is how a worker thread's
                // `EventLoopWaker` keeps its pump advancing off-focus.
                self.want_frame();
            }
            UserEvent::CloseRequested => {
                // First SIGINT / SIGTERM, forwarded by the
                // `lumen-signal-watcher` thread. The whole app closes; a
                // second signal force-exits from the watcher thread if a
                // hook vetoes or teardown wedges.
                self.close.run(&mut self.app, None, CloseReason::Shutdown);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(os) = self.windows.get_mut(&window_id) else {
            return;
        };
        // The accessibility bridge sees every event of its window before
        // the app does, as its platform adapter requires.
        if let Some(a11y) = os.a11y.as_mut() {
            a11y.window_event(&event as &dyn Any);
        }
        let entity = os.entity;
        // Clone the handle rather than borrowing `self`: the arms below
        // need the window and the app at the same time.
        let window = os.window.clone();
        // Input aimed at a window a modal one blocks is dropped; a press
        // raises the window that blocks it.
        if is_input(&event)
            && let Some(by) = self
                .app
                .world
                .get::<lumen_core::window::InputBlocked>(entity)
                .map(|b| b.by)
        {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } | WindowEvent::Touch(_)
            ) && let Some(blocker) = self.ids.get(&by).and_then(|id| self.windows.get(id))
            {
                blocker.window.focus_window();
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // Run the veto tick synchronously - see
                // [`CloseRouting::run`]. Doing it here (instead of deferring
                // to the next `RedrawRequested`) means a close on an
                // unfocused or occluded window still commits.
                self.close
                    .run(&mut self.app, Some(entity), CloseReason::Platform);
            }
            WindowEvent::ThemeChanged(theme) => {
                // Last-resort path on Linux when the XDG portal listener
                // isn't running. On macOS/Windows this is the primary
                // source of `system_dark` updates.
                self.app
                    .world
                    .resource_mut::<StyleManager>()
                    .set_system_dark(matches!(theme, winit::window::Theme::Dark));
                self.want_frame();
            }
            WindowEvent::Resized(new_size) => self.resized(entity, &window, new_size),
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                mut inner_size_writer,
            } => {
                // winit asks us to choose the new physical inner size
                // for the new scale. Honour the OS suggestion by keeping
                // the same LOGICAL size: multiply the current logical
                // viewport by the new dpr. This matches what GTK /
                // QWindow do under fractional scaling and lets layout
                // stay stable across monitor hot-swaps.
                let scale_f32 = scale_factor as f32;
                let logical = self
                    .app
                    .world
                    .get::<WindowViewport>(entity)
                    .map(|v| v.size)
                    .unwrap_or_default();
                let new_w = (logical.x as f64 * scale_factor).round().max(1.0) as u32;
                let new_h = (logical.y as f64 * scale_factor).round().max(1.0) as u32;
                let desired = winit::dpi::PhysicalSize::new(new_w, new_h);
                if let Err(e) = inner_size_writer.request_inner_size(desired) {
                    // `Ignored` here is non-fatal - winit will fall back
                    // to its own suggestion and emit a `Resized`. Log so
                    // the failure mode is visible in tracing.
                    tracing::debug!(
                        target: "lumen::window",
                        ?e,
                        "ScaleFactorChanged: request_inner_size ignored; \
                         falling back to OS-suggested size",
                    );
                }
                // Reconfigure the surface immediately at the new physical
                // size - winit guarantees the change is synchronous so
                // a subsequent `Resized` may not fire on every backend
                // before the next `RedrawRequested`.
                self.renderer.resize(entity, new_w.max(1), new_h.max(1));
                // The logical size stays put; the scale factor flips to the
                // new value.
                if let Some(mut vp) = self.app.world.get_mut::<WindowViewport>(entity) {
                    vp.scale_factor = scale_f32;
                }
                refresh_monitors(&mut self.app.world, event_loop);
                record_runtime(&mut self.app.world, entity, &window);
                // Paint synchronously at the new DPI so the monitor
                // hot-swap doesn't flash a stale-scale frame.
                self.tick();
                // DPI change recreated the frame texture - force a full
                // repaint of this window.
                self.present(Some(entity));
                self.want_frame();
            }
            WindowEvent::Moved(_) => {
                // The window may now be on another monitor.
                record_runtime(&mut self.app.world, entity, &window);
            }
            WindowEvent::RedrawRequested => {
                // Anchor the animation-pacing deadline at the frame START so
                // `about_to_wait` measures the full present-to-present period
                // (see `ANIM_FRAME_INTERVAL`).
                self.last_frame_at = Some(std::time::Instant::now());
                if !self.ticked {
                    // Drain native menu clicks before the tick so handlers
                    // fire on the same frame as the user's click.
                    lumen_os_menu::poll_native_menu_events(&mut self.app.world);
                    // Apply anything an assistive technology queued from
                    // its own thread, for the same reason: a screen
                    // reader's click lands in the tick that paints its
                    // result.
                    for os in self.windows.values_mut() {
                        if let Some(a11y) = os.a11y.as_mut() {
                            a11y.pump(&mut self.app.world);
                        }
                    }
                    self.tick();
                    self.apply_input_requests(event_loop);
                }
                // One tick paints every window whose frame changed.
                self.present(None);
            }
            WindowEvent::CursorMoved { position, .. } => {
                // winit delivers `PhysicalPosition`; convert to logical so
                // layout, hit-test, and pointer-routed primitives all share
                // one coordinate space with the window's viewport.
                let scale_factor = window.scale_factor();
                let logical = position.to_logical::<f32>(scale_factor);
                let p = glam::Vec2::new(logical.x, logical.y);
                {
                    let mut state = self.app.world.resource_mut::<PointerState>();
                    state.position = Some(p);
                    state.window = Some(entity);
                }
                self.app.world.write_message(PointerMoved {
                    position: p,
                    local: None,
                    pointer: self.pointers.mouse().stamped(),
                });
                self.want_frame();
            }
            WindowEvent::CursorLeft { .. } => {
                // Only the window the pointer is over clears it: the pointer
                // entering another of the app's windows may report there
                // first.
                let over = self.app.world.resource::<PointerState>().window;
                if over.is_none() || over == Some(entity) {
                    self.app.world.resource_mut::<PointerState>().position = None;
                    self.app.world.write_message(PointerLeft);
                }
                self.want_frame();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let v = wheel_delta(delta);
                let pos = self.pointer_position(entity);
                self.app.world.write_message(MouseWheel {
                    delta: v,
                    position: pos,
                    local: None,
                    time_stamp: lumen_core::time::Instant::now(),
                });
                self.want_frame();
            }
            WindowEvent::ModifiersChanged(mods) => {
                let m = map_modifiers(mods.state());
                self.app.world.resource_mut::<ModifiersState>().0 = m;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let mods = self.app.world.resource::<ModifiersState>().0;
                if let Some(key) = map_key(&event) {
                    match event.state {
                        ElementState::Pressed => {
                            self.app.world.write_message(KeyPressed {
                                key,
                                modifiers: mods,
                                repeat: event.repeat,
                                time_stamp: lumen_core::time::Instant::now(),
                            });
                        }
                        ElementState::Released => {
                            self.app.world.write_message(KeyReleased {
                                key,
                                modifiers: mods,
                                time_stamp: lumen_core::time::Instant::now(),
                            });
                        }
                    }
                }
                self.want_frame();
            }
            WindowEvent::HoveredFile(path) => {
                let pos = self.pointer_position(entity);
                self.app.world.write_message(FileHovered {
                    path,
                    position: pos,
                });
                self.want_frame();
            }
            WindowEvent::HoveredFileCancelled => {
                self.app.world.write_message(FileHoverCancelled);
                self.want_frame();
            }
            WindowEvent::DroppedFile(path) => {
                // Stash the raw path + current pointer pos for the dispatch
                // system (in lumen-input), which hit-tests it against
                // DropTarget entities in the window the pointer is over.
                let pos = self.pointer_position(entity);
                self.app
                    .world
                    .resource_mut::<PendingFileDrops>()
                    .drops
                    .push((path, pos));
                self.want_frame();
            }
            WindowEvent::Ime(ime) => {
                // When winit deactivates the IME, the OS already forgot
                // whatever `set_ime_allowed(true)` we last called; mirror the
                // OS reset locally so the next non-trivial `ImeRequest`
                // reapplies cleanly.
                let is_disabled = matches!(ime, WinitIme::Disabled);
                let mapped = match ime {
                    WinitIme::Enabled => ImeEvent::Enabled,
                    WinitIme::Preedit(text, cursor) => ImeEvent::Preedit { text, cursor },
                    WinitIme::Commit(text) => ImeEvent::Commit(text),
                    WinitIme::Disabled => ImeEvent::Disabled,
                };
                if is_disabled {
                    self.last_ime.1.allowed = false;
                    self.last_ime.1.cursor_area = None;
                    if let Some(mut req) = self.app.world.get_resource_mut::<ImeRequest>() {
                        req.allowed = false;
                        req.cursor_area = None;
                    }
                }
                self.app.world.write_message(mapped);
                // Push the focused field's text and cursor so an OS-bound
                // forwarder (Wayland text-input-v3 / IBus) has them.
                push_ime_surrounding_response(&mut self.app.world);
                self.want_frame();
            }
            WindowEvent::Focused(focused) => {
                // Focus does not pause the redraw pump: a visible-but-
                // unfocused window (tiling WMs) must keep animating.
                self.app.world.write_message(WindowFocused {
                    window: entity,
                    focused,
                });
                if let Some(mut runtime) = self.app.world.get_mut::<WindowRuntime>(entity) {
                    runtime.focused = focused;
                }
                self.roll_up();
                if focused {
                    // The focused window sets the pace: it presents on the
                    // display's refresh, the others do not wait on theirs.
                    let windows: Vec<Entity> = self.ids.keys().copied().collect();
                    for w in windows {
                        let mode = if w == entity {
                            PresentMode::Vsync
                        } else {
                            PresentMode::NoWait
                        };
                        self.renderer.set_present_mode(w, mode);
                    }
                    // Coming back into focus provokes a paint so any state
                    // the app changed meanwhile is visible immediately.
                    self.want_frame();
                }
            }
            WindowEvent::Occluded(occluded) => {
                // Park the window while occluded; resume on reveal. An app
                // whose every window is occluded parks its redraw loop.
                self.app.world.write_message(WindowOccluded {
                    window: entity,
                    occluded,
                });
                if let Some(mut runtime) = self.app.world.get_mut::<WindowRuntime>(entity) {
                    runtime.occluded = occluded;
                }
                self.roll_up();
                if !occluded {
                    self.want_frame();
                }
            }
            WindowEvent::Touch(touch) => {
                let logical = touch.location.to_logical::<f32>(window.scale_factor());
                let p = glam::Vec2::new(logical.x, logical.y);
                let pen = pointer::pen_sample(touch.id);
                if let Some(output) =
                    self.pointers
                        .touch(touch.phase, touch.id, p, touch.force, pen)
                {
                    self.write_pointer_output(entity, output);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let lumen_button = map_button(button);
                let data = self
                    .pointers
                    .mouse_button(lumen_button, matches!(state, ElementState::Pressed));
                let pos = self.pointer_position(entity);
                if matches!(lumen_button, PointerButton::Primary) {
                    self.app.world.resource_mut::<PointerState>().primary_down =
                        matches!(state, ElementState::Pressed);
                }
                match state {
                    ElementState::Pressed => {
                        self.app.world.write_message(PointerPressed {
                            position: pos,
                            button: lumen_button,
                            local: None,
                            pointer: data.stamped(),
                        });
                    }
                    ElementState::Released => {
                        self.app.world.write_message(PointerReleased {
                            position: pos,
                            button: lumen_button,
                            local: None,
                            pointer: data.stamped(),
                        });
                    }
                }
                self.want_frame();
            }
            _ => {}
        }
    }

    /// Called by winit after every batch of events and before the loop
    /// goes back to wait:
    ///
    /// 1. A close the app asked for runs its veto path; one that ends the
    ///    app exits the loop.
    /// 2. Every window entity gets a platform window, a closed one's goes,
    ///    and what the app changed on a [`lumen_core::window::Window`]
    ///    reaches its platform window.
    /// 3. `RedrawScheduler.pending` becomes a redraw request on one shown,
    ///    unoccluded window, whose redraw ticks the app and presents every
    ///    window whose frame changed. Focus is not a factor - a
    ///    visible-but-unfocused window keeps animating.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.ticked = false;
        // A `window.close()` / `app.quit()` the ticks of this batch asked
        // for runs the same veto path the close button does.
        self.close.run_pending(&mut self.app);
        if self.close.committed {
            event_loop.exit();
            // Windows waits for the next message after `about_to_wait`
            // before it checks for an exit, and a close the app asked for
            // brings no further message; do not wait.
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        self.sync_windows(event_loop);
        self.apply_focus_requests();
        // A deadline a system asked to be woken at (a script timer) that
        // has come due needs a tick: this is the pass the `WaitUntil` set
        // below resumes into.
        let wake_at = wake_deadline(&self.app.world);
        let now = std::time::Instant::now();
        if wake_at.is_some_and(|at| at <= now) {
            self.want_frame();
        }
        let Some(driver) = self.redraw_driver() else {
            // No window to draw in (every one hidden): wait for events.
            event_loop.set_control_flow(idle_control_flow(wake_at, now));
            return;
        };
        let should_paint = self
            .app
            .world
            .get_resource::<RedrawScheduler>()
            .map(RedrawScheduler::should_forward_redraw)
            .unwrap_or(false);
        // A pending off-thread screenshot request must be serviced even
        // when the scheduler is paused (occluded window - the common case
        // on a headless X server, or any minimized/covered window).
        let capture_pending = self
            .app
            .render_world
            .get_resource::<SurfaceCapture>()
            .map(|c| c.is_requested())
            .unwrap_or(false);
        if capture_pending {
            driver.request_redraw();
            event_loop.set_control_flow(ControlFlow::Wait);
        } else if should_paint {
            // Pace the self-re-armed animation redraw against a deadline
            // anchored at the current frame's start (`ANIM_FRAME_INTERVAL`).
            // With a vsync-blocking present the deadline is already in the
            // past here, so the redraw fires immediately and vsync stays
            // the clock. With a non-blocking (headless) present the loop
            // parks until the deadline instead of spinning.
            let deadline = self.last_frame_at.map(|t| t + ANIM_FRAME_INTERVAL);
            match deadline {
                Some(d) if d > now => event_loop.set_control_flow(ControlFlow::WaitUntil(d)),
                _ => {
                    driver.request_redraw();
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
            }
        } else {
            // Idle: park until the next event (input, resize, MCP wake)
            // or the earliest requested deadline.
            event_loop.set_control_flow(idle_control_flow(wake_at, now));
        }
    }
}

/// Whether `event` is input a blocked window refuses.
fn is_input(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::DroppedFile(_)
            | WindowEvent::HoveredFile(_)
    )
}

/// Control flow for an idle loop: sleep until `wake_at` while it lies in the
/// future, else wait for the next event. A deadline already past is not
/// waited on here; it raised `pending` above, and a paused (occluded) window
/// holds it like any other pending frame instead of spinning on it.
fn idle_control_flow(wake_at: Option<std::time::Instant>, now: std::time::Instant) -> ControlFlow {
    match wake_at {
        Some(at) if at > now => ControlFlow::WaitUntil(at),
        _ => ControlFlow::Wait,
    }
}

/// A platform window's logical inner size and scale factor.
fn platform_viewport(window: &Window) -> (glam::Vec2, f32) {
    let scale = window.scale_factor() as f32;
    let inner = window.inner_size();
    let logical = glam::Vec2::new(inner.width as f32 / scale, inner.height as f32 / scale);
    (logical, scale)
}

/// Record where `window` is: its outer position and the monitor it is on.
fn record_runtime(world: &mut World, entity: Entity, window: &Window) {
    let position = window
        .outer_position()
        .ok()
        .map(|p| glam::IVec2::new(p.x, p.y));
    let monitor = window.current_monitor().and_then(|m| m.name());
    let maximized = window.is_maximized();
    let minimized = window.is_minimized().unwrap_or(false);
    if let Some(mut runtime) = world.get_mut::<WindowRuntime>(entity) {
        let next = WindowRuntime {
            position,
            monitor,
            maximized,
            minimized,
            ..runtime.clone()
        };
        if *runtime != next {
            *runtime = next;
        }
    }
}

/// Read the desktop's monitors into [`Monitors`], writing only on change.
fn refresh_monitors(world: &mut World, event_loop: &ActiveEventLoop) {
    let monitors = Monitors(
        event_loop
            .available_monitors()
            .enumerate()
            .map(|(i, m)| {
                let position = m.position();
                let size = m.size();
                let position = glam::IVec2::new(position.x, position.y);
                let size = glam::UVec2::new(size.width, size.height);
                MonitorInfo {
                    name: m.name().unwrap_or_else(|| format!("monitor-{}", i + 1)),
                    position,
                    size,
                    // winit 0.30 reports no work area; the whole screen
                    // stands in for it.
                    work_area: (position, size),
                    scale: m.scale_factor() as f32,
                }
            })
            .collect(),
    );
    if world.get_resource::<Monitors>() != Some(&monitors) {
        world.insert_resource(monitors);
    }
}

impl WinitHandler {
    /// Run one tick and keep any close the app asked for during it. A quit
    /// outranks a window close.
    fn tick(&mut self) {
        self.app.tick();
        self.ticked = true;
        self.close.after_tick(&self.app.world);
    }

    /// Ask for a frame the next time the loop goes idle.
    fn want_frame(&mut self) {
        if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
            sch.pending = true;
        }
    }

    /// Recompute the redraw roll-up from every window's runtime state.
    fn roll_up(&mut self) {
        let runtimes: Vec<WindowRuntime> = self
            .ids
            .keys()
            .filter_map(|e| self.app.world.get::<WindowRuntime>(*e).cloned())
            .collect();
        if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
            sch.roll_up(runtimes);
        }
    }

    /// The pointer's position, in `entity`'s coordinates when the pointer
    /// is over that window.
    fn pointer_position(&self, entity: Entity) -> glam::Vec2 {
        let state = self.app.world.resource::<PointerState>();
        if state.window.is_none() || state.window == Some(entity) {
            state.position.unwrap_or(glam::Vec2::ZERO)
        } else {
            glam::Vec2::ZERO
        }
    }

    /// The window whose redraw ticks the app: the primary one when it is
    /// shown and not occluded, else the first that is.
    fn redraw_driver(&self) -> Option<Arc<Window>> {
        let usable = |e: Entity| -> bool {
            let shown = self
                .app
                .world
                .get::<lumen_core::window::Window>(e)
                .is_some_and(|w| w.visible);
            let parked = self
                .app
                .world
                .get::<WindowRuntime>(e)
                .is_some_and(|r| r.occluded || r.minimized);
            shown && !parked
        };
        let pick = |e: Entity| {
            self.ids
                .get(&e)
                .and_then(|id| self.windows.get(id))
                .map(|w| w.window.clone())
        };
        self.primary
            .filter(|p| usable(*p))
            .and_then(pick)
            .or_else(|| {
                let mut all: Vec<Entity> = self.ids.keys().copied().collect();
                all.sort();
                all.into_iter().find(|e| usable(*e)).and_then(pick)
            })
            .or_else(|| self.primary.and_then(pick))
    }

    /// Build `entity`'s accessibility bridge on `window`, which has not
    /// been shown yet.
    fn bridge(
        &self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
        entity: Entity,
    ) -> Option<Box<dyn A11yBackend>> {
        let factory = self.a11y_factory.as_ref()?;
        let wake_proxy = self.proxy.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = wake_proxy.send_event(UserEvent::Wake);
        });
        Some(factory(event_loop, window.clone(), entity, wake))
    }

    /// Record a platform window for `entity`.
    fn insert(
        &mut self,
        entity: Entity,
        window: Arc<Window>,
        a11y: Option<Box<dyn A11yBackend>>,
        sync: WindowSync,
    ) {
        let id = window.id();
        self.ids.insert(entity, id);
        self.windows.insert(
            id,
            OsWindow {
                entity,
                a11y,
                window,
                sync,
            },
        );
    }

    /// Bring the platform windows in line with the window entities: open a
    /// platform window for each new one, close the one of each entity that
    /// went, and hand each window's changes to it.
    fn sync_windows(&mut self, event_loop: &ActiveEventLoop) {
        if self.primary.is_none() {
            return;
        }
        let wanted: Vec<(Entity, lumen_core::window::Window)> = {
            let mut q = self
                .app
                .world
                .query::<(Entity, &lumen_core::window::Window)>();
            let mut all: Vec<_> = q
                .iter(&self.app.world)
                .map(|(e, w)| (e, w.clone()))
                .collect();
            all.sort_by_key(|(e, _)| *e);
            all
        };
        // Close the windows whose entity went.
        let gone: Vec<Entity> = self
            .ids
            .keys()
            .copied()
            .filter(|e| !wanted.iter().any(|(w, _)| w == e))
            .collect();
        for entity in gone {
            self.renderer.detach(entity);
            if let Some(id) = self.ids.remove(&entity)
                && let Some(mut os) = self.windows.remove(&id)
            {
                // The bridge goes before the window it wraps.
                os.a11y = None;
                drop(os);
            }
        }
        for (entity, wanted) in wanted {
            if !self.ids.contains_key(&entity) {
                self.open(event_loop, entity, &wanted);
                continue;
            }
            self.apply_window_changes(entity, &wanted);
        }
        self.roll_up();
    }

    /// Open a platform window for the window entity `entity`: created
    /// hidden, its accessibility bridge bound, the renderer attached, and
    /// then shown when the app wants it shown (#551 order).
    fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        entity: Entity,
        wanted: &lumen_core::window::Window,
    ) {
        let owner = wanted
            .owner
            .or(self.primary)
            .filter(|_| wanted.kind != WindowKind::Normal)
            .and_then(|o| self.ids.get(&o))
            .and_then(|id| self.windows.get(id))
            .map(|w| w.window.clone());
        let attrs = window_attributes(wanted, owner.as_deref());
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                lumen_core::warn_line!("lumen-window-winit: create_window failed: {e:?}");
                return;
            }
        };
        let (logical, scale) = platform_viewport(&window);
        if let Some(mut vp) = self.app.world.get_mut::<WindowViewport>(entity) {
            vp.size = logical;
            vp.scale_factor = scale;
        }
        seed_window(&mut self.app.world, entity, &wanted.title, logical);
        record_runtime(&mut self.app.world, entity, &window);
        let a11y = self.bridge(event_loop, &window, entity);
        if let Err(e) = self.renderer.attach(
            entity,
            FrameTarget::Window(Arc::new(WinitTarget(window.clone()))),
        ) {
            lumen_core::warn_line!("lumen-window-winit: a window's renderer failed: {e}");
        }
        // A window that does not set the pace does not wait for the
        // display's refresh.
        self.renderer.set_present_mode(entity, PresentMode::NoWait);
        if wanted.visible {
            window.set_visible(true);
        }
        let mut sync = WindowSync::opened(&wanted.title, logical);
        sync.visible = wanted.visible;
        sync.resizable = wanted.resizable;
        sync.decorations = wanted.decorations;
        sync.position = wanted.position;
        sync.min_size = wanted.min_size;
        sync.max_size = wanted.max_size;
        self.insert(entity, window, a11y, sync);
        self.want_frame();
    }

    /// Hand the app's [`lumen_core::window::Window`] changes to `entity`'s
    /// platform window: a new title, visibility, chrome, position and size
    /// limits at once, a new size as a request the window manager answers
    /// with a resize (or refuses). The getters then report what the window
    /// has: the title once applied, the size once the platform resized the
    /// window.
    fn apply_window_changes(&mut self, entity: Entity, wanted: &lumen_core::window::Window) {
        let Some(id) = self.ids.get(&entity).copied() else {
            return;
        };
        let Some(os) = self.windows.get_mut(&id) else {
            return;
        };
        let changes = os.sync.take_changes(wanted);
        if changes.is_empty() {
            return;
        }
        let window = os.window.clone();
        if let Some(title) = &changes.title {
            window.set_title(title);
            lumen_core::window_state::publish_title(Some(entity), title);
        }
        if let Some(visible) = changes.visible {
            window.set_visible(visible);
            if visible {
                self.want_frame();
            }
        }
        if let Some(resizable) = changes.resizable {
            window.set_resizable(resizable);
        }
        if let Some(decorations) = changes.decorations {
            window.set_decorations(decorations);
        }
        if let Some(Some(position)) = changes.position {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(position.x, position.y));
        }
        if let Some(min) = changes.min_size {
            window.set_min_inner_size(min.map(|s| winit::dpi::LogicalSize::new(s.x, s.y)));
        }
        if let Some(max) = changes.max_size {
            window.set_max_inner_size(max.map(|s| winit::dpi::LogicalSize::new(s.x, s.y)));
        }
        if let Some(size) = changes.size {
            // A maximized window has the size the screen gives it and keeps
            // a size request for later (Windows) or refuses it (window
            // managers). Asking for a size leaves the maximized state
            // first, as the window's restore button does.
            if window.is_maximized() {
                window.set_maximized(false);
            }
            let requested = winit::dpi::LogicalSize::new(size.x as f64, size.y as f64);
            // `Some` means the platform applied the size at once and may
            // send no `Resized`; take it the way a resize event lands.
            if let Some(physical) = window.request_inner_size(requested) {
                self.resized(entity, &window, physical);
            }
        }
    }

    /// Raise the windows the app asked to raise.
    fn apply_focus_requests(&mut self) {
        let Some(msgs) = self
            .app
            .world
            .get_resource::<Messages<lumen_core::window::WindowRequest>>()
        else {
            return;
        };
        let raise: Vec<Entity> = self
            .focus_requests
            .read(msgs)
            .filter_map(|r| match r {
                lumen_core::window::WindowRequest::Focus(w) => Some(*w),
                lumen_core::window::WindowRequest::Close(_) => None,
            })
            .collect();
        for entity in raise {
            if let Some(os) = self.ids.get(&entity).and_then(|id| self.windows.get(id)) {
                os.window.focus_window();
            }
        }
    }

    /// After a tick: start a title-bar drag the tick asked for, and show
    /// the cursor and the IME state the UI wants, on the window each one
    /// belongs to.
    fn apply_input_requests(&mut self, event_loop: &ActiveEventLoop) {
        let pointer_window = self
            .app
            .world
            .resource::<PointerState>()
            .window
            .or(self.primary);
        let under_pointer = pointer_window
            .and_then(|e| self.ids.get(&e).copied())
            .and_then(|id| self.windows.get(&id).map(|w| (id, w.window.clone())));
        // Consume any title-bar press -> request a native window drag on the
        // window the press was in. Cleared after the call so a single press
        // initiates a single drag.
        if let Some(mut req) = self
            .app
            .world
            .get_resource_mut::<lumen_core::components::WindowDragRequest>()
            && req.0
        {
            req.0 = false;
            if let Some((_, window)) = under_pointer.as_ref()
                && let Err(e) = window.drag_window()
            {
                lumen_core::warn_line!("lumen-window-winit: drag_window failed: {e}");
            }
        }
        // The cursor shape the UI asked for this tick, on the window the
        // pointer is over. Change-gated so winit isn't hammered.
        if let Some((id, window)) = under_pointer.as_ref()
            && let Some(req) = self.app.world.get_resource::<CursorRequest>()
            && self.last_cursor.as_ref().map(|(w, c)| (w, c)) != Some((id, req))
        {
            let req = req.clone();
            apply_cursor(window, event_loop, &req, &mut self.custom_cursors);
            self.last_cursor = Some((*id, req));
        }
        // IME state follows the focused element's window.
        let focus_window = self
            .app
            .world
            .resource::<FocusTracker>()
            .0
            .and_then(|e| {
                self.app
                    .world
                    .get_resource::<lumen_core::window::WindowPartitions>()
                    .and_then(|p| p.window_of(e))
            })
            .or(self.primary);
        let Some((id, window)) = focus_window
            .and_then(|e| self.ids.get(&e).copied())
            .and_then(|id| self.windows.get(&id).map(|w| (id, w.window.clone())))
        else {
            return;
        };
        let req = *self.app.world.resource::<ImeRequest>();
        let same_window = self.last_ime.0 == Some(id);
        let last = if same_window {
            self.last_ime.1
        } else {
            ImeRequest::default()
        };
        if req.allowed != last.allowed || !same_window {
            window.set_ime_allowed(req.allowed);
        }
        if let Some((origin, size)) = req.cursor_area
            && req.cursor_area != last.cursor_area
        {
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(origin.x as f64, origin.y as f64),
                winit::dpi::PhysicalSize::new(size.x as f64, size.y as f64),
            );
        }
        self.last_ime = (Some(id), req);
    }

    /// Write a touch or pen contact's message, from the window `entity`.
    /// The primary contact also drives the pointer Lumen's own widgets
    /// follow (hover, press, drag), the way a browser makes the primary
    /// touch act as the mouse.
    fn write_pointer_output(&mut self, entity: Entity, output: pointer::PointerOutput) {
        use pointer::PointerOutput;
        let world = &mut self.app.world;
        let data = output.data();
        let stamp = |d: PointerData| d.stamped();
        if data.is_primary {
            let mut state = world.resource_mut::<PointerState>();
            state.window = Some(entity);
            match output {
                PointerOutput::Moved(p, _) => state.position = Some(p),
                PointerOutput::Pressed(p, button, _) => {
                    state.position = Some(p);
                    if button == PointerButton::Primary {
                        state.primary_down = true;
                    }
                }
                PointerOutput::Released(p, _, _) | PointerOutput::Cancelled(p, _) => {
                    state.position = Some(p);
                    state.primary_down = false;
                }
            }
        }
        match output {
            PointerOutput::Moved(position, pointer) => {
                world.write_message(PointerMoved {
                    position,
                    local: None,
                    pointer: stamp(pointer),
                });
            }
            PointerOutput::Pressed(position, button, pointer) => {
                // The press lands where the contact is: hit testing reads
                // the position the move carries first.
                world.write_message(PointerMoved {
                    position,
                    local: None,
                    pointer: stamp(pointer),
                });
                world.write_message(PointerPressed {
                    position,
                    button,
                    local: None,
                    pointer: stamp(pointer),
                });
            }
            PointerOutput::Released(position, button, pointer) => {
                world.write_message(PointerReleased {
                    position,
                    button,
                    local: None,
                    pointer: stamp(pointer),
                });
            }
            PointerOutput::Cancelled(position, pointer) => {
                world.write_message(lumen_core::input::PointerCancelled {
                    position,
                    pointer: stamp(pointer),
                });
            }
        }
        self.want_frame();
    }

    /// `entity`'s window inner size changed (a `Resized` event, or a size
    /// request the platform applied at once).
    ///
    /// Resize-flood coalescing: a live drag delivers many `Resized` events.
    /// The renderer returns `false` when the physical size is unchanged, so
    /// we skip the surface recreate + viewport rewrite + relayout + paint that
    /// a duplicate event would otherwise force. Only a genuine size change
    /// does work, and each distinct size gets exactly one synchronous paint.
    fn resized(
        &mut self,
        entity: Entity,
        window: &Window,
        new_size: winit::dpi::PhysicalSize<u32>,
    ) {
        if new_size.width > 0
            && new_size.height > 0
            && self
                .renderer
                .resize(entity, new_size.width, new_size.height)
        {
            // The viewport carries LOGICAL pixels. winit delivers
            // `new_size` as `PhysicalSize`; convert via the window's
            // current scale factor before writing.
            let scale_factor = window.scale_factor() as f32;
            let logical = new_size.to_logical::<f32>(scale_factor as f64);
            let size = glam::Vec2::new(logical.width, logical.height);
            tracing::debug!(
                target: "lumen::window::resize",
                physical_w = new_size.width,
                physical_h = new_size.height,
                scale = scale_factor,
                logical_w = size.x,
                logical_h = size.y,
                "resize",
            );
            self.platform_resized(entity, size, scale_factor);
            record_runtime(&mut self.app.world, entity, window);
            // Smart reactive resize: relayout + repaint synchronously
            // inside the resize event. Layout sees the new viewport and
            // re-lays out the window's roots; the frame-dirty roll-up folds
            // the viewport change in; the present then commits a correctly
            // sized buffer this frame. This is the smooth path GTK/Qt take
            // (paint in the configure/resize callback) versus deferring to
            // the next loop iteration.
            self.tick();
            // Resize recreated the frame texture - force a full repaint of
            // this window even if its tree matches the last one.
            self.present(Some(entity));
        }
        // Fallback: request one follow-up redraw in case the synchronous
        // present skipped (surface `Outdated` during a fast drag). It's a
        // cheap no-op tick when the viewport is already settled and
        // nothing is dirty.
        self.want_frame();
    }

    /// The platform window of `entity` is `size` logical pixels at
    /// `scale_factor`: its viewport takes it, and so does the app's window,
    /// through [`take_platform_size`].
    fn platform_resized(&mut self, entity: Entity, size: glam::Vec2, scale_factor: f32) {
        if let Some(mut vp) = self.app.world.get_mut::<WindowViewport>(entity) {
            vp.size = size;
            vp.scale_factor = scale_factor;
        }
        let Some(os) = self
            .ids
            .get(&entity)
            .and_then(|id| self.windows.get_mut(id))
        else {
            return;
        };
        match self.app.world.get_mut::<lumen_core::window::Window>(entity) {
            Some(mut wanted) => {
                take_platform_size(wanted.bypass_change_detection(), &mut os.sync.size, size);
            }
            None => os.sync.size = size,
        }
    }

    /// Publish every window's accessibility tree, then present every shown,
    /// unoccluded window whose frame changed; `force` names a window whose
    /// buffers were just recreated and must repaint in full.
    fn present(&mut self, force: Option<Entity>) {
        let mut targets: Vec<PresentTarget<'_>> = Vec::with_capacity(self.windows.len());
        let world = &self.app.world;
        let mut entries: Vec<&mut OsWindow> = self.windows.values_mut().collect();
        entries.sort_by_key(|w| (Some(w.entity) != self.primary, w.entity));
        for os in entries {
            let shown = world
                .get::<lumen_core::window::Window>(os.entity)
                .is_some_and(|w| w.visible)
                && !world
                    .get::<WindowRuntime>(os.entity)
                    .is_some_and(|r| r.occluded || r.minimized);
            targets.push(PresentTarget {
                window: os.entity,
                a11y: os.a11y.as_deref_mut(),
                shown,
                force_full: force == Some(os.entity),
            });
        }
        present_frame(&mut self.app, self.renderer.as_mut(), &mut targets);
    }
}

/// The attributes of the platform window a [`lumen_core::window::Window`]
/// opens as: created hidden (shown once its accessibility bridge is bound),
/// at the size, position, chrome and size limits the app asked for, owned by
/// `owner` when it is a dialog or tool window.
fn window_attributes(
    wanted: &lumen_core::window::Window,
    owner: Option<&Window>,
) -> WindowAttributes {
    let mut attrs = WindowAttributes::default()
        .with_title(&wanted.title)
        .with_inner_size(winit::dpi::LogicalSize::new(
            wanted.size.x as f64,
            wanted.size.y as f64,
        ))
        .with_resizable(wanted.resizable)
        .with_decorations(wanted.decorations)
        .with_visible(false);
    if let Some(p) = wanted.position {
        attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(p.x, p.y));
    }
    if let Some(min) = wanted.min_size {
        attrs = attrs.with_min_inner_size(winit::dpi::LogicalSize::new(min.x, min.y));
    }
    if let Some(max) = wanted.max_size {
        attrs = attrs.with_max_inner_size(winit::dpi::LogicalSize::new(max.x, max.y));
    }
    owned_window_attributes(attrs, wanted.kind, owner)
}

/// Make `attrs` a window owned by `owner` where the platform has the notion:
/// on Windows an owned window (above its owner, minimized with it, no
/// taskbar entry), on macOS a child window that moves with its parent, on
/// X11 a dialog or utility window type the window manager places as one.
#[allow(unused_variables, unused_mut)]
fn owned_window_attributes(
    mut attrs: WindowAttributes,
    kind: WindowKind,
    owner: Option<&Window>,
) -> WindowAttributes {
    if kind == WindowKind::Normal {
        return attrs;
    }
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::RawWindowHandle;
        use winit::platform::windows::WindowAttributesExtWindows;
        if let Some(RawWindowHandle::Win32(handle)) = owner
            .and_then(|o| o.window_handle().ok())
            .map(|h| h.as_raw())
        {
            attrs = attrs
                .with_owner_window(handle.hwnd.get())
                .with_skip_taskbar(true);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(raw) = owner
            .and_then(|o| o.window_handle().ok())
            .map(|h| h.as_raw())
        {
            // SAFETY: `raw` comes from a live window this backend owns, and
            // that window outlives the child it parents: an owned window
            // closes before its owner (the owner's close ends the app).
            attrs = unsafe { attrs.with_parent_window(Some(raw)) };
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use winit::platform::x11::{WindowAttributesExtX11, WindowType};
        let x11_type = match kind {
            WindowKind::Tool => WindowType::Utility,
            _ => WindowType::Dialog,
        };
        attrs = attrs.with_x11_window_type(vec![x11_type]);
    }
    attrs
}

/// What a platform window was last given, so only what the app changes
/// since reaches the platform.
#[derive(Clone, Debug, PartialEq)]
struct WindowSync {
    title: String,
    /// The inner size, in logical pixels, last asked of the platform window
    /// or reported by it. A [`lumen_core::window::Window`] size that differs
    /// is a new request.
    size: glam::Vec2,
    visible: bool,
    resizable: bool,
    decorations: bool,
    position: Option<glam::IVec2>,
    min_size: Option<glam::Vec2>,
    max_size: Option<glam::Vec2>,
}

/// The changes [`WindowSync::take_changes`] found: each `Some` is a value to
/// hand the platform window.
#[derive(Clone, Debug, Default, PartialEq)]
struct WindowChanges {
    title: Option<String>,
    size: Option<glam::Vec2>,
    visible: Option<bool>,
    resizable: Option<bool>,
    decorations: Option<bool>,
    position: Option<Option<glam::IVec2>>,
    min_size: Option<Option<glam::Vec2>>,
    max_size: Option<Option<glam::Vec2>>,
}

impl WindowChanges {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl WindowSync {
    /// A window the platform opened titled `title` at `size`, shown,
    /// resizable and decorated.
    fn opened(title: &str, size: glam::Vec2) -> Self {
        Self {
            title: title.to_string(),
            size,
            visible: true,
            resizable: true,
            decorations: true,
            position: None,
            min_size: None,
            max_size: None,
        }
    }

    /// Take what of `wanted` the platform window does not have yet and record
    /// it as handed over. A size no window can have is never asked for.
    fn take_changes(&mut self, wanted: &lumen_core::window::Window) -> WindowChanges {
        let mut changes = WindowChanges::default();
        if wanted.title != self.title {
            self.title = wanted.title.clone();
            changes.title = Some(wanted.title.clone());
        }
        if wanted.size != self.size && is_window_size(wanted.size.x, wanted.size.y) {
            self.size = wanted.size;
            changes.size = Some(wanted.size);
        }
        if wanted.visible != self.visible {
            self.visible = wanted.visible;
            changes.visible = Some(wanted.visible);
        }
        if wanted.resizable != self.resizable {
            self.resizable = wanted.resizable;
            changes.resizable = Some(wanted.resizable);
        }
        if wanted.decorations != self.decorations {
            self.decorations = wanted.decorations;
            changes.decorations = Some(wanted.decorations);
        }
        if wanted.position != self.position {
            self.position = wanted.position;
            changes.position = Some(wanted.position);
        }
        if wanted.min_size != self.min_size {
            self.min_size = wanted.min_size;
            changes.min_size = Some(wanted.min_size);
        }
        if wanted.max_size != self.max_size {
            self.max_size = wanted.max_size;
            changes.max_size = Some(wanted.max_size);
        }
        changes
    }
}

/// The close path, shared by the windows' close buttons
/// ([`WindowEvent::CloseRequested`]), Unix signals
/// ([`UserEvent::CloseRequested`]), and the app's own `window.close()` /
/// `app.quit()`.
#[derive(Default)]
struct CloseRouting {
    /// The app's `window.close()` / `app.quit()` requests, read after every
    /// tick: a request lives for two ticks, and one batch of events can run
    /// more than that.
    requests: CloseRequests,
    /// What the app asked to close, run from `about_to_wait`.
    pending: Option<CloseAsk>,
    /// Set once a close ended the app (no system wrote
    /// `CloseRequest { vetoed: true }`, and what closed was the primary
    /// window or the whole app). Read by
    /// [`ApplicationHandler::about_to_wait`] to trigger `event_loop.exit()`.
    committed: bool,
}

impl CloseRouting {
    /// Keep any close the app asked for during the tick that just ran. A
    /// quit outranks window closes.
    fn after_tick(&mut self, world: &World) {
        if let Some(ask) = self.requests.take(world) {
            self.pending = Some(match (self.pending.take(), ask) {
                (Some(CloseAsk::Quit), _) | (_, CloseAsk::Quit) => CloseAsk::Quit,
                (Some(CloseAsk::Windows(mut before)), CloseAsk::Windows(now)) => {
                    for w in now {
                        if !before.contains(&w) {
                            before.push(w);
                        }
                    }
                    CloseAsk::Windows(before)
                }
                (None, ask) => ask,
            });
        }
    }

    /// Run the close the app asked for since the last call, if any.
    fn run_pending(&mut self, app: &mut App) {
        if self.committed {
            return;
        }
        if let Some(ask) = self.pending.take() {
            let outcome = run_close_ask(app, &mut self.requests, ask);
            self.settle(app, outcome);
        }
    }

    /// Graceful close of `window`, or of the whole app for `None`.
    ///
    /// Runs [`run_close_request`]: emits `CloseRequest { window, reason,
    /// vetoed: false }`, then one synchronous tick so every app-level close
    /// hook observes it before any teardown. A hook keeps the window open by
    /// writing a fresh `CloseRequest { vetoed: true, .. }` during that tick -
    /// mirroring `QCloseEvent::ignore()` / GTK4's `close-request -> TRUE`.
    /// A secondary window that closes is despawned and its platform window
    /// goes on the next `about_to_wait`; a close that ends the app sets
    /// `committed` and `about_to_wait` exits the loop.
    ///
    /// Runs synchronously in the event arm rather than deferring to the
    /// next `RedrawRequested`, so the close also works while the
    /// [`RedrawScheduler`] is paused (unfocused / occluded window).
    fn run(&mut self, app: &mut App, window: Option<Entity>, reason: CloseReason) {
        if self.committed {
            return;
        }
        let outcome = run_close_request(app, &mut self.requests, window, reason);
        self.settle(app, outcome);
    }

    fn settle(&mut self, app: &mut App, outcome: CloseOutcome) {
        match outcome {
            CloseOutcome::Quit => self.committed = true,
            // Keep running; schedule a paint so whatever the veto handler
            // changed (e.g. a "save before quit?" dialog), or whatever the
            // closed window's content moved back into, becomes visible.
            CloseOutcome::Vetoed | CloseOutcome::Closed | CloseOutcome::Hidden => {
                if let Some(mut sch) = app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
        }
    }
}

/// Record that the platform window has `size`. The app's [`Window`] takes it
/// unless the app asked for a size the platform has not been asked for yet: a
/// resize the platform made on its own (maximizing the window it just showed,
/// say) must not drop that request, which goes out after the tick.
///
/// [`Window`]: lumen_core::window::Window
fn take_platform_size(
    wanted: &mut lumen_core::window::Window,
    applied_size: &mut glam::Vec2,
    size: glam::Vec2,
) {
    if wanted.size == *applied_size {
        wanted.size = size;
    }
    *applied_size = size;
}

/// W3.5: build an [`ImeSurroundingResponse`] for the currently focused
/// editable and push it onto the message bus so any OS-bound forwarder
/// (Wayland text-input-v3, IBus) can ship it to the IME. No-op when no
/// editable is focused; cheap when one is (a single rope->String snapshot).
fn push_ime_surrounding_response(world: &mut World) {
    let Some(focused) = world.resource::<FocusTracker>().0 else {
        return;
    };
    // Snapshot first to drop the entity borrow before grabbing the
    // message-bus resource.
    let snapshot = {
        let mut q = world.query::<(&TextBuffer, &TextCursor)>();
        q.get(world, focused).ok().map(|(buf, cur)| {
            let text: Arc<str> = buf.into();
            (text, cur.anchor.byte, cur.head.byte)
        })
    };
    let Some((text, anchor_byte, cursor_byte)) = snapshot else {
        return;
    };
    let Some(mut msgs) = world.get_resource_mut::<Messages<ImeSurroundingResponse>>() else {
        // Bus not yet initialized: install it lazily so consumers don't
        // have to remember the workspace wiring.
        world.init_resource::<Messages<ImeSurroundingResponse>>();
        if let Some(mut msgs) = world.get_resource_mut::<Messages<ImeSurroundingResponse>>() {
            msgs.write(ImeSurroundingResponse {
                entity: focused,
                text,
                anchor_byte,
                cursor_byte,
            });
        }
        return;
    };
    msgs.write(ImeSurroundingResponse {
        entity: focused,
        text,
        anchor_byte,
        cursor_byte,
    });
}

fn map_modifiers(m: WinitModifiers) -> Modifiers {
    Modifiers {
        shift: m.shift_key(),
        ctrl: m.control_key(),
        alt: m.alt_key(),
        super_: m.super_key(),
    }
}

/// Platform cursors made from decoded cursor images, keyed by the image's
/// identity and hotspot. Each entry keeps the pixels it was made from, which
/// keeps their address, and so the identity, from being reused by another
/// image.
type CustomCursors =
    std::collections::HashMap<(u64, (u32, u32)), (Arc<[u8]>, winit::window::CustomCursor)>;

/// Show `req` over `window`: its decoded image when it has one, its keyword
/// otherwise. `none` hides the pointer.
fn apply_cursor(
    window: &Window,
    event_loop: &ActiveEventLoop,
    req: &CursorRequest,
    cache: &mut CustomCursors,
) {
    let custom = req.image.as_ref().and_then(|image| {
        let key = (image.id, image.hotspot);
        if let Some((pixels, cursor)) = cache.get(&key)
            && Arc::ptr_eq(pixels, &image.rgba)
        {
            return Some(cursor.clone());
        }
        let size = |v: u32| u16::try_from(v).ok();
        let source = winit::window::CustomCursor::from_rgba(
            image.rgba.to_vec(),
            size(image.width)?,
            size(image.height)?,
            size(image.hotspot.0)?,
            size(image.hotspot.1)?,
        );
        match source {
            Ok(source) => {
                let cursor = event_loop.create_custom_cursor(source);
                cache.insert(key, (image.rgba.clone(), cursor.clone()));
                Some(cursor)
            }
            Err(e) => {
                lumen_core::warn_line!("lumen-window-winit: cannot use the cursor image: {e}");
                None
            }
        }
    });
    let hidden = custom.is_none() && req.cursor.keyword == CursorKeyword::None;
    window.set_cursor_visible(!hidden);
    match custom {
        Some(cursor) => window.set_cursor(cursor),
        None => window.set_cursor(map_cursor_keyword(req.cursor.keyword)),
    }
}

/// Map a CSS cursor keyword onto winit's OS cursor icon set, which names
/// the same cursors.
fn map_cursor_keyword(keyword: CursorKeyword) -> winit::window::CursorIcon {
    use winit::window::CursorIcon;
    match keyword {
        CursorKeyword::Auto | CursorKeyword::Default | CursorKeyword::None => CursorIcon::Default,
        CursorKeyword::ContextMenu => CursorIcon::ContextMenu,
        CursorKeyword::Help => CursorIcon::Help,
        CursorKeyword::Pointer => CursorIcon::Pointer,
        CursorKeyword::Progress => CursorIcon::Progress,
        CursorKeyword::Wait => CursorIcon::Wait,
        CursorKeyword::Cell => CursorIcon::Cell,
        CursorKeyword::Crosshair => CursorIcon::Crosshair,
        CursorKeyword::Text => CursorIcon::Text,
        CursorKeyword::VerticalText => CursorIcon::VerticalText,
        CursorKeyword::Alias => CursorIcon::Alias,
        CursorKeyword::Copy => CursorIcon::Copy,
        CursorKeyword::Move => CursorIcon::Move,
        CursorKeyword::NoDrop => CursorIcon::NoDrop,
        CursorKeyword::NotAllowed => CursorIcon::NotAllowed,
        CursorKeyword::Grab => CursorIcon::Grab,
        CursorKeyword::Grabbing => CursorIcon::Grabbing,
        CursorKeyword::EResize => CursorIcon::EResize,
        CursorKeyword::NResize => CursorIcon::NResize,
        CursorKeyword::NeResize => CursorIcon::NeResize,
        CursorKeyword::NwResize => CursorIcon::NwResize,
        CursorKeyword::SResize => CursorIcon::SResize,
        CursorKeyword::SeResize => CursorIcon::SeResize,
        CursorKeyword::SwResize => CursorIcon::SwResize,
        CursorKeyword::WResize => CursorIcon::WResize,
        CursorKeyword::EwResize => CursorIcon::EwResize,
        CursorKeyword::NsResize => CursorIcon::NsResize,
        CursorKeyword::NeswResize => CursorIcon::NeswResize,
        CursorKeyword::NwseResize => CursorIcon::NwseResize,
        CursorKeyword::ColResize => CursorIcon::ColResize,
        CursorKeyword::RowResize => CursorIcon::RowResize,
        CursorKeyword::AllScroll => CursorIcon::AllScroll,
        CursorKeyword::ZoomIn => CursorIcon::ZoomIn,
        CursorKeyword::ZoomOut => CursorIcon::ZoomOut,
    }
}

fn map_key(ev: &KeyEvent) -> Option<Key> {
    match &ev.logical_key {
        WinitKey::Named(named) => Some(match named {
            // Names that already exist in [`lumen_core::input::NamedKey`].
            WinitNamed::Tab => Key::Named(NamedKey::Tab),
            WinitNamed::Enter => Key::Named(NamedKey::Enter),
            WinitNamed::Escape => Key::Named(NamedKey::Escape),
            WinitNamed::Backspace => Key::Named(NamedKey::Backspace),
            WinitNamed::Space => Key::Named(NamedKey::Space),
            WinitNamed::ArrowUp => Key::Named(NamedKey::ArrowUp),
            WinitNamed::ArrowDown => Key::Named(NamedKey::ArrowDown),
            WinitNamed::ArrowLeft => Key::Named(NamedKey::ArrowLeft),
            WinitNamed::ArrowRight => Key::Named(NamedKey::ArrowRight),
            WinitNamed::Home => Key::Named(NamedKey::Home),
            WinitNamed::End => Key::Named(NamedKey::End),
            WinitNamed::Delete => Key::Named(NamedKey::Delete),
            // Bug 12 - winit named keys that don't have a typed
            // `NamedKey` variant on our side are forwarded as
            // `Key::Character(canonical_name)` so apps can still react
            // (eg. `on_keydown("F1") {...}`). Canonical names follow the
            // W3C UI Events `key` attribute where possible
            // (https://w3c.github.io/uievents-key/) so cross-platform
            // bindings stay portable.
            WinitNamed::PageUp => Key::Character("PageUp".into()),
            WinitNamed::PageDown => Key::Character("PageDown".into()),
            WinitNamed::Insert => Key::Character("Insert".into()),
            WinitNamed::CapsLock => Key::Character("CapsLock".into()),
            WinitNamed::NumLock => Key::Character("NumLock".into()),
            WinitNamed::ScrollLock => Key::Character("ScrollLock".into()),
            WinitNamed::PrintScreen => Key::Character("PrintScreen".into()),
            WinitNamed::Pause => Key::Character("Pause".into()),
            WinitNamed::ContextMenu => Key::Character("ContextMenu".into()),
            WinitNamed::Shift => Key::Character("Shift".into()),
            WinitNamed::Control => Key::Character("Control".into()),
            WinitNamed::Alt => Key::Character("Alt".into()),
            WinitNamed::Meta => Key::Character("Meta".into()),
            WinitNamed::Super => Key::Character("Super".into()),
            WinitNamed::F1 => Key::Character("F1".into()),
            WinitNamed::F2 => Key::Character("F2".into()),
            WinitNamed::F3 => Key::Character("F3".into()),
            WinitNamed::F4 => Key::Character("F4".into()),
            WinitNamed::F5 => Key::Character("F5".into()),
            WinitNamed::F6 => Key::Character("F6".into()),
            WinitNamed::F7 => Key::Character("F7".into()),
            WinitNamed::F8 => Key::Character("F8".into()),
            WinitNamed::F9 => Key::Character("F9".into()),
            WinitNamed::F10 => Key::Character("F10".into()),
            WinitNamed::F11 => Key::Character("F11".into()),
            WinitNamed::F12 => Key::Character("F12".into()),
            WinitNamed::F13 => Key::Character("F13".into()),
            WinitNamed::F14 => Key::Character("F14".into()),
            WinitNamed::F15 => Key::Character("F15".into()),
            WinitNamed::F16 => Key::Character("F16".into()),
            WinitNamed::F17 => Key::Character("F17".into()),
            WinitNamed::F18 => Key::Character("F18".into()),
            WinitNamed::F19 => Key::Character("F19".into()),
            WinitNamed::F20 => Key::Character("F20".into()),
            WinitNamed::F21 => Key::Character("F21".into()),
            WinitNamed::F22 => Key::Character("F22".into()),
            WinitNamed::F23 => Key::Character("F23".into()),
            WinitNamed::F24 => Key::Character("F24".into()),
            WinitNamed::F25 => Key::Character("F25".into()),
            WinitNamed::F26 => Key::Character("F26".into()),
            WinitNamed::F27 => Key::Character("F27".into()),
            WinitNamed::F28 => Key::Character("F28".into()),
            WinitNamed::F29 => Key::Character("F29".into()),
            WinitNamed::F30 => Key::Character("F30".into()),
            WinitNamed::F31 => Key::Character("F31".into()),
            WinitNamed::F32 => Key::Character("F32".into()),
            WinitNamed::F33 => Key::Character("F33".into()),
            WinitNamed::F34 => Key::Character("F34".into()),
            WinitNamed::F35 => Key::Character("F35".into()),
            WinitNamed::BrowserBack => Key::Character("BrowserBack".into()),
            WinitNamed::BrowserForward => Key::Character("BrowserForward".into()),
            WinitNamed::BrowserHome => Key::Character("BrowserHome".into()),
            WinitNamed::BrowserRefresh => Key::Character("BrowserRefresh".into()),
            WinitNamed::BrowserSearch => Key::Character("BrowserSearch".into()),
            WinitNamed::BrowserStop => Key::Character("BrowserStop".into()),
            WinitNamed::BrowserFavorites => Key::Character("BrowserFavorites".into()),
            WinitNamed::MediaPlayPause => Key::Character("MediaPlayPause".into()),
            WinitNamed::MediaPlay => Key::Character("MediaPlay".into()),
            WinitNamed::MediaPause => Key::Character("MediaPause".into()),
            WinitNamed::MediaStop => Key::Character("MediaStop".into()),
            WinitNamed::MediaTrackNext => Key::Character("MediaTrackNext".into()),
            WinitNamed::MediaTrackPrevious => Key::Character("MediaTrackPrevious".into()),
            WinitNamed::AudioVolumeUp => Key::Character("AudioVolumeUp".into()),
            WinitNamed::AudioVolumeDown => Key::Character("AudioVolumeDown".into()),
            WinitNamed::AudioVolumeMute => Key::Character("AudioVolumeMute".into()),
            // Truly-unmapped variant. Log at trace so CI can surface
            // missing mappings without spamming production logs; return
            // None so the event is dropped (same as pre-W1.8 silent
            // behaviour, but discoverable).
            other => {
                tracing::trace!(
                    target: "lumen::window::key",
                    ?other,
                    "map_key: dropping unmapped winit NamedKey",
                );
                return None;
            }
        }),
        WinitKey::Character(s) => Some(Key::Character(s.to_string())),
        other => {
            tracing::trace!(
                target: "lumen::window::key",
                ?other,
                "map_key: dropping non-Named non-Character winit key",
            );
            None
        }
    }
}

fn map_button(b: MouseButton) -> PointerButton {
    match b {
        MouseButton::Left => PointerButton::Primary,
        MouseButton::Right => PointerButton::Secondary,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Other(n) => PointerButton::Other(n),
        MouseButton::Back => PointerButton::Back,
        MouseButton::Forward => PointerButton::Forward,
    }
}

/// Emit the one-time startup marker on the first on-screen present.
///
/// Off unless `LUMEN_BOOT_TRACE` is set, so a normal run prints nothing:
/// the very first render swaps the guard and every later frame short-
/// circuits on the atomic. When on, prints to stdout
///
/// ```text
/// first_frame
/// startup_ms:<exec->first-frame ms>
/// ```
///
/// The bare `first_frame` line is the spawn->marker signal the benchmark
/// harness reads for external startup - the same line every native bench
/// app prints - so Lumen is timed by an identical method instead of the
/// old MCP frame-counter poll. `startup_ms:` carries the in-app
/// exec->first-frame duration (from [`lumen_core::app::mark_process_start`]),
/// the windowed counterpart of the headless boot-trace total. The
/// `startup_ms:` line is omitted if no process-start instant was recorded
/// (embedders that never call `mark_process_start`).
fn emit_first_frame_marker() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: AtomicBool = AtomicBool::new(false);
    if EMITTED.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("LUMEN_BOOT_TRACE").is_none() {
        return;
    }
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "first_frame");
    if let Some(start) = lumen_core::app::process_start() {
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let _ = writeln!(out, "startup_ms:{ms:.3}");
    }
    let _ = out.flush();
}

/// Whether an off-thread screenshot waits on `window`'s frame.
fn capture_wanted(render_world: &World, window: Entity) -> bool {
    render_world
        .get_resource::<SurfaceCapture>()
        .is_some_and(|c| c.wants(window, is_primary_target(render_world, window)))
}

/// One window [`present_frame`] presents: its entity (the renderer's
/// target), its accessibility bridge, whether it is shown and unoccluded,
/// and whether its buffers were just recreated.
struct PresentTarget<'a> {
    window: Entity,
    a11y: Option<&'a mut dyn A11yBackend>,
    shown: bool,
    force_full: bool,
}

/// Present the current world state: publish each window's accessibility
/// tree, then ask the renderer for a frame of each shown window when this
/// tick produced one, and clear the dirty + pending flags.
///
/// Shared by the `RedrawRequested` arm (normal cadence) and the `Resized` /
/// `ScaleFactorChanged` arms (synchronous live-resize paint). Doing the
/// paint inside the resize event - instead of only requesting a deferred
/// redraw - is what gives smooth live resize on Wayland and macOS, where
/// the compositor expects a correctly sized buffer committed in response to
/// each configure rather than a round trip later (content otherwise
/// stretches and lags behind the drag). Assumes `app.tick()` has already
/// run this iteration, so `FrameDirty` and the extracted scenes are current.
///
/// A target's `force_full` tells the renderer its buffers were just
/// recreated, so it must repaint even when that window's scene is
/// unchanged. Each window's own diff decides whether it repaints, so one
/// window changing does not repaint the others.
fn present_frame(app: &mut App, renderer: &mut dyn Renderer, targets: &mut [PresentTarget<'_>]) {
    // The accessibility trees are built in `TickStage::A11ySync`; this only
    // publishes them, and does nothing when the tick produced no update.
    for target in targets.iter_mut() {
        if let Some(a11y) = target.a11y.as_deref_mut() {
            a11y.publish(&mut app.world);
        }
    }
    // `FrameDirty` folds every render-relevant `Changed<T>` and property
    // write - including a window's viewport change on resize - into a single
    // bool. It over-approximates: a signal re-set to the same value or a
    // hover class that resolves to the same visuals raises it while leaving
    // the painted trees identical. The renderer applies whatever finer test
    // it has (the retained-scene diff) and answers whether a frame is
    // worth putting up.
    let frame_dirty = app
        .world
        .get_resource::<FrameDirty>()
        .map(|f| f.dirty)
        .unwrap_or(true);
    let mut presented = false;
    for target in targets.iter() {
        // A covered or minimized window still answers a screenshot asked of
        // it.
        if !target.shown && !capture_wanted(&app.render_world, target.window) {
            continue;
        }
        let request = FrameRequest {
            dirty: frame_dirty,
            force_full: target.force_full,
        };
        if renderer.wants_present(target.window, &mut app.render_world, request) {
            match renderer.present(target.window, &mut app.render_world) {
                // First real on-screen present: emit the startup marker (once,
                // env-gated). This is the windowed analog of the headless
                // boot-trace's exec->first-frame line.
                Ok(()) => emit_first_frame_marker(),
                Err(e) => lumen_core::warn_line!("lumen-window-winit: render failed: {e}"),
            }
            presented = true;
        }
    }
    if presented {
        lumen_core::render_backend::publish_renderer_info(&mut app.render_world, renderer.info());
        // The presents happened after the tick recorded its timing; add them.
        app.record_render_reports();
    }
    // `FrameDirty` is consumed whichever branch ran: an empty-damage dirty flag
    // has been fully accounted for (no visible change this tick), and leaving it
    // set would spin the `work_pending` re-arm below into an endless redraw loop
    // of skipped frames.
    if frame_dirty && let Some(mut fd) = app.world.get_resource_mut::<FrameDirty>() {
        fd.dirty = false;
    }
    // Clear the pending flag - this paint is now in flight. Future
    // redraws are scheduled by event handlers writing `pending = true`;
    // `about_to_wait` forwards to winit only when some window is not
    // occluded (focus is not a factor).
    if let Some(mut sch) = app.world.get_resource_mut::<RedrawScheduler>() {
        sch.pending = false;
    }
    // Self-schedule a follow-up frame when this tick left work behind.
    // Nothing else wakes the loop once the OS event queue drains, so
    // without this the app parks with a stale frame until an unrelated
    // event arrives (the "click counter only updates on the next mouse
    // move", "press tint never finishes fading" class of bugs).
    if work_pending(&app.world)
        && let Some(mut sch) = app.world.get_resource_mut::<RedrawScheduler>()
    {
        sch.pending = true;
    }
}

/// Thin shim adapting the winit `Arc<Window>` to the
/// `raw_window_handle::HasWindowHandle` trait `lumen-os-menu` expects.
/// The muda integration that previously lived here moved to
/// `lumen-os-menu` per W6.3.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn attach_menubar_via_os_menu(window: &Arc<Window>, spec: &MenuModel) {
    lumen_os_menu::attach_native_menubar(spec, Some(window.as_ref()));
}

/// Linux / other-target stub - `lumen-os-menu::attach_native_menubar`
/// takes a unit-typed `&()` instead of a `HasWindowHandle` reference
/// when the muda dep is absent.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn attach_menubar_via_os_menu(_window: &Arc<Window>, spec: &MenuModel) {
    lumen_os_menu::attach_native_menubar(spec, None);
}

/// Best-effort: subscribe to the desktop's color-scheme preference via
/// `org.freedesktop.portal.Settings` (the standard XDG Settings portal)
/// and translate every change into a
/// [`lumen_core::command::Command::Typed`] carrying an
/// [`XdgColorSchemeUpdate`] payload. The [`run`]-registered
/// handler then applies it to [`lumen_core::components::StyleManager`]
/// during the next [`lumen_core::tick::TickStage::CommandDrain`].
///
/// ## Execution model
///
/// The listener runs on its own dedicated OS thread, driving the ashpd
/// futures with a plain `pollster::block_on`. ashpd is compiled with
/// its `async-io` feature (see the Cargo.toml note), so every future
/// completes through async-io's self-contained global reactor thread -
/// no external executor context is needed or assumed. The previous
/// version spawned onto the shared `lumen_async_tokio::TokioRuntime`,
/// which (a) the default `lumenc` stack never installs, silently
/// disabling the listener in every markup app, and (b) parked
/// zbus-adjacent code inside a tokio context it must never depend on -
/// the "no reactor running" panic class whenever cargo feature
/// unification flips any zbus consumer onto the tokio backend.
///
/// Spawns at most once per process (`resumed` can run again after a
/// suspend). Falls back silently when the portal call returns an error
/// (no `xdg-desktop-portal` daemon, e.g. headless CI / minimal
/// containers) - the runtime then relies on the
/// [`WindowEvent::ThemeChanged`] path, which on Linux only fires on a
/// subset of compositors but is the only fallback we can offer there.
#[cfg(target_os = "linux")]
fn try_spawn_xdg_color_scheme_listener(world: &mut World) {
    use futures_util::StreamExt;
    static SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SPAWNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    // Clone the bounded `CommandQueue` sender so the listener thread
    // can post `Command::Typed` updates without re-borrowing the world.
    let sender = world
        .resource::<lumen_core::command::CommandQueue>()
        .sender()
        .clone();
    // Wake handle: a queued command is invisible until a tick runs, and
    // a parked loop gets none - without the wake a theme flip sat
    // undrained until the next incidental input event (skins froze on
    // the old theme; restyle tweens stalled mid-flight).
    let waker = world
        .get_resource::<lumen_core::app::EventLoopWaker>()
        .cloned();
    let spawn_result = std::thread::Builder::new()
        .name("lumen-xdg-theme".into())
        .spawn(move || {
            pollster::block_on(async move {
                let settings = match ashpd::desktop::settings::Settings::new().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!(
                            "lumen-window-winit: XDG Settings portal unavailable, falling back to winit ThemeChanged ({e})"
                        );
                        return;
                    }
                };
                let mut stream = match settings.receive_color_scheme_changed().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!(
                            "lumen-window-winit: failed to subscribe to color-scheme changes ({e})"
                        );
                        return;
                    }
                };
                while let Some(scheme) = stream.next().await {
                    let dark = matches!(
                        scheme,
                        ashpd::desktop::settings::ColorScheme::PreferDark
                    );
                    let cmd = lumen_core::command::Command::Typed {
                        type_id: std::any::TypeId::of::<XdgColorSchemeUpdate>(),
                        payload: Box::new(XdgColorSchemeUpdate { dark }),
                    };
                    if sender.try_send(cmd).is_err() {
                        // Bounded queue is full - the main thread is wedged or
                        // overloaded; drop this update and wait for the next.
                        tracing::warn!(
                            "lumen-window-winit: CommandQueue full while delivering XDG color-scheme update"
                        );
                    } else if let Some(waker) = &waker {
                        // Interrupt the parked loop so the flip applies now.
                        waker.wake();
                    }
                }
            });
        });
    if let Err(e) = spawn_result {
        tracing::debug!("lumen-window-winit: failed to spawn XDG theme listener thread ({e})");
    }
}

/// A winit wheel delta as the logical pixels [`MouseWheel`] carries.
///
/// Line-based wheel events are normalised at 32 px/line, which matches
/// GTK/X11's gtk-scroll-lines default for a 16pt line; apps tune
/// per-container feel through `Scroll::sensitivity` rather than this. winit
/// reports a wheel turned toward the user as negative, and `MouseWheel`
/// follows the DOM, where that is positive, so the sign flips.
fn wheel_delta(delta: MouseScrollDelta) -> glam::Vec2 {
    const LINE_PX: f32 = 32.0;
    -match delta {
        MouseScrollDelta::LineDelta(x, y) => glam::Vec2::new(x * LINE_PX, y * LINE_PX),
        MouseScrollDelta::PixelDelta(p) => glam::Vec2::new(p.x as f32, p.y as f32),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CloseRouting, PresentTarget, WindowSync, attach_first, idle_control_flow, is_input,
        present_frame, take_platform_size, wheel_delta, window_attributes,
    };
    use bevy_ecs::entity::Entity;
    use bevy_ecs::world::World;
    use lumen_core::prelude::{
        A11yBackend, AnimationsActive, App, FrameDirty, FrameRequest, FrameTarget, RenderError,
        RenderTarget, Renderer,
    };
    use lumen_core::window_backend::{CloseAsk, RedrawScheduler};
    use raw_window_handle::{DisplayHandle, HandleError, WindowHandle};
    use std::any::Any;
    use std::sync::Arc;

    /// A renderer that records what it was asked for and answers whatever
    /// the test tells it to, so the frame gate can be exercised with no
    /// display and no GPU.
    #[derive(Default)]
    struct FakeRenderer {
        /// What `wants_present` should answer.
        answer: bool,
        /// Every request the gate passed down, with its target.
        asked: Vec<(Entity, FrameRequest)>,
        presents: Vec<Entity>,
        /// When set, `present` fails with it instead of succeeding.
        fail: Option<&'static str>,
        attached: Vec<(Entity, (u32, u32))>,
    }

    impl Renderer for FakeRenderer {
        fn attach(&mut self, target: Entity, frame: FrameTarget) -> Result<(), RenderError> {
            let FrameTarget::Window(window) = frame else {
                return Err(RenderError::Init("a window renderer".into()));
            };
            self.attached.retain(|(t, _)| *t != target);
            self.attached.push((target, window.physical_size()));
            Ok(())
        }

        fn resize(&mut self, target: Entity, width: u32, height: u32) -> bool {
            let Some(slot) = self.attached.iter_mut().find(|(t, _)| *t == target) else {
                return false;
            };
            let changed = slot.1 != (width, height);
            slot.1 = (width, height);
            changed
        }

        fn wants_present(
            &mut self,
            target: Entity,
            _render_world: &mut World,
            request: FrameRequest,
        ) -> bool {
            self.asked.push((target, request));
            self.answer
        }

        fn present(
            &mut self,
            target: Entity,
            _render_world: &mut World,
        ) -> Result<(), RenderError> {
            self.presents.push(target);
            match self.fail {
                Some(why) => Err(RenderError::Present(why.to_string())),
                None => Ok(()),
            }
        }

        fn targets(&self) -> Vec<Entity> {
            self.attached.iter().map(|(t, _)| *t).collect()
        }

        fn detach(&mut self, target: Entity) {
            self.attached.retain(|(t, _)| *t != target);
        }

        fn detach_all(&mut self) {
            self.attached.clear();
        }
    }

    /// An accessibility bridge that only counts the calls it receives.
    #[derive(Default)]
    struct FakeA11y {
        published: usize,
    }

    impl A11yBackend for FakeA11y {
        fn window_event(&mut self, _event: &dyn Any) {}

        fn pump(&mut self, _world: &mut World) {}

        fn publish(&mut self, _world: &mut World) {
            self.published += 1;
        }
    }

    fn app_with_frame_state(dirty: bool) -> App {
        let mut app = App::new();
        app.world.insert_resource(FrameDirty { dirty });
        app.world.insert_resource(RedrawScheduler::default());
        app
    }

    fn primary(app: &mut App) -> Entity {
        lumen_core::window::primary_window(&mut app.world).expect("a primary window")
    }

    /// A window that reports a size and no handles. The renderer under
    /// test never dereferences one, which is what makes the seam
    /// exercisable with no display attached.
    struct SizedWindow {
        size: (u32, u32),
    }

    impl raw_window_handle::HasWindowHandle for SizedWindow {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl raw_window_handle::HasDisplayHandle for SizedWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl RenderTarget for SizedWindow {
        fn physical_size(&self) -> (u32, u32) {
            self.size
        }
    }

    fn target(window: Entity, shown: bool) -> PresentTarget<'static> {
        PresentTarget {
            window,
            a11y: None,
            shown,
            force_full: false,
        }
    }

    /// The sequence the event loop performs on the renderer, through the
    /// trait object it holds: bind each window that appears, adopt each new
    /// size once, and release everything before the platform connection
    /// closes. The window backend never learns which renderer it got, so
    /// this is the whole of what it can rely on.
    #[test]
    fn the_window_lifecycle_dispatches_through_the_trait() {
        let mut renderer = FakeRenderer::default();
        let seam: &mut dyn Renderer = &mut renderer;
        let (a, b) = (
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
        );

        seam.attach(
            a,
            FrameTarget::Window(Arc::new(SizedWindow { size: (1024, 768) })),
        )
        .expect("attaching to a live window");
        seam.attach(
            b,
            FrameTarget::Window(Arc::new(SizedWindow { size: (300, 200) })),
        )
        .expect("a second window on the same renderer");
        assert!(!seam.resize(a, 1024, 768), "the same size is not a resize");
        assert!(seam.resize(a, 800, 600), "a new size is");
        seam.detach(b);
        assert_eq!(seam.targets(), vec![a]);
        seam.detach_all();

        assert!(renderer.attached.is_empty());
    }

    /// A tick that changed nothing asks the renderer anyway - only the
    /// renderer knows whether its buffers still hold a valid frame - and
    /// presents nothing when the answer is no. The accessibility tree is
    /// published either way, because a tree change and a visual change are
    /// not the same event.
    #[test]
    fn a_clean_tick_publishes_the_tree_and_presents_nothing() {
        let mut app = app_with_frame_state(false);
        let window = primary(&mut app);
        let mut renderer = FakeRenderer::default();
        let mut a11y = FakeA11y::default();

        present_frame(
            &mut app,
            &mut renderer,
            &mut [PresentTarget {
                window,
                a11y: Some(&mut a11y),
                shown: true,
                force_full: false,
            }],
        );

        assert!(renderer.presents.is_empty());
        assert_eq!(a11y.published, 1);
        assert_eq!(renderer.asked.len(), 1);
        assert!(!renderer.asked[0].1.dirty);
        assert!(!renderer.asked[0].1.force_full);
    }

    /// A dirty tick the renderer accepts presents each shown window once,
    /// leaves a hidden one alone, and consumes the dirty flag so the
    /// follow-up re-arm below does not spin the loop on a frame that was
    /// already painted.
    #[test]
    fn a_dirty_tick_presents_each_shown_window_once_and_consumes_the_flag() {
        let mut app = app_with_frame_state(true);
        let window = primary(&mut app);
        let second = Entity::from_raw_u32(40).unwrap();
        let hidden = Entity::from_raw_u32(41).unwrap();
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(
            &mut app,
            &mut renderer,
            &mut [
                target(window, true),
                target(second, true),
                target(hidden, false),
            ],
        );

        assert_eq!(renderer.presents, vec![window, second]);
        assert!(renderer.asked[0].1.dirty);
        assert!(!app.world.resource::<FrameDirty>().dirty);
        assert!(!app.world.resource::<RedrawScheduler>().pending);
    }

    /// A covered window is skipped, unless a screenshot is waiting on its
    /// frame: then it presents, so the capture is answered.
    #[test]
    fn a_covered_window_still_answers_a_screenshot() {
        use lumen_core::render_world::{RenderWindows, SurfaceCapture, spawn_render_window};

        let mut app = app_with_frame_state(true);
        let window = primary(&mut app);
        let covered = Entity::from_raw_u32(42).unwrap();
        app.render_world.init_resource::<RenderWindows>();
        spawn_render_window(
            &mut app.render_world,
            window,
            lumen_core::window::WindowViewport::default(),
            true,
        );
        let capture = SurfaceCapture::default();
        app.render_world.insert_resource(capture.clone());
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(
            &mut app,
            &mut renderer,
            &mut [target(window, false), target(covered, false)],
        );
        assert!(renderer.presents.is_empty(), "nothing asked for a frame");

        capture.request_window(Some(covered));
        present_frame(
            &mut app,
            &mut renderer,
            &mut [target(window, false), target(covered, false)],
        );
        assert_eq!(renderer.presents, vec![covered]);

        renderer.presents.clear();
        capture.request_window(None);
        present_frame(
            &mut app,
            &mut renderer,
            &mut [target(window, false), target(covered, false)],
        );
        assert_eq!(
            renderer.presents,
            vec![window],
            "no window named is the primary"
        );
    }

    /// A recreated surface is passed down as a forced full frame for that
    /// window only, so the renderer repaints it even though its scene may be
    /// identical to the one it last put up.
    #[test]
    fn a_recreated_surface_forces_a_full_frame_of_its_window() {
        let mut app = app_with_frame_state(true);
        let window = primary(&mut app);
        let other = Entity::from_raw_u32(42).unwrap();
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        let mut forced = target(window, true);
        forced.force_full = true;
        present_frame(&mut app, &mut renderer, &mut [forced, target(other, true)]);

        assert!(renderer.asked[0].1.force_full);
        assert!(!renderer.asked[1].1.force_full);
        assert_eq!(renderer.presents.len(), 2);
    }

    /// A failed present is reported and dropped: the loop keeps running,
    /// the flags are still consumed, and the next frame tries again.
    #[test]
    fn a_failed_present_does_not_wedge_the_loop() {
        let mut app = app_with_frame_state(true);
        let window = primary(&mut app);
        let mut renderer = FakeRenderer {
            answer: true,
            fail: Some("device lost"),
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, &mut [target(window, true)]);

        assert_eq!(renderer.presents.len(), 1);
        assert!(!app.world.resource::<FrameDirty>().dirty);
    }

    /// An animation still in flight re-arms the redraw. Nothing else wakes
    /// the loop once the event queue drains, so without this a tween stops
    /// halfway until unrelated input arrives.
    #[test]
    fn a_running_animation_rearms_the_redraw() {
        let mut app = app_with_frame_state(true);
        let window = primary(&mut app);
        app.world.insert_resource(AnimationsActive::default());
        app.world.resource::<AnimationsActive>().request();
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, &mut [target(window, true)]);

        assert!(
            app.world.resource::<RedrawScheduler>().pending,
            "an animation mid-flight must schedule the next frame",
        );
    }

    /// A renderer answering yes to a clean tick still gets its frame: a
    /// pending screenshot is the renderer's own business, and the gate
    /// must not second-guess it.
    #[test]
    fn the_renderer_has_the_last_word_on_a_clean_tick() {
        let mut app = app_with_frame_state(false);
        let window = primary(&mut app);
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, &mut [target(window, true)]);

        assert_eq!(renderer.presents.len(), 1);
    }

    /// The bridge the runtime hands this backend is one it accepts, so a
    /// windowed app gets accessibility; a factory for some other backend is
    /// dropped instead of failing the launch.
    #[test]
    fn the_accesskit_bridge_is_the_factory_this_backend_accepts() {
        use super::winit_a11y_factory;
        assert!(winit_a11y_factory(lumen_a11y_accesskit::bridge_factory()).is_some());
        assert!(winit_a11y_factory(lumen_core::traits::A11yBridgeFactory::new(7_u8)).is_none());
    }

    #[test]
    fn an_idle_loop_sleeps_until_a_future_wake_deadline_and_no_longer() {
        use std::time::{Duration, Instant};
        use winit::event_loop::ControlFlow;
        let now = Instant::now();
        let later = now + Duration::from_secs(2);
        assert_eq!(idle_control_flow(None, now), ControlFlow::Wait);
        assert_eq!(
            idle_control_flow(Some(later), now),
            ControlFlow::WaitUntil(later)
        );
        // A passed deadline is a pending frame, never a WaitUntil in the
        // past that would spin the loop.
        assert_eq!(
            idle_control_flow(Some(now - Duration::from_millis(1)), now),
            ControlFlow::Wait
        );
    }

    /// A renderer whose bind fails the way a renderer with no usable device
    /// does.
    struct NoDevice(RenderError);

    impl Renderer for NoDevice {
        fn attach(&mut self, _: Entity, _target: FrameTarget) -> Result<(), RenderError> {
            Err(match &self.0 {
                RenderError::Init(why) => RenderError::Init(why.clone()),
                RenderError::Present(why) => RenderError::Present(why.clone()),
                RenderError::Detached => RenderError::Detached,
            })
        }
        fn resize(&mut self, _: Entity, _: u32, _: u32) -> bool {
            false
        }
        fn wants_present(&mut self, _: Entity, _: &mut World, _: FrameRequest) -> bool {
            false
        }
        fn present(&mut self, _: Entity, _: &mut World) -> Result<(), RenderError> {
            Ok(())
        }
        fn targets(&self) -> Vec<Entity> {
            Vec::new()
        }
        fn detach(&mut self, _: Entity) {}
        fn detach_all(&mut self) {}
    }

    /// A renderer that cannot initialise against the window hands over to
    /// the next one; the one that binds is the one kept.
    #[test]
    fn a_renderer_that_cannot_initialise_falls_through_to_the_next() {
        let window: Arc<dyn RenderTarget> = Arc::new(SizedWindow { size: (640, 480) });
        let entity = Entity::from_raw_u32(5).unwrap();
        let mut renderer: Box<dyn Renderer> =
            Box::new(NoDevice(RenderError::Init("no adapter".into())));
        let mut fallbacks: std::collections::VecDeque<Box<dyn Renderer>> =
            std::collections::VecDeque::from([Box::new(FakeRenderer::default()) as Box<_>]);

        attach_first(&mut renderer, &mut fallbacks, entity, window).expect("the fallback binds");
        assert!(fallbacks.is_empty());
        assert!(
            !renderer.resize(entity, 640, 480),
            "the fallback took the window's size"
        );
    }

    /// With nothing left to try, the bind failure is the launch's failure,
    /// and a failure that is not about initialising is never papered over.
    #[test]
    fn running_out_of_renderers_or_a_non_init_failure_is_an_error() {
        let window: Arc<dyn RenderTarget> = Arc::new(SizedWindow { size: (8, 8) });
        let entity = Entity::from_raw_u32(5).unwrap();
        let mut alone: Box<dyn Renderer> =
            Box::new(NoDevice(RenderError::Init("no adapter".into())));
        let mut none = std::collections::VecDeque::new();
        assert!(matches!(
            attach_first(&mut alone, &mut none, entity, window.clone()),
            Err(RenderError::Init(_))
        ));

        let mut broken: Box<dyn Renderer> = Box::new(NoDevice(RenderError::Present("lost".into())));
        let mut fallbacks: std::collections::VecDeque<Box<dyn Renderer>> =
            std::collections::VecDeque::from([Box::new(FakeRenderer::default()) as Box<_>]);
        assert!(matches!(
            attach_first(&mut broken, &mut fallbacks, entity, window),
            Err(RenderError::Present(_))
        ));
        assert_eq!(
            fallbacks.len(),
            1,
            "nothing was tried past a non-init failure"
        );
    }

    /// A wheel turned toward the user scrolls down, which the DOM writes as a
    /// positive `deltaY`; winit writes it negative.
    #[test]
    fn a_wheel_toward_the_user_is_a_positive_delta() {
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta;

        assert_eq!(
            wheel_delta(MouseScrollDelta::LineDelta(0.0, -1.0)),
            glam::Vec2::new(0.0, 32.0)
        );
        assert_eq!(
            wheel_delta(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                -5.0, -40.0
            ))),
            glam::Vec2::new(5.0, 40.0)
        );
    }

    fn wanted(title: &str, w: f32, h: f32) -> lumen_core::window::Window {
        lumen_core::window::Window::new(title, glam::Vec2::new(w, h))
    }

    /// The platform window gets what the app asked for and does not have,
    /// once, and nothing else: an unchanged window costs no platform call.
    #[test]
    fn only_what_changed_reaches_the_platform_window() {
        let mut sync = WindowSync::opened("App", glam::Vec2::new(800.0, 600.0));
        assert!(sync.take_changes(&wanted("App", 800.0, 600.0)).is_empty());

        let changes = sync.take_changes(&wanted("Docs", 800.0, 600.0));
        assert_eq!(changes.title.as_deref(), Some("Docs"));
        assert_eq!(changes.size, None);
        assert!(sync.take_changes(&wanted("Docs", 800.0, 600.0)).is_empty());

        let changes = sync.take_changes(&wanted("Docs", 640.0, 480.0));
        assert_eq!(changes.size, Some(glam::Vec2::new(640.0, 480.0)));

        let mut chrome = wanted("Docs", 640.0, 480.0);
        chrome.visible = false;
        chrome.resizable = false;
        chrome.decorations = false;
        chrome.position = Some(glam::IVec2::new(10, 20));
        chrome.min_size = Some(glam::Vec2::new(100.0, 100.0));
        chrome.max_size = Some(glam::Vec2::new(900.0, 900.0));
        let changes = sync.take_changes(&chrome);
        assert_eq!(changes.visible, Some(false));
        assert_eq!(changes.resizable, Some(false));
        assert_eq!(changes.decorations, Some(false));
        assert_eq!(changes.position, Some(Some(glam::IVec2::new(10, 20))));
        assert_eq!(changes.min_size, Some(Some(glam::Vec2::new(100.0, 100.0))));
        assert_eq!(changes.max_size, Some(Some(glam::Vec2::new(900.0, 900.0))));
        assert!(sync.take_changes(&chrome).is_empty());
    }

    /// A resize the platform makes on its own reaches the app's window
    /// unless the app has asked for a size the platform has not seen yet.
    #[test]
    fn a_platform_resize_keeps_a_size_the_app_has_not_sent() {
        let maximized = glam::Vec2::new(1024.0, 697.0);
        // Nothing pending: the window takes what the platform did.
        let mut window = wanted("App", 800.0, 600.0);
        let mut applied = glam::Vec2::new(800.0, 600.0);
        take_platform_size(&mut window, &mut applied, maximized);
        assert_eq!((window.size, applied), (maximized, maximized));
        // A request not yet sent survives, and is still a change to send.
        let mut window = wanted("App", 640.0, 480.0);
        let mut sync = WindowSync::opened("App", glam::Vec2::new(800.0, 600.0));
        take_platform_size(&mut window, &mut sync.size, maximized);
        assert_eq!(window.size, glam::Vec2::new(640.0, 480.0));
        assert_eq!(
            sync.take_changes(&window).size,
            Some(glam::Vec2::new(640.0, 480.0))
        );
        // Once sent, the platform's answer is what the window has.
        let adjusted = glam::Vec2::new(650.0, 480.0);
        take_platform_size(&mut window, &mut sync.size, adjusted);
        assert_eq!((window.size, sync.size), (adjusted, adjusted));
    }

    /// A size no window can have is never asked of the platform.
    #[test]
    fn an_impossible_size_is_not_requested() {
        for (w, h) in [(0.0, 480.0), (640.0, -1.0), (f32::NAN, 480.0)] {
            let mut sync = WindowSync::opened("App", glam::Vec2::new(800.0, 600.0));
            assert_eq!(
                sync.take_changes(&wanted("App", w, h)).size,
                None,
                "{w} x {h}"
            );
        }
    }

    /// A window opens hidden, at the size, chrome and limits it asks for, so
    /// its accessibility bridge binds before it is first shown.
    #[test]
    fn a_window_opens_hidden_with_what_it_asks_for() {
        let mut window = wanted("Panel", 320.0, 240.0);
        window.resizable = false;
        window.min_size = Some(glam::Vec2::new(100.0, 80.0));
        let attrs = window_attributes(&window, None);
        assert!(!attrs.visible);
        assert!(!attrs.resizable);
        assert_eq!(attrs.title, "Panel");
        assert_eq!(
            attrs.inner_size,
            Some(winit::dpi::Size::Logical(winit::dpi::LogicalSize::new(
                320.0, 240.0
            )))
        );
        assert!(attrs.min_inner_size.is_some());
        // A dialog with no owner to belong to still opens.
        window.kind = lumen_core::window::WindowKind::Dialog;
        assert!(!window_attributes(&window, None).visible);
    }

    /// Input a modal window blocks covers every pointer and key event and
    /// nothing about the window itself.
    #[test]
    fn blocked_input_is_pointer_and_keys_only() {
        use winit::event::WindowEvent;
        assert!(!is_input(&WindowEvent::HoveredFileCancelled));
        assert!(!is_input(&WindowEvent::CloseRequested));
        assert!(!is_input(&WindowEvent::Focused(true)));
        assert!(is_input(&WindowEvent::DroppedFile("a".into())));
    }

    /// An app with the window-free half a window backend installs.
    fn windowed_app() -> App {
        let mut app = App::new();
        app.add_plugin(lumen_core::window_backend::WindowCorePlugin);
        app
    }

    /// Ticks with `msg` written before the tick and the close path's state
    /// read after it, the way the handler ticks.
    fn tick_with<M: bevy_ecs::message::Message>(app: &mut App, close: &mut CloseRouting, msg: M) {
        app.world.write_message(msg);
        app.tick();
        close.after_tick(&app.world);
    }

    /// A close the app asks for waits for the end of the batch, closes of
    /// several windows gather, and a quit asked in any tick of the batch
    /// outranks them.
    #[test]
    fn a_quit_in_the_batch_outranks_window_closes() {
        use lumen_core::window::{QuitRequest, WindowRequest};

        let mut app = windowed_app();
        let window = primary(&mut app);
        let other = app.world.spawn(lumen_core::window::Window::default()).id();
        let mut close = CloseRouting::default();
        app.tick();
        close.after_tick(&app.world);
        assert_eq!(close.pending, None, "nothing asked");

        tick_with(&mut app, &mut close, WindowRequest::Close(other));
        tick_with(&mut app, &mut close, WindowRequest::Close(window));
        assert_eq!(close.pending, Some(CloseAsk::Windows(vec![other, window])));
        tick_with(&mut app, &mut close, QuitRequest);
        assert_eq!(close.pending, Some(CloseAsk::Quit));
        tick_with(&mut app, &mut close, WindowRequest::Close(window));
        assert_eq!(close.pending, Some(CloseAsk::Quit));
    }

    /// The close reasons a hook saw; it vetoes each one when `veto` is set.
    #[derive(bevy_ecs::prelude::Resource, Default)]
    struct Hook {
        veto: bool,
        seen: Vec<(Entity, lumen_core::input::CloseReason)>,
    }

    fn hook(
        mut hook: bevy_ecs::prelude::ResMut<Hook>,
        mut msgs: bevy_ecs::prelude::ResMut<
            bevy_ecs::message::Messages<lumen_core::input::CloseRequest>,
        >,
        mut cursor: bevy_ecs::prelude::Local<
            bevy_ecs::message::MessageCursor<lumen_core::input::CloseRequest>,
        >,
    ) {
        let asked: Vec<_> = cursor
            .read(&msgs)
            .filter(|m| !m.vetoed)
            .map(|m| (m.window, m.reason))
            .collect();
        for (window, reason) in asked {
            hook.seen.push((window, reason));
            if hook.veto {
                msgs.write(lumen_core::input::CloseRequest {
                    window,
                    reason,
                    vetoed: true,
                });
            }
        }
    }

    fn hooked_app(veto: bool) -> App {
        let mut app = windowed_app();
        app.world.insert_resource(Hook {
            veto,
            seen: Vec::new(),
        });
        app.add_systems(lumen_core::tick::TickStage::Systems, hook);
        app.tick();
        app.world.resource_mut::<RedrawScheduler>().pending = false;
        app
    }

    /// A vetoed close keeps the window open and schedules a paint, so what
    /// the hook showed (a "save first?" prompt) is drawn.
    #[test]
    fn a_vetoed_close_keeps_the_window_and_repaints() {
        use lumen_core::input::CloseReason;
        let mut app = hooked_app(true);
        let window = primary(&mut app);
        let mut close = CloseRouting::default();
        close.run(&mut app, Some(window), CloseReason::Platform);
        assert!(!close.committed);
        assert!(app.world.resource::<RedrawScheduler>().pending);
        assert_eq!(
            app.world.resource::<Hook>().seen,
            [(window, CloseReason::Platform)]
        );
    }

    /// Closing a secondary window closes just it; closing the primary one
    /// ends the app, and once ended no close asks the hook again.
    #[test]
    fn a_secondary_window_closes_alone_and_the_primary_ends_the_app() {
        use lumen_core::input::CloseReason;
        let mut app = hooked_app(false);
        let window = primary(&mut app);
        let other = app.world.spawn(lumen_core::window::Window::default()).id();
        let mut close = CloseRouting::default();
        close.run(&mut app, Some(other), CloseReason::Platform);
        assert!(!close.committed, "a secondary window's close is not a quit");
        assert!(app.world.get_entity(other).is_err(), "the window went");

        close.pending = Some(CloseAsk::Quit);
        close.run_pending(&mut app);
        assert!(close.committed);
        assert_eq!(close.pending, None);
        close.run(&mut app, Some(window), CloseReason::Shutdown);
        close.run_pending(&mut app);
        let seen = &app.world.resource::<Hook>().seen;
        assert_eq!(seen[0], (other, CloseReason::Platform));
        assert_eq!(seen[1], (window, CloseReason::AppQuit));
        assert_eq!(seen.len(), 2, "nothing asked after the app ended");
    }
}
