//! Worker processes sharing one listening socket.
//!
//! A process renders one page at a time, so a server that answers more at
//! once runs more processes. The supervisor binds the port, then starts its
//! own executable again as each worker, handing it the listening socket as
//! an inherited descriptor. Every worker accepts from that one socket, and
//! the supervisor keeps it open for as long as it runs: a worker that exits,
//! whether it retired after its renders or a render never came back, is
//! replaced, and a connection that arrives meanwhile waits in the socket's
//! backlog rather than being refused.
//!
//! On SIGTERM or SIGINT the supervisor passes SIGTERM to every worker, waits
//! for them to finish what they are answering, and exits.

use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cli::{EXIT_WEDGED, bind, load_site};
use crate::config::Config;
use crate::log::Log;
use crate::server::Shutdown;

/// The variable a worker finds its listening descriptor in. The supervisor
/// sets it; nothing else should.
const LISTEN_FD: &str = "LUMEN_SERVER_LISTEN_FD";

/// A worker that exits sooner than this after it started is restarting too
/// fast to be doing any good, so the next start waits.
const QUICK_EXIT: Duration = Duration::from_secs(2);

/// The longest a restart waits after a worker keeps exiting at once.
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// The listener a supervisor handed this process, when it is a worker.
pub(crate) fn inherited_listener() -> Option<TcpListener> {
    let fd: RawFd = std::env::var(LISTEN_FD).ok()?.parse().ok()?;
    // SAFETY: the supervisor put a listening socket at this descriptor and
    // cleared its close-on-exec flag for this exec, and nothing else in this
    // process has taken ownership of it: the variable is read once, here.
    let listener = unsafe { TcpListener::from_raw_fd(fd) };
    // Not a socket after all: give the descriptor back rather than closing
    // something this process does not own.
    if listener.local_addr().is_err() {
        std::mem::forget(listener);
        return None;
    }
    // SAFETY: removing a variable is unsound only while another thread reads
    // the environment, and none has been started yet.
    unsafe { std::env::remove_var(LISTEN_FD) };
    Some(listener)
}

/// Stop the worker if its supervisor goes away, so a killed supervisor does
/// not leave workers answering on a port nobody manages.
pub(crate) fn watch_parent(stop: Shutdown) {
    // SAFETY: getppid has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new()
        .name("lumen-server-parent".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // SAFETY: as above.
                if unsafe { libc::getppid() } != parent {
                    stop.shutdown();
                    return;
                }
            }
        });
}

/// One worker place: the process in it, and when it started.
struct Place {
    child: Option<Child>,
    started: Instant,
    /// When the next start may happen, after a quick exit.
    wait_until: Option<Instant>,
    backoff: Duration,
}

/// Bind, start the workers, keep them running, and stop them on a signal.
pub(crate) fn run(config: &Config, log: &Arc<Log>) -> ExitCode {
    // A site that will not load fails here, once, rather than in every
    // worker in a loop.
    let base = match config.site.as_deref().map(load_site) {
        Some(Ok((_, base))) => base,
        Some(Err(message)) => {
            log.error(&message);
            return ExitCode::FAILURE;
        }
        None => {
            log.error("no site directory named");
            return ExitCode::from(2);
        }
    };
    let listener = match bind(config) {
        Ok(listener) => listener,
        Err(message) => {
            log.error(&message);
            return ExitCode::FAILURE;
        }
    };
    let fd = listener.as_raw_fd();
    if let Err(message) = inheritable(fd) {
        log.error(&message);
        return ExitCode::FAILURE;
    }
    let stopping = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if let Err(e) = signal_hook::flag::register(signal, Arc::clone(&stopping)) {
            log.error(&format!("cannot watch for signal {signal}: {e}"));
            return ExitCode::FAILURE;
        }
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log.error(&format!(
                "cannot find this executable to start workers: {e}"
            ));
            return ExitCode::FAILURE;
        }
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let start = |log: &Log| -> Option<Child> {
        match Command::new(&exe)
            .args(&args)
            .env(LISTEN_FD, fd.to_string())
            .spawn()
        {
            Ok(child) => Some(child),
            Err(e) => {
                log.error(&format!("cannot start a worker: {e}"));
                None
            }
        }
    };

    let addr = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_default();
    log.info(&format!(
        "listening on http://{addr}{base} with {} worker{}",
        config.workers,
        if config.workers == 1 { "" } else { "s" }
    ));
    let mut places: Vec<Place> = (0..config.workers)
        .map(|_| Place {
            child: start(log),
            started: Instant::now(),
            wait_until: None,
            backoff: Duration::from_millis(250),
        })
        .collect();

    while !stopping.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
        for place in &mut places {
            if let Some(child) = &mut place.child
                && let Ok(Some(status)) = child.try_wait()
            {
                let pid = child.id();
                place.child = None;
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                log.info(&format!(
                    "worker {pid} {}; starting another",
                    describe(status)
                ));
                // A worker that retired, or that a runaway render ended,
                // did its job; one that exits at once for any other reason
                // is failing to start, and starting it again at once only
                // spins.
                let expected = matches!(status.code(), Some(0 | EXIT_WEDGED));
                if !expected && place.started.elapsed() < QUICK_EXIT {
                    place.wait_until = Some(Instant::now() + place.backoff);
                    place.backoff = (place.backoff * 2).min(MAX_BACKOFF);
                } else {
                    place.backoff = Duration::from_millis(250);
                }
            }
            if place.child.is_none()
                && place.wait_until.is_none_or(|until| Instant::now() >= until)
                && !stopping.load(Ordering::SeqCst)
            {
                place.child = start(log);
                place.started = Instant::now();
                place.wait_until = None;
            }
        }
    }

    log.info("stopping: letting the workers finish what they are answering");
    for place in &places {
        if let Some(child) = &place.child {
            // SAFETY: kill with a pid this process started and has not yet
            // reaped, so the pid still names that child.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
    }
    let deadline = Instant::now() + config.limits.shutdown_grace + Duration::from_secs(5);
    loop {
        let mut running = 0;
        for place in &mut places {
            if let Some(child) = &mut place.child {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => place.child = None,
                    Ok(None) => running += 1,
                }
            }
        }
        if running == 0 {
            break;
        }
        if Instant::now() >= deadline {
            log.warn("workers still running past the shutdown grace; killing them");
            for place in &mut places {
                if let Some(child) = &mut place.child {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(listener);
    log.info("stopped");
    ExitCode::SUCCESS
}

/// Let `fd` survive into the workers this process starts.
fn inheritable(fd: RawFd) -> Result<(), String> {
    // SAFETY: fcntl on a descriptor this process owns, reading and then
    // writing its descriptor flags; no memory is passed.
    let cleared = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) >= 0
    };
    if cleared {
        Ok(())
    } else {
        Err(format!(
            "cannot hand the listening socket to workers: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// Why a worker exited, in words.
fn describe(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(0), _) => "exited".to_string(),
        (Some(EXIT_WEDGED), _) => "exited after a render ran past its time limit".to_string(),
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "ended".to_string(),
    }
}
