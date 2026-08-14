//! The async seam: [`Spawn`] for running work off the main thread, [`Timer`]
//! for waiting without blocking it, and the two service resources that carry
//! the selected backend through the world.
//!
//! Lumen ships one implementation of both (`lumen-async-tokio`), and a crate
//! that needs async work asks the world for [`SpawnService`] rather than
//! naming that backend. Nothing installs an async backend by default, so a
//! consumer must have a path that works when the resource is absent: a file
//! dialog, for instance, falls back to a blocking call.
//!
//! Futures cross the seam boxed ([`BoxFuture`]) because both traits are used
//! as trait objects.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bevy_ecs::prelude::Resource;

/// A boxed, `Send` future with a `'static` lifetime: what the async seam
/// passes across a trait-object boundary.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// An executor that runs futures away from the tick loop.
///
/// Implementations own their worker threads and outlive every future they are
/// handed, so [`Self::spawn`] is fire-and-forget: a task that wants to report
/// back to the world pushes a [`Command`](crate::command::Command) onto the
/// [`CommandQueue`](crate::command::CommandQueue), which the next tick drains
/// on the main thread.
pub trait Spawn: Send + Sync + 'static {
    /// Run `fut` to completion on the executor and return immediately.
    ///
    /// Dropping the returned unit does not cancel the task; there is no
    /// handle, by design. Cancellation, timeouts, and result delivery are the
    /// future's own business, which keeps the seam free of executor-specific
    /// join-handle types.
    fn spawn(&self, fut: BoxFuture<()>);

    /// Run a blocking closure on the executor's blocking pool.
    ///
    /// For work that parks the calling thread (a synchronous file read, a
    /// platform dialog with no async form). Implementations that have no
    /// separate pool may run it on a plain thread.
    fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>);
}

/// A source of futures that complete after a delay.
///
/// Separate from [`Spawn`] because waiting and executing are different
/// capabilities: a caller that is already inside an async task needs the
/// timer, not the executor.
pub trait Timer: Send + Sync + 'static {
    /// Returns a future that completes once `duration` has elapsed.
    ///
    /// Creating the future must not require an ambient executor context, so a
    /// caller can build it anywhere and await it wherever it likes.
    fn sleep(&self, duration: Duration) -> BoxFuture<()>;
}

/// The executor an app runs async work on, held as a `Resource` so any crate
/// can reach it without naming a backend.
///
/// Install one in a plugin (`lumen-async-tokio`'s does), or from an app hook
/// to swap in your own. Absence is meaningful: a consumer that finds no
/// `SpawnService` takes its blocking path.
#[derive(Resource, Clone)]
pub struct SpawnService(Arc<dyn Spawn>);

impl SpawnService {
    /// Wrap an executor.
    pub fn new<S: Spawn>(spawn: S) -> Self {
        Self(Arc::new(spawn))
    }

    /// Borrow the executor as a trait object.
    pub fn as_spawn(&self) -> &dyn Spawn {
        self.0.as_ref()
    }

    /// Clone out the shared executor handle, for a consumer that stores it
    /// past the borrow of the world.
    pub fn handle(&self) -> Arc<dyn Spawn> {
        Arc::clone(&self.0)
    }
}

impl<S: Spawn> From<S> for SpawnService {
    fn from(spawn: S) -> Self {
        Self::new(spawn)
    }
}

impl std::ops::Deref for SpawnService {
    type Target = dyn Spawn;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// The timer source an app waits on, held as a `Resource` alongside
/// [`SpawnService`].
#[derive(Resource, Clone)]
pub struct TimerService(Arc<dyn Timer>);

impl TimerService {
    /// Wrap a timer.
    pub fn new<T: Timer>(timer: T) -> Self {
        Self(Arc::new(timer))
    }

    /// Borrow the timer as a trait object.
    pub fn as_timer(&self) -> &dyn Timer {
        self.0.as_ref()
    }

    /// Clone out the shared timer handle.
    pub fn handle(&self) -> Arc<dyn Timer> {
        Arc::clone(&self.0)
    }
}

impl<T: Timer> From<T> for TimerService {
    fn from(timer: T) -> Self {
        Self::new(timer)
    }
}

impl std::ops::Deref for TimerService {
    type Target = dyn Timer;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Executor that runs each future on its own thread and joins it before
    /// returning. Enough to prove the trait objects dispatch.
    #[derive(Default)]
    struct ThreadSpawn {
        spawned: Arc<AtomicUsize>,
    }

    impl Spawn for ThreadSpawn {
        fn spawn(&self, fut: BoxFuture<()>) {
            self.spawned.fetch_add(1, Ordering::SeqCst);
            let handle = std::thread::spawn(move || block_on_unit(fut));
            handle.join().expect("task thread joins");
        }

        fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>) {
            self.spawned.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(task).join().expect("task thread joins");
        }
    }

    struct InstantTimer;

    impl Timer for InstantTimer {
        fn sleep(&self, _duration: Duration) -> BoxFuture<()> {
            Box::pin(std::future::ready(()))
        }
    }

    /// Minimal executor for a future that never yields pending forever: polls
    /// with a no-op waker until it resolves. The test futures either resolve
    /// on the first poll or are driven by another thread.
    fn block_on_unit(mut fut: BoxFuture<()>) {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        loop {
            if fut.as_mut().poll(&mut cx).is_ready() {
                return;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn spawn_service_dispatches_to_the_installed_executor() {
        let counter = Arc::new(AtomicUsize::new(0));
        let service = SpawnService::new(ThreadSpawn {
            spawned: Arc::clone(&counter),
        });
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_in_task = Arc::clone(&ran);
        service.spawn(Box::pin(async move {
            ran_in_task.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn spawn_service_dispatches_blocking_work() {
        let service = SpawnService::from(ThreadSpawn::default());
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_in_task = Arc::clone(&ran);
        service.spawn_blocking(Box::new(move || {
            ran_in_task.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timer_service_returns_a_future_from_the_installed_timer() {
        let service = TimerService::from(InstantTimer);
        block_on_unit(service.sleep(Duration::from_secs(3600)));
    }

    #[test]
    fn services_are_cloneable_handles_on_one_backend() {
        let counter = Arc::new(AtomicUsize::new(0));
        let service = SpawnService::new(ThreadSpawn {
            spawned: Arc::clone(&counter),
        });
        let clone = service.clone();
        service.spawn(Box::pin(async {}));
        clone.spawn(Box::pin(async {}));
        clone.handle().spawn(Box::pin(async {}));
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
