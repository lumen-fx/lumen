//! The process work behind `process::start`: starting a child, turning its
//! two pipes into lines, and reporting how it ended.
//!
//! Everything here answers with an [`Event`] handed to a caller-supplied sink,
//! so the plugin decides where an event goes and this half stays testable
//! without an app around it.
//!
//! Three rules run through all of it:
//!
//! - **A child is supervised, not awaited.** Starting one returns as soon as
//!   the program is running; the output that follows arrives over the sink,
//!   for as long as the child lives.
//! - **Exit is last.** The supervisor joins both readers before it waits on
//!   the child, so every line a child wrote is delivered before its
//!   [`Event::Exit`].
//! - **Every child is waited on exactly once.** Only one side ever takes the
//!   handle, so a finished child is reaped rather than left as a zombie.

use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use lumen_module::lumen_core::app_paths;

/// How much of one line is delivered at a time. A child that writes more than
/// this without a newline has its line handed over in pieces, so a program
/// emitting an unbroken stream cannot grow a buffer without bound.
pub const LINE_CAP: usize = 64 * 1024;

/// Something one child produced.
pub enum Event {
    /// One line the child wrote to stdout, without its newline.
    Stdout(String),
    /// One line the child wrote to stderr, without its newline.
    Stderr(String),
    /// The child ended. Always the last event for a child.
    Exit(i64),
}

/// Where a child's events go. Shared by the supervisor and both reader
/// threads, so it is called from any of them, in any order.
pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// What starting a child could not do: the line an author reads on stderr,
/// without the `lumen-process: ` prefix.
pub type Refusal = String;

/// Start `cmd` with `args` and supervise it, reporting to `emit` under
/// `tag`. Answers the child's process id, or the refusal to report.
///
/// The child runs in the app directory, reads end-of-file from stdin, and has
/// both output pipes captured. A `cmd` carrying a path separator names a
/// program relative to the app; a bare `cmd` is looked up on `PATH`.
pub fn start(cmd: &str, args: &[String], tag: &str, emit: Emit) -> Result<u32, Refusal> {
    let child = Command::new(program(cmd))
        .args(args)
        .current_dir(app_paths::app_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start({cmd}): {e}"))?;
    supervise(tag, child, emit)
}

/// The program a `cmd` names: a path the app ships when it carries a
/// separator, and a `PATH` lookup when it does not.
fn program(cmd: &str) -> PathBuf {
    if cmd.contains('/') || (cfg!(windows) && cmd.contains('\\')) {
        app_paths::resolve(cmd)
    } else {
        PathBuf::from(cmd)
    }
}

/// Take over an already-started `child`: read both pipes into lines, wait for
/// it, and report through `emit`. Answers the child's process id.
///
/// One thread per child owns the handle and two more read the pipes, rather
/// than a pooled task per read: a child lives for as long as it likes, and a
/// pool sized for bounded work would be held by the first program that waits
/// for input.
pub fn supervise(tag: &str, mut child: Child, emit: Emit) -> Result<u32, Refusal> {
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let name = format!("lumen-process-{tag}");
    // The handle sits in a cell both sides can reach, so it is still there
    // when the supervisor thread cannot start: a child nothing waits on
    // becomes a zombie, and that path ends it instead.
    let slot = Arc::new(Mutex::new(Some(child)));
    let body = {
        let name = name.clone();
        let emit = Arc::clone(&emit);
        let slot = Arc::clone(&slot);
        move || {
            let out = stdout.and_then(|pipe| reader(&name, "out", pipe, &emit, Event::Stdout));
            let err = stderr.and_then(|pipe| reader(&name, "err", pipe, &emit, Event::Stderr));
            for handle in [out, err].into_iter().flatten() {
                let _ = handle.join();
            }
            let code = take(&slot).map_or(-1, |mut child| {
                child.wait().map_or(-1, |status| exit_code(&status))
            });
            emit(Event::Exit(code));
        }
    };
    match thread::Builder::new().name(name).spawn(body) {
        Ok(_) => Ok(pid),
        Err(e) => {
            if let Some(mut child) = take(&slot) {
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(format!("supervisor thread for {tag}: {e}"))
        }
    }
}

/// The child handle, for whichever side reaches it first. Taking it is what
/// makes waiting on a child happen exactly once.
fn take(slot: &Mutex<Option<Child>>) -> Option<Child> {
    slot.lock().ok().and_then(|mut held| held.take())
}

/// One pipe reader thread. A thread that cannot start closes the pipe instead,
/// which the child sees as a reader that went away.
fn reader<R>(
    name: &str,
    which: &str,
    pipe: R,
    emit: &Emit,
    wrap: fn(String) -> Event,
) -> Option<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    let emit = Arc::clone(emit);
    let body = move || read_lines(pipe, |line| emit(wrap(line)));
    match thread::Builder::new()
        .name(format!("{name}-{which}"))
        .spawn(body)
    {
        Ok(handle) => Some(handle),
        Err(e) => {
            lumen_module::lumen_core::warn_line!("lumen-process: {name} {which} reader: {e}");
            None
        }
    }
}

/// Split everything `source` produces into lines and hand each one to
/// `on_line`.
///
/// A line is what precedes a newline; the newline itself is not part of it,
/// and the last stretch before end of file is a line even without one. Bytes
/// that are not utf-8 are replaced rather than dropped, and a line longer than
/// [`LINE_CAP`] arrives in pieces, split on a character boundary where the
/// bytes allow one.
pub fn read_lines<R: Read>(source: R, mut on_line: impl FnMut(String)) {
    let mut reader = BufReader::new(source);
    let mut pending: Vec<u8> = Vec::new();
    // Whether the line being assembled has already been handed over in part,
    // which is what tells an empty remainder from an empty line.
    let mut split = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if available.is_empty() {
            break;
        }
        let (take, used) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i, i + 1),
            None => (available.len(), available.len()),
        };
        let complete = take < used;
        pending.extend_from_slice(&available[..take]);
        reader.consume(used);

        while pending.len() >= LINE_CAP {
            let end = piece_end(&pending);
            let rest = pending.split_off(end);
            let piece = std::mem::replace(&mut pending, rest);
            on_line(lossy(&piece));
            split = true;
        }
        if complete {
            if !pending.is_empty() || !split {
                on_line(lossy(&pending));
            }
            pending.clear();
            split = false;
        }
    }
    if !pending.is_empty() {
        on_line(lossy(&pending));
    }
}

/// Where to cut a line that has grown past [`LINE_CAP`]: on a character
/// boundary when the cut would otherwise land inside a character, and on the
/// cap itself for bytes that are not utf-8 at all.
fn piece_end(pending: &[u8]) -> usize {
    match std::str::from_utf8(&pending[..LINE_CAP]) {
        Ok(_) => LINE_CAP,
        Err(e) => {
            let valid = e.valid_up_to();
            // `error_len` of `None` is a character cut short by the cap, and
            // one is at most three bytes from complete.
            if e.error_len().is_none() && valid > 0 && LINE_CAP - valid <= 3 {
                valid
            } else {
                LINE_CAP
            }
        }
    }
}

/// One line's bytes as text, with anything that is not utf-8 replaced.
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The number a finished child reports: its own exit code, `128` plus the
/// signal that killed it on Unix, and `-1` for an ending the system does not
/// describe.
pub fn exit_code(status: &ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return 128 + i64::from(signal);
    }
    -1
}
