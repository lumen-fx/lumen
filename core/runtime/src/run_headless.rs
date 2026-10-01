//! True headless run mode: the full app pipeline - layout, real
//! rendering through the app's render backend and the shared Node-IR
//! walker, the MCP server, input simulation, hot reload, and screenshots -
//! with zero windows. No winit event loop is created, so the desktop / compositor
//! is never touched. This is the automation / CI mode behind
//! `lumenc run <app> --headless`.
//!
//! ## How it differs from [`crate::run::run_app_headless`]
//!
//! The bare `run_app_headless` (kept as the FFI / SDK contract) only
//! ticks the main-world schedule - no renderer, no pixels. This mode
//! additionally installs the offscreen renderer of the backend
//! `[render] backend` selects (under `auto`, the first that starts, GPU
//! before CPU), driven by the same retained-scene walker the windowed
//! backend runs - so extracted geometry, dpr scaling, and text shaping
//! behave identically to the windowed path.
//!
//! ## Frame pacing
//!
//! Ticks run on demand, mirroring the windowed `RedrawScheduler`
//! semantics without the pause-on-unfocused gate (there is no focus):
//!
//! * a wake through [`lumen_core::app::EventLoopWaker`] runs a tick
//!   immediately: the MCP server thread (simulate push or screenshot
//!   request), a finished HTTP request, or a module event on the core bus;
//! * while work is pending (animations mid-flight, undrained external
//!   property writes, dirty frame), ticks are paced at ~60 Hz - the
//!   stand-in for vsync;
//! * a deadline a system asked to be woken at (a script timer, see
//!   [`lumen_core::tick::WakeDeadline`]) ends the park when it comes due;
//! * otherwise the loop parks. With hot reload active it re-ticks every
//!   ~250 ms so the source watcher polls; without it, the loop sleeps
//!   until the next wake (SIGINT/SIGTERM are still observed within one
//!   250 ms slice).
//!
//! ## Exit
//!
//! SIGINT / SIGTERM (and the end of a bounded `--ticks N` run) take the
//! graceful-close path: a `CloseRequest { vetoed: false }` is written to
//! the message bus and one final tick runs so close-observing systems
//! fire, then the fn returns `Ok(())` (process exit code 0). Unlike the
//! windowed close, a veto does not keep the app alive - a signalled CI
//! run must terminate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::Resource;
use lumen_core::input::CloseRequest;
use lumen_core::prelude::*;
use lumen_core::render_backend::{OffscreenRenderer, RenderBackend};
use lumen_core::render_world::{FrameDirty, SurfaceCapture};
use lumen_core::tick::{wake_deadline, work_pending};
use lumen_text::{ShapeOptions, ShaperService, TextShaper};
use lumen_text_cosmic::CosmicShaper;

use crate::run::{RunError, RunOptions, build_headless_app};

/// Wall-clock pacing for back-to-back work ticks (animations, pending
/// writes). Stands in for vsync; 16.67 ms = 60 Hz. Deadlines are
/// anchored (`deadline += interval`, park until deadline) rather than
/// slept after the tick, so the frame period is exactly this - not
/// `tick work + sleep`, which drifted every frame's worth of work.
const WORK_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// Idle park slice. Signals and the hot-reload watcher are observed at
/// this cadence; an MCP wake interrupts it immediately.
const IDLE_PARK_SLICE: Duration = Duration::from_millis(250);

/// Opt-in boot-phase timing. Set `LUMEN_BOOT_TRACE=1` to print a
/// phase-by-phase startup breakdown (build/parse/font-scan, renderer
/// bring-up, shaper warmup, first frame) to stderr - the reproducible
/// backing for the startup regression story. Off by default: the checks
/// are a single `env::var_os` read plus a few `Instant::now()` calls on
/// the cold path, so a normal run pays nothing measurable.
struct BootTrace {
    on: bool,
    start: Instant,
}

impl BootTrace {
    fn new() -> Self {
        Self {
            on: std::env::var_os("LUMEN_BOOT_TRACE").is_some(),
            start: Instant::now(),
        }
    }

    /// Print one phase line (elapsed within that phase).
    fn mark(&self, phase: &str, dur: Duration) {
        if self.on {
            eprintln!(
                "boot-trace: {phase:<34} {:>8.2} ms",
                dur.as_secs_f64() * 1000.0
            );
        }
    }

