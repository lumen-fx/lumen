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

use std::fmt::Arguments;
use std::io::Write;

/// Write a line to stdout, or drop it.
pub fn to_stdout(message: Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{message}");
}

/// Write a line to stderr, or drop it.
pub fn to_stderr(message: Arguments<'_>) {
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
}
