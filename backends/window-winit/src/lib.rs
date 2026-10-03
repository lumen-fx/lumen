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
//! Lifecycle:
//!   resumed -> window + accessibility bridge + renderer attach
//!   Resized -> renderer resize + viewport rewrite + synchronous repaint
//!   RedrawRequested -> pump accessibility requests, app.tick(), present
//!   CloseRequested / SIGINT / SIGTERM -> emit CloseRequest, run one veto
//!             tick (script `on_close` / `lumen_app_on_close` / app
//!             systems), then exit unless vetoed; `exiting` persists
//!             window state and detaches the renderer while the platform
//!             connection is still alive

#![warn(missing_docs)]

pub mod capability;

use bevy_ecs::message::Messages;
use lumen_core::input::{CloseRequest, PendingFileDrops, WindowFocused, WindowOccluded};
use lumen_core::prelude::*;
use lumen_core::text_events::ImeSurroundingResponse;
use lumen_core::text_model::TextBuffer;
use lumen_core::tick::{wake_deadline, work_pending};
use lumen_core::traits::{
    A11yBackend, A11yBridgeFactory, FrameRequest, FrameTarget, RenderTarget, Renderer,
    WindowBackend, WindowError,
};
use lumen_core::window::{MenuModel, WindowGeometry, WindowOptions};
use lumen_core::window_backend::{RedrawScheduler, WindowCorePlugin};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::any::Any;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, Ime as WinitIme, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState as WinitModifiers, NamedKey as WinitNamed};
use winit::window::{Window, WindowAttributes};

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
/// driven through [`A11yBackend`]. The third argument wakes a parked event
/// loop, which the bridge calls when an assistive technology queues a
/// request from its own thread. A factory of any other type is ignored with
/// a warning, and the window runs without accessibility.
pub type WinitA11yFactory =
    Box<dyn Fn(&ActiveEventLoop, Arc<Window>, Arc<dyn Fn() + Send + Sync>) -> Box<dyn A11yBackend>>;

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

    let mut handler = WinitHandler {
        app,
        opts,
        renderer,
        fallbacks: fallbacks.into(),
        a11y_factory: a11y,
        a11y: None,
        window: None,
        proxy,
        last_ime: ImeRequest::default(),
        last_cursor: CursorShape::Default,
        close_committed: false,
        last_frame_at: None,
    };
    {
        let size = glam::Vec2::new(handler.opts.size.0 as f32, handler.opts.size.1 as f32);
        let clear = handler.opts.clear;
        let mut vp = handler.app.world.resource_mut::<Viewport>();
        vp.size = size;
        vp.clear = clear;
        let mut vp = handler.app.render_world.resource_mut::<Viewport>();
        vp.size = size;
        vp.clear = clear;
    }
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

/// Bind `renderer` to `target`, moving on to the next of `fallbacks` each
/// time a renderer cannot initialise against the window. Leaves the one that
/// bound in `renderer`. Any other failure, or running out of renderers,
/// returns the last error.
fn attach_first(
    renderer: &mut Box<dyn Renderer>,
    fallbacks: &mut std::collections::VecDeque<Box<dyn Renderer>>,
    target: Arc<dyn RenderTarget>,
) -> Result<(), lumen_core::traits::RenderError> {
    loop {
        match renderer.attach(FrameTarget::Window(target.clone())) {
            Ok(()) => return Ok(()),
            Err(lumen_core::traits::RenderError::Init(why)) if !fallbacks.is_empty() => {
                eprintln!(
                    "lumen-window-winit: renderer init failed ({why}); trying the next renderer"
                );
                *renderer = fallbacks.pop_front().expect("checked non-empty");
            }
            Err(e) => return Err(e),
        }
    }
}

struct WinitHandler {
    app: App,
    opts: WindowOptions,
    /// Presents the frames. Attached to the window in
    /// [`ApplicationHandler::resumed`] and detached in
    /// [`ApplicationHandler::exiting`].
    renderer: Box<dyn Renderer>,
    /// Renderers to try, in order, when [`Self::renderer`] cannot bind to
    /// the window.
    fallbacks: std::collections::VecDeque<Box<dyn Renderer>>,
    /// Builds [`Self::a11y`] once the window exists. `None` runs without
    /// accessibility.
    a11y_factory: Option<WinitA11yFactory>,
    /// Accessibility bridge; receives every winit `WindowEvent` and hands
    /// queued assistive-technology requests to the world each frame.
    a11y: Option<Box<dyn A11yBackend>>,
    /// The window itself. `None` until `resumed` creates it; every event
    /// arm gates on it, so nothing touches the platform after `exiting`
    /// drops it.
    window: Option<Arc<Window>>,
    /// EventLoop proxy backing [`lumen_core::app::EventLoopWaker`] and the
    /// signal watchers.
    proxy: EventLoopProxy<UserEvent>,
    /// Last IME control values applied to the window. Compared against the
    /// current [`ImeRequest`] resource each frame to avoid hammering
    /// winit with redundant calls.
    last_ime: ImeRequest,
    /// Last cursor shape applied via `Window::set_cursor`. Compared
    /// against the main world's [`lumen_core::input::CursorRequest`]
    /// each frame so the OS call only happens on change.
    last_cursor: CursorShape,
    /// Set by [`WinitHandler::process_close_request`] once a close
    /// request survived the veto tick (no system wrote
    /// `CloseRequest { vetoed: true }`). Read by
    /// [`ApplicationHandler::about_to_wait`] to trigger
    /// `event_loop.exit()`.
    close_committed: bool,
    /// Wall-clock start of the most recent `RedrawRequested` paint, used by
    /// [`ApplicationHandler::about_to_wait`] to pace self-re-armed
    /// animation frames against [`ANIM_FRAME_INTERVAL`]. `None` until the
    /// first paint. See [`ANIM_FRAME_INTERVAL`] for why this is a no-op on
    /// a vsync-blocking present path and only bites on a headless /
    /// non-blocking one.
    last_frame_at: Option<std::time::Instant>,
}