    /// Under trace only: time a throwaway [`CosmicShaper::new`] so the
    /// system-font-directory scan cost is attributable in isolation
    /// (the real scan is buried inside `TaffyLayoutPlugin::build`). The
    /// shaper is dropped immediately; it exists purely to price the
    /// `FontSystem::new` disk walk that every cold start pays once.
    fn standalone_fontscan(&self) {
        if self.on {
            let t = Instant::now();
            let s = CosmicShaper::new();
            let dur = t.elapsed();
            std::hint::black_box(&s);
            self.mark("  |- FontSystem::new (standalone)", dur);
        }
    }

    /// Total-to-first-frame + resident-set + thread-count summary.
    fn finish(&self) {
        if !self.on {
            return;
        }
        self.mark("TOTAL exec->first-frame", self.start.elapsed());
        #[cfg(target_os = "linux")]
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            let field = |k: &str| {
                status
                    .lines()
                    .find(|l| l.starts_with(k))
                    .map(|l| l.trim())
                    .unwrap_or("")
                    .to_string()
            };
            eprintln!("boot-trace: {}", field("VmHWM:"));
            eprintln!("boot-trace: {}", field("Threads:"));
        }
    }
}

/// Options specific to the rendered headless mode. Sizing comes from
/// [`RunOptions::size`] / `lumen.toml [window] size` exactly like the
/// windowed path, so it is not duplicated here.
#[derive(Debug, Clone, Copy)]
pub struct HeadlessOptions {
    /// Device pixel ratio for the offscreen target. `Viewport.size` stays
    /// logical (like the windowed path); the render texture - and thus
    /// every screenshot - is `logical x dpr` physical pixels.
    pub dpr: f32,
    /// `Some(n)`: run exactly `n` ticks back-to-back, then take the
    /// graceful-close path and return. `None`: run until SIGINT/SIGTERM.
    pub ticks: Option<u64>,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            dpr: 1.0,
            ticks: None,
        }
    }
}

/// Condvar-backed stand-in for the winit event loop's parked wait.
/// [`Self::wake`] is handed out as the [`lumen_core::app::EventLoopWaker`]
/// so cross-thread producers (MCP simulate queue, screenshot requests)
/// interrupt the park exactly like `EventLoopProxy::send_event` would.
#[derive(Default)]
struct Parker {
    notified: Mutex<bool>,
    cv: Condvar,
}

impl Parker {
    fn wake(&self) {
        let mut g = self
            .notified
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *g = true;
        self.cv.notify_one();
    }

