//! The process work behind `process::start` and `process::stop`: starting a
//! child, turning its two pipes into lines, ending it on request, and
//! reporting how it ended.
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
//! - **Exit is last.** The supervisor joins both readers before it collects
//!   the child's status, so every line a child wrote is delivered before its
//!   [`Event::Exit`]. A child ended by [`Running::stop`] reports its exit the
//!   same way.
//! - **Every child is reaped exactly once, and signalled only while it is
//!   unreaped.** The handle sits behind one lock; whoever holds the lock asks
//!   the system whether the child has ended before signalling it, so a signal
//!   never reaches a process id the system has already handed to someone
//!   else.

use std::fmt;
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process};

use lumen_module::lumen_core::app_paths;

/// How much of one line is delivered at a time. A child that writes more than
/// this without a newline has its line handed over in pieces, so a program
/// emitting an unbroken stream cannot grow a buffer without bound.
pub const LINE_CAP: usize = 64 * 1024;

/// How long a child asked to end has before it is killed outright. On Unix
/// [`Running::stop`] sends `SIGTERM` first, which a program may catch to save
/// its state; one still running after this long gets `SIGKILL`.
pub const GRACE: Duration = Duration::from_secs(2);

/// The longest the supervisor sleeps between two looks at a child that has
/// closed its pipes but not yet ended.
const POLL_CAP: Duration = Duration::from_millis(50);

