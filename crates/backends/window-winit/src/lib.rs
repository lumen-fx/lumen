//! winit 0.30 on-screen window + input.
//!
//! This crate owns the window, the event loop, and the translation of
//! platform events into Lumen messages. It owns no pixels and no
//! accessibility tree: a [`SurfaceRenderer`] presents frames and an
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

use bevy_ecs::message::Messages;
use lumen_core::input::{CloseRequest, PendingFileDrops, WindowFocused, WindowOccluded};
use lumen_core::prelude::*;
use lumen_core::text_events::{ImeSurroundingRequested, ImeSurroundingResponse, TextEditRequest};
use lumen_core::text_model::TextBuffer;
use lumen_core::traits::{A11yBackend, FrameRequest, RenderTarget, SurfaceRenderer};
use lumen_core::window::{MenuModel, WindowGeometry, WindowOptions};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::any::Any;
use std::sync::Arc;
use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, Ime as WinitIme, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState as WinitModifiers, NamedKey as WinitNamed};
use winit::window::{Window, WindowAttributes};

/// Construction-time errors from [`run`].
#[derive(Debug, Error)]
pub enum WinitError {
    /// winit event-loop construction failed.
    #[error("event loop creation failed: {0}")]
    EventLoop(String),
    /// winit event-loop run terminated with an error.
    #[error("event loop run error: {0}")]
    Run(String),
}

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

/// Marker for the winit windowing backend.
pub struct WinitWindow;

impl lumen_core::traits::WindowBackend for WinitWindow {}

/// Redraw pacing resource read by the `about_to_wait` callback.
///
/// Other systems request a paint by writing `pending = true`; the backend
/// only forwards the call to `winit::Window::request_redraw` when the
/// window is **visible** (not occluded). This stops the uncapped vsync
/// poll that the old "request_redraw at the end of every event" pattern
/// produced and matches Qt's `requestUpdate` gated on `isExposed()` /
/// GTK's `frame-clock` (see `docs/audits/window-backend.md` Bug 7).
///
/// The pump gates on VISIBILITY, not focus. On tiling WMs (Hyprland,
/// sway) an unfocused window stays fully on-screen, so it must keep
/// animating: the audio position pump (`poll_audio`), restyle tweens,
/// and scroll inertia all ride the redraw loop, and freezing them the
/// moment focus leaves is the "audio slider stops advancing while
/// unfocused" bug. Only a truly occluded / minimized window parks
/// (preserving the battery/idle win).
///
/// `paused = occluded` is recomputed from `occluded` by the
/// `WindowEvent::Occluded` arm (and defensively by the `Focused` arm).
/// `focused` is still tracked for the `WindowFocused` message and to
/// force a repaint on focus-return, but it no longer gates the pump.
/// System code outside the backend should not write `paused`,
/// `focused`, or `occluded` directly.
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug)]
pub struct RedrawScheduler {
    /// Set to `true` to request a paint on the next `about_to_wait`.
    /// Cleared after the redraw is dispatched.
    pub pending: bool,
    /// `true` when the window currently has keyboard focus. Updated by
    /// the `WindowEvent::Focused` arm. Tracked for the `WindowFocused`
    /// message and the focus-return repaint; deliberately not part of the
    /// pump gate (see [`RedrawScheduler::compute_paused`]).
    pub focused: bool,
    /// `true` when the window is fully occluded (covered by another
    /// window or moved off-screen). Updated by the
    /// `WindowEvent::Occluded` arm.
    pub occluded: bool,
    /// Cached `occluded`; recomputed whenever `occluded` (or `focused`)
    /// changes. The backend suppresses redraw requests while paused, i.e.
    /// only while the window is genuinely occluded.
    pub paused: bool,
}

impl Default for RedrawScheduler {
    fn default() -> Self {
        // Start focused and visible so the first frame paints. winit
        // sends `Focused(true)` / `Occluded(false)` shortly after window
        // creation; until then we want to render so the user sees the
        // app rather than a black surface.
        Self {
            pending: true,
            focused: true,
            occluded: false,
            paused: false,
        }
    }
}

impl RedrawScheduler {
    fn recompute_paused(&mut self) {
        self.paused = Self::compute_paused(self.focused, self.occluded);
    }

