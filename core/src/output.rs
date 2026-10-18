//! Saying things without staking the process on being heard.
//!
//! `println!` and `eprintln!` panic when the write fails, which is the right
//! call for a program whose output is the result and the wrong one for
//! anything that has to keep running. A process whose output goes to a pipe
//! nobody reads any more, or that a supervisor started with its streams
//! closed, hits a broken pipe the next time it says anything; a server dies
//! there, in the middle of answering somebody. A lost line of log is the
//! smaller loss.
//!
//! So diagnostics go out through here: the line is written whenever anything
//! is listening, and dropped when nothing is.
//!
//! A subsystem that wants the lines as well (to keep the last few for a
//! report, say) adds a listener with [`add_listener`]; every line is handed
//! to each listener whether or not the stream took it.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Something every line said through this module is also handed to.
pub type Listener = fn(&str);

static LISTENERS: RwLock<Vec<Listener>> = RwLock::new(Vec::new());
/// Whether any listener is registered, so a process with none formats no
/// line twice.
static LISTENING: AtomicBool = AtomicBool::new(false);

/// Hand every later line to `listener` too, once per line, without its
/// newline. A listener runs on the thread that said the line and must not
/// say a line itself.
pub fn add_listener(listener: Listener) {
    let mut listeners = LISTENERS.write().unwrap_or_else(|e| e.into_inner());
    listeners.push(listener);
    LISTENING.store(true, Ordering::Release);
}

fn tell(message: &Arguments<'_>) {
    if !LISTENING.load(Ordering::Acquire) {
        return;
    }
    let line = message.to_string();
    let listeners = LISTENERS.read().unwrap_or_else(|e| e.into_inner());
    for listener in listeners.iter() {
        listener(&line);
    }
}

/// Write a line to stdout, or drop it.
pub fn to_stdout(message: Arguments<'_>) {
    tell(&message);
    let _ = writeln!(std::io::stdout(), "{message}");
}

/// Write a line to stderr, or drop it.
pub fn to_stderr(message: Arguments<'_>) {
    tell(&message);
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// Tell the person running this something. Reads like `println!`, and cannot
/// end the process.
#[macro_export]
macro_rules! say_line {
    ($($arg:tt)*) => { $crate::output::to_stdout(format_args!($($arg)*)) };
}

/// Warn the person running this about something. Reads like `eprintln!`, and
/// cannot end the process.
#[macro_export]
macro_rules! warn_line {
    ($($arg:tt)*) => { $crate::output::to_stderr(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the write does, the caller carries on. A stream that is gone
    /// cannot be arranged from inside the process that owns it, so the test
    /// that serves through a closed pipe is the one that proves the rest.
    #[test]
    fn a_line_goes_out_without_the_caller_having_to_care() {
        to_stdout(format_args!("lumen: {} {}", "a", 1));
        to_stderr(format_args!("lumen: {} {}", "b", 2));
        say_line!("lumen: {}", "said");
        warn_line!("lumen: {}", "warned");
    }

    static HEARD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    fn hear(line: &str) {
        HEARD.lock().unwrap().push(line.to_string());
    }

    #[test]
    fn a_listener_hears_every_line_after_it_was_added() {
        add_listener(hear);
        say_line!("lumen: heard {}", 1);
        warn_line!("lumen: heard {}", 2);
        let heard = HEARD.lock().unwrap();
        assert!(heard.iter().any(|l| l == "lumen: heard 1"), "{heard:?}");
        assert!(heard.iter().any(|l| l == "lumen: heard 2"), "{heard:?}");
    }
}
