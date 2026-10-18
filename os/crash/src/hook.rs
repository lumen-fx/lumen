//! The panic hook.
//!
//! Chained in front of the hook that was installed before it, which still
//! runs (so a panic still prints where it always did). Each panic is passed
//! to the helper with its backtrace; the first panic at each source location
//! also has the helper dump the process at that moment, while the stacks
//! still show it. Whether a panic becomes a report is decided later, when
//! the app either exits normally or dies.
//!
//! Lumen survives some panics on purpose (a script that panics every tick
//! is contained), so a panic seen again at a location already reported is
//! cheap: no new backtrace is captured and no new dump is taken.

use std::backtrace::Backtrace;
use std::cell::Cell;
use std::collections::HashMap;
use std::panic::PanicHookInfo;
use std::sync::Mutex;
use std::time::Duration;

use lumen_crash_report::protocol::{Message, PanicMessage};

/// How long a panic waits for a helper that is still being reached.
const WAIT_FOR_HELPER: Duration = Duration::from_secs(2);
/// How many panics in one process get a dump of their own.
const MAX_DUMPS: usize = 4;
/// How many source locations the hook remembers backtraces for.
const MAX_LOCATIONS: usize = 64;

/// The backtrace first captured at each location, and how many dumps were
/// taken. One lock, so two threads panicking at once take turns.
struct Seen {
    backtraces: HashMap<String, String>,
    dumps: usize,
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Put the hook in front of the current one.
pub(crate) fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !IN_HOOK.with(|h| h.replace(true)) {
            on_panic(info);
            IN_HOOK.with(|h| h.set(false));
        }
        previous(info);
    }));
}

fn on_panic(info: &PanicHookInfo<'_>) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen.get_or_insert_with(|| Seen {
        backtraces: HashMap::new(),
        dumps: 0,
    });
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_default();
    let first_here = !seen.backtraces.contains_key(&location);
    let backtrace = match seen.backtraces.get(&location) {
        Some(earlier) => format!("(as at the first panic here)\n{earlier}"),
        None => {
            let captured = from_the_panic(&Backtrace::force_capture().to_string());
            if seen.backtraces.len() < MAX_LOCATIONS {
                seen.backtraces.insert(location.clone(), captured.clone());
            }
            captured
        }
    };
    let thread = std::thread::current();
    let panic = PanicMessage {
        message: payload_text(info),
        location,
        thread: thread
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{:?}", thread.id())),
        backtrace,
    };

    let Some(client) = crate::client::wait(WAIT_FOR_HELPER) else {
        if first_here {
            crate::fallback::write_panic(&panic);
        }
        return;
    };
    // The dump drops the connection it is asked on, so the next one is open
    // before the request goes out.
    let next = (first_here && seen.dumps < MAX_DUMPS)
        .then(crate::client::open_next)
        .flatten();
    let (kind, payload) = Message::Panic(panic.clone(), next.is_some()).encode();
    if client.send_message(kind, payload).is_err() {
        // The helper is gone; the report is the app's to write.
        if first_here {
            crate::fallback::write_panic(&panic);
        }
        return;
    }
    if let Some(next) = next {
        seen.dumps += 1;
        crate::native::dump_now();
        crate::client::replace(next);
    }
}

/// A backtrace without its first frames: the hook's own and the standard
/// library's panic machinery, which every panic shares and nobody reading
/// a report needs. A backtrace that does not read as expected is kept whole.
fn from_the_panic(backtrace: &str) -> String {
    const MACHINERY: &[&str] = &[
        "lumen_os_crash::hook",
        "on_panic",
        "core::ops::function::Fn",
        "std::panicking::",
        "std::sys::backtrace::",
        "rust_begin_unwind",
        "__rustc::rust_begin_unwind",
        "core::panicking::",
        "core::result::unwrap_failed",
        "core::option::unwrap_failed",
        "core::option::expect_failed",
    ];
    let lines: Vec<&str> = backtrace.lines().collect();
    // A frame starts with its number: `  12: name`.
    let is_frame = |line: &str| {
        let t = line.trim_start();
        t.split_once(": ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut start = None;
    for (i, line) in lines.iter().enumerate() {
        if !is_frame(line) {
            continue;
        }
        let name = line.trim_start().split_once(": ").map_or("", |(_, n)| n);
        let shared = MACHINERY.iter().any(|m| name.starts_with(m))
            || name.starts_with("<alloc::boxed::Box<dyn");
        if !shared {
            start = Some(i);
            break;
        }
    }
    match start {
        Some(i) if i > 0 => lines[i..].join("\n"),
        _ => backtrace.to_string(),
    }
}

/// The panic's message: its `&str` or `String` payload.
fn payload_text(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with a payload that is not text".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panic_machinery_is_cut_from_the_top_of_a_backtrace() {
        let bt = "   0: on_panic\n             at hook.rs:69:28\n   1: lumen_os_crash::hook::install::{closure#0}\n   2: std::panicking::panic_with_hook\n   3: __rustc::rust_begin_unwind\n   4: core::panicking::panic_fmt\n   5: app::main\n             at src/main.rs:3:5\n   6: std::rt::lang_start\n";
        assert_eq!(
            from_the_panic(bt),
            "   5: app::main\n             at src/main.rs:3:5\n   6: std::rt::lang_start"
        );
        assert_eq!(from_the_panic("disabled backtrace"), "disabled backtrace");
    }
}
