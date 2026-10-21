//! `lumen-crash`: the crash helper a Lumen app starts beside itself.
//!
//! The app starts this process when it starts, connects to it, and tells it
//! who it is, which GPU it draws on, its recent log lines, and any panic it
//! hits. When the app faults, it asks the helper for a minidump; the helper
//! writes the dump from outside, so a crash that wrecked the app's own memory
//! still leaves a report. The helper then writes the readable `.txt` beside
//! the dump, and, once the app is gone, shows a dialog naming the report.
//!
//! A panic is not a crash until the app dies of it: Lumen survives panics in
//! scripts and modules by design. So the helper keeps a panic it is told
//! about, with its dump, until the app either says it is ending normally
//! (the panic was survived; the dump is deleted) or disappears without
//! saying so (the panic ended it; the report is written).
//!
//! Usage: `lumen-crash --socket <name> --pid <app pid>`. The app passes both;
//! nobody runs the helper by hand.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod dialog;
mod summary;
mod watch;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lumen_crash_report::protocol::{Hello, Message, PanicMessage};
use lumen_crash_report::{LOG_LINES, Report};
use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler, SocketName};

/// How long the helper waits for its app to connect before it gives up: an
/// app that died before connecting, or never meant to.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How many panics after the first a report lists.
const MAX_LATER_PANICS: usize = 16;

fn main() -> ExitCode {
    let Some(args) = Args::parse(std::env::args().skip(1)) else {
        eprintln!("usage: lumen-crash --socket <name> --pid <pid>");
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lumen-crash: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The command line.
struct Args {
    socket: String,
    pid: u32,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Option<Self> {
        let (mut socket, mut pid) = (None, None);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => socket = args.next(),
                "--pid" => pid = Some(args.next()?.parse().ok()?),
                _ => return None,
            }
        }
        Some(Self {
            socket: socket?,
            pid: pid?,
        })
    }
}