    /// Pure pump-gate decision: should the redraw loop park?
    ///
    /// Only occlusion parks the loop. `focused` is intentionally accepted
    /// and ignored: the decision deliberately does not consider focus, so
    /// a visible-but-unfocused window (the tiling-WM common case) keeps
    /// animating. Extracted as a pure fn so the focus-vs-visibility policy
    /// is unit-testable without a live event loop.
    fn compute_paused(_focused: bool, occluded: bool) -> bool {
        occluded
    }

    /// Pure forward-gate decision used by `about_to_wait`: forward a
    /// `request_redraw` to winit only when a paint is pending AND the
    /// window is not parked. Anything that wants a frame while unfocused
    /// (the audio ticker's `EventLoopWaker`, a restyle tween, scroll
    /// inertia) sets `pending`; this gate no longer swallows it just
    /// because focus left.
    fn should_forward_redraw(&self) -> bool {
        self.pending && !self.paused
    }
}

/// Plugin that registers window-event messages and the redraw scheduler.
///
/// `run` installs this automatically; embedders that drive the winit
/// loop themselves can call `app.add_plugin(WinitPlugin)` to opt in.
///
/// Registers [`WindowFocused`], [`WindowOccluded`], [`CloseRequest`] so
/// `MessageReader`s can subscribe, and inserts a default
/// [`RedrawScheduler`] resource.
pub struct WinitPlugin;

impl Plugin for WinitPlugin {
    fn build(self, app: &mut App) {
        app.add_message::<WindowFocused>();
        app.add_message::<WindowOccluded>();
        // NOTE: `CloseRequest` is deliberately not registered here.
        // `lumen_core::App::new` already registers it (close hooks must
        // work headless too), and `MessageRegistry::register_message` is
        // not idempotent: a second registration pushes a second update
        // entry, the buffer then cycles twice per tick, and any
        // `CloseRequest` written by the backend before the veto tick is
        // dropped before Systems-stage readers (script `on_close`,
        // `lumen_app_on_close`) ever see it.
        // W3.5: IME surrounding-text bus. Push side lives in this crate
        // (window-winit produces a response on every Ime event); the
        // request side is currently a no-op queue waiting for a backend
        // that can ferry text-input-v3 / IBus surrounding-text asks.
        app.add_message::<ImeSurroundingRequested>();
        app.add_message::<ImeSurroundingResponse>();
        // W3.2: text-edit request bus shared with `lumen-input` /
        // `lumen-text-edit`. Initialized here so window-backend can
        // also write to it (W3.5 IME commit-with-replacement).
        app.add_message::<TextEditRequest>();
        app.world.insert_resource(RedrawScheduler::default());
        // Register a typed-command handler so the (Linux-only) XDG
        // portal listener can push `XdgColorSchemeUpdate { dark }`
        // through [`lumen_core::command::Command::Typed`] and have it
        // applied on the main thread inside [`TickStage::CommandDrain`].
        // Idempotent across worlds - even non-Linux builds register the
        // handler (it just never fires).
        app.register_command::<XdgColorSchemeUpdate, _>(|world, payload| {
            let dark = payload.dark;
            world.resource_mut::<StyleManager>().set_system_dark(dark);
        });
    }
}

/// Cross-thread payload pushed by the Linux XDG color-scheme listener.
/// Translated to a mutation on [`lumen_core::components::StyleManager`]
/// by the [`WinitPlugin`]-registered typed-command handler.
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

/// Builds the accessibility bridge once the window exists.
///
/// The window backend does not name an accessibility library: the
/// composition point passes this factory in, and whatever it returns is
/// driven through [`A11yBackend`]. The third argument wakes a parked event
/// loop, which the bridge calls when an assistive technology queues a
/// request from its own thread. Pass `None` to [`run`] to run without
/// accessibility.
pub type A11yBridgeFactory =
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
///
/// `renderer` presents the frames; it is attached to the window once the
/// window exists and detached before the platform connection closes.
/// `a11y` builds the accessibility bridge at the same moment, or is
/// `None` for a run without one. Both ride beside [`WindowOptions`]
/// rather than inside it: the options are pure data every launch path
/// resolves, while these are live backend objects the composition point
/// chose.
pub fn run(
    mut app: App,
    opts: WindowOptions,
    renderer: Box<dyn SurfaceRenderer>,
    a11y: Option<A11yBridgeFactory>,
) -> Result<(), WinitError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| WinitError::EventLoop(e.to_string()))?;
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
    app.world.insert_resource(waker);

    // Install backend-side messages and the redraw scheduler before any
    // other system can read them - unless the embedder already added the
    // plugin. The guard matters: `add_message` re-registration is not
    // idempotent (each call adds another per-tick buffer update, so a
    // double-registered message type cycles twice per tick and drops
    // pre-tick writes before Systems-stage readers run).
    if !app.is_plugin_added::<WinitPlugin>() {
        app.add_plugin(WinitPlugin);
    }

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
        .map_err(|e| WinitError::Run(e.to_string()))
}