    /// Park for at most `timeout`. Returns `true` when woken by
    /// [`Self::wake`] (including a wake that arrived before parking -
    /// no lost-wakeup window), `false` on timeout.
    fn park_timeout(&self, timeout: Duration) -> bool {
        let mut g = self
            .notified
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !*g {
            let (g2, _res) = self
                .cv
                .wait_timeout(g, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            g = g2;
        }
        let was = *g;
        *g = false;
        was
    }
}

/// Run the full pipeline headless. See the module docs for semantics.
pub fn run_app_headless_rendered(
    mut opts: RunOptions,
    headless: HeadlessOptions,
) -> Result<(), RunError> {
    // Rendered headless is an automation / CI run with no interactive
    // session: gate off the MCP server (unless `[mcp] simulate` is on) and
    // the hot-reload watcher via the `bounded` flag. See `build_app`.
    opts.bounded = true;
    let boot = BootTrace::new();
    let dpr = headless.dpr.max(0.01);
    // Renderer bring-up (a GPU device and its pipelines, or a rasterizer)
    // needs nothing from the app world, so `build_app` starts it on a
    // spawned thread as soon as the render backends have registered, and it
    // runs overlapped with the rest of the build (markup/CSS parse, scripts,
    // ECS spawn) and the shaper warmup below. Sized from the CLI size as a
    // guess; the render system resizes the target to the viewport, and
    // every expensive init step is size-independent.
    opts.offscreen_prestart = Some((
        (opts.size.0 as f32 * dpr).round().max(1.0) as u32,
        (opts.size.1 as f32 * dpr).round().max(1.0) as u32,
    ));

    let t_build = Instant::now();
    let (mut app, mut window) = build_headless_app(opts)?;
    boot.mark("build_app (parse+ecs+fontscan)", t_build.elapsed());
    boot.standalone_fontscan();
    let renderer_init = app
        .world
        .remove_resource::<OffscreenPrestart>()
        .ok_or_else(|| RunError::Headless("the app build started no renderer".into()))?;

    // While the renderer thread finishes: pre-warm the shapers' cold path
    // (sans-serif face load + fallback-chain init inside cosmic-text,
    // ~10-15 ms on first shape) so the first real layout/render tick
    // doesn't pay it. The strings are throwaway; only the font-system
    // warmup matters, so a short ASCII pangram at the default UI sizes
    // and both common weights is enough.
    let t_warm = Instant::now();
    {
        const WARMUP: &str = "The quick brown fox jumps over 0123456789.";
        let warm = |shaper: &mut dyn TextShaper| {
            for (size, weight) in [(14.0_f32, 400_u16), (16.0, 400), (14.0, 700)] {
                let _ = shaper.shape(
                    WARMUP,
                    size,
                    ShapeOptions {
                        weight,
                        ..Default::default()
                    },
                );
            }
        };
        if let Some(mut layout_shaper) = app.world.get_non_send_mut::<ShaperService>() {
            warm(&mut **layout_shaper);
        }
        if let Some(render_shaper) = window.text_shaper.as_deref_mut() {
            warm(render_shaper);
        }
    }
    boot.mark("shaper warmup (overlap renderer)", t_warm.elapsed());
    // Hot-reload wakes: with the notify watcher active (the default), fs
    // events wake the parked loop directly and idle stays at zero ticks.
    // Only the poll fallback (`LUMEN_HOT_RELOAD_POLL` / watcher init
    // failure) still needs the periodic idle slices to re-tick.
    // Hot reload (and its poll driver) only exists in `runtime-parse` builds;
    // a parser-free runtime has no source to re-read, so it never needs the
    // periodic re-tick slices.
    #[cfg(feature = "runtime-parse")]
    let hot_reload_poll = matches!(
        app.world.get_resource::<crate::run::HotReloadDriver>(),
        Some(crate::run::HotReloadDriver::Poll)
    );
    #[cfg(not(feature = "runtime-parse"))]
    let hot_reload_poll = false;

    // Viewport: logical size from the resolved window options (CLI --size
    // beats `lumen.toml [window] size` beats the built-in default, exactly
    // like windowed), scale factor from --dpr. Mirrors the pre-loop seed
    // in `lumen_window_winit::run` plus the dpr reconcile `resumed` does.
    let logical = glam::Vec2::new(window.options.size.0 as f32, window.options.size.1 as f32);
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = logical;
        vp.scale_factor = dpr;
        vp.clear = window.options.clear;
    }

    // Join the renderer thread. A GPU adapter is requested without a
    // surface, so a machine with no display still gets one where a driver
    // exists; under `auto` a backend that cannot start hands over to the
    // next. Init failure of every candidate surfaces as an error.
    let t_join = Instant::now();
    let (renderer_res, init_wall) = renderer_init.join()?;
    boot.mark("renderer_join_wait (main blocked)", t_join.elapsed());
    boot.mark("  |- renderer_init_wall (bg thread)", init_wall);
    let renderer = renderer_res.map_err(RunError::Headless)?;
    // The text shaper built for the windowed path is the render world's,
    // so glyph output matches the window byte-for-byte.
    if let Some(shaper) = window.text_shaper.take() {
        app.render_world
            .insert_non_send(ShaperService::from(shaper));
    }
    renderer.install(&mut app);

    // Wake plumbing: the same EventLoopWaker contract the winit backend
    // provides, backed by a condvar instead of an event-loop proxy.
    // `wire_simulate_waker` (MCP plugin) picks the resource up on the
    // first tick; the SurfaceCapture waker is shared via its OnceLock.
    let parker = Arc::new(Parker::default());
    let waker = {
        let p = Arc::clone(&parker);
        lumen_core::app::EventLoopWaker(Arc::new(move || p.wake()))
    };
    if let Some(capture) = app.world.get_resource::<SurfaceCapture>() {
        capture.set_waker(waker.clone());
    }
    // A module event pushed onto the core bus while the loop is idle-parked
    // wakes it, mirroring the windowed backend.
    lumen_core::plugin_events::set_plugin_event_waker(waker.clone());
    app.world.insert_resource(waker);

