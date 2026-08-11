//! Process-wide, opt-in HTTP capture sink for dev tooling.
//!
//! The scripting HTTP layer (`lumen-script`'s `fetch()` / `http()`
//! builtins) unconditionally reports request/response lifecycle events to
//! [`record`]. When no sink has been installed - the default in a release
//! build, and always when `lumen-devtools` is not compiled in - [`record`]
//! is a single atomic load plus branch and drops the event on the floor, so
//! there is zero capture cost and no unbounded buffer growth.
//!
//! A dev-only consumer (the devtools Network tab) calls [`init_net_capture`]
//! once at startup to install the sink, then drains accumulated events each
//! tick via [`drain`] into its own bounded ring. This mirrors the
//! [`crate::signals`] external-mutation channel pattern: one global
//! `OnceLock` sender, one `OnceLock<Mutex<Receiver>>`, both idempotent.

use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender, unbounded};

/// One HTTP lifecycle event emitted by the scripting fetch/http layer.
///
/// A request produces a [`NetEvent::Started`] when it is dispatched and a
/// matching [`NetEvent::Completed`] (correlated by `tag`) when the worker
/// thread's reply lands. The devtools Network tab pairs them by `tag`, the
/// same identifier scripts pass to `fetch(url, tag)` / `http(#{...})`.
#[derive(Clone, Debug)]
pub enum NetEvent {
    /// A request was dispatched to the off-thread worker.
    Started {
        /// Script-supplied correlation tag (`fetch(url, tag)`).
        tag: String,
        /// HTTP method (`"GET"`, `"POST"`, ...).
        method: String,
        /// Target URL.
        url: String,
    },
    /// A previously-[`NetEvent::Started`] request's reply arrived.
    Completed {
        /// Correlation tag matching the [`NetEvent::Started`] event.
        tag: String,
        /// `true` when the transport succeeded (any HTTP status), `false`
        /// on a transport error (DNS, connect, timeout, bad method/url).
        ok: bool,
        /// HTTP status code when `ok`; `0` on a transport error.
        status: u16,
        /// Error string when `!ok`; empty otherwise.
        error: String,
    },
}

static NET_TX: OnceLock<Sender<NetEvent>> = OnceLock::new();
static NET_RX: OnceLock<Mutex<Receiver<NetEvent>>> = OnceLock::new();

/// Idempotently install the capture sink. Safe to call multiple times; only
/// the first call creates the channel. After this returns, [`record`] starts
/// forwarding events for [`drain`] to collect.
pub fn init_net_capture() {
    NET_TX.get_or_init(|| {
        let (tx, rx) = unbounded();
        let _ = NET_RX.set(Mutex::new(rx));
        tx
    });
}

/// Report an HTTP lifecycle event. No-op (one atomic load + branch) until
/// [`init_net_capture`] has run, so release builds without dev tooling pay
/// nothing and never accumulate an unbounded buffer.
pub fn record(event: NetEvent) {
    if let Some(tx) = NET_TX.get() {
        let _ = tx.send(event);
    }
}

/// Drain up to `max` pending events (oldest first). Returns empty when the
/// sink was never installed or nothing is queued.
pub fn drain(max: usize) -> Vec<NetEvent> {
    let Some(rx_cell) = NET_RX.get() else {
        return Vec::new();
    };
    let Ok(rx) = rx_cell.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while out.len() < max {
        match rx.try_recv() {
            Ok(ev) => out.push(ev),
            Err(_) => break,
        }
    }
    out
}
