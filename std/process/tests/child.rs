//! The process half on its own: line splitting, exit codes, and one real
//! child supervised end to end.
//!
//! What these prove, once per concern:
//!
//! - a stream becomes lines the way the module documents: the newline is not
//!   part of the line, a last stretch without one is still a line, bytes that
//!   are not utf-8 are replaced, and a line past the cap arrives in pieces
//!   that reassemble into what was written;
//! - an exit is the program's own code, or 128 plus the signal that ended it;
//! - a supervised child delivers every line before its exit, and is waited on
//!   rather than left as a zombie;
//! - a program that cannot start is a refusal with the reason in it, and no
//!   events at all.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumen_process::child::{self, Event, LINE_CAP};

/// Every line `read_lines` produced from `input`.
fn lines(input: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    child::read_lines(input, |line| out.push(line));
    out
}

/// The events one child produced, in the order they arrived, rendered as
/// `out:`, `err:`, and `exit:` prefixed strings.
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn sink(&self) -> child::Emit {
        let entries = Arc::clone(&self.0);
        Arc::new(move |event| {
            let entry = match event {
                Event::Stdout(line) => format!("out:{line}"),
                Event::Stderr(line) => format!("err:{line}"),
                Event::Exit(code) => format!("exit:{code}"),
            };
            entries.lock().expect("log").push(entry);
        })
    }

    /// Wait for the exit entry and answer everything that arrived, or panic
    /// when the child never ended.
    fn drained(&self) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let entries = self.0.lock().expect("log").clone();
            if entries.iter().any(|e| e.starts_with("exit:")) {
                return entries;
            }
            assert!(
                Instant::now() < deadline,
                "the child never reported an exit; got {entries:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The test program, as the absolute path a test starts it by.
fn test_child() -> String {
    env!("CARGO_BIN_EXE_lumen-process-test-child").to_string()
}

/// Run the test program with `args` and answer its events.
fn run(args: &[&str]) -> Vec<String> {
    let log = Log::new();
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let pid = child::start(&test_child(), &args, "case", log.sink()).expect("the child starts");
    let entries = log.drained();
    assert_reaped(pid);
    entries
}

/// The child is gone from the process table, rather than waiting there as a
/// zombie for a parent that never asked how it ended.
#[cfg(target_os = "linux")]
fn assert_reaped(pid: u32) {
    let entry = std::path::PathBuf::from(format!("/proc/{pid}"));
    assert!(
        !entry.exists(),
        "process {pid} is still in the process table after its exit was reported"
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_reaped(_pid: u32) {}

/// A newline ends a line and is not part of it, and the stretch before end of
/// file is a line of its own even without one.
#[test]
fn a_stream_becomes_the_lines_that_were_written() {
    assert_eq!(lines(b"one\ntwo\n"), vec!["one", "two"]);
    assert_eq!(lines(b"one\ntwo"), vec!["one", "two"]);
    assert_eq!(lines(b""), Vec::<String>::new());
    assert_eq!(lines(b"\n"), vec![""]);
    assert_eq!(lines(b"a\n\nb\n"), vec!["a", "", "b"]);
}

/// Bytes that are not utf-8 are replaced rather than dropped, so a line of
/// binary output still arrives as a line.
#[test]
fn invalid_utf8_is_replaced() {
    assert_eq!(lines(b"ab\xffcd\n"), vec!["ab\u{fffd}cd"]);
}

/// A line longer than the cap is handed over in pieces, and the pieces put
/// back together are exactly what the program wrote.
#[test]
fn a_line_past_the_cap_arrives_in_pieces() {
    let written = "a".repeat(LINE_CAP * 2 + 5);
    let mut input = written.clone().into_bytes();
    input.push(b'\n');

    let pieces = lines(&input);
    assert_eq!(pieces.len(), 3, "a line of two caps and a tail is 3 pieces");
    assert_eq!(pieces[0].len(), LINE_CAP);
    assert_eq!(pieces[1].len(), LINE_CAP);
    assert_eq!(pieces[2].len(), 5);
    assert_eq!(pieces.concat(), written);
}

/// A piece boundary that would fall inside a character moves back to the
/// start of it, so neither piece carries half a character.
#[test]
fn a_piece_ends_on_a_character_boundary() {
    // The two-byte character straddles the cap: one byte before it, one after.
    let written = format!("{}\u{e9}{}", "a".repeat(LINE_CAP - 1), "b".repeat(16));
    let mut input = written.clone().into_bytes();
    input.push(b'\n');

    let pieces = lines(&input);
    assert_eq!(pieces.len(), 2);
    assert!(
        pieces[0].chars().all(|c| c == 'a'),
        "the first piece stops before the character that would be cut"
    );
    assert!(pieces[1].starts_with('\u{e9}'));
    assert_eq!(pieces.concat(), written);
    assert!(
        !pieces.concat().contains('\u{fffd}'),
        "nothing was replaced: the split avoided the character"
    );
}

/// A program's own exit code is what a script reads.
#[test]
fn an_exit_code_is_the_program_s_own() {
    assert_eq!(run(&["0"]).last().map(String::as_str), Some("exit:0"));
    assert_eq!(run(&["3"]).last().map(String::as_str), Some("exit:3"));
}

/// A child killed by a signal reports 128 plus the signal, the shell's
/// convention, so a script tells "ended badly" from "ended with status 9".
#[cfg(unix)]
#[test]
fn a_signal_reports_as_128_plus_the_signal() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    assert_eq!(child::exit_code(&ExitStatus::from_raw(9)), 137);
    assert_eq!(child::exit_code(&ExitStatus::from_raw(15)), 143);
    // The other half of the raw form: an ordinary exit, code in the high byte.
    assert_eq!(child::exit_code(&ExitStatus::from_raw(2 << 8)), 2);
}

/// Every argument reaches the program, in order, and its stderr is delivered
/// as its own kind of line.
#[test]
fn the_argument_list_and_both_pipes_reach_the_caller() {
    let entries = run(&["0", "--flag", "value with spaces"]);

    assert_eq!(
        entries
            .iter()
            .filter(|e| e.starts_with("out:"))
            .collect::<Vec<_>>(),
        vec!["out:0", "out:--flag", "out:value with spaces"]
    );
    assert!(entries.contains(&"err:child stderr".to_string()));
}

/// The exit is last: a chatty child's every line is delivered before the
/// handler that says it ended.
#[test]
fn every_line_arrives_before_the_exit() {
    let entries = run(&["0", "--lines", "500"]);

    let exit = entries
        .iter()
        .position(|e| e.starts_with("exit:"))
        .expect("an exit arrived");
    assert_eq!(exit, entries.len() - 1, "the exit is the last event");
    assert_eq!(
        entries.iter().filter(|e| e.starts_with("out:")).count(),
        503,
        "three echoed arguments and 500 flooded lines"
    );
    assert_eq!(entries[exit], "exit:0");
}

/// A program that is not there is a refusal naming it, and nothing is
/// reported under the tag, because the tag never named a running program.
#[test]
fn a_program_that_cannot_start_is_a_refusal() {
    let log = Log::new();
    let outcome = child::start("no-such-program-8f2c", &[], "case", log.sink());

    let message = outcome.expect_err("a missing program cannot start");
    assert!(
        message.contains("no-such-program-8f2c"),
        "the refusal names the program: {message}"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        log.0.lock().expect("log").is_empty(),
        "a start that failed reports no events"
    );
}