fn run(args: &Args) -> Result<(), minidumper::Error> {
    let mut server = Server::with_name(socket_name(&args.socket))?;
    let state = Arc::new(Mutex::new(State::default()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let connected = Arc::new(AtomicBool::new(false));
    watch::spawn(
        args.pid,
        shutdown.clone(),
        connected.clone(),
        CONNECT_TIMEOUT,
    );
    server.run(
        Box::new(Handler {
            state: state.clone(),
            connected,
        }),
        &shutdown,
        None,
    )?;
    // The loop is over: the app said goodbye, faulted, or went away. Settle
    // whatever is still open, then tell the user, now that the app is gone
    // and the dialog cannot hold it up.
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if state.finished.is_none() {
        state.finish_on_exit();
    }
    if let (Some(report), Some(hello)) = (state.finished.take(), state.hello.clone())
        && hello.dialog
    {
        drop(state);
        dialog::show(&hello.app_name, &report);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn socket_name(name: &str) -> SocketName<'_> {
    SocketName::abstract_namespace(name)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn socket_name(name: &str) -> SocketName<'_> {
    SocketName::Path(Path::new(name))
}

/// What the helper knows about its app.
#[derive(Default)]
struct State {
    hello: Option<Hello>,
    gpu: Option<String>,
    logs: VecDeque<String>,
    /// The first panic since the app last said it was fine, while nobody
    /// knows whether the app survives it. Later panics are often its echo
    /// (a `join().unwrap()` on the thread that panicked), so the first is
    /// the one a report leads with.
    panic: Option<PanicMessage>,
    /// One line for each panic after the first.
    later_panics: Vec<String>,
    /// A panic announced a dump request that has not arrived yet.
    panic_dump_expected: bool,
    /// The dump of the first panic, under its pending name.
    panic_dump: Option<PathBuf>,
    /// Every pending dump written so far, deleted unless one becomes a
    /// report's.
    pending_dumps: Vec<PathBuf>,
    /// The file the dump being written goes to, and whether it is a panic's.
    writing: Option<(PathBuf, bool)>,
    /// The report written, once one is.
    finished: Option<PathBuf>,
}

impl State {
    fn crash_dir(&self) -> PathBuf {
        self.hello
            .as_ref()
            .map(|h| h.crash_dir.clone())
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::temp_dir().join("lumen-crashes"))
    }

    fn report(&self, time: SystemTime) -> Report {
        let mut report = Report::new(self.hello.clone().unwrap_or_default(), time);
        report.os = lumen_crash_report::os::description();
        report.gpu = self.gpu.clone();
        report.panic = self.panic.clone();
        report.later_panics = self.later_panics.clone();
        report.logs = self.logs.iter().cloned().collect();
        report
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::Hello(hello) => self.hello = Some(hello),
            Message::Gpu(line) => self.gpu = Some(line),
            Message::Log(line) => {
                if self.logs.len() == LOG_LINES {
                    self.logs.pop_front();
                }
                self.logs.push_back(line);
            }
            Message::Panic(panic, dump) => {
                self.panic_dump_expected = dump;
                if self.panic.is_none() {
                    self.panic = Some(panic);
                } else if self.later_panics.len() < MAX_LATER_PANICS {
                    self.later_panics.push(one_line(&panic));
                }
            }
            // Everything reported so far was survived. The app may run on
            // (a host that runs a second app); a later panic starts over.
            Message::Exit => {
                self.panic = None;
                self.later_panics.clear();
                self.panic_dump_expected = false;
                self.discard_pending(None);
            }
        }
    }

    /// The app went away. A panic it never said it survived is what ended
    /// it; anything else (a normal exit, a kill) leaves no report.
    fn finish_on_exit(&mut self) {
        if self.panic.is_none() {
            self.discard_pending(None);
            return;
        }
        let dir = self.crash_dir();
        let time = SystemTime::now();
        let stem = lumen_crash_report::free_stem(&dir, time);
        let mut report = self.report(time);
        if let Some(dump) = self.panic_dump.take() {
            let target = dir.join(format!("{stem}.dmp"));
            if std::fs::rename(&dump, &target).is_ok() {
                report.minidump = Some(file_name(&target));
            }
        }
        self.discard_pending(None);
        self.finished = report.write(&dir, &stem).ok();
    }

    /// The app faulted and its dump is at `dump` (or failed to write).
    fn finish_on_fault(&mut self, dump: &Path, contents: Option<&[u8]>) {
        let dir = self.crash_dir();
        let time = SystemTime::now();
        let stem = dump
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| lumen_crash_report::free_stem(&dir, time));
        let mut report = self.report(time);
        let bytes = contents
            .map(<[u8]>::to_vec)
            .or_else(|| std::fs::read(dump).ok());
        if let Some(bytes) = bytes {
            report.fault = summary::fault(&bytes);
            report.minidump = Some(file_name(dump));
        }
        if report.fault.is_none() {
            report.fault = Some(lumen_crash_report::Fault {
                description: "unknown (the minidump could not be read)".into(),
                ..Default::default()
            });
        }
        self.discard_pending(Some(dump));
        self.finished = report.write(&dir, &stem).ok();
    }

    fn discard_pending(&mut self, keep: Option<&Path>) {
        for dump in self.pending_dumps.drain(..) {
            if Some(dump.as_path()) != keep {
                let _ = std::fs::remove_file(dump);
            }
        }
        self.panic_dump = None;
    }
}

