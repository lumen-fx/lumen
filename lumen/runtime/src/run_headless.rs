//! True headless run mode: the FULL app pipeline - layout, real GPU
//! rendering (wgpu + vello via the shared Node-IR walker), the MCP
//! server, input simulation, hot reload, and screenshots - with zero
//! windows. No winit event loop is created, so the desktop / compositor
//! is never touched. This is the automation / CI mode behind
//! `lumenc run <app> --headless`.
//!
//! ## How it differs from [`crate::run::run_app_headless`]
//!
//! The bare `run_app_headless` (kept as the FFI / SDK contract) only
//! ticks the main-world schedule - no renderer, no pixels. This mode
//! additionally installs [`lumen_render_wgpu::WgpuRendererPlugin`], the
//! same offscreen renderer the golden-image tests use, driven by the
//! same retained-scene walker the windowed backend runs - so extracted
//! geometry, dpr scaling, text shaping, and fragment caching behave
//! identically to the windowed path.
//!
//! ## Frame pacing
//!
//! Ticks run on demand, mirroring the windowed `RedrawScheduler`
//! semantics without the pause-on-unfocused gate (there is no focus):
//!
//! * a wake from the MCP server thread (simulate push or screenshot
//!   request, via [`lumen_core::app::EventLoopWaker`]) runs a tick
//!   immediately;
//! * while work is pending (animations mid-flight, undrained external
//!   property writes, dirty frame), ticks are paced at ~60 Hz - the
//!   stand-in for vsync;
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
use lumen_core::input::CloseRequest;
use lumen_core::prelude::*;
use lumen_core::render_world::{AnimationsActive, FrameDirty, SurfaceCapture, SurfaceFrame};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin};

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
/// phase-by-phase startup breakdown (build/parse/font-scan, GPU
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
            let s = lumen_text_cosmic::CosmicShaper::new();
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
    // GPU bring-up (wgpu instance/adapter/device + vello pipeline
    // compilation) costs ~30-35 ms and needs nothing from the app world,
    // so it runs on a spawned thread OVERLAPPED with `build_headless_app`
    // (markup/CSS parse, system-font scan, ECS spawn). Sized from the
    // CLI size as a guess; if `lumen.toml [window] size` overrides it the
    // offscreen target is re-allocated after the join - `resize` only
    // swaps the texture, every expensive init step is size-independent.
    let boot = BootTrace::new();
    let dpr = headless.dpr.max(0.01);
    let guess_w = (opts.size.0 as f32 * dpr).round().max(1.0) as u32;
    let guess_h = (opts.size.1 as f32 * dpr).round().max(1.0) as u32;
    let gpu_init = std::thread::Builder::new()
        .name("lumen-gpu-init".into())
        .spawn(move || {
            // Time the whole bring-up (instance + adapter + device +
            // vello pipeline/shader compile) on the bg thread so the
            // trace can compare it against the concurrent build wall.
            let t = Instant::now();
            let r = WgpuRenderer::new_offscreen(guess_w, guess_h);
            (r, t.elapsed())
        })
        .map_err(|e| RunError::Headless(format!("spawn GPU init thread: {e}")))?;

    let t_build = Instant::now();
    let (mut app, mut winit_opts) = build_headless_app(opts)?;
    boot.mark("build_app (parse+ecs+fontscan)", t_build.elapsed());
    boot.standalone_fontscan();

    // While the GPU thread finishes: pre-warm the shapers' cold path
    // (sans-serif face load + fallback-chain init inside cosmic-text,
    // ~10-15 ms on first shape) so the first real layout/render tick
    // doesn't pay it. The strings are throwaway; only the font-system
    // warmup matters, so a short ASCII pangram at the default UI sizes
    // and both common weights is enough.
    let t_warm = Instant::now();
    {
        const WARMUP: &str = "The quick brown fox jumps over 0123456789.";
        let warm = |shaper: &mut dyn lumen_text::TextShaper| {
            for (size, weight) in [(14.0_f32, 400_u16), (16.0, 400), (14.0, 700)] {
                let _ = shaper.shape(
                    WARMUP,
                    size,
                    lumen_text::ShapeOptions {
                        weight,
                        ..Default::default()
                    },
                );
            }
        };
        if let Some(mut layout_shaper) = app
            .world
            .get_non_send_mut::<lumen_text_cosmic::CosmicShaper>()
        {
            warm(&mut *layout_shaper);
        }
        if let Some(render_shaper) = winit_opts.text_shaper.as_deref_mut() {
            warm(render_shaper);
        }
    }
    boot.mark("shaper warmup (overlap gpu)", t_warm.elapsed());
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
    let logical = glam::Vec2::new(winit_opts.size.0 as f32, winit_opts.size.1 as f32);
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = logical;
        vp.scale_factor = dpr;
        vp.clear = winit_opts.clear;
    }

    // Offscreen GPU context: join the init thread spawned before
    // `build_headless_app` (adapter requested WITHOUT a surface - falls
    // back to lavapipe/llvmpipe where no hardware GPU is reachable). The
    // renderer is pre-built here so init failure surfaces as an error
    // instead of the plugin's panic.
    let phys_w = (logical.x * dpr).round().max(1.0) as u32;
    let phys_h = (logical.y * dpr).round().max(1.0) as u32;
    let t_join = Instant::now();
    let (renderer_res, gpu_wall) = gpu_init
        .join()
        .map_err(|_| RunError::Headless("GPU init thread panicked".into()))?;
    boot.mark("gpu_join_wait (main blocked)", t_join.elapsed());
    boot.mark("  |- gpu_init_wall (bg thread)", gpu_wall);
    let mut renderer =
        renderer_res.map_err(|e| RunError::Headless(format!("offscreen GPU init: {e}")))?;
    // No-op when lumen.toml didn't override the CLI size guess.
    renderer.resize(phys_w, phys_h);
    let mut render_plugin = WgpuRendererPlugin::new(phys_w, phys_h).with_renderer(renderer);
    // Reuse the text shaper built for the windowed path (CosmicShaper)
    // so glyph output matches the window byte-for-byte.
    if let Some(shaper) = winit_opts.text_shaper.take() {
        render_plugin = render_plugin.with_boxed_text_shaper(shaper);
    }
    app.add_plugin(render_plugin);

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
    app.world.insert_resource(waker);

    // Snapshot cadence: headless ticks are on-demand, so per-tick MCP
    // snapshots are effectively free and make `lumen.simulate`'s
    // frame-advance wait deterministic (the windowed 1 Hz throttle would
    // otherwise leave a woken tick invisible to the polling server
    // thread for up to a second). Compiled out with the `mcp` feature
    // (Part B tree-shaking): a trimmed bundle installs no MCP schedule.
    #[cfg(feature = "mcp")]
    for world in [&mut app.world, &mut app.render_world] {
        if let Some(mut sched) = world.get_resource_mut::<lumen_mcp::McpSnapshotSchedule>() {
            sched.interval = Duration::ZERO;
        }
    }

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
        winit_opts.size.0,
        winit_opts.size.1,
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
        // A pending off-thread screenshot must force a fresh encode even
        // when nothing changed (mirrors the windowed `present_frame`
        // capture bypass of the idle-frame retain).
        let capture = app.render_world.get_resource::<SurfaceCapture>().cloned();
        let capture_pending = capture.as_ref().is_some_and(|c| c.is_requested());
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

        if capture_pending && let Some(capture) = capture {
            service_capture(&mut app, &capture);
        }

        if let Some(n) = headless.ticks
            && ticked >= n
        {
            break;
        }

        // Pending-work sources - the same three the windowed
        // `present_frame` re-arms the redraw on.
        let work_pending = lumen_core::property_store::external_properties_pending()
            || app
                .world
                .get_resource::<AnimationsActive>()
                .is_some_and(|a| a.get())
            || app
                .world
                .get_resource::<FrameDirty>()
                .is_some_and(|f| f.dirty);

        // Bounded runs tick back-to-back; `--ticks N` bounds wall time.
        if headless.ticks.is_some() {
            continue;
        }

        if work_pending {
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
            // Idle-park until an MCP wake. Timeout slices keep signals -
            // and, when hot reload is on, the source watcher - serviced.
            loop {
                if exit_flag.load(Ordering::Relaxed) {
                    break;
                }
                let woken = parker.park_timeout(IDLE_PARK_SLICE);
                // Tick on: an explicit wake (MCP, notify hot-reload
                // watcher), or a poll slice when the mtime fallback is
                // active. Otherwise stay parked - zero ticks at idle,
                // like the windowed scheduler.
                if woken || hot_reload_poll {
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

/// Fulfil a pending [`SurfaceCapture`] request from the offscreen target.
/// Headless counterpart of the readback in the windowed `render_frame`:
/// the texture holds the last encoded frame (this tick's, since a pending
/// capture forces `FrameDirty` before the tick), so the copy is exact.
fn service_capture(app: &mut lumen_core::app::App, capture: &SurfaceCapture) {
    let readback = app
        .render_world
        .get_non_send::<WgpuRenderer>()
        .map(|renderer| (renderer.size(), renderer.read_rgba8()));
    match readback {
        Some(((width, height), Ok(rgba8))) => {
            capture.write(SurfaceFrame {
                width,
                height,
                rgba8,
            });
        }
        Some((_, Err(e))) => eprintln!("lumenc: headless surface readback failed: {e}"),
        None => eprintln!("lumenc: headless surface readback failed: no renderer installed"),
    }
    // Always clear so a persistent GPU error can't wedge the requester.
    capture.clear_request();
}