/// How a child is set up beyond its command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The directory the child starts in, resolved against the app directory
    /// when relative. `None` starts it in the app directory.
    pub cwd: Option<String>,
    /// Variables set for the child on top of the environment it inherits,
    /// replacing an inherited variable of the same name.
    pub env: Vec<(String, String)>,
}

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
/// `tag`. Answers the running child, or the refusal to report.
///
/// The child runs in the app directory unless `options` names another,
/// inherits the app's environment with `options.env` laid over it, reads
/// end-of-file from stdin, and has both output pipes captured. A `cmd`
/// carrying a path separator names a program relative to the app; a bare
/// `cmd` is looked up on `PATH`.
pub fn start(
    cmd: &str,
    args: &[String],
    tag: &str,
    options: &Options,
    emit: Emit,
) -> Result<Running, Refusal> {
    let cwd = options
        .cwd
        .as_deref()
        .map_or_else(app_paths::app_dir, app_paths::resolve);
    let child = Command::new(program(cmd))
        .args(args)
        .current_dir(cwd)
        .envs(options.env.iter().map(|(k, v)| (k, v)))
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

/// Take over an already-started `child`: read both pipes into lines, collect
/// its exit, and report through `emit`. Answers the running child.
///
/// One thread per child owns the supervision and two more read the pipes,
/// rather than a pooled task per read: a child lives for as long as it likes,
/// and a pool sized for bounded work would be held by the first program that
/// waits for input.
pub fn supervise(tag: &str, mut child: Child, emit: Emit) -> Result<Running, Refusal> {
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let name = format!("lumen-process-{tag}");
    // The handle sits in a cell the supervisor, a stop, and a failed thread
    // start can all reach. The supervisor empties it once the child's status
    // is collected, so an empty cell is a child that has ended.
    let running = Running {
        pid,
        slot: Arc::new(Mutex::new(Some(child))),
    };
    let body = {
        let name = name.clone();
        let emit = Arc::clone(&emit);
        let running = running.clone();
        move || {
            let out = stdout.and_then(|pipe| reader(&name, "out", pipe, &emit, Event::Stdout));
            let err = stderr.and_then(|pipe| reader(&name, "err", pipe, &emit, Event::Stderr));
            for handle in [out, err].into_iter().flatten() {
                let _ = handle.join();
            }
            emit(Event::Exit(running.collect()));
        }
    };
    match thread::Builder::new().name(name).spawn(body) {
        Ok(_) => Ok(running),
        Err(e) => {
            let held = running.slot.lock().ok().and_then(|mut held| held.take());
            if let Some(mut child) = held {
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(format!("supervisor thread for {tag}: {e}"))
        }
    }
}

/// A started, supervised child: the handle a stop ends it through. Clones
/// share the one child.
#[derive(Clone)]
pub struct Running {
    pid: u32,
    slot: Arc<Mutex<Option<Child>>>,
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Running").field("pid", &self.pid).finish()
    }
}

impl Running {
    /// The child's process id, as the system reported it at start.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the child is still running. A child that has ended but whose
    /// exit has not been reported yet answers false.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.with_live(|_| ()).is_some()
    }

    /// Ask the child to end: `SIGTERM` on Unix, which the program may catch,
    /// and `TerminateProcess` on Windows, which it cannot. Answers false when
    /// the child had already ended.
    pub fn terminate(&self) -> bool {
        self.with_live(|child| {
            #[cfg(unix)]
            {
                let _ = kill_process(Pid::from_child(child), Signal::TERM);
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        })
        .is_some()
    }

    /// End the child outright: `SIGKILL` on Unix, `TerminateProcess` on
    /// Windows. Does nothing to a child that has already ended.
    pub fn kill(&self) {
        let _ = self.with_live(|child| {
            let _ = child.kill();
        });
    }

    /// End the child the way `process::stop` does: [`terminate`](Self::terminate)
    /// it now, and on Unix [`kill`](Self::kill) it after [`GRACE`] if it is
    /// still running then. Returns at once; the child's exit arrives through
    /// the sink as always. Answers false when the child had already ended.
    pub fn stop(&self) -> bool {
        if !self.terminate() {
            return false;
        }
        if cfg!(unix) {
            let running = self.clone();
            let waiter = thread::Builder::new()
                .name(format!("lumen-process-stop-{}", self.pid))
                .spawn(move || {
                    let until = Instant::now() + GRACE;
                    while running.is_running() && Instant::now() < until {
                        thread::sleep(POLL_CAP);
                    }
                    running.kill();
                });
            if waiter.is_err() {
                // No thread to wait out the grace period on: end it now
                // rather than leave a child that ignores SIGTERM running.
                self.kill();
            }
        }
        true
    }

    /// Run `f` on the child while it is unreaped and still running, and
    /// answer what it returned; `None` for a child that has ended. Asking the
    /// system first, under the lock, is what keeps a signal away from a
    /// process id that has been reused.
    fn with_live<T>(&self, f: impl FnOnce(&mut Child) -> T) -> Option<T> {
        let mut held = self.slot.lock().ok()?;
        let child = held.as_mut()?;
        match child.try_wait() {
            Ok(None) => Some(f(child)),
            _ => None,
        }
    }

    /// Wait for the child to end, reap it, and answer its exit code. Only the
    /// supervisor calls this, once both pipes are closed; most children end as
    /// their pipes close, so the first look usually finds the exit.
    ///
    /// It looks rather than blocks so the lock is free between looks: a stop
    /// needs the handle while the child is still running.
    fn collect(&self) -> i64 {
        let mut pause = Duration::from_millis(1);
        loop {
            {
                let Ok(mut held) = self.slot.lock() else {
                    return -1;
                };
                let Some(child) = held.as_mut() else {
                    return -1;
                };
                match child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(status)) => {
                        held.take();
                        return exit_code(&status);
                    }
                    Err(_) => {
                        // A child the system will not report on: kill it and
                        // wait, so it cannot linger as a zombie.
                        return held.take().map_or(-1, |mut child| {
                            let _ = child.kill();
                            child.wait().map_or(-1, |status| exit_code(&status))
                        });
                    }
                }
            }
            thread::sleep(pause);
            pause = (pause * 2).min(POLL_CAP);
        }
    }
}

/// End every child in `children` at once, the way an app's exit does: ask
/// each to [`terminate`](Running::terminate), wait up to [`GRACE`] for all of
/// them, and [`kill`](Running::kill) whatever is still running then. Returns
/// once none of them is running.
pub fn stop_all(children: &[Running]) {
    let asked: Vec<&Running> = children.iter().filter(|c| c.terminate()).collect();
    let until = Instant::now() + GRACE;
    while asked.iter().any(|c| c.is_running()) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    for child in asked {
        child.kill();
    }
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
