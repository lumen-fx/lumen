//! Input simulation queue: cross-thread bridge that lets an MCP client (or
//! `lumenc` CLI) inject pointer / key / scroll events into the main world.
//!
//! Pattern mirrors `SurfaceCapture` from `lumen-core::render_world`:
//!
//! 1. The TCP handler thread (tokio) pushes a [`SimulateRequest`] onto the
//!    shared queue; [`SimulateQueue::push`] hands back a monotonic sequence
//!    number.
//! 2. A tick-side system pops exactly ONE request per tick in
//!    `TickStage::Input` (BEFORE the real input dispatch) and converts it
//!    into the same `MessageWriter<PointerMoved>` / `PointerPressed` / ...
//!    events that the winit backend would emit. One-request-per-tick (W6
//!    T4) keeps rapid-fire requests deterministic: an Escape pushed right
//!    after a click can never share the click's tick - it always lands on
//!    the NEXT tick, after the click's systems (popup spawn, focus move,
//!    ...) have completed.
//! 3. An end-of-tick system (`TickStage::A11ySync`) publishes the popped
//!    sequence number via [`SimulateQueue::completed_seq`]; the handler
//!    polls it until `completed >= its own seq` (or a 500 ms timeout), so
//!    the RPC response returns only after the event's full tick ran.
//!
//! The queue is only **drained** when [`LumenMcpPlugin::with_simulate_enabled`]
//! is set, which keeps real apps safe by default - accidental MCP connections
//! can never inject input.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use bevy_ecs::prelude::*;
use serde::Deserialize;

use lumen_core::app::EventLoopWaker;
use lumen_core::input::Modifiers;

/// Cross-thread queue of pending simulate requests. Cloning is cheap (one
/// `Arc`); locking is brief (push or drain-all). Inserted as a `Resource`
/// into the main world by `LumenMcpPlugin`.
#[derive(Resource, Clone, Default, Debug)]
pub struct SimulateQueue {
    inner: Arc<Mutex<VecDeque<(u64, SimulateRequest)>>>,
    /// Wakeup handle for the platform event loop, wired in once
    /// [`EventLoopWaker`] shows up as a world resource (see
    /// `LumenMcpPlugin::build`'s wiring system). `OnceLock` so wiring can
    /// run every tick without re-cloning the `Arc` after the first hit,
    /// and so [`Self::push`] never blocks on a lock shared with readers.
    waker: Arc<OnceLock<EventLoopWaker>>,
    /// Sequence source for [`Self::push`]. Starts at 0; the first pushed
    /// request gets seq 1, so `completed` (also starting at 0) reads as
    /// "nothing processed yet".
    next_seq: Arc<AtomicU64>,
    /// Highest sequence number whose request has been injected AND whose
    /// tick has run to `TickStage::A11ySync` (published by
    /// `publish_simulate_completion` in `plugin.rs`). The TCP handler
    /// polls this to decide when its RPC may respond (W6 T4).
    completed: Arc<AtomicU64>,
}

