//! Crash handling for Lumen apps.
//!
//! A Lumen app that panics or faults leaves a report instead of vanishing.
//! [`install`] sets up three things in the app's process:
//!
//! - a panic hook, chained in front of whatever hook was there, that records
//!   every panic with its backtrace;
//! - a native fault handler (signals on Linux, Mach exceptions and signals
//!   on macOS, structured exceptions on Windows) from EmbarkStudios'
//!   `crash-handler`;
//! - the `lumen-crash` helper, started beside the app and connected over
//!   `minidumper`'s IPC. The helper writes the minidump from outside the
//!   crashed process, writes the readable report beside it, and shows the
//!   dialog. Reports go to `crashes/` in the app's data directory.
//!
//! A panic is only a crash when the app dies of it: Lumen survives panics
//! in scripts and modules by design. The report for a panic is therefore
//! settled when the process ends: an app that ends normally says so
//! ([`report_clean_exit`]), and the panics it survived leave nothing behind.
//!
//! When the helper cannot be found or started, the app writes the `.txt`
//! itself: in full for a panic, and a minimal one for a native fault, from
//! inside the fault handler with nothing but system calls. There is no
//! minidump and no dialog then.
//!
//! The runtime installs all of this for every interactive launch through
//! the `os-crash` capability ([`capability`]); an app turns it off with
//! `[crash] enabled = false` in `lumen.toml`.

#![warn(missing_docs)]

pub mod capability;
mod client;
mod fallback;
mod hook;
mod logs;
mod native;

use std::path::PathBuf;
use std::sync::OnceLock;

pub use lumen_crash_report as report;

/// What [`install`] is set up with.
#[derive(Debug, Clone)]
pub struct Config {
    /// The app's name, as the dialog and the report say it.
    pub app_name: String,
    /// The app's stable id.
    pub app_id: String,
    /// The folder reports go in.
    pub crash_dir: PathBuf,
    /// The helper to start. `None` looks beside the running executable,
    /// then beside the library this code is linked into.
    pub helper: Option<PathBuf>,
    /// Show the dialog after a crash.
    pub dialog: bool,
}

/// Set in the environment, turns the crash dialog off: the report is still
/// written. For test runs and automation, where nobody is there to click.
pub const NO_DIALOG_ENV: &str = "LUMEN_CRASH_NO_DIALOG";

impl Config {
    /// The configuration for the app `app_id`: reports in `crashes/` under
    /// its data directory, the helper looked up, the dialog on unless
    /// [`NO_DIALOG_ENV`] is set to anything but `0`.
    pub fn for_app(app_id: &str, app_name: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            app_id: app_id.to_string(),
            crash_dir: lumen_crash_report::crash_dir(&lumen_core::app_paths::data_dir_for(app_id)),
            helper: None,
            dialog: std::env::var_os(NO_DIALOG_ENV).is_none_or(|v| v.is_empty() || v == "0"),
        }
    }
}

/// How a crash will be reported in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The helper was started; it connects in the background. The path is
    /// the helper's.
    Helper(PathBuf),
    /// No helper: the app writes the `.txt` reports itself. The text says
    /// why.
    InProcess(String),
}

struct Shared {
    config: Config,
    installed: Installed,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Set up crash handling for this process. The first call does the work and
/// later calls return what the first one set up, so a process that runs
/// more than one app keeps the first app's reports.
///
/// Costs the app no waiting: the helper is started and connected on a
/// thread of its own, and a crash in the moments before it answers is
/// written in-process.
pub fn install(config: Config) -> Installed {
    SHARED
        .get_or_init(|| {
            logs::install();
            fallback::prepare(&config);
            hook::install();
            native::install();
            let installed = client::start(&config);
            Shared { config, installed }
        })
        .installed
        .clone()
}

/// What [`install`] set up, `None` before it ran.
pub fn installed() -> Option<Installed> {
    SHARED.get().map(|s| s.installed.clone())
}

fn config() -> Option<&'static Config> {
    SHARED.get().map(|s| &s.config)
}

/// The app is ending normally: every panic reported so far was survived,
/// so none of them becomes a report. The runtime calls this as the app's
/// world is dropped outside a panic; an embedder that drives the app itself
/// calls it before its process exits.
pub fn report_clean_exit() {
    if SHARED.get().is_none() {
        return;
    }
    client::send(lumen_crash_report::protocol::Message::Exit);
    fallback::discard_pending();
}

/// Tell the crash handling which renderer and device the app draws on, for
/// the report's `GPU:` line.
pub fn set_gpu(line: impl Into<String>) {
    let line = line.into();
    if logs::set_gpu(&line) {
        client::send(lumen_crash_report::protocol::Message::Gpu(line));
    }
}

/// Add a line to the log a report quotes. Lines the app prints through
/// `lumen_core`'s output and `tracing` warnings already arrive here.
pub fn log_line(line: impl Into<String>) {
    logs::push(line.into());
}

/// Wait up to `timeout` for the helper to answer; whether it did. Crash
/// handling never needs this (a crash before the helper answers is written
/// in-process); it is for a caller that wants to know the full report is in
/// place, such as a test.
pub fn wait_for_helper(timeout: std::time::Duration) -> bool {
    client::wait(timeout).is_some()
}