struct WinitHandler {
    app: App,
    opts: WindowOptions,
    /// Presents the frames. Attached to the window in
    /// [`ApplicationHandler::resumed`] and detached in
    /// [`ApplicationHandler::exiting`].
    renderer: Box<dyn SurfaceRenderer>,
    /// Builds [`Self::a11y`] once the window exists. `None` runs without
    /// accessibility.
    a11y_factory: Option<A11yBridgeFactory>,
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
        if let Err(e) = self.renderer.attach(Arc::new(WinitTarget(window.clone()))) {
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
                // (tiling WM) does wake, which is how the audio ticker's
                // `EventLoopWaker` keeps `poll_audio` advancing off-focus.
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
                    msgs.write(PointerMoved { position: p });
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
                // unfocused window (tiling WMs) must keep animating so the
                // audio position pump / tweens / inertia advance while
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
                let now = std::time::Instant::now();
                let deadline = self.last_frame_at.map(|t| t + ANIM_FRAME_INTERVAL);
                match deadline {
                    Some(d) if d > now => event_loop.set_control_flow(ControlFlow::WaitUntil(d)),
                    _ => {
                        window.request_redraw();
                        event_loop.set_control_flow(ControlFlow::Wait);
                    }
                }
            } else {
                // Idle: park until the next event (input, resize, MCP wake).
                // Clears any WaitUntil left from a just-settled animation so
                // the loop doesn't keep waking.
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
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
    renderer: &mut dyn SurfaceRenderer,
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
    //
    // Three sources of pending work, each of which reaches `false` on its
    // own once the system settles - so this can never become a permanent
    // vsync spin:
    //   1. External typed-property bus still has undrained writes (a
    //      cross-thread producer, or a main-thread script write that
    //      landed after this tick's drain). Empties once drained.
    //   2. An animation driver (hover/press tween, opacity transition,
    //      scroll inertia) reported it still has motion this tick via
    //      `AnimationsActive`. Cleared at the top of every tick and only
    //      re-raised while a value is genuinely mid-flight, so it falls to
    //      `false` the moment every animation settles.
    //   3. `FrameDirty` is somehow still set (defensive - a system dirtied
    //      state after the encode). Cleared by the next present.
    let work_pending = lumen_core::property_store::external_properties_pending()
        || app
            .world
            .get_resource::<AnimationsActive>()
            .is_some_and(|a| a.get())
        || app
            .world
            .get_resource::<FrameDirty>()
            .is_some_and(|f| f.dirty);
    if work_pending && let Some(mut sch) = app.world.get_resource_mut::<RedrawScheduler>() {
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
/// [`XdgColorSchemeUpdate`] payload. The [`WinitPlugin`]-installed
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
    use super::RedrawScheduler;

    /// The pump-gate policy: only occlusion parks the loop. Focus is not a
    /// factor, so a visible-but-unfocused window (Hyprland/sway, where an
    /// unfocused window is still fully on-screen) keeps animating. This is
    /// the "audio slider freeze while unfocused on a tiling WM" fix.
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
    /// A pending paint raised while UNFOCUSED-but-VISIBLE (the audio ticker's
    /// `EventLoopWaker`, a restyle tween, or scroll inertia all set `pending`)
    /// must forward; an occluded window must not.
    #[test]
    fn forward_redraw_wakes_unfocused_visible_but_parks_occluded() {
        // Audio playing / tween in flight while unfocused-but-visible: the
        // ticker/tween set `pending = true`; the gate must forward it.
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