impl SimulateQueue {
    /// Push one request from any thread. Returns the request's sequence
    /// number; wait on [`Self::completed_seq`] `>= seq` to know the
    /// request's full tick has run (W6 T4).
    ///
    /// On a poisoned mutex we log + recover the inner queue via
    /// `into_inner()` semantics. A previous panic in this critical section
    /// doesn't invalidate the `VecDeque` itself (it's just a buffer of
    /// `SimulateRequest`s), so silently dropping the push is the worst
    /// option - an LLM agent calling `lumen.simulate` would never know
    /// the call vanished.
    ///
    /// Also nudges the platform event loop (if [`Self::set_waker`] wired
    /// one in) so the push doesn't sit invisible until an unrelated OS
    /// event ticks the app - the loop only wakes because we just handed
    /// it real work, never spontaneously.
    pub fn push(&self, req: SimulateRequest) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        match self.inner.lock() {
            Ok(mut q) => q.push_back((seq, req)),
            Err(poisoned) => {
                tracing::warn!("lumen-mcp: SimulateQueue mutex poisoned; recovering inner queue");
                let mut q = poisoned.into_inner();
                q.push_back((seq, req));
            }
        }
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
        seq
    }

    /// Attach the event-loop wakeup handle. Idempotent - only the first
    /// call takes effect, so a per-tick wiring system can call this
    /// unconditionally without re-arming anything on later ticks.
    pub fn set_waker(&self, waker: EventLoopWaker) {
        let _ = self.waker.set(waker);
    }

    /// Pop the OLDEST pending request - exactly one per call, preserving
    /// FIFO order. Called by the tick-side system once per
    /// `TickStage::Input` (W6 T4: one input-batch per tick). Returns the
    /// request's sequence number alongside it, plus whether more
    /// requests remain (the caller re-wakes the loop for a follow-up
    /// tick so queued requests don't stall in a parked event loop).
    /// Recovers from a poisoned mutex (see [`Self::push`]).
    pub fn pop_front(&self) -> (Option<(u64, SimulateRequest)>, bool) {
        let mut guard = match self.inner.lock() {
            Ok(q) => q,
            Err(poisoned) => {
                tracing::warn!("lumen-mcp: SimulateQueue mutex poisoned; recovering inner queue");
                poisoned.into_inner()
            }
        };
        let popped = guard.pop_front();
        let remaining = !guard.is_empty();
        (popped, remaining)
    }

    /// Highest sequence number whose request has been injected and whose
    /// tick has run through `TickStage::A11ySync`. `0` = none yet.
    pub fn completed_seq(&self) -> u64 {
        self.completed.load(Ordering::Acquire)
    }

    /// Publish tick completion for `seq` (monotonic max). Called from the
    /// end-of-tick system in `plugin.rs`; public so integration tests can
    /// exercise the contract.
    pub fn publish_completed(&self, seq: u64) {
        self.completed.fetch_max(seq, Ordering::AcqRel);
    }

    /// Re-fire the event-loop waker if one is wired - used by the drain
    /// system when requests remain queued after its one-per-tick pop.
    pub fn wake(&self) {
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
    }
}

/// One simulate request. Constructed from JSON params in the TCP handler.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SimulateKind {
    /// Move the OS-level pointer reading to a new logical-pixel coordinate.
    /// Does NOT fire press/release - useful for hover-only tests.
    PointerMove {
        /// X in window coordinates.
        x: f32,
        /// Y in window coordinates.
        y: f32,
    },
    /// Issue a press / release pair at `(x, y)` with `button` (default
    /// `primary`). A `PointerMoved` is also emitted to keep hit-test in sync.
    Click {
        /// X in window coordinates.
        x: f32,
        /// Y in window coordinates.
        y: f32,
        /// Optional button override; defaults to `primary`.
        #[serde(default)]
        button: Option<String>,
    },
    /// Press-only half of [`SimulateKind::Click`]: press `button` at
    /// `(x, y)` and leave it held (`PointerState.primary_down` stays
    /// `true` for the primary button). Pair with
    /// [`SimulateKind::PointerMove`] and [`SimulateKind::PointerUp`] to
    /// drive drag gestures - press-drag-off a button, slider thumb
    /// drags, text drag-selection.
    PointerDown {
        /// X in window coordinates.
        x: f32,
        /// Y in window coordinates.
        y: f32,
        /// Optional button override; defaults to `primary`.
        #[serde(default)]
        button: Option<String>,
    },
    /// Release-only half of [`SimulateKind::Click`]: release `button` at
    /// `(x, y)`.
    PointerUp {
        /// X in window coordinates.
        x: f32,
        /// Y in window coordinates.
        y: f32,
        /// Optional button override; defaults to `primary`.
        #[serde(default)]
        button: Option<String>,
    },
    /// Press a key (with optional modifiers). For text input, prefer
    /// [`SimulateKind::Type`].
    Key {
        /// Key name: `"Enter"`, `"Tab"`, `"Escape"`, `"Backspace"`,
        /// `"ArrowUp"`, single character like `"a"`, etc.
        key: String,
        /// Modifier state. Defaults to all-false.
        #[serde(default)]
        modifiers: SimulateModifiers,
    },
    /// Type a string. Each character becomes one `KeyPressed` +
    /// `KeyReleased` pair on the same tick. Modifiers always false.
    Type {
        /// The text to type.
        text: String,
    },
    /// Scroll at `(x, y)` by `(dx, dy)` logical pixels.
    Scroll {
        /// X in window coordinates.
        x: f32,
        /// Y in window coordinates.
        y: f32,
        /// Horizontal scroll delta (logical pixels).
        dx: f32,
        /// Vertical scroll delta (logical pixels).
        dy: f32,
    },
}

