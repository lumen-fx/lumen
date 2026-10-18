//! The log lines and the GPU line a report quotes.
//!
//! Lines arrive from `lumen_core`'s output (what the runtime and the scripts
//! print), from `tracing` warnings and errors when no other subscriber owns
//! them, and from [`crate::log_line`]. The last [`LOG_LINES`] are kept here
//! for a report the app writes itself, and each one is also passed to the
//! helper as it arrives.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;

use lumen_crash_report::LOG_LINES;
use lumen_crash_report::protocol::Message;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static GPU: Mutex<Option<String>> = Mutex::new(None);

/// Start collecting: listen to `lumen_core`'s output, and take `tracing`'s
/// warnings when nothing else does.
pub(crate) fn install() {
    lumen_core::output::add_listener(push_str);
    if !tracing::dispatcher::has_been_set() {
        // Losing the race to an app that sets its own subscriber at the
        // same moment is fine: that one wins and its output is its own.
        let _ = tracing::subscriber::set_global_default(Warnings);
    }
}

fn push_str(line: &str) {
    push(line.to_string());
}

/// Keep `line` and pass it to the helper.
pub(crate) fn push(line: String) {
    let mut ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == LOG_LINES {
        ring.pop_front();
    }
    crate::client::send(Message::Log(line.clone()));
    ring.push_back(line);
}

/// The kept lines, oldest first.
pub(crate) fn lines() -> Vec<String> {
    RING.lock()
        .map(|r| r.iter().cloned().collect())
        .unwrap_or_default()
}

/// Send every kept line and the GPU line to a helper that just connected,
/// then run `publish`. Holds the ring throughout, so a line logged
/// meanwhile waits and goes out once, after these, on the published
/// connection.
pub(crate) fn replay(send: impl Fn(Message), publish: impl FnOnce()) {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    for line in ring.iter() {
        send(Message::Log(line.clone()));
    }
    if let Some(gpu) = gpu() {
        send(Message::Gpu(gpu));
    }
    publish();
}

/// Remember the GPU line; whether it changed.
pub(crate) fn set_gpu(line: &str) -> bool {
    let mut gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if gpu.as_deref() == Some(line) {
        return false;
    }
    *gpu = Some(line.to_string());
    true
}

/// The GPU line, once one was set.
pub(crate) fn gpu() -> Option<String> {
    GPU.lock().ok().and_then(|g| g.clone())
}

/// A `tracing` subscriber that keeps warnings and errors as log lines and
/// ignores everything else. Spans are not tracked.
struct Warnings;

impl Warnings {
    fn wanted(metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= Level::WARN
    }
}

impl Subscriber for Warnings {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if Self::wanted(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        Self::wanted(metadata)
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut line = format!("{} {}: ", metadata.level(), metadata.target());
        event.record(&mut Line(&mut line));
        push(line);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Formats an event's fields: the message bare, the rest as `name=value`.
struct Line<'a>(&'a mut String);

impl Visit for Line<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}