/// `message (at location, thread name)` for a report's list of later
/// panics.
fn one_line(panic: &PanicMessage) -> String {
    let message = panic.message.lines().next().unwrap_or_default();
    match (panic.location.is_empty(), panic.thread.is_empty()) {
        (false, false) => format!("{message} (at {}, thread {})", panic.location, panic.thread),
        (false, true) => format!("{message} (at {})", panic.location),
        _ => message.to_string(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

struct Handler {
    state: Arc<Mutex<State>>,
    connected: Arc<AtomicBool>,
}

impl Handler {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ServerHandler for Handler {
    fn create_minidump_file(&self) -> Result<(std::fs::File, PathBuf), std::io::Error> {
        let mut state = self.state();
        let dir = state.crash_dir();
        std::fs::create_dir_all(&dir)?;
        let for_panic = std::mem::take(&mut state.panic_dump_expected);
        let path = if for_panic {
            // Pending until the app's fate is known: the name keeps it out of
            // the way of the reports, and a later report renames it.
            let n = state.pending_dumps.len() + 1;
            let pid = state.hello.as_ref().map_or(0, |h| h.pid);
            dir.join(format!(".pending-{pid}-{n}.dmp"))
        } else {
            let stem = lumen_crash_report::free_stem(&dir, SystemTime::now());
            dir.join(format!("{stem}.dmp"))
        };
        let file = std::fs::File::create(&path)?;
        state.writing = Some((path.clone(), for_panic));
        Ok((file, path))
    }

    fn on_minidump_created(&self, result: Result<MinidumpBinary, minidumper::Error>) -> LoopAction {
        let mut state = self.state();
        let Some((path, for_panic)) = state.writing.take() else {
            return LoopAction::Continue;
        };
        let written = result.ok();
        if let Some(binary) = &written {
            let _ = binary.file.sync_all();
        }
        if for_panic {
            if written.is_some() {
                state.pending_dumps.push(path.clone());
                // The first panic's dump is the report's; a later one is
                // kept only while the first has none.
                state.panic_dump.get_or_insert(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
            return LoopAction::Continue;
        }
        let contents = written.as_ref().and_then(|b| b.contents.as_deref());
        if written.is_none() {
            let _ = std::fs::remove_file(&path);
        }
        state.finish_on_fault(&path, contents);
        LoopAction::Exit
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        if let Some(message) = Message::decode(kind, &buffer) {
            self.state().apply(message);
        }
    }

    fn on_client_connected(&self, _num_clients: usize) -> LoopAction {
        self.connected.store(true, Ordering::Relaxed);
        LoopAction::Continue
    }

    fn on_client_disconnected(&self, num_clients: usize) -> LoopAction {
        // A panic's dump drops the connection it came on; the app opened its
        // next one before asking, so the count only reaches zero when the
        // app is gone.
        //
        // Unless the app never said who it is: a connection that closes
        // before its hello is an attempt that failed, not the app leaving.
        // On macOS the socket listens a moment before the Mach port the
        // dumps travel on is registered, and an app that connects in that
        // moment finds no port, closes, and tries again. The watch thread
        // ends the loop if the app dies before it gets through.
        if num_clients == 0 && self.state().hello.is_some() {
            LoopAction::Exit
        } else {
            LoopAction::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(dir: &Path) -> Hello {
        Hello {
            app_name: "demo".into(),
            app_id: "demo".into(),
            crash_dir: dir.to_path_buf(),
            pid: 1,
            ..Hello::default()
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn reports(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|d| d.flatten().map(|e| file_name(&e.path())).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn the_command_line_names_the_socket_and_the_pid() {
        let args = |a: &[&str]| Args::parse(a.iter().map(|s| s.to_string()));
        let parsed = args(&["--socket", "s", "--pid", "12"]).unwrap();
        assert_eq!((parsed.socket.as_str(), parsed.pid), ("s", 12));
        assert!(args(&["--socket", "s"]).is_none());
        assert!(args(&["--pid", "x", "--socket", "s"]).is_none());
        assert!(args(&["--other"]).is_none());
    }

    #[test]
    fn a_survived_panic_leaves_nothing_behind() {
        let dir = temp("survived");
        let mut state = State::default();
        state.apply(Message::Hello(hello(&dir)));
        state.apply(Message::Panic(PanicMessage::default(), true));
        std::fs::create_dir_all(&dir).unwrap();
        let dump = dir.join(".pending-1-1.dmp");
        std::fs::write(&dump, b"MDMP").unwrap();
        state.pending_dumps.push(dump.clone());
        state.panic_dump = Some(dump);
        state.apply(Message::Exit);
        state.finish_on_exit();
        assert!(state.finished.is_none());
        assert!(reports(&dir).is_empty(), "{:?}", reports(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_panic_the_app_died_of_becomes_a_report_with_its_dump() {
        let dir = temp("died");
        let mut state = State::default();
        state.apply(Message::Hello(hello(&dir)));
        state.apply(Message::Log("lumen: before".into()));
        state.apply(Message::Panic(
            PanicMessage {
                message: "boom".into(),
                ..PanicMessage::default()
            },
            true,
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dump = dir.join(".pending-1-1.dmp");
        std::fs::write(&dump, b"MDMP").unwrap();
        state.pending_dumps.push(dump.clone());
        state.panic_dump = Some(dump);
        state.finish_on_exit();
        let names = reports(&dir);
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].ends_with(".dmp") && names[1].ends_with(".txt"));
        let text = std::fs::read_to_string(state.finished.as_ref().unwrap()).unwrap();
        assert!(text.contains("demo stopped because of a panic."));
        assert!(text.contains("boom"));
        assert!(text.contains("lumen: before"));
        assert!(text.contains(&format!("Minidump:   {}", names[0])));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_kill_without_a_panic_writes_no_report() {
        let dir = temp("killed");
        let mut state = State::default();
        state.apply(Message::Hello(hello(&dir)));
        state.finish_on_exit();
        assert!(state.finished.is_none());
        assert!(!dir.exists());
    }

    #[test]
    fn the_log_keeps_the_newest_lines() {
        let mut state = State::default();
        for i in 0..LOG_LINES + 5 {
            state.apply(Message::Log(format!("line {i}")));
        }
        assert_eq!(state.logs.len(), LOG_LINES);
        assert_eq!(state.logs.front().map(String::as_str), Some("line 5"));
    }
}
