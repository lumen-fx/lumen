//! Tokio-backed implementation of [`lumen_core::traits::Spawn`] and [`lumen_core::traits::Timer`].
//!
//! ## What this crate provides
//!
//! - [`TokioRuntime`] - a `Resource` that owns a multi-threaded tokio
//!   runtime. Cloning is cheap (the runtime is wrapped in an [`Arc`]).
//!   Drop joins the worker threads.
//! - [`AsyncCommandQueue`] - a crossbeam MPSC channel that lets a spawned
//!   future fire-and-forget a `Command` back into the main world. A
//!   [`drain_async_commands`] system runs in [`TickStage::Systems`] each
//!   tick and ferries received items into [`CommandQueue`] (the lumen
//!   analog of Qt::QueuedConnection / GLib::g_idle_add).
//! - [`AsyncTokioPlugin`] - registers both resources + the drain system,
//!   so the host (mcp, future os crates, app code) can call
//!   `world.resource::<TokioRuntime>().spawn(fut)` and have the result
//!   land on the main thread.
//!
//! ## Why a shared runtime
//!
//! Before this crate, `lumen-mcp` built its own current-thread runtime,
//! `lumen-lsp` got one from `tower-lsp`, and `lumen-ffi` had none -
//! three executors per process. Reusing one multi-threaded runtime
//! (4 workers max, bounded by available parallelism) cuts the thread
//! pool footprint and shares I/O reactor state.

#![warn(missing_docs)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender, TrySendError, unbounded};
use lumen_core::app::{App, Plugin};
use lumen_core::command::{Command, CommandQueue};
use lumen_core::tick::TickStage;
use lumen_core::traits::{Spawn, Timer};
use tokio::runtime::{Builder, Runtime};
use tracing::warn;

/// Multi-threaded tokio runtime exposed as an ECS [`Resource`].
///
/// Multi-threaded so async I/O work (HTTP fetches, file dialog
/// portals, future mcp transport) doesn't block on a single thread.
/// Worker count is capped at 4 to avoid thread sprawl on big-core
/// machines - async tasks here are I/O-bound, not CPU-bound.
#[derive(Resource, Clone)]
pub struct TokioRuntime {
    inner: Arc<Runtime>,
}

impl TokioRuntime {
    /// Build a runtime sized as `min(available_parallelism(), 4)`.
    pub fn new() -> Self {
        let n = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(2)
            .clamp(1, 4);
        let rt = Builder::new_multi_thread()
            .worker_threads(n)
            .enable_all()
            .thread_name("lumen-async")
            .build()
            .expect("lumen-async-tokio: failed to build tokio runtime");
        Self {
            inner: Arc::new(rt),
        }
    }

    /// Get a [`tokio::runtime::Handle`] for the shared runtime. Useful
    /// when an inner library wants to opt into the same executor (e.g.
    /// the mcp tokio server thread).
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.inner.handle().clone()
    }

    /// Spawn a future on the runtime and return a [`TaskHandle`].
    pub fn spawn<F>(&self, fut: F) -> TaskHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        TaskHandle {
            inner: self.inner.spawn(fut),
        }
    }

    /// Spawn a blocking task on the runtime's blocking pool.
    pub fn spawn_blocking<F, R>(&self, f: F) -> TaskHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        TaskHandle {
            inner: self.inner.spawn_blocking(f),
        }
    }

    /// Block the current thread on a future. Convenience for
    /// synchronous callers (FFI entry points, test helpers).
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.inner.block_on(fut)
    }

    /// Returns a future that completes after `d`.
    pub fn delay(&self, d: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(d))
    }
}

impl Default for TokioRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Spawn for TokioRuntime {}
impl Timer for TokioRuntime {}

/// Handle to a spawned future. Wraps [`tokio::task::JoinHandle`]; can
/// be awaited from any tokio context, or aborted.
pub struct TaskHandle<T> {
    inner: tokio::task::JoinHandle<T>,
}

impl<T> TaskHandle<T> {
    /// Abort the underlying task. Safe to call multiple times.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Returns true if the task has finished (completed, panicked, or
    /// aborted).
    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

/// Cross-thread sink that converts arbitrary `Command`s produced by
/// async tasks into main-world `Command`s applied on the next tick.
///
/// Mirrors `Qt::QueuedConnection` / `g_idle_add` semantics:
///   - Push from any thread (cheap, lock-free unbounded MPSC).
///   - Drained on the main thread during [`TickStage::Systems`] by
///     [`drain_async_commands`].
///
/// The drain forwards into the shared [`CommandQueue`] so the existing
/// [`TickStage::CommandDrain`] path applies them on the NEXT tick.
/// (One extra tick of latency is acceptable; this is the same model
/// as Qt's posted events arriving on the next event loop iteration.)
#[derive(Resource, Clone)]
pub struct AsyncCommandQueue {
    tx: Sender<Command>,
    rx: Arc<crossbeam_channel::Receiver<Command>>,
}

impl AsyncCommandQueue {
    /// Build an unbounded queue. Unbounded because the drain runs once
    /// per tick (~16 ms at 60 Hz) and async producers are I/O-bound;
    /// they shouldn't burst faster than the drain can consume.
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx: Arc::new(rx),
        }
    }

    /// Push a command from any thread. Non-blocking.
    pub fn push(&self, cmd: Command) -> Result<(), TrySendError<Command>> {
        self.tx.try_send(cmd)
    }

    /// Receiver clone for advanced consumers that want to drain
    /// directly. Most callers should use the [`drain_async_commands`]
    /// system instead.
    pub fn receiver(&self) -> Receiver<Command> {
        (*self.rx).clone()
    }
}