impl ApplicationHandler<UserEvent> for WinitHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = WindowAttributes::default()
            .with_title(&self.opts.title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.opts.size.0,
                self.opts.size.1,
            ))
            .with_maximized(self.opts.maximized)
            .with_decorations(!self.opts.frameless);
        if let Some((x, y)) = self.opts.start_position {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("lumen-window-winit: create_window failed: {e:?}");
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
        // (Linux only). Spawns once per window create; tracks the system
        // color-scheme preference and pushes
        // `Command::SetStyleManagerSystemDark` updates via the
        // bounded `CommandQueue` so the next tick
        // re-runs `style_manager_to_signal`. When the portal call fails
        // (no XDG portal daemon, e.g. inside CI / headless containers),
        // the spawn returns early and the runtime falls back to the
        // winit-reported `WindowEvent::ThemeChanged` path.
        #[cfg(target_os = "linux")]
        try_spawn_xdg_color_scheme_listener(&mut self.app.world);
        // Seed `Viewport.scale_factor` + `Viewport.size` (logical) from
        // the freshly-created window. The `run()` pre-seed wrote the
        // option size verbatim assuming dpr=1; on a HiDPI display the
        // OS-chosen size after `create_window` differs, so reconcile
        // now: physical inner_size / scale -> logical.
        let scale_factor = window.scale_factor() as f32;
        {
            let inner = window.inner_size();
            let logical_w = inner.width as f32 / scale_factor;
            let logical_h = inner.height as f32 / scale_factor;
            let logical = glam::Vec2::new(logical_w, logical_h);
            tracing::debug!(
                target: "lumen::window::resize",
                physical_w = inner.width,
                physical_h = inner.height,
                scale = scale_factor,
                logical_w,
                logical_h,
                "resumed",
            );
            for vp in [
                self.app.world.resource_mut::<Viewport>(),
                self.app.render_world.resource_mut::<Viewport>(),
            ] {
                let mut vp = vp;
                vp.size = logical;
                vp.scale_factor = scale_factor;
            }
        }
        // Seed `Viewport.monitor_*` from the window's current monitor.
        // `Window::current_monitor` returns `None` before the window is mapped on some platforms; the values reseed via `MonitorChanged`.
        if let Some(monitor) = window.current_monitor() {
            let scale = monitor.scale_factor() as f32;
            let mon_size = monitor.size();
            let size = glam::Vec2::new(mon_size.width as f32, mon_size.height as f32);
            let name = monitor.name();
            for vp in [
                self.app.world.resource_mut::<Viewport>(),
                self.app.render_world.resource_mut::<Viewport>(),
            ] {
                let mut vp = vp;
                vp.monitor_scale = Some(scale);
                vp.monitor_size = Some(size);
                vp.monitor_name = name.clone();
            }
        }
        // The accessibility bridge must exist before the first
        // RedrawRequested so the platform's initial-tree handshake arrives
        // in time. It queues requests from assistive-technology threads and
        // wakes the loop through the same proxy everything else uses.
        if let Some(factory) = self.a11y_factory.as_ref() {
            let wake_proxy = self.proxy.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            self.a11y = Some(factory(event_loop, window.clone(), wake));
        }
        // W5.2: tell the a11y tree-build system what the human-readable
        // root label is so it can stop hard-coding "Lumen app". Sourced
        // from the window options title; the app passes the same string
        // it set on the OS window.
        self.app
            .world
            .insert_resource(lumen_core::components::A11yRootLabel(
                self.opts.title.clone(),
            ));
        // Attach the native menubar from the markup spec. On macOS and Windows builds a `muda::Menu` and binds it to the app/window; Linux is a stub. The muda integration lives in `lumen-os-menu` after W6.3.
        if let Some(spec) = self.opts.menubar.take() {
            attach_menubar_via_os_menu(&window, &spec);
        }
        if let Err(e) = attach_first(
            &mut self.renderer,
            &mut self.fallbacks,
            Arc::new(WinitTarget(window.clone())),
        ) {
            eprintln!("lumen-window-winit: renderer init failed: {e}");
            event_loop.exit();
            return;
        }
        self.window = Some(window);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Persist final window geometry through whatever the caller
        // wired up (typically `lumenc::window_state::save`).
        if let (Some(cb), Some(window)) = (self.opts.on_close_state.take(), self.window.as_ref()) {
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
        // Stop redraw scheduling - nothing may touch the window past this
        // point (`about_to_wait` / `window_event` both gate on
        // `self.window`).
        if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
            sch.pending = false;
        }
        // Release the accessibility bridge before the window it wraps.
        self.a11y = None;
        // Orderly renderer teardown WHILE the platform connection is still
        // alive. `event_loop.run_app` consumes the `EventLoop`, so once it
        // returns the Wayland/X11 connection is already gone - and
        // releasing a GPU surface at that point makes a GLES driver
        // call `eglTerminate` against a dead `wl_display`, which segfaults
        // (observed: exit 139 on every close under Hyprland/EGL).
        // Detaching here runs the whole release chain while the display
        // connection is still valid, and drops the renderer's handle on
        // the window so the window itself goes last.
        self.renderer.detach();
        self.window = None;
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                // Cross-thread producer (e.g. `lumen-mcp`'s
                // `SimulateQueue::push`) nudged the loop after pushing
                // work the tick doesn't otherwise observe until the next
                // `RedrawRequested`. Schedule a redraw the same way every
                // other pending-work source does; `about_to_wait` forwards
                // to `Window::request_redraw` only when the window isn't
                // paused (occluded), so this still can't spin a covered /
                // minimized window - but a visible-but-unfocused window
                // (tiling WM) does wake, which is how a worker thread's
                // `EventLoopWaker` keeps its pump advancing off-focus.
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            UserEvent::CloseRequested => {
                // First SIGINT / SIGTERM, forwarded by the
                // `lumen-signal-watcher` thread. Same graceful path as
                // the window close button; a second signal force-exits
                // from the watcher thread if a hook vetoes or teardown
                // wedges.
                self.process_close_request();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        // The accessibility bridge sees every event before the app does,
        // as its platform adapter requires.
        if let Some(a11y) = self.a11y.as_mut() {
            a11y.window_event(&event as &dyn Any);
        }
        // Clone the handle rather than borrowing `self`: the arms below
        // need the window and the app at the same time.
        let Some(window) = self.window.clone() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                // Run the veto tick synchronously - see
                // [`WinitHandler::process_close_request`]. Doing it here
                // (instead of deferring to the next `RedrawRequested`)
                // means a close on an unfocused or occluded window still
                // commits: the deferred design gated the veto tick on a
                // redraw that a paused `RedrawScheduler` never scheduled,
                // so closing an unfocused window was silently dropped.
                self.process_close_request();
            }
            WindowEvent::ThemeChanged(theme) => {
                // Last-resort path on Linux when the XDG portal listener
                // isn't running. On macOS/Windows this is the primary
                // source of `system_dark` updates.
                self.app
                    .world
                    .resource_mut::<StyleManager>()
                    .set_system_dark(matches!(theme, winit::window::Theme::Dark));
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::Resized(new_size) => {
                // Resize-flood coalescing: a live drag delivers many
                // `Resized` events. The renderer returns `false` when the
                // physical size is unchanged, so we skip the surface
                // recreate + viewport rewrite + relayout + paint that a
                // duplicate event would otherwise force. Only a genuine
                // size change does work, and each distinct size gets
                // exactly one synchronous paint.
                if new_size.width > 0
                    && new_size.height > 0
                    && self.renderer.resize(new_size.width, new_size.height)
                {
                    // `Viewport.size` carries LOGICAL pixels (see
                    // `lumen_core::render_world::Viewport::size`). winit
                    // delivers `new_size` as `PhysicalSize`; convert via
                    // the window's current scale factor before writing.
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
                    for vp in [
                        self.app.world.resource_mut::<Viewport>(),
                        self.app.render_world.resource_mut::<Viewport>(),
                    ] {
                        let mut vp = vp;
                        vp.size = size;
                        vp.scale_factor = scale_factor;
                    }
                    // Smart reactive resize: relayout + repaint synchronously
                    // inside the resize event. `sync_viewport` (LayoutSync)
                    // sees the new `Viewport.size` and re-lays out every
                    // root; `roll_up_frame_dirty` folds `Viewport::is_changed`
                    // into `FrameDirty`; `present_frame` then commits a
                    // correctly-sized buffer this frame. This is the smooth
                    // path GTK/Qt take (paint in the configure/resize
                    // callback) versus deferring to the next loop iteration.
                    self.app.tick();
                    // Resize recreated the intermediate texture - force a full
                    // repaint even if this tick's tree matches the last one.
                    present_frame(
                        &mut self.app,
                        self.renderer.as_mut(),
                        self.a11y.as_deref_mut(),
                        true,
                    );
                }
                // Fallback: request one follow-up redraw in case the
                // synchronous present skipped (surface `Outdated` during a
                // fast drag). It's a cheap no-op tick when the viewport is
                // already settled and nothing is dirty.
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
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
                let logical = self.app.world.resource::<Viewport>().size;
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
                // Reconfigure surface immediately at the new physical
                // size - winit guarantees the change is synchronous so
                // a subsequent `Resized` may not fire on every backend
                // before the next `RedrawRequested`.
                self.renderer.resize(new_w.max(1), new_h.max(1));
                // Update Viewport. Logical stays put; scale_factor flips
                // to the new value.
                for vp in [
                    self.app.world.resource_mut::<Viewport>(),
                    self.app.render_world.resource_mut::<Viewport>(),
                ] {
                    let mut vp = vp;
                    vp.scale_factor = scale_f32;
                }
                // Paint synchronously at the new DPI so the monitor
                // hot-swap doesn't flash a stale-scale frame: the
                // scale_factor write trips `Viewport::is_changed`, the
                // render walker re-seeds its root transform with the new
                // dpr, and `present_frame` commits the correctly-scaled
                // buffer this frame.
                self.app.tick();
                // DPI change recreated the intermediate texture - force a full
                // repaint.
                present_frame(
                    &mut self.app,
                    self.renderer.as_mut(),
                    self.a11y.as_deref_mut(),
                    true,
                );
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::Moved(_) => {
                // Re-sample the current monitor on window-position changes and mirror the values into both worlds' [`Viewport`].
                if let Some(monitor) = window.current_monitor() {
                    let scale = monitor.scale_factor() as f32;
                    let mon_size = monitor.size();
                    let size = glam::Vec2::new(mon_size.width as f32, mon_size.height as f32);
                    let name = monitor.name();
                    for vp in [
                        self.app.world.resource_mut::<Viewport>(),
                        self.app.render_world.resource_mut::<Viewport>(),
                    ] {
                        let mut vp = vp;
                        vp.monitor_scale = Some(scale);
                        vp.monitor_size = Some(size);
                        vp.monitor_name = name.clone();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Anchor the animation-pacing deadline at the frame START so
                // `about_to_wait` measures the full present-to-present period
                // (see `ANIM_FRAME_INTERVAL`). On a vsync-blocking present the
                // tick+present below runs past the deadline, so pacing is a
                // no-op; on a non-blocking (headless) present it caps the
                // self-driven redraw cadence at 60 Hz instead of spinning.
                self.last_frame_at = Some(std::time::Instant::now());
                {
                    let req = *self.app.world.resource::<ImeRequest>();
                    if req.allowed != self.last_ime.allowed {
                        window.set_ime_allowed(req.allowed);
                    }
                    if let Some((origin, size)) = req.cursor_area
                        && req.cursor_area != self.last_ime.cursor_area
                    {
                        window.set_ime_cursor_area(
                            winit::dpi::PhysicalPosition::new(origin.x as f64, origin.y as f64),
                            winit::dpi::PhysicalSize::new(size.x as f64, size.y as f64),
                        );
                    }
                    self.last_ime = req;
                }
                // Drain native menu clicks before the tick so handlers
                // fire on the same frame as the user's click. No-op on
                // Linux (muda dep absent). Implementation moved to
                // `lumen-os-menu` per W6.3.
                lumen_os_menu::poll_native_menu_events(&mut self.app.world);
                // Apply anything an assistive technology queued from its
                // own thread, for the same reason: a screen reader's click
                // lands in the tick that paints its result.
                if let Some(a11y) = self.a11y.as_mut() {
                    a11y.pump(&mut self.app.world);
                }
                self.app.tick();
                // Consume any title-bar press -> request a native
                // window drag. Cleared after the call so a single
                // press initiates a single drag.
                if let Some(mut req) = self
                    .app
                    .world
                    .get_resource_mut::<lumen_core::components::WindowDragRequest>()
                    && req.0
                {
                    req.0 = false;
                    if let Err(e) = window.drag_window() {
                        eprintln!("lumen-window-winit: drag_window failed: {e}");
                    }
                }
                // Apply the cursor shape the UI asked for this tick
                // (`lumen_primitives::update_cursor_request` writes the
                // resource; absent when the embedder skipped the
                // plugin). Change-gated so winit isn't hammered.
                if let Some(req) = self.app.world.get_resource::<CursorRequest>()
                    && req.0 != self.last_cursor
                {
                    self.last_cursor = req.0;
                    window.set_cursor(map_cursor_shape(req.0));
                }
                // Publish the tree the A11ySync stage built, then present
                // when FrameDirty / a capture requires it, and clear the
                // dirty + pending flags. Shared with the synchronous
                // live-resize paint in the `Resized` / `ScaleFactorChanged`
                // arms.
                present_frame(
                    &mut self.app,
                    self.renderer.as_mut(),
                    self.a11y.as_deref_mut(),
                    false,
                );
            }
            WindowEvent::CursorMoved { position, .. } => {
                // winit delivers `PhysicalPosition`; convert to logical so
                // layout, hit-test, and pointer-routed primitives all share
                // one coordinate space with `Viewport.size`.
                let scale_factor = window.scale_factor();
                let logical = position.to_logical::<f32>(scale_factor);
                let p = glam::Vec2::new(logical.x, logical.y);
                self.app.world.resource_mut::<PointerState>().position = Some(p);
                if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<PointerMoved>>()
                {
                    msgs.write(PointerMoved {
                        position: p,
                        local: None,
                    });
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.app.world.resource_mut::<PointerState>().position = None;
                if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<PointerLeft>>() {
                    msgs.write(PointerLeft);
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Normalize line-based wheel events to logical pixels.
                // 32 px/line matches GTK/X11's gtk-scroll-lines default for
                // a 16pt line. Apps tune per-container feel via
                // `Scroll::sensitivity` rather than changing this default.
                const LINE_PX: f32 = 32.0;
                let v = match delta {
                    MouseScrollDelta::LineDelta(x, y) => glam::Vec2::new(x * LINE_PX, y * LINE_PX),
                    MouseScrollDelta::PixelDelta(p) => glam::Vec2::new(p.x as f32, p.y as f32),
                };
                let pos = self
                    .app
                    .world
                    .resource::<PointerState>()
                    .position
                    .unwrap_or(glam::Vec2::ZERO);
                if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<MouseWheel>>() {
                    msgs.write(MouseWheel {
                        delta: v,
                        position: pos,
                        local: None,
                    });
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
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
                            if let Some(mut msgs) =
                                self.app.world.get_resource_mut::<Messages<KeyPressed>>()
                            {
                                msgs.write(KeyPressed {
                                    key,
                                    modifiers: mods,
                                    repeat: event.repeat,
                                });
                            }
                        }
                        ElementState::Released => {
                            if let Some(mut msgs) =
                                self.app.world.get_resource_mut::<Messages<KeyReleased>>()
                            {
                                msgs.write(KeyReleased {
                                    key,
                                    modifiers: mods,
                                });
                            }
                        }
                    }
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::HoveredFile(path) => {
                let pos = self
                    .app
                    .world
                    .resource::<PointerState>()
                    .position
                    .unwrap_or(glam::Vec2::ZERO);
                if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<FileHovered>>() {
                    msgs.write(FileHovered {
                        path,
                        position: pos,
                    });
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(mut msgs) = self
                    .app
                    .world
                    .get_resource_mut::<Messages<FileHoverCancelled>>()
                {
                    msgs.write(FileHoverCancelled);
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::DroppedFile(path) => {
                // Stash the raw path + current pointer pos as a
                // FileDroppedRaw marker the dispatch system (in
                // lumen-input) hit-tests against DropTarget entities.
                // We push it through a transient resource because the
                // window backend doesn't know about hit-testing.
                let pos = self
                    .app
                    .world
                    .resource::<PointerState>()
                    .position
                    .unwrap_or(glam::Vec2::ZERO);
                self.app
                    .world
                    .resource_mut::<PendingFileDrops>()
                    .drops
                    .push((path, pos));
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::Ime(ime) => {
                // Bug 14: when winit deactivates the IME, the OS already
                // forgot whatever `set_ime_allowed(true)` we last called;
                // if we don't reset `last_ime.allowed` here, the next
                // frame's diff-against-`last_ime` skips the re-enable and
                // the user loses IME input until the focus router
                // toggles `ImeRequest.allowed` off and on again. Mirror
                // the OS reset locally so the next non-trivial
                // `ImeRequest` reapplies cleanly.
                let is_disabled = matches!(ime, WinitIme::Disabled);
                let mapped = match ime {
                    WinitIme::Enabled => Some(ImeEvent::Enabled),
                    WinitIme::Preedit(text, cursor) => Some(ImeEvent::Preedit { text, cursor }),
                    WinitIme::Commit(text) => Some(ImeEvent::Commit(text)),
                    WinitIme::Disabled => Some(ImeEvent::Disabled),
                };
                if is_disabled {
                    self.last_ime.allowed = false;
                    self.last_ime.cursor_area = None;
                    if let Some(mut req) = self.app.world.get_resource_mut::<ImeRequest>() {
                        req.allowed = false;
                        req.cursor_area = None;
                    }
                }
                if let Some(ev) = mapped
                    && let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<ImeEvent>>()
                {
                    msgs.write(ev);
                }
                // W3.5: emit an ImeSurroundingResponse so any future
                // OS-bound forwarder (Wayland text-input-v3 / IBus) has
                // the current focused-entity text + cursor available.
                // winit doesn't currently expose a SurroundingTextRequested
                // signal, so we push the response opportunistically on
                // every IME event - backends that don't need it ignore
                // the queue. Safe no-op when no editable is focused.
                push_ime_surrounding_response(&mut self.app.world);
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            WindowEvent::Focused(focused) => {
                // Emit a `WindowFocused` message and track focus state.
                // Focus does not pause the redraw pump: a visible-but-
                // unfocused window (tiling WMs) must keep animating so
                // worker-thread pumps / tweens / inertia advance while
                // unfocused. `recompute_paused` therefore leaves `paused`
                // driven by occlusion alone; we still call it so the field
                // stays consistent if the policy ever changes.
                if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<WindowFocused>>()
                {
                    msgs.write(WindowFocused { focused });
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.focused = focused;
                    sch.recompute_paused();
                    if focused {
                        // Coming back into focus should provoke a paint
                        // so any state the app changed while hidden is
                        // visible immediately.
                        sch.pending = true;
                    }
                }
            }
            WindowEvent::Occluded(occluded) => {
                // Pause the redraw scheduler while occluded; resume on
                // reveal. Matches `docs/audits/window-backend.md` Bug 7
                // and brings us in line with Qt's `requestUpdate` gate
                // on `isExposed()` / GTK's `frame-clock` pacing.
                if let Some(mut msgs) = self
                    .app
                    .world
                    .get_resource_mut::<Messages<WindowOccluded>>()
                {
                    msgs.write(WindowOccluded { occluded });
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.occluded = occluded;
                    sch.recompute_paused();
                    if !occluded {
                        sch.pending = true;
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let lumen_button = map_button(button);
                let pos = self
                    .app
                    .world
                    .resource::<PointerState>()
                    .position
                    .unwrap_or(glam::Vec2::ZERO);
                if matches!(lumen_button, PointerButton::Primary) {
                    self.app.world.resource_mut::<PointerState>().primary_down =
                        matches!(state, ElementState::Pressed);
                }
                match state {
                    ElementState::Pressed => {
                        if let Some(mut msgs) = self
                            .app
                            .world
                            .get_resource_mut::<Messages<PointerPressed>>()
                        {
                            msgs.write(PointerPressed {
                                position: pos,
                                button: lumen_button,
                                local: None,
                            });
                        }
                    }
                    ElementState::Released => {
                        if let Some(mut msgs) = self
                            .app
                            .world
                            .get_resource_mut::<Messages<PointerReleased>>()
                        {
                            msgs.write(PointerReleased {
                                position: pos,
                                button: lumen_button,
                                local: None,
                            });
                        }
                    }
                }
                if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                    sch.pending = true;
                }
            }
            _ => {}
        }
    }

    /// Called by winit after every batch of events and before the loop
    /// goes back to wait. Two responsibilities:
    ///
    /// 1. If [`WinitHandler::process_close_request`] committed a close
    ///    this iteration (window button, SIGINT/SIGTERM - the veto tick
    ///    already ran synchronously inside the event arm), call
    ///    `event_loop.exit()`.
    /// 2. Forward `RedrawScheduler.pending` to `Window::request_redraw`
    ///    only when the window is not paused (i.e. not occluded). Focus is
    ///    not a factor - a visible-but-unfocused window keeps animating.
    ///    Replaces the pre-W1.8 "request_redraw at end of every event"
    ///    spinner; see [`RedrawScheduler`] doc comment for the Qt/GTK
    ///    parallel.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.close_committed {
            event_loop.exit();
            return;
        }
        // A deadline a system asked to be woken at (a script timer) that
        // has come due needs a tick: this is the pass the `WaitUntil` set
        // below resumes into.
        let wake_at = wake_deadline(&self.app.world);
        let now = std::time::Instant::now();
        if wake_at.is_some_and(|at| at <= now)
            && let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>()
        {
            sch.pending = true;
        }
        if let Some(window) = self.window.as_ref() {
            let should_paint = self
                .app
                .world
                .get_resource::<RedrawScheduler>()
                .map(RedrawScheduler::should_forward_redraw)
                .unwrap_or(false);
            // A pending off-thread screenshot request must be serviced even
            // when the scheduler is paused (occluded window - the common case
            // on a headless X server, or any minimized/covered window).
            // The renderer performs the readback that fulfils the request
            // while it presents; without forcing a paint here it sits unhandled
            // until it times out ("no SurfaceCapture wired"). Reading the flag
            // is a single atomic load, so this stays free on ordinary frames.
            let capture_pending = self
                .app
                .render_world
                .get_resource::<SurfaceCapture>()
                .map(|c| c.is_requested())
                .unwrap_or(false);
            // Off-thread screenshot requests bypass animation pacing so MCP
            // introspection stays prompt: request the redraw now.
            if capture_pending {
                window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            } else if should_paint {
                // Pace the self-re-armed animation redraw against a deadline
                // anchored at the current frame's start (`ANIM_FRAME_INTERVAL`).
                // With a vsync-blocking present the deadline is already in the
                // past here (the paint blocked past it), so the redraw fires
                // immediately and vsync stays the clock. With a non-blocking
                // (headless) present the loop parks until the deadline instead
                // of spinning - capping the self-driven cadence at ~60 Hz. A
                // stale anchor (first frame after idle) is also already past,
                // so a fresh input event still paints without delay.
                let deadline = self.last_frame_at.map(|t| t + ANIM_FRAME_INTERVAL);
                match deadline {
                    Some(d) if d > now => event_loop.set_control_flow(ControlFlow::WaitUntil(d)),
                    _ => {
                        window.request_redraw();
                        event_loop.set_control_flow(ControlFlow::Wait);
                    }
                }
            } else {
                // Idle: park until the next event (input, resize, MCP wake)
                // or the earliest requested deadline. Replaces any WaitUntil
                // left from a just-settled animation so the loop doesn't keep
                // waking.
                event_loop.set_control_flow(idle_control_flow(wake_at, now));
            }
        }
    }
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

impl WinitHandler {
    /// Graceful close, shared by the window close button
    /// ([`WindowEvent::CloseRequested`]) and Unix signals
    /// ([`UserEvent::CloseRequested`]).
    ///
    /// Emits `CloseRequest { vetoed: false }`, then runs one synchronous
    /// tick so every app-level close hook observes it before any
    /// teardown: the script host's `on_close` dispatcher, the C-ABI
    /// `lumen_app_on_close` router, and any app system reading
    /// [`CloseRequest`]. A hook keeps the window open by writing a fresh
    /// `CloseRequest { vetoed: true }` during that tick - mirroring
    /// `QCloseEvent::ignore()` / GTK4's `close-request -> TRUE`. When
    /// nothing vetoes, `close_committed` is set and `about_to_wait`
    /// exits the loop; [`ApplicationHandler::exiting`] then persists
    /// window state and releases the renderer in order.
    ///
    /// Runs synchronously in the event arm (the same pattern as the
    /// live-resize paint in `Resized`) rather than deferring to the next
    /// `RedrawRequested`, so the close also works while the
    /// [`RedrawScheduler`] is paused (unfocused / occluded window) and
    /// before the window exists.
    fn process_close_request(&mut self) {
        if self.close_committed {
            return;
        }
        if let Some(mut msgs) = self.app.world.get_resource_mut::<Messages<CloseRequest>>() {
            msgs.write(CloseRequest { vetoed: false });
        }
        self.app.tick();
        let vetoed = self
            .app
            .world
            .get_resource::<Messages<CloseRequest>>()
            .map(|msgs| msgs.iter_current_update_messages().any(|m| m.vetoed))
            .unwrap_or(false);
        if vetoed {
            // Keep running; schedule a paint so whatever the veto
            // handler changed (e.g. a "save before quit?" dialog)
            // becomes visible.
            if let Some(mut sch) = self.app.world.get_resource_mut::<RedrawScheduler>() {
                sch.pending = true;
            }
        } else {
            self.close_committed = true;
        }
    }
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

/// Map Lumen's cursor-shape request onto winit's OS cursor icon set.
fn map_cursor_shape(shape: CursorShape) -> winit::window::CursorIcon {
    use winit::window::CursorIcon;
    match shape {
        CursorShape::Default => CursorIcon::Default,
        CursorShape::Text => CursorIcon::Text,
        CursorShape::Pointer => CursorIcon::Pointer,
        CursorShape::Grab => CursorIcon::Grab,
        CursorShape::Grabbing => CursorIcon::Grabbing,
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
        MouseButton::Back | MouseButton::Forward => PointerButton::Other(0),
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

/// Present the current world state: publish the accessibility tree, then
/// ask the renderer for a frame when this tick produced one, and clear the
/// dirty + pending flags.
///
/// Shared by the `RedrawRequested` arm (normal cadence) and the `Resized` /
/// `ScaleFactorChanged` arms (synchronous live-resize paint). Doing the
/// paint inside the resize event - instead of only requesting a deferred
/// redraw - is what gives smooth live resize on Wayland and macOS, where
/// the compositor expects a correctly sized buffer committed in response to
/// each configure rather than a round trip later (content otherwise
/// stretches and lags behind the drag). Assumes `app.tick()` has already
/// run this iteration, so `FrameDirty` and the extracted scene are current.
///
/// `force_full` tells the renderer its buffers were just recreated, so it
/// must repaint even when the scene is unchanged.
fn present_frame(
    app: &mut App,
    renderer: &mut dyn Renderer,
    a11y: Option<&mut dyn A11yBackend>,
    force_full: bool,
) {
    // The accessibility tree is built in `TickStage::A11ySync`; this only
    // publishes it, and does nothing when the tick produced no update.
    if let Some(a11y) = a11y {
        a11y.publish(&mut app.world);
    }
    // `FrameDirty` folds every render-relevant `Changed<T>` and property
    // write - including `Viewport::is_changed()` on resize - into a single
    // bool. It over-approximates: a signal re-set to the same value or a
    // hover class that resolves to the same visuals raises it while leaving
    // the painted tree identical. The renderer applies whatever finer test
    // it has (the retained-scene diff, for the GPU path) and answers
    // whether a frame is actually worth putting up.
    let frame_dirty = app
        .world
        .get_resource::<FrameDirty>()
        .map(|f| f.dirty)
        .unwrap_or(true);
    let request = FrameRequest {
        dirty: frame_dirty,
        force_full,
    };
    if renderer.wants_present(&mut app.render_world, request) {
        match renderer.present(&mut app.render_world) {
            // First real on-screen present: emit the startup marker (once,
            // env-gated). This is the windowed analog of the headless
            // boot-trace's exec->first-frame line, and gives the benchmark
            // harness a stdout marker measured identically to every native
            // framework (spawn->`first_frame`) plus an in-app `startup_ms:`.
            Ok(()) => emit_first_frame_marker(),
            Err(e) => eprintln!("lumen-window-winit: render failed: {e}"),
        }
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
    // `about_to_wait` forwards to winit only when the window is not
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

#[cfg(test)]
mod tests {
    use super::{attach_first, idle_control_flow, present_frame};
    use bevy_ecs::world::World;
    use lumen_core::prelude::{
        A11yBackend, AnimationsActive, App, FrameDirty, FrameRequest, FrameTarget, RenderError,
        RenderTarget, Renderer, Viewport,
    };
    use lumen_core::window_backend::RedrawScheduler;
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
        /// Every request the gate passed down.
        asked: Vec<FrameRequest>,
        presents: usize,
        /// When set, `present` fails with it instead of succeeding.
        fail: Option<&'static str>,
        attached: bool,
        size: (u32, u32),
    }

    impl Renderer for FakeRenderer {
        fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError> {
            let FrameTarget::Window(window) = target else {
                return Err(RenderError::Init("a window renderer".into()));
            };
            self.size = window.physical_size();
            self.attached = true;
            Ok(())
        }

        fn resize(&mut self, width: u32, height: u32) -> bool {
            let changed = self.size != (width, height);
            self.size = (width, height);
            changed
        }

        fn wants_present(&mut self, _render_world: &mut World, request: FrameRequest) -> bool {
            self.asked.push(request);
            self.answer
        }

        fn present(&mut self, _render_world: &mut World) -> Result<(), RenderError> {
            self.presents += 1;
            match self.fail {
                Some(why) => Err(RenderError::Present(why.to_string())),
                None => Ok(()),
            }
        }

        fn detach(&mut self) {
            self.attached = false;
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
        app.render_world.insert_resource(Viewport::default());
        app
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

    /// The sequence the event loop performs on the renderer, through the
    /// trait object it holds: bind to the window that just appeared, adopt
    /// each new size once, and release everything before the platform
    /// connection closes. The window backend never learns which renderer
    /// it got, so this is the whole of what it can rely on.
    #[test]
    fn the_window_lifecycle_dispatches_through_the_trait() {
        let mut renderer = FakeRenderer::default();
        let seam: &mut dyn Renderer = &mut renderer;

        seam.attach(FrameTarget::Window(Arc::new(SizedWindow {
            size: (1024, 768),
        })))
        .expect("attaching to a live window");
        assert!(!seam.resize(1024, 768), "the same size is not a resize");
        assert!(seam.resize(800, 600), "a new size is");
        seam.detach();

        assert!(!renderer.attached);
        assert_eq!(renderer.size, (800, 600));
    }

    /// A tick that changed nothing asks the renderer anyway - only the
    /// renderer knows whether its buffers still hold a valid frame - and
    /// presents nothing when the answer is no. The accessibility tree is
    /// published either way, because a tree change and a visual change are
    /// not the same event.
    #[test]
    fn a_clean_tick_publishes_the_tree_and_presents_nothing() {
        let mut app = app_with_frame_state(false);
        let mut renderer = FakeRenderer::default();
        let mut a11y = FakeA11y::default();

        present_frame(&mut app, &mut renderer, Some(&mut a11y), false);

        assert_eq!(renderer.presents, 0);
        assert_eq!(a11y.published, 1);
        assert_eq!(renderer.asked.len(), 1);
        assert!(!renderer.asked[0].dirty);
        assert!(!renderer.asked[0].force_full);
    }

    /// A dirty tick the renderer accepts presents once, and the dirty flag
    /// is consumed so the follow-up re-arm below does not spin the loop on
    /// a frame that was already painted.
    #[test]
    fn a_dirty_tick_presents_once_and_consumes_the_flag() {
        let mut app = app_with_frame_state(true);
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, None, false);

        assert_eq!(renderer.presents, 1);
        assert!(renderer.asked[0].dirty);
        assert!(!app.world.resource::<FrameDirty>().dirty);
        assert!(!app.world.resource::<RedrawScheduler>().pending);
    }

    /// A recreated surface is passed down as a forced full frame, so the
    /// renderer repaints even though the scene may be identical to the one
    /// it last put up.
    #[test]
    fn a_recreated_surface_forces_a_full_frame() {
        let mut app = app_with_frame_state(true);
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, None, true);

        assert!(renderer.asked[0].force_full);
        assert_eq!(renderer.presents, 1);
    }

    /// A failed present is reported and dropped: the loop keeps running,
    /// the flags are still consumed, and the next frame tries again.
    #[test]
    fn a_failed_present_does_not_wedge_the_loop() {
        let mut app = app_with_frame_state(true);
        let mut renderer = FakeRenderer {
            answer: true,
            fail: Some("device lost"),
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, None, false);

        assert_eq!(renderer.presents, 1);
        assert!(!app.world.resource::<FrameDirty>().dirty);
    }

    /// An animation still in flight re-arms the redraw. Nothing else wakes
    /// the loop once the event queue drains, so without this a tween stops
    /// halfway until unrelated input arrives.
    #[test]
    fn a_running_animation_rearms_the_redraw() {
        let mut app = app_with_frame_state(true);
        app.world.insert_resource(AnimationsActive::default());
        app.world.resource::<AnimationsActive>().request();
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, None, false);

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
        let mut renderer = FakeRenderer {
            answer: true,
            ..FakeRenderer::default()
        };

        present_frame(&mut app, &mut renderer, None, false);

        assert_eq!(renderer.presents, 1);
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
        fn attach(&mut self, _target: FrameTarget) -> Result<(), RenderError> {
            Err(match &self.0 {
                RenderError::Init(why) => RenderError::Init(why.clone()),
                RenderError::Present(why) => RenderError::Present(why.clone()),
                RenderError::Detached => RenderError::Detached,
            })
        }
        fn resize(&mut self, _: u32, _: u32) -> bool {
            false
        }
        fn wants_present(&mut self, _: &mut World, _: FrameRequest) -> bool {
            false
        }
        fn present(&mut self, _: &mut World) -> Result<(), RenderError> {
            Ok(())
        }
        fn detach(&mut self) {}
    }

    /// A renderer that cannot initialise against the window hands over to
    /// the next one; the one that binds is the one kept.
    #[test]
    fn a_renderer_that_cannot_initialise_falls_through_to_the_next() {
        let window: Arc<dyn RenderTarget> = Arc::new(SizedWindow { size: (640, 480) });
        let mut renderer: Box<dyn Renderer> =
            Box::new(NoDevice(RenderError::Init("no adapter".into())));
        let mut fallbacks: std::collections::VecDeque<Box<dyn Renderer>> =
            std::collections::VecDeque::from([Box::new(FakeRenderer::default()) as Box<_>]);

        attach_first(&mut renderer, &mut fallbacks, window).expect("the fallback binds");
        assert!(fallbacks.is_empty());
        assert!(
            !renderer.resize(640, 480),
            "the fallback took the window's size"
        );
    }

    /// With nothing left to try, the bind failure is the launch's failure,
    /// and a failure that is not about initialising is never papered over.
    #[test]
    fn running_out_of_renderers_or_a_non_init_failure_is_an_error() {
        let window: Arc<dyn RenderTarget> = Arc::new(SizedWindow { size: (8, 8) });
        let mut alone: Box<dyn Renderer> =
            Box::new(NoDevice(RenderError::Init("no adapter".into())));
        let mut none = std::collections::VecDeque::new();
        assert!(matches!(
            attach_first(&mut alone, &mut none, window.clone()),
            Err(RenderError::Init(_))
        ));

        let mut broken: Box<dyn Renderer> = Box::new(NoDevice(RenderError::Present("lost".into())));
        let mut fallbacks: std::collections::VecDeque<Box<dyn Renderer>> =
            std::collections::VecDeque::from([Box::new(FakeRenderer::default()) as Box<_>]);
        assert!(matches!(
            attach_first(&mut broken, &mut fallbacks, window),
            Err(RenderError::Present(_))
        ));
        assert_eq!(
            fallbacks.len(),
            1,
            "nothing was tried past a non-init failure"
        );
    }
}