    // SIGINT / SIGTERM (Unix) or Ctrl+C / Ctrl+Break / console-close
    // (Windows) -> flag; the loop notices within one park slice.
    //
    // The Windows console handler is process-wide and ctrlc rejects a second
    // registration, so it is installed once and its flag is shared by every
    // headless run in the process. Registering per run would fail the second
    // app a process starts.
    #[cfg(windows)]
    let exit_flag = {
        static CTRL_C_FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
        let flag = CTRL_C_FLAG.get_or_init(|| {
            let flag = Arc::new(AtomicBool::new(false));
            let handler_flag = Arc::clone(&flag);
            if let Err(e) = ctrlc::set_handler(move || handler_flag.store(true, Ordering::SeqCst)) {
                eprintln!("lumen: no console-ctrl handler ({e}); Ctrl+C will not exit cleanly");
            }
            flag
        });
        // Start from a clear flag: an earlier run in this process may have set it.
        flag.store(false, Ordering::SeqCst);
        Arc::clone(flag)
    };
    #[cfg(not(windows))]
    let exit_flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, Arc::clone(&exit_flag))
            .map_err(|e| RunError::Headless(format!("signal handler: {e}")))?;
    }

    eprintln!(
        "lumenc: headless mode - no window; {}x{} logical @ dpr {dpr}{}",
        window.options.size.0,
        window.options.size.1,
        match headless.ticks {
            Some(n) => format!(", bounded to {n} tick(s)"),
            None => String::new(),
        }
    );

    let mut ticked: u64 = 0;
    // Deadline anchor for work-paced frames. `Some(d)` = the deadline the
    // frame we just ran was released at; the next frame is due at
    // `d + WORK_FRAME_INTERVAL` regardless of how long the tick took -
    // deadline-anchored pacing with no per-frame work drift and no
    // accumulation error. Cleared on idle so the next burst re-anchors
    // to "now" instead of firing a catch-up run.
    let mut next_frame_deadline: Option<Instant> = None;
    while !exit_flag.load(Ordering::Relaxed) {
        // A pending off-thread screenshot runs a tick even when nothing
        // changed, so the renderer's system answers it this iteration
        // (mirrors the windowed `present_frame` capture bypass of the
        // idle-frame retain).
        let capture_pending = app
            .render_world
            .get_resource::<SurfaceCapture>()
            .is_some_and(|c| c.is_requested());
        if capture_pending && let Some(mut fd) = app.world.get_resource_mut::<FrameDirty>() {
            fd.dirty = true;
        }

        let t_tick = (boot.on && ticked == 0).then(Instant::now);
        app.tick();
        ticked += 1;
        if let Some(t) = t_tick {
            boot.mark("first_tick (layout+extract+render)", t.elapsed());
            boot.finish();
        }

        // The frame (if any) is encoded; clear dirty like the windowed
        // present does. Systems that dirtied state after the encode
        // re-raise it and the work check below schedules a follow-up.
        if let Some(mut fd) = app.world.get_resource_mut::<FrameDirty>() {
            fd.dirty = false;
        }

        if let Some(n) = headless.ticks
            && ticked >= n
        {
            break;
        }

        // The same check the windowed `present_frame` re-arms the redraw on.
        let pending = work_pending(&app.world);

        // Bounded runs tick back-to-back; `--ticks N` bounds wall time.
        if headless.ticks.is_some() {
            continue;
        }

        if pending {
            // Pace follow-up frames at 60 Hz (vsync stand-in) against an
            // advancing deadline. An MCP wake cuts the park short (the
            // anchor is kept, so an early wake doesn't shift the phase of
            // subsequent frames); falling more than one frame behind
            // re-anchors to "now" instead of bursting catch-up ticks.
            let now = Instant::now();
            let mut deadline = match next_frame_deadline {
                Some(d) => d + WORK_FRAME_INTERVAL,
                None => now + WORK_FRAME_INTERVAL,
            };
            if deadline < now {
                deadline = now;
            }
            // Wake-cut-short ticks run ahead of their deadline; if a
            // burst of wakes outpaces 60 Hz the chained anchor would run
            // arbitrarily far into the future and stall the next paced
            // frame. Never schedule more than one interval out.
            if deadline > now + WORK_FRAME_INTERVAL {
                deadline = now + WORK_FRAME_INTERVAL;
            }
            next_frame_deadline = Some(deadline);
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                if parker.park_timeout(deadline - now) {
                    break;
                }
            }
        } else {
            next_frame_deadline = None;
            // Idle-park until an MCP wake or the earliest deadline a
            // driver asked to be woken at (a script timer). Timeout
            // slices keep signals - and, when hot reload is on, the
            // source watcher - serviced.
            let wake_at = wake_deadline(&app.world);
            loop {
                if exit_flag.load(Ordering::Relaxed) {
                    break;
                }
                let slice = match wake_at {
                    Some(at) => match at.checked_duration_since(Instant::now()) {
                        Some(left) if !left.is_zero() => left.min(IDLE_PARK_SLICE),
                        _ => break,
                    },
                    None => IDLE_PARK_SLICE,
                };
                let woken = parker.park_timeout(slice);
                // Tick on: an explicit wake (MCP, notify hot-reload
                // watcher), a poll slice when the mtime fallback is
                // active, or the deadline coming due. Otherwise stay
                // parked - zero ticks at idle, like the windowed
                // scheduler.
                if woken || hot_reload_poll || wake_at.is_some_and(|at| Instant::now() >= at) {
                    break;
                }
            }
        }
    }

    // Graceful close: same message the windowed backend emits on
    // `CloseRequested`, plus one tick so close-observing systems fire.
    if let Some(mut msgs) = app.world.get_resource_mut::<Messages<CloseRequest>>() {
        msgs.write(CloseRequest { vetoed: false });
    }
    app.tick();
    Ok(())
}