/// JSON-friendly modifier struct (`Modifiers` from lumen-core uses a Rust
/// keyword `super_` for the Cmd key; serde's `rename` keeps the wire format
/// natural).
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SimulateModifiers {
    /// Shift held.
    pub shift: bool,
    /// Ctrl (or Cmd on macOS).
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Super / Cmd / Windows-key.
    #[serde(rename = "super")]
    pub super_: bool,
}

/// One pending request bundle. Includes the kind and an optional `wait_for`
/// driver name used by the TCP handler to decide which message ring to poll.
#[derive(Clone, Debug, Deserialize)]
pub struct SimulateRequest {
    #[serde(flatten)]
    /// The event kind.
    pub kind: SimulateKind,
    /// Optional ring name to poll after dispatch - e.g. `"ClickEvent"` to
    /// confirm the click hit an interactive element. Handler matches the
    /// same string constants used by `lumen.recent_messages`.
    #[serde(default)]
    pub wait_for: Option<String>,
}

impl From<SimulateModifiers> for Modifiers {
    fn from(m: SimulateModifiers) -> Self {
        Modifiers {
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            super_: m.super_,
        }
    }
}

#[cfg(test)]
mod waker_tests {
    //! `SimulateQueue`'s wakeup handle: pushing from a cross-thread producer
    //! (the MCP server thread, in production) must nudge a parked platform
    //! event loop rather than leave it waiting for an unrelated OS event.
    //! `EventLoopWaker` wraps an arbitrary closure, so these tests stand in
    //! for the real `winit::event_loop::EventLoopProxy::send_event` without
    //! needing a live display.
    use super::*;
    use lumen_core::app::EventLoopWaker;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_waker() -> (EventLoopWaker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        (
            EventLoopWaker(Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            })),
            count,
        )
    }

    fn sample_request() -> SimulateRequest {
        SimulateRequest {
            kind: SimulateKind::PointerMove { x: 1.0, y: 2.0 },
            wait_for: None,
        }
    }

    /// Once a waker is wired in, every `push` must invoke it - this is the
    /// core fix: a push from the MCP server thread must not sit invisible
    /// until an unrelated OS event ticks the app.
    #[test]
    fn push_invokes_the_waker_once_wired() {
        let queue = SimulateQueue::default();
        let (waker, count) = counting_waker();
        queue.set_waker(waker);

        queue.push(sample_request());
        assert_eq!(count.load(Ordering::SeqCst), 1, "one push, one wake");

        queue.push(sample_request());
        assert_eq!(count.load(Ordering::SeqCst), 2, "second push wakes again");
    }

    /// Idle quiescence: wiring the waker in does not itself fire it, and a
    /// push before any waker exists is silent (no panic, no phantom wake
    /// once one shows up later) - the loop only wakes because real work was
    /// pushed, never spontaneously.
    #[test]
    fn no_waker_no_push_means_no_wake() {
        let queue = SimulateQueue::default();
        // Push before a waker exists: must not panic.
        queue.push(sample_request());

        let (waker, count) = counting_waker();
        queue.set_waker(waker);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "attaching a waker must not retroactively wake for an earlier push"
        );
    }

    /// `set_waker` is a `OnceLock` write: the first call wins and later
    /// calls are no-ops, matching the per-tick wiring system in
    /// `LumenMcpPlugin` that calls it unconditionally every tick.
    #[test]
    fn set_waker_is_idempotent() {
        let queue = SimulateQueue::default();
        let (first, first_count) = counting_waker();
        let (second, second_count) = counting_waker();
        queue.set_waker(first);
        queue.set_waker(second);

        queue.push(sample_request());
        assert_eq!(first_count.load(Ordering::SeqCst), 1, "first waker wins");
        assert_eq!(
            second_count.load(Ordering::SeqCst),
            0,
            "second set_waker call is a no-op"
        );
    }
}
