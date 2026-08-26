//! The wakeup pump that lets a playing track move a seek bar.
//!
//! Playback advances off the UI thread, but a reactive signal may only be
//! written on it. So nothing here touches a signal: the ticker wakes the event
//! loop on a fixed cadence while a track is playing, and the woken tick reads
//! the transport on the UI thread and writes the position itself.
//!
//! While paused or stopped the ticker is quiet, so the event loop parks and an
//! idle app costs nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lumen_module::lumen_core::app::EventLoopWaker;

/// Cadence of the position-update wakeups while a track is playing.
///
/// Smooth enough for a seek bar without a busy loop. Qt's `QMediaPlayer`
/// defaults to a coarser one-second `notifyInterval`; a media UI with a
/// scrubber wants finer, so this sits between the two.
pub const TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Background thread that wakes the event loop while a track plays.
///
/// Dropping it asks the thread to exit at its next wake, so a backend that
/// holds one in an `Option` can shut the pump down by clearing the field.
pub struct PositionTicker {
    alive: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<EventLoopWaker>>>,
}

impl PositionTicker {
    /// Start a ticker that wakes the loop while `playing` is set. Pair it with
    /// [`crate::Transport::playing_flag`].
    pub fn spawn(playing: Arc<AtomicBool>) -> Self {
        let waker: Arc<Mutex<Option<EventLoopWaker>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        {
            let waker = Arc::clone(&waker);
            let alive = Arc::clone(&alive);
            std::thread::Builder::new()
                .name("lumen-audio-ticker".into())
                .spawn(move || {
                    while alive.load(Ordering::Acquire) {
                        std::thread::sleep(TICK_INTERVAL);
                        if playing.load(Ordering::Acquire)
                            && let Ok(guard) = waker.lock()
                            && let Some(w) = guard.as_ref()
                        {
                            w.wake();
                        }
                    }
                })
                .expect("spawn lumen-audio ticker");
        }
        Self { alive, waker }
    }

    /// Wire the loop waker. Idempotent; the embedder calls it once the waker
    /// resource exists, which is after the backend is built.
    pub fn set_waker(&self, waker: EventLoopWaker) {
        if let Ok(mut guard) = self.waker.lock() {
            *guard = Some(waker);
        }
    }
}

impl Drop for PositionTicker {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}