/// An offscreen renderer coming up on its own thread while the app builds.
#[derive(Resource)]
pub(crate) struct OffscreenPrestart(
    std::sync::Mutex<Option<std::thread::JoinHandle<(OffscreenResult, Duration)>>>,
);

/// What starting an offscreen renderer produced.
type OffscreenResult = Result<Box<dyn OffscreenRenderer>, String>;

impl OffscreenPrestart {
    /// Start the first of `candidates` that comes up, at `width` x `height`,
    /// on a new thread.
    pub(crate) fn spawn(
        candidates: Vec<RenderBackend>,
        width: u32,
        height: u32,
    ) -> Result<Self, RunError> {
        let handle = std::thread::Builder::new()
            .name("lumen-renderer-init".into())
            .spawn(move || {
                let t = Instant::now();
                let r = first_offscreen(&candidates, width, height);
                (r, t.elapsed())
            })
            .map_err(|e| RunError::Headless(format!("spawn renderer init thread: {e}")))?;
        Ok(Self(std::sync::Mutex::new(Some(handle))))
    }

    /// Wait for the renderer, and how long its bring-up took.
    fn join(self) -> Result<(OffscreenResult, Duration), RunError> {
        let handle = self
            .0
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ok_or_else(|| RunError::Headless("the renderer was already taken".into()))?;
        handle
            .join()
            .map_err(|_| RunError::Headless("renderer init thread panicked".into()))
    }
}

/// The offscreen renderer of the first of `candidates` that starts, at
/// `width` x `height` physical pixels. A backend that cannot start is
/// reported and the next one tried; when none starts, the error names each
/// one's reason.
fn first_offscreen(
    candidates: &[RenderBackend],
    width: u32,
    height: u32,
) -> Result<Box<dyn OffscreenRenderer>, String> {
    let mut failures = Vec::new();
    for backend in candidates {
        match (backend.offscreen)(width, height) {
            Ok(renderer) => {
                if !failures.is_empty() {
                    eprintln!(
                        "lumenc: {}; rendering with the '{}' backend",
                        failures.join("; "),
                        backend.name
                    );
                }
                return Ok(renderer);
            }
            Err(why) => failures.push(format!(
                "the '{}' render backend did not start: {why}",
                backend.name
            )),
        }
    }
    Err(if failures.is_empty() {
        "no render backend to start".to_string()
    } else {
        failures.join("; ")
    })
}