impl Default for AsyncCommandQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Main-thread drain system. Pulls everything queued by async tasks
/// since the previous tick and forwards into the host's [`CommandQueue`]
/// so [`TickStage::CommandDrain`] handles them next tick.
pub fn drain_async_commands(async_q: Res<AsyncCommandQueue>, cmd_q: Res<CommandQueue>) {
    let rx = async_q.receiver();
    while let Ok(cmd) = rx.try_recv() {
        if let Err(e) = cmd_q.try_push(cmd) {
            warn!("lumen-async-tokio: CommandQueue full, dropping async command: {e}");
        }
    }
}

/// Registers [`TokioRuntime`] + [`AsyncCommandQueue`] + the drain
/// system. Idempotent on re-add (resources are skipped if present).
#[derive(Default, Debug, Clone, Copy)]
pub struct AsyncTokioPlugin;

impl Plugin for AsyncTokioPlugin {
    fn build(self, app: &mut App) {
        if app.world.get_resource::<TokioRuntime>().is_none() {
            app.world.insert_resource(TokioRuntime::new());
        }
        if app.world.get_resource::<AsyncCommandQueue>().is_none() {
            app.world.insert_resource(AsyncCommandQueue::new());
        }
        app.add_systems(TickStage::Systems, drain_async_commands);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn runtime_spawns_and_awaits() {
        let rt = TokioRuntime::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let handle = rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            c.fetch_add(1, Ordering::SeqCst);
            42u32
        });
        let out = rt.block_on(handle).expect("task ok");
        assert_eq!(out, 42);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn async_command_queue_round_trips() {
        let q = AsyncCommandQueue::new();
        q.push(Command::ScriptUpdate(Box::new(7u32))).unwrap();
        let rx = q.receiver();
        match rx.try_recv().unwrap() {
            Command::ScriptUpdate(payload) => {
                let v = payload.downcast::<u32>().expect("u32 payload");
                assert_eq!(*v, 7);
            }
            _ => panic!("wrong command kind"),
        }
    }

    /// In tests, install whatever globally-expected resources the
    /// host crate's tick pipeline currently needs so a bare `App::new`
    /// can run a tick. Foundation churn (wave 1's property-store
    /// migration in `lumen-core`) keeps adding bare-essentials systems
    /// to `App::new`; this guard isolates the test from that churn.
    fn ensure_tick_compatible(app: &mut App) {
        // `PropertyStore` backs the wave-D property pipeline (drain / mirror
        // systems) that superseded the legacy `Signals` resource. `App::new`
        // installs it by default; insert defensively in case a bare `World`
        // reached this helper without going through `App::new`.
        if app
            .world
            .get_resource::<lumen_core::property_store::PropertyStore>()
            .is_none()
        {
            app.world
                .init_resource::<lumen_core::property_store::PropertyStore>();
        }
    }

    #[test]
    fn plugin_inserts_resources_and_drain_system() {
        let mut app = App::new();
        AsyncTokioPlugin.build(&mut app);
        ensure_tick_compatible(&mut app);
        assert!(app.world.get_resource::<TokioRuntime>().is_some());
        assert!(app.world.get_resource::<AsyncCommandQueue>().is_some());
        // Drain on an empty queue is a no-op; running a single tick
        // must not panic.
        app.tick();
    }

    #[test]
    fn async_queue_drains_into_command_queue() {
        let mut app = App::new();
        AsyncTokioPlugin.build(&mut app);
        ensure_tick_compatible(&mut app);
        {
            let q = app.world.resource::<AsyncCommandQueue>().clone();
            q.push(Command::ScriptUpdate(Box::new("hi".to_string())))
                .unwrap();
        }
        app.tick();
        let mut recv = app.commands();
        let mut found = false;
        for cmd in recv.drain() {
            if let Command::ScriptUpdate(payload) = cmd {
                if let Ok(s) = payload.downcast::<String>() {
                    assert_eq!(*s, "hi");
                    found = true;
                }
            }
        }
        assert!(found, "expected the async-pushed command on next tick");
    }
}
