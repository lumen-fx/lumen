//! App-lifecycle host for Lumen.
//!
//! Three independent surfaces (per audit section 416-447):
//!
//! - [`LifecycleService::ensure_single_instance`] - Linux and macOS bind a Unix domain socket at
//!   `<app_id>.sock` under a private, per-user directory resolved per platform (`$XDG_RUNTIME_DIR` on
//!   Linux, `confstr(_CS_DARWIN_USER_TEMP_DIR)` on macOS - both verified by ownership and mode, not
//!   assumed), locked to `0600` right after bind so another local user cannot connect and feed argv
//!   into a running app. When no such directory can be resolved and verified, the lock fails closed
//!   (no lock, runs as primary) rather than falling back to a world-writable temp directory a local
//!   attacker could squat ahead of the real launch. Windows uses a named pipe at `\\.\pipe\<app_id>`,
//!   claimed with `FILE_FLAG_FIRST_PIPE_INSTANCE` so the create fails once the name exists, and
//!   carrying a security descriptor that grants the calling user and nobody else. The second
//!   launch connects, sends its argv as length-prefixed JSON, and exits. The primary spawns a recv thread
//!   that reads incoming args and pushes them into [`LifecycleService::take_secondary_args`], which
//!   [`poll_second_instance`] drains every tick into `lumen_core::input::SecondInstanceLaunched` ECS
//!   messages, reaching a script as `on_second_instance(args)`. The runtime gates the check itself
//!   behind `lumen.toml [app] single_instance`; this crate carries no config surface of its own.
//!   Mirrors `GApplication`'s "unique by default" behaviour and Qt's `QSingleInstance` pattern.
//! - [`AutostartService`] - writes a `.desktop` entry on Linux, a LaunchAgent plist on macOS, or a
//!   `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` value on Windows. Reachable
//!   from script as `set_autostart(on)` / `query_autostart(tag)`.
//! - [`RecentFilesService`] - per-app JSON list under XDG_DATA_HOME (Linux), `~/Library/Application
//!   Support` (macOS), or `%APPDATA%` (Windows). Mirrors `GtkRecentManager` / `QSettings`'s "recent
//!   documents" patterns. Reachable from script as `add_recent_file(path, label)` /
//!   `list_recent_files(tag)` / `clear_recent_files()`.
//!
//! All three pieces share an [`AppId`] for path scoping. The id maps to `lumen.toml [app] id` at the
//! caller layer; this crate is ECS-clean and doesn't read TOML. The runtime crate
//! (`core/runtime/src/run/subsystems.rs`) is what installs these as resources, translates the shared
//! `ScriptCommand` variants into calls on them, and dispatches their replies back to script - this
//! crate itself names no script host and no config key.

#![warn(missing_docs)]

// The optional-subsystem entry: how the run loop installs this crate. The
// constructor that registers it lives in the `-capability` crate beside
// this one, in an object of its own.
#[cfg(not(target_arch = "wasm32"))]
pub mod capability;

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Stable app identifier (`com.example.lumen` or a path-derived
/// slug). Used by every lifecycle surface for storage path scoping.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppId(pub String);

impl From<&str> for AppId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for AppId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Single-instance lock outcome.
///
/// Mirrors `g_application_get_is_remote`'s tri-state - "I'm primary"
/// vs "I'm secondary and forwarded my argv to the primary".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SingleInstance {
    /// This process holds the lock. Run normally.
    Primary,
    /// Another process already holds the lock. `args_sent` is true
    /// when this instance successfully forwarded argv to the primary
    /// (so the secondary can exit cleanly).
    Secondary {
        /// True when the secondary instance successfully forwarded
        /// its argv to the primary.
        args_sent: bool,
    },
}

/// Inbox of argv batches received from secondary launches.
///
/// The primary's listener thread pushes each forwarded `Vec<String>` here; the embedder drains and
/// dispatches as ECS messages. Wrapped in `Arc<Mutex<...>>` so the listener thread and the main thread
/// share access without an extra channel.
#[derive(Clone, Default)]
pub struct SecondaryArgsInbox {
    inner: Arc<Mutex<Vec<Vec<String>>>>,
}

impl SecondaryArgsInbox {
    /// Empty inbox.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, args: Vec<String>) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(args);
        }
    }

    /// Drain all pending argv batches.
    pub fn drain(&self) -> Vec<Vec<String>> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

/// Lifecycle service. Resource-shaped so apps can `Res<LifecycleService>`.
///
/// Holds the resolved storage directories and the secondary-args inbox. Constructing the service is
/// cheap and side-effect free; the socket / pipe binding only happens inside [`Self::ensure_single_instance`].
#[derive(Resource, Clone, Default)]
pub struct LifecycleService {
    /// Cached XDG_DATA_HOME / Library / %APPDATA% root.
    data_root: Option<PathBuf>,
    /// Inbox shared with the listener thread.
    inbox: SecondaryArgsInbox,
}

impl LifecycleService {
    /// Empty service.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the data-root used by [`RecentFilesService`] /
    /// [`AutostartService`]. Useful in tests.
    pub fn with_data_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.data_root = Some(root.into());
        self
    }

    /// Resolve the per-app data dir, same shape as
    /// [`lumen_core::app_paths::data_dir`]: `<root>/lumen/<app_id>` where
    /// root is `$XDG_DATA_HOME` / `$HOME/.local/share` (Linux),
    /// `$HOME/Library/Application Support` (macOS), or `%APPDATA%`
    /// (Windows). Falls back to the process's app directory when nothing
    /// resolves (tests, sandboxes). Delegates to `app_paths` rather than
    /// resolving the root itself, so there is one definition of the shape
    /// shared by every per-app data directory in Lumen.
    pub fn data_dir(&self, app: &AppId) -> PathBuf {
        match &self.data_root {
            Some(root) => lumen_core::app_paths::data_dir_under(root, &app.0),
            None => lumen_core::app_paths::data_dir_for(&app.0),
        }
    }

    /// Returns the secondary-args inbox so embedders can drain it each tick.
    pub fn secondary_args_inbox(&self) -> SecondaryArgsInbox {
        self.inbox.clone()
    }

    /// Drain any argv batches that secondary launches have forwarded since the last drain. Convenience
    /// wrapper around the embedded [`SecondaryArgsInbox::drain`].
    pub fn take_secondary_args(&self) -> Vec<Vec<String>> {
        self.inbox.drain()
    }

    /// Try to acquire a single-instance lock for `app`.
    ///
    /// On Linux + macOS this binds a Unix domain socket at `<app_id>.sock` under a private, per-user
    /// directory resolved and verified per platform (see the module docs) - never a shared temp
    /// directory; on Windows it claims the first instance of a named pipe at `\\.\pipe\<app_id>`,
    /// granted to the calling user alone. On a successful bind or claim the
    /// caller becomes the primary and a recv thread spawns. When the name is already held, the caller
    /// connects as a secondary, forwards `args` as length-prefixed JSON, and returns
    /// [`SingleInstance::Secondary`] so the caller can exit. When no private directory can be resolved
    /// (unix) or no security descriptor can be built for the pipe (Windows), the lock is skipped and
    /// the caller runs as [`SingleInstance::Primary`] unlocked.
    pub fn ensure_single_instance(&self, app: &AppId, args: &[String]) -> SingleInstance {
        #[cfg(unix)]
        {
            unix::ensure(app, args, self.inbox.clone())
        }
        #[cfg(windows)]
        {
            windows_pipe::ensure(app, args, self.inbox.clone())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (app, args);
            SingleInstance::Primary
        }
    }
}

/// Per-tick drain: forward argv batches a secondary launch sent this tick as
/// [`lumen_core::input::SecondInstanceLaunched`] messages.
///
/// A no-op when [`LifecycleService::ensure_single_instance`] was never
/// called (the inbox stays empty forever), so this is safe to register
/// unconditionally - the same DEFAULT-ON shape the other OS host crates use
/// for their own idle-cost-nil per-tick drains.
pub fn poll_second_instance(
    svc: Res<LifecycleService>,
    mut out: MessageWriter<lumen_core::input::SecondInstanceLaunched>,
) {
    for args in svc.take_secondary_args() {
        out.write(lumen_core::input::SecondInstanceLaunched { args });
    }
}

/// One recent-files entry, persisted as JSON.
///
/// Mirrors `GtkRecentInfo` (`uri`, `display_name`, `mime_type`,
/// `last_visited`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentFile {
    /// Filesystem path of the recently-opened file.
    pub path: PathBuf,
    /// Optional human-readable label (defaults to file name).
    pub label: Option<String>,
    /// Optional MIME type (`text/plain`, `image/png`, ...).
    pub mime: Option<String>,
    /// Unix epoch seconds of the last open.
    pub last_opened: u64,
}

/// Recent-files service.
///
/// Stores a per-app JSON list at `<data_dir>/recent.json`.
#[derive(Resource, Clone)]
pub struct RecentFilesService {
    /// Per-app data dir (matches [`LifecycleService::data_dir`]).
    pub data_dir: PathBuf,
    /// Maximum number of entries to retain. Older entries are evicted
    /// after each [`Self::add`].
    pub max_entries: usize,
}

impl RecentFilesService {
    /// Construct a service rooted at `data_dir`. Creates the
    /// directory on first write - no `mkdir` happens here so
    /// constructing the service in tests is side-effect-free.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            max_entries: 32,
        }
    }

    /// Builder: change the max-entries cap.
    pub fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    fn file(&self) -> PathBuf {
        self.data_dir.join("recent.json")
    }

    /// Load the on-disk recent-files list. Empty on first run / parse
    /// failure.
    pub fn list(&self, limit: usize) -> Vec<RecentFile> {
        let path = self.file();
        let Ok(bytes) = std::fs::read(&path) else {
            return Vec::new();
        };
        let mut entries: Vec<RecentFile> = serde_json::from_slice(&bytes).unwrap_or_default();
        if entries.len() > limit {
            entries.truncate(limit);
        }
        entries
    }

    /// Add (or move-to-front) a recent file. Errors writing the JSON
    /// are logged but don't propagate - the recent-files list is a
    /// best-effort cache, not a transactional store.
    pub fn add(&self, entry: RecentFile) {
        let mut entries = self.list(self.max_entries);
        // Move-to-front: drop any existing entry for the same path,
        // then push the new one at the front.
        entries.retain(|e| e.path != entry.path);
        entries.insert(0, entry);
        if entries.len() > self.max_entries {
            entries.truncate(self.max_entries);
        }
        if let Err(e) = std::fs::create_dir_all(&self.data_dir) {
            eprintln!(
                "lumen-os-lifecycle: mkdir {} failed: {e}",
                self.data_dir.display()
            );
            return;
        }
        let path = self.file();
        let json = match serde_json::to_vec_pretty(&entries) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("lumen-os-lifecycle: recent.json serialize: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&path, &json) {
            eprintln!("lumen-os-lifecycle: write {} failed: {e}", path.display());
        }
    }

    /// Clear the on-disk list.
    pub fn clear(&self) {
        let path = self.file();
        let _ = std::fs::remove_file(&path);
    }
}

/// Autostart service.
///
/// - Linux writes / removes `~/.config/autostart/<app_id>.desktop`.
/// - macOS writes / removes `~/Library/LaunchAgents/<app_id>.plist`. A minimal `RunAtLoad` plist is
///   emitted - the OS load step picks it up on next login.
/// - Windows writes / removes a `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` value
///   keyed by the app id, pointing at [`Self::exe_path`].
#[derive(Resource, Clone)]
pub struct AutostartService {
    /// App id for storage scoping.
    pub app_id: AppId,
    /// Path to the binary to launch on login. Required on every
    /// platform - autostart entries can't reference a logical name.
    pub exe_path: PathBuf,
}

/// Escape a plain Desktop Entry value (e.g. `Name=`) per the spec: escape
/// `\` and the whitespace escapes so an embedded newline can't inject an
/// extra key/value line.
#[cfg(target_os = "linux")]
fn desktop_value_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Quote a Desktop Entry `Exec=` argument per the spec: wrap in double
/// quotes when the path holds whitespace or a reserved char, escaping the
/// in-quote reserved set (`"`, backtick, `$`, `\`). An unquoted path with
/// spaces would otherwise be parsed as multiple arguments.
#[cfg(target_os = "linux")]
fn desktop_exec_quote(path: &str) -> String {
    let reserved = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '`'
                    | '$'
                    | '\\'
                    | '\''
                    | '<'
                    | '>'
                    | '~'
                    | '|'
                    | '&'
                    | ';'
                    | '*'
                    | '?'
                    | '#'
                    | '('
                    | ')'
            )
    };
    if !path.is_empty() && !path.chars().any(reserved) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

impl AutostartService {
    /// Construct with the app id and executable path.
    pub fn new(app_id: AppId, exe_path: PathBuf) -> Self {
        Self { app_id, exe_path }
    }

    /// Enable or disable the autostart entry. Returns `true` on
    /// success, `false` when the platform helper fails (logged to
    /// stderr).
    pub fn set_enabled(&self, on: bool) -> bool {
        if on { self.enable() } else { self.disable() }
    }

    /// True when the autostart entry currently exists.
    ///
    /// `None` when the platform helper could not even resolve where to
    /// look (Linux/macOS: `XDG_CONFIG_HOME` and `HOME` both unset) - a
    /// distinct outcome from a resolved, absent entry, and the caller
    /// must not read it as "disabled".
    pub fn is_enabled(&self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            self.linux_desktop_path().map(|p| p.exists())
        }
        #[cfg(target_os = "macos")]
        {
            self.macos_plist_path().map(|p| p.exists())
        }
        #[cfg(target_os = "windows")]
        {
            Some(self.windows_run_key_exists())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Some(false)
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_desktop_path(&self) -> Option<PathBuf> {
        let xdg = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(
            xdg.join("autostart")
                .join(format!("{}.desktop", self.app_id.0)),
        )
    }

    #[cfg(target_os = "macos")]
    fn macos_plist_path(&self) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{}.plist", self.app_id.0)),
        )
    }

    #[cfg(target_os = "windows")]
    fn windows_run_key_exists(&self) -> bool {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run") {
            Ok(run) => run.get_value::<String, _>(&self.app_id.0).is_ok(),
            Err(_) => false,
        }
    }

    fn enable(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            let Some(path) = self.linux_desktop_path() else {
                return false;
            };
            if let Some(parent) = path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!("lumen-os-lifecycle: mkdir {} failed: {e}", parent.display());
                return false;
            }
            let body = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name={}\n\
                 Exec={}\n\
                 Hidden=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                desktop_value_escape(&self.app_id.0),
                desktop_exec_quote(&self.exe_path.display().to_string())
            );
            match std::fs::write(&path, body) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: write {} failed: {e}", path.display());
                    false
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let Some(path) = self.macos_plist_path() else {
                return false;
            };
            if let Some(parent) = path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!("lumen-os-lifecycle: mkdir {} failed: {e}", parent.display());
                return false;
            }
            // Minimal LaunchAgents plist - RunAtLoad + ProgramArguments. The launchd schema is documented
            // in `man launchd.plist`. Escape XML metacharacters in the exe path defensively.
            let exe = xml_escape(&self.exe_path.to_string_lossy());
            let label = xml_escape(&self.app_id.0);
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\">\n\
                 <dict>\n  <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{exe}</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n"
            );
            match std::fs::write(&path, body) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: write {} failed: {e}", path.display());
                    false
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            use winreg::RegKey;
            use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let (run, _) = match hkcu.create_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                KEY_WRITE,
            ) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: HKCU\\...\\Run create_subkey failed: {e}");
                    return false;
                }
            };
            let value = self.exe_path.to_string_lossy().into_owned();
            match run.set_value(&self.app_id.0, &value) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: set Run\\{} failed: {e}", self.app_id.0);
                    false
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    fn disable(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            let Some(path) = self.linux_desktop_path() else {
                return false;
            };
            match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: rm {} failed: {e}", path.display());
                    false
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let Some(path) = self.macos_plist_path() else {
                return false;
            };
            match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: rm {} failed: {e}", path.display());
                    false
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            use winreg::RegKey;
            use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let run = match hkcu.open_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                KEY_WRITE,
            ) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
                Err(e) => {
                    eprintln!("lumen-os-lifecycle: HKCU\\...\\Run open failed: {e}");
                    return false;
                }
            };
            match run.delete_value(&self.app_id.0) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => {
                    eprintln!(
                        "lumen-os-lifecycle: delete Run\\{} failed: {e}",
                        self.app_id.0
                    );
                    false
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            true
        }
    }
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(unix)]
mod unix {
    //! Unix domain socket single-instance backend.
    //!
    //! Primary binds `<runtime_dir>/<app_id>.sock`; if the bind fails we attempt to connect - success means
    //! another instance is alive (secondary path); a `ConnectionRefused` / `NotFound` on the connect means
    //! the socket file is stale (previous instance crashed). Stale sockets are unlinked + rebind retried
    //! once.

    use super::{AppId, SecondaryArgsInbox, SingleInstance};
    #[cfg(target_os = "macos")]
    use std::ffi::CStr;
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    /// The private, per-user directory a single-instance socket lives
    /// under, or `None` when no such directory can be resolved AND
    /// verified.
    ///
    /// No fallback to [`std::env::temp_dir`]: that directory is world-writable
    /// on most systems, so a socket path under it is squattable by any local
    /// user ahead of the real launch. A squatter's listener accepts the real
    /// launch's connect, the real launch reads that as "an instance is
    /// already running", and exits having forwarded its actual argv to the
    /// squatter without ever starting. Failing closed - no lock, run as
    /// [`SingleInstance::Primary`] - is strictly safer than trusting an
    /// unauthenticated shared directory.
    ///
    /// The candidate itself is platform-specific ([`candidate_runtime_dir`]):
    /// `XDG_RUNTIME_DIR` is a Linux-only convention, so a rule of "that
    /// variable or nothing" leaves single-instance permanently unavailable
    /// on macOS, which has no such variable but does have its own
    /// OS-provided private-per-user directory. Either way the candidate is
    /// verified, not assumed private: [`verified_private_dir`] checks
    /// ownership and mode rather than trusting a spec or a convention.
    // `pub(crate)`, not `pub`: this is the exact question the production
    // path (`socket_path` -> `ensure` -> `ensure_at`) asks; a test that
    // wants to know whether THIS machine offers a private directory asks
    // it too, rather than hardcoding the Linux half of the platform split.
    pub(crate) fn runtime_dir() -> Option<PathBuf> {
        candidate_runtime_dir().and_then(verified_private_dir)
    }

    /// `$XDG_RUNTIME_DIR` on Linux: the XDG base-directory spec requires it
    /// to be created by the OS as `0700` and owned by the user, which is
    /// exactly what [`verified_private_dir`] checks rather than assumes.
    #[cfg(target_os = "linux")]
    fn candidate_runtime_dir() -> Option<PathBuf> {
        std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from)
    }

    /// macOS has no `XDG_RUNTIME_DIR` convention, but the OS provides the
    /// same shape of thing under a different name: a per-user, per-boot
    /// directory it creates and owns exclusively for the calling user,
    /// returned by `confstr(_CS_DARWIN_USER_TEMP_DIR)` (the same directory
    /// `$TMPDIR` usually already names in an interactive session - this
    /// asks the OS directly instead of trusting an inherited environment
    /// variable that a bare launchd job or a CI runner may not set).
    #[cfg(target_os = "macos")]
    fn candidate_runtime_dir() -> Option<PathBuf> {
        let mut buf = vec![0u8; 1024];
        // SAFETY: `buf` is a valid, uniquely-owned buffer of `buf.len()`
        // bytes; `confstr` writes at most that many bytes into it and
        // returns the length it needed (including the NUL), which we check
        // against the buffer's capacity before reading anything back out.
        let needed = unsafe {
            libc::confstr(
                libc::_CS_DARWIN_USER_TEMP_DIR,
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
            )
        };
        if needed == 0 || needed > buf.len() {
            return None;
        }
        // SAFETY: `needed > 0` and `needed <= buf.len()` was just checked, so
        // `buf` holds a NUL-terminated string confstr wrote within bounds.
        let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
        Some(PathBuf::from(cstr.to_str().ok()?))
    }

    /// Every other unix this crate's `#[cfg(unix)]` reaches (the BSDs) has
    /// neither convention; fail closed rather than guessing a path.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn candidate_runtime_dir() -> Option<PathBuf> {
        None
    }

    /// Accept `dir` as the private runtime directory only once checked:
    /// it must exist, be a directory, be owned by the calling process's
    /// user, and be unreadable and unwritable by anyone else (mode `0700`
    /// or tighter). A directory that fails any of these is not a safe
    /// place to bind a socket another local user must not reach, whatever
    /// convention says it should be. `pub(crate)`, not `pub`: a check
    /// this crate's own tests exercise directly, not public API.
    pub(crate) fn verified_private_dir(dir: PathBuf) -> Option<PathBuf> {
        let meta = std::fs::metadata(&dir).ok()?;
        if !meta.is_dir() {
            return None;
        }
        // SAFETY: `getuid` takes no arguments, performs no memory access,
        // and cannot fail - it is `unsafe` only because it crosses the FFI
        // boundary.
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid {
            return None;
        }
        // Reject anything readable or writable by group or other.
        if meta.mode() & 0o077 != 0 {
            return None;
        }
        Some(dir)
    }

    fn socket_path(app: &AppId) -> Option<PathBuf> {
        runtime_dir().map(|dir| dir.join(format!("{}.sock", app.0)))
    }

    /// Restrict the socket to this user right after bind.
    ///
    /// `bind` creates the special file under the process umask, which on a
    /// permissive umask could leave it group- or world-accessible; forcing
    /// `0600` closes that regardless of umask, so a co-resident user cannot
    /// connect and feed attacker-chosen argv into `on_second_instance`. A
    /// TOCTOU window remains between `bind` and this call - the standard
    /// caveat for this fix on any platform whose bind+chmod aren't one
    /// syscall - but it is a window inside a directory the XDG spec already
    /// requires to be `0700` and user-owned, not an open one.
    fn lock_down(path: &Path) {
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::debug!(
                "lumen-os-lifecycle: chmod 0600 {} failed: {e}",
                path.display()
            );
        }
    }

    pub fn ensure(app: &AppId, args: &[String], inbox: SecondaryArgsInbox) -> SingleInstance {
        ensure_at(socket_path(app), args, inbox)
    }

    /// Core of [`ensure`], parameterized on the resolved socket path so a
    /// test can drive the "no `XDG_RUNTIME_DIR`" (`None`) branch without
    /// mutating the process environment, which would race every other test
    /// in this binary. `pub(crate)`, not `pub`: a branch point for this
    /// crate's own tests, not public API.
    pub(crate) fn ensure_at(
        path: Option<PathBuf>,
        args: &[String],
        inbox: SecondaryArgsInbox,
    ) -> SingleInstance {
        let Some(path) = path else {
            tracing::debug!(
                "lumen-os-lifecycle: XDG_RUNTIME_DIR is unset; running without a single-instance lock"
            );
            return SingleInstance::Primary;
        };

        // Try to bind first. If the path is held by a live primary, this fails with AddrInUse and we
        // fall through to the secondary path. If the path is a stale leftover, the connect will fail
        // and we unlink + retry once.
        match UnixListener::bind(&path) {
            Ok(listener) => {
                lock_down(&path);
                spawn_recv(listener, inbox);
                SingleInstance::Primary
            }
            Err(_) => {
                // Try to forward args to the existing primary.
                match UnixStream::connect(&path) {
                    Ok(mut stream) => {
                        let sent = send_args(&mut stream, args).is_ok();
                        SingleInstance::Secondary { args_sent: sent }
                    }
                    Err(_) => {
                        // Stale socket - unlink and try one more bind.
                        let _ = std::fs::remove_file(&path);
                        match UnixListener::bind(&path) {
                            Ok(listener) => {
                                lock_down(&path);
                                spawn_recv(listener, inbox);
                                SingleInstance::Primary
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "lumen-os-lifecycle: bind {} after unlink failed: {e}",
                                    path.display()
                                );
                                SingleInstance::Secondary { args_sent: false }
                            }
                        }
                    }
                }
            }
        }
    }

    fn spawn_recv(listener: UnixListener, inbox: SecondaryArgsInbox) {
        std::thread::Builder::new()
            .name("lumen-os-lifecycle/uds".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(mut s) => {
                            if let Some(args) = recv_args(&mut s) {
                                inbox.push(args);
                            }
                        }
                        Err(e) => {
                            tracing::debug!("lumen-os-lifecycle: accept: {e}");
                        }
                    }
                }
            })
            .ok();
    }

    fn send_args(stream: &mut UnixStream, args: &[String]) -> std::io::Result<()> {
        let payload = serde_json::to_vec(args)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = payload.len() as u32;
        stream.write_all(&len.to_le_bytes())?;
        stream.write_all(&payload)?;
        Ok(())
    }

    fn recv_args(stream: &mut UnixStream) -> Option<Vec<String>> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > 16 * 1024 * 1024 {
            // Defensive cap: don't accept multi-MB payloads from a hostile second instance.
            return None;
        }
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }
}

#[cfg(windows)]
mod windows_pipe {
    //! Named-pipe single-instance backend (Windows).
    //!
    //! The primary claims `\\.\pipe\<app_id>` with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the create
    //! fails once the name exists instead of quietly adding another server instance to a name a live
    //! instance already holds; that failure is what tells a second launch it is the secondary. The
    //! pipe carries an explicit discretionary ACL granting the user this process runs as and nobody
    //! else, rather than the default descriptor, which also grants read access to Everyone and to the
    //! anonymous account, and it rejects remote clients, because the lock is a question about this
    //! machine. Secondaries connect with the standard file APIs and write length-prefixed JSON. Uses
    //! the `windows` crate's raw Win32 bindings - winit already pulls `windows` in transitively, so no
    //! extra dep churn.

    use super::{AppId, SecondaryArgsInbox, SingleInstance};
    use std::io::{Read, Write};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    };
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    };
    use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;

    fn pipe_name(app: &AppId) -> String {
        format!("\\\\.\\pipe\\{}", app.0)
    }

    /// What a create call is asking for.
    enum Claim {
        /// The name itself: fail if any instance of it already exists.
        Name,
        /// One more instance of a name this process already holds.
        Instance,
    }

    /// What a create call settled.
    enum Attempt {
        /// The instance is ours.
        Created(HANDLE),
        /// Another process holds the name.
        Taken,
        /// No lock could be attempted: the descriptor could not be built, the
        /// system refused the name, or it was out of resources. The caller
        /// runs unlocked rather than exiting, the same way the unix side runs
        /// unlocked when it cannot resolve a private directory.
        Unavailable,
    }

    pub fn ensure(app: &AppId, args: &[String], inbox: SecondaryArgsInbox) -> SingleInstance {
        let name = pipe_name(app);
        match create_pipe(&name, Claim::Name) {
            Attempt::Created(handle) => {
                spawn_recv(handle, name, inbox);
                SingleInstance::Primary
            }
            Attempt::Taken => {
                // Forward argv to the instance that holds the name.
                match std::fs::OpenOptions::new()
                    .write(true)
                    .read(true)
                    .open(&name)
                {
                    Ok(mut f) => {
                        let sent = send_args(&mut f, args).is_ok();
                        SingleInstance::Secondary { args_sent: sent }
                    }
                    Err(e) => {
                        tracing::debug!("lumen-os-lifecycle: open pipe {name}: {e}");
                        SingleInstance::Secondary { args_sent: false }
                    }
                }
            }
            // No lock was taken, so the app runs as an unlocked primary
            // rather than exiting over a lock it never acquired.
            Attempt::Unavailable => SingleInstance::Primary,
        }
    }

    /// Create one instance of the lock pipe.
    ///
    /// [`Claim::Name`] adds `FILE_FLAG_FIRST_PIPE_INSTANCE`, which is what makes the create fail on a
    /// name someone already holds: without that flag `CreateNamedPipeW` returns another server
    /// instance of an existing name, so every launch gets a handle and every launch calls itself the
    /// primary.
    fn create_pipe(name: &str, claim: Claim) -> Attempt {
        let Some(mut security) = owner_only_security() else {
            tracing::debug!(
                "lumen-os-lifecycle: no owner-only security descriptor for the lock pipe"
            );
            return Attempt::Unavailable;
        };
        let attributes = security.attributes();
        let open_mode = match claim {
            Claim::Name => PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            Claim::Instance => PIPE_ACCESS_DUPLEX,
        };
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated copy of `name`, and `attributes`
        // points at a descriptor `security` owns; both outlive the call. The
        // returned handle is checked before it is used.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                Some(&raw const attributes),
            )
        };
        if !handle.is_invalid() {
            return Attempt::Created(handle);
        }
        // SAFETY: `GetLastError` reads this thread's last-error value, which
        // the failed call above just set.
        let err = unsafe { GetLastError() };
        // `ERROR_ACCESS_DENIED` is what the first-instance flag raises on a
        // name that already exists, and also what an instance whose type or
        // access mode disagrees with the existing name raises. Either way the
        // name belongs to someone else.
        if err == ERROR_ACCESS_DENIED {
            Attempt::Taken
        } else {
            tracing::debug!(
                "lumen-os-lifecycle: CreateNamedPipeW {name}: error {}",
                err.0
            );
            Attempt::Unavailable
        }
    }

    /// The pipe's security descriptor and the DACL it points into.
    ///
    /// An absolute `SECURITY_DESCRIPTOR` stores pointers rather than one flat blob, so the ACL it
    /// points at has to live as long as it does; one value owning both is what makes that hold. The
    /// SID is not in here because `AddAccessAllowedAce` copies it into the ACE.
    struct OwnerOnly {
        descriptor: Box<SECURITY_DESCRIPTOR>,
        /// Reached through `descriptor`, never through this field.
        _dacl: Vec<u32>,
    }

    impl OwnerOnly {
        fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&raw mut *self.descriptor).cast(),
                // The pipe handle is this process's alone; a child has no
                // business inheriting the lock.
                bInheritHandle: false.into(),
            }
        }
    }

    /// Build a descriptor granting the pipe to the user this process runs as, and to nobody else.
    ///
    /// The alternative is the default descriptor, which grants full control to LocalSystem,
    /// administrators and the creator owner, and read access to Everyone and to the anonymous
    /// account. Read access is not enough to push argv at the primary, since a client has to write to
    /// send anything, but it is enough to connect to the lock and occupy an instance of it, and a
    /// lock is better off stating who may reach it than inheriting the answer.
    fn owner_only_security() -> Option<OwnerOnly> {
        let token_user = current_token_user()?;
        // SAFETY: `token_user` is a pointer-aligned buffer holding the
        // `TOKEN_USER` the kernel wrote into it, so the read is aligned, in
        // bounds, and of the right type. `Sid` points inside that same
        // buffer, which outlives the calls below that read through it.
        let sid = unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        // SAFETY: `sid` is the SID the kernel wrote into a buffer still alive
        // here.
        let sid_len = unsafe { GetLengthSid(sid) } as usize;

        // One access-allowed ACE holding one SID. `ACCESS_ALLOWED_ACE` already
        // counts four bytes of SID in its `SidStart` field, so adding the
        // SID's whole length over-reserves by that much rather than
        // under-reserving. The buffer is `u32`-typed for the DWORD alignment
        // an `ACL` needs.
        let dacl_len = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() + sid_len;
        let mut dacl = vec![0u32; dacl_len.div_ceil(size_of::<u32>())];
        let dacl_ptr = dacl.as_mut_ptr().cast::<ACL>();
        // SAFETY: `dacl_ptr` points at a zeroed, DWORD-aligned buffer of at
        // least `dacl_len` bytes, which is the length `InitializeAcl` is told
        // about and the space the single ACE goes into. Moving the `Vec` into
        // `OwnerOnly` below does not move its heap buffer, so the pointer the
        // descriptor keeps on holding stays valid.
        unsafe {
            InitializeAcl(dacl_ptr, dacl_len as u32, ACL_REVISION).ok()?;
            AddAccessAllowedAce(dacl_ptr, ACL_REVISION, FILE_ALL_ACCESS.0, sid).ok()?;
        }

        let mut descriptor = Box::new(SECURITY_DESCRIPTOR::default());
        let psd = PSECURITY_DESCRIPTOR((&raw mut *descriptor).cast());
        // SAFETY: `psd` points at a live `SECURITY_DESCRIPTOR` owned here and
        // `dacl_ptr` at the ACL initialized just above; both are boxed or
        // heap-backed, so the values `OwnerOnly` carries away keep them at the
        // addresses the descriptor records.
        unsafe {
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION).ok()?;
            SetSecurityDescriptorDacl(psd, true, Some(dacl_ptr.cast_const()), false).ok()?;
        }
        Some(OwnerOnly {
            descriptor,
            _dacl: dacl,
        })
    }

    /// The `TOKEN_USER` of this process's token, in a pointer-aligned buffer.
    ///
    /// `TOKEN_USER` holds a pointer, so the buffer the kernel writes it into has to be
    /// pointer-aligned; a `Vec<u8>` is byte-aligned whatever the allocator happens to hand back.
    fn current_token_user() -> Option<Vec<usize>> {
        let mut token = HANDLE::default();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // closing, and `token` is a live local the call writes the opened
        // handle into.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
        let user = read_token_user(token);
        // SAFETY: `token` was opened just above, is owned here, and is not
        // used again.
        unsafe {
            let _ = CloseHandle(token);
        }
        user
    }

    fn read_token_user(token: HANDLE) -> Option<Vec<usize>> {
        let mut needed = 0u32;
        // The sizing call: with no buffer it fails and writes the length the
        // token information needs.
        // SAFETY: a null buffer with a zero length is the documented way to
        // ask for that length, and `needed` is a live local.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        // SAFETY: `buf` is at least `needed` bytes of writable, pointer-aligned
        // storage, which is what the length argument promises the call.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
        }
        .ok()?;
        Some(buf)
    }

    fn spawn_recv(primary_handle: HANDLE, name: String, inbox: SecondaryArgsInbox) {
        // The first instance is already open and waiting for a connection;
        // every later one comes from a fresh create.
        let initial = PrimaryPipe(primary_handle);
        std::thread::Builder::new()
            .name("lumen-os-lifecycle/pipe".to_string())
            .spawn(move || {
                let mut current = initial;
                loop {
                    if !connect(current.0) {
                        // Drop the failed instance and re-arm.
                        drop(current);
                        match create_pipe(&name, Claim::Instance) {
                            Attempt::Created(handle) => {
                                current = PrimaryPipe(handle);
                                continue;
                            }
                            _ => {
                                tracing::debug!("lumen-os-lifecycle: re-create pipe failed");
                                return;
                            }
                        }
                    }
                    // Arm the next instance before servicing this one. A name
                    // exists only while an instance of it is open, so letting
                    // the count reach zero would hand the lock to whichever
                    // launch created it next while this instance is still
                    // running; it also leaves somewhere to connect for a
                    // secondary that arrives while this connection is being
                    // read.
                    let next = match create_pipe(&name, Claim::Instance) {
                        Attempt::Created(handle) => Some(PrimaryPipe(handle)),
                        _ => None,
                    };
                    // Transfer sole ownership of the handle to `stream`:
                    // `HANDLE` is `Copy`, so we must `forget` the `PrimaryPipe`
                    // wrapper to avoid its `Drop` also `CloseHandle`-ing the
                    // same handle (double-close UB).
                    let handle = current.0;
                    std::mem::forget(current);
                    let mut stream = PipeStream(handle);
                    if let Some(args) = recv_args(&mut stream) {
                        inbox.push(args);
                    }
                    // Drop disconnects + closes the handle (sole owner now).
                    drop(stream);
                    match next {
                        Some(pipe) => current = pipe,
                        None => {
                            tracing::debug!("lumen-os-lifecycle: re-arm pipe failed");
                            return;
                        }
                    }
                }
            })
            .ok();
    }

    fn connect(handle: HANDLE) -> bool {
        // A client that connected in the window between `CreateNamedPipeW`
        // and `ConnectNamedPipe` makes the latter fail with
        // `ERROR_PIPE_CONNECTED` - that is success, not failure (dropping it
        // would lose a secondary instance's argv).
        // SAFETY: `handle` is a live pipe instance the caller owns, and
        // `GetLastError` reads the value the failed connect just set.
        unsafe {
            if ConnectNamedPipe(handle, None).is_ok() {
                return true;
            }
            GetLastError() == ERROR_PIPE_CONNECTED
        }
    }

    /// Owned named-pipe handle (closed on drop).
    struct PrimaryPipe(HANDLE);
    // SAFETY: a Win32 `HANDLE` is a process-wide kernel object reference;
    // it is valid on any thread and this wrapper has sole ownership (the
    // handle is created on the caller thread, then moved once into the
    // dedicated pipe-accept thread and never aliased). Moving it across
    // the thread boundary is sound.
    unsafe impl Send for PrimaryPipe {}
    impl Drop for PrimaryPipe {
        fn drop(&mut self) {
            // SAFETY: sole owner of a handle that is not used again.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct PipeStream(HANDLE);
    impl Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut n = 0u32;
            // SAFETY: `self.0` is a live pipe instance this value owns, and
            // `buf` and `n` are live for the call.
            unsafe {
                ReadFile(self.0, Some(buf), Some(&mut n), None)
                    .map_err(|e| std::io::Error::other(format!("ReadFile: {e}")))?;
            }
            Ok(n as usize)
        }
    }
    impl Drop for PipeStream {
        fn drop(&mut self) {
            // SAFETY: sole owner of a handle that is not used again.
            unsafe {
                let _ = DisconnectNamedPipe(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn send_args(stream: &mut std::fs::File, args: &[String]) -> std::io::Result<()> {
        let payload = serde_json::to_vec(args)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = payload.len() as u32;
        stream.write_all(&len.to_le_bytes())?;
        stream.write_all(&payload)?;
        stream.flush()?;
        Ok(())
    }

    fn recv_args<R: Read>(stream: &mut R) -> Option<Vec<String>> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > 16 * 1024 * 1024 {
            return None;
        }
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_exec_quote_handles_spaces_and_reserved() {
        // A plain path is left untouched.
        assert_eq!(desktop_exec_quote("/usr/bin/app"), "/usr/bin/app");
        // Spaces force quoting (the regression: an unquoted spaced path is
        // parsed as multiple Exec arguments).
        assert_eq!(
            desktop_exec_quote("/opt/My App/bin/app"),
            "\"/opt/My App/bin/app\""
        );
        // Reserved chars inside the quotes are backslash-escaped.
        assert_eq!(
            desktop_exec_quote("/opt/a$b`c\"d\\e"),
            "\"/opt/a\\$b\\`c\\\"d\\\\e\""
        );
        // Empty path still quotes (defensive).
        assert_eq!(desktop_exec_quote(""), "\"\"");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_value_escape_neutralises_newlines() {
        assert_eq!(desktop_value_escape("My App"), "My App");
        // A newline must not be able to inject a second key line.
        assert_eq!(
            desktop_value_escape("evil\nExec=/bin/sh"),
            "evil\\nExec=/bin/sh"
        );
        assert_eq!(desktop_value_escape("back\\slash"), "back\\\\slash");
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!(
            "lumen-os-lifecycle-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&t);
        t
    }

    #[test]
    fn app_id_from_str_and_string() {
        let a: AppId = "com.example.x".into();
        let b: AppId = String::from("com.example.x").into();
        assert_eq!(a, b);
    }

    #[test]
    fn first_launch_is_primary() {
        let svc = LifecycleService::new();
        // Use a unique id per process so concurrent test workers don't collide.
        let id = AppId::from(format!("test.lumen.first.{}", std::process::id()));
        let res = svc.ensure_single_instance(&id, &[]);
        assert_eq!(res, SingleInstance::Primary);
        // Cleanup the socket the primary thread bound.
        cleanup_uds(&id);
    }

    #[cfg(unix)]
    #[test]
    fn second_launch_forwards_args() {
        let id = AppId::from(format!("test.lumen.forward.{}", std::process::id()));
        let svc = LifecycleService::new();
        let primary = svc.ensure_single_instance(&id, &[]);
        assert_eq!(primary, SingleInstance::Primary);
        // Give the recv thread a moment to enter its accept loop.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let svc2 = LifecycleService::new();
        let args = vec!["foo".to_string(), "bar".to_string()];
        let secondary = svc2.ensure_single_instance(&id, &args);
        assert_eq!(secondary, SingleInstance::Secondary { args_sent: true });

        // Wait briefly for the primary's recv thread to process.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let received = svc.take_secondary_args();
        assert!(
            received.iter().any(|v| v == &args),
            "primary should have received argv {args:?}, got {received:?}"
        );
        cleanup_uds(&id);
    }

    /// A second launch of an id the first launch still holds resolves to
    /// [`SingleInstance::Secondary`], and its argv reaches the primary.
    ///
    /// Windows-only, because it exercises the named-pipe backend;
    /// `second_launch_forwards_args` covers the same ground over the socket on
    /// unix. Without `FILE_FLAG_FIRST_PIPE_INSTANCE` the second
    /// `CreateNamedPipeW` succeeds as one more server instance of a name the
    /// first launch still holds, and this returns `Primary` instead.
    #[cfg(windows)]
    #[test]
    fn second_launch_on_windows_is_secondary() {
        let id = AppId::from(format!("test.lumen.pipe.{}", std::process::id()));
        let svc = LifecycleService::new();
        assert_eq!(
            svc.ensure_single_instance(&id, &[]),
            SingleInstance::Primary
        );

        let svc2 = LifecycleService::new();
        let args = vec!["--open".to_string(), "notes.txt".to_string()];
        assert_eq!(
            svc2.ensure_single_instance(&id, &args),
            SingleInstance::Secondary { args_sent: true }
        );

        // The primary's recv thread pushes on its own schedule, so poll for
        // the argv rather than sleeping a fixed span on a loaded runner.
        let mut received = Vec::new();
        for _ in 0..200 {
            received.extend(svc.take_secondary_args());
            if !received.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            received.iter().any(|v| v == &args),
            "primary should have received argv {args:?}, got {received:?}"
        );
        // Nothing to unlink afterwards: a pipe lives as long as a handle to it
        // does, and the recv thread's handles go when the process does.
    }

    #[cfg(unix)]
    fn cleanup_uds(app: &AppId) {
        // Ask the same question the production path does rather than
        // hardcoding `XDG_RUNTIME_DIR` - on macOS the real socket lives
        // under `confstr(_CS_DARWIN_USER_TEMP_DIR)`, not that variable, and
        // a `runtime_dir()` of `None` means nothing was ever bound to
        // clean up.
        if let Some(rt) = unix::runtime_dir() {
            let _ = std::fs::remove_file(rt.join(format!("{}.sock", app.0)));
        }
    }
    #[cfg(not(unix))]
    fn cleanup_uds(_app: &AppId) {}

    /// The bound socket is `0600` - other local users on the same machine
    /// cannot connect and feed `on_second_instance` attacker-chosen argv.
    ///
    /// Asks [`unix::runtime_dir`] the same question the production path
    /// asks, rather than hardcoding `XDG_RUNTIME_DIR`: that's the Linux
    /// half of the platform split this crate now resolves per-OS, and
    /// hardcoding it here would make this test fail on macOS even though
    /// `confstr(_CS_DARWIN_USER_TEMP_DIR)` resolves a real directory there.
    /// A machine offering neither skips with a printed reason, the same
    /// house pattern used for a display or a GPU the sandbox lacks.
    #[cfg(unix)]
    #[test]
    fn single_instance_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let Some(rt) = unix::runtime_dir() else {
            eprintln!(
                "skip: this machine offers no private per-user directory (checked \
                 the same way the production path does) to bind a single-instance \
                 socket under"
            );
            return;
        };

        let id = AppId::from(format!("test.lumen.mode.{}", std::process::id()));
        let svc = LifecycleService::new();
        let res = svc.ensure_single_instance(&id, &[]);
        assert_eq!(res, SingleInstance::Primary);

        let path = rt.join(format!("{}.sock", id.0));
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "socket permissions: {mode:o}");
        cleanup_uds(&id);
    }

    /// No `XDG_RUNTIME_DIR` means no lock, not a `std::env::temp_dir()`
    /// fallback: a world-writable directory is squattable by another local
    /// user ahead of the real launch. Drives the branch directly (rather
    /// than unsetting the process environment) so it cannot race any other
    /// test in this binary that depends on `XDG_RUNTIME_DIR` being set.
    #[cfg(unix)]
    #[test]
    fn unset_runtime_dir_takes_the_primary_branch() {
        let res = unix::ensure_at(None, &[], SecondaryArgsInbox::new());
        assert_eq!(
            res,
            SingleInstance::Primary,
            "no runtime dir means run unlocked, never a temp-dir fallback"
        );
    }

    /// A candidate directory readable or writable by group or other is
    /// rejected regardless of platform convention - the check this crate
    /// now runs instead of assuming `XDG_RUNTIME_DIR` (or its macOS
    /// equivalent) is private just because the spec says it should be.
    #[cfg(unix)]
    #[test]
    fn verified_private_dir_rejects_a_world_readable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmpdir("insecure");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod 0755");
        assert_eq!(
            unix::verified_private_dir(dir.clone()),
            None,
            "0755 is readable by group and other; must not be trusted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owned, `0700` case that `verified_private_dir` is meant to
    /// accept - the positive side of the same check.
    #[cfg(unix)]
    #[test]
    fn verified_private_dir_accepts_an_owned_0700_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmpdir("secure");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        assert_eq!(unix::verified_private_dir(dir.clone()), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`poll_second_instance`] drains whatever a secondary launch pushed
    /// into the inbox into a [`lumen_core::input::SecondInstanceLaunched`]
    /// message, the half of the pipeline before script dispatch (which
    /// `lumen-runtime`'s `script_fn_lifecycle_commands` integration test
    /// covers from there, since it needs a real `ScriptHost`).
    #[test]
    fn poll_second_instance_drains_pending_args_into_a_message() {
        use bevy_ecs::message::Messages;
        use bevy_ecs::system::RunSystemOnce;
        use bevy_ecs::world::World;

        let mut world = World::new();
        world.init_resource::<Messages<lumen_core::input::SecondInstanceLaunched>>();
        let svc = LifecycleService::new();
        svc.secondary_args_inbox()
            .push(vec!["--open".to_string(), "report.pdf".to_string()]);
        world.insert_resource(svc);

        world.run_system_once(poll_second_instance).unwrap();

        let drained: Vec<_> = world
            .resource_mut::<Messages<lumen_core::input::SecondInstanceLaunched>>()
            .drain()
            .collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].args,
            vec!["--open".to_string(), "report.pdf".to_string()]
        );
    }

    #[test]
    fn data_dir_uses_override() {
        let root = tmpdir("data-dir");
        let svc = LifecycleService::new().with_data_root(&root);
        let dir = svc.data_dir(&AppId::from("foo"));
        // Same `<root>/lumen/<id>` shape `lumen_core::app_paths` uses, with
        // the override standing in for the resolved platform root.
        assert_eq!(dir, root.join("lumen").join("foo"));
    }

    /// With no override, the shape agrees with
    /// `lumen_core::app_paths::data_dir_for` - the crate delegates rather
    /// than keeping its own copy of the root-resolution logic.
    #[test]
    fn data_dir_agrees_with_app_paths() {
        let svc = LifecycleService::new();
        let app = AppId::from("agree-check");
        assert_eq!(
            svc.data_dir(&app),
            lumen_core::app_paths::data_dir_for(&app.0)
        );
    }

    #[test]
    fn recent_files_round_trips() {
        let root = tmpdir("round-trip");
        std::fs::create_dir_all(&root).unwrap();
        let svc = RecentFilesService::new(root.clone());
        let entry = RecentFile {
            path: PathBuf::from("/tmp/a.txt"),
            label: Some("a".to_string()),
            mime: Some("text/plain".to_string()),
            last_opened: 12345,
        };
        svc.add(entry.clone());
        let listed = svc.list(10);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], entry);
        // Adding the same path moves-to-front but doesn't duplicate.
        svc.add(entry.clone());
        assert_eq!(svc.list(10).len(), 1);
        svc.clear();
        assert!(svc.list(10).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recent_files_caps_at_max() {
        let root = tmpdir("caps");
        std::fs::create_dir_all(&root).unwrap();
        let svc = RecentFilesService::new(root.clone()).with_max_entries(2);
        for i in 0..4 {
            svc.add(RecentFile {
                path: PathBuf::from(format!("/tmp/{i}.txt")),
                label: None,
                mime: None,
                last_opened: i,
            });
        }
        assert_eq!(svc.list(99).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn autostart_service_constructs() {
        let _a = AutostartService::new(AppId::from("test"), PathBuf::from("/usr/bin/test"));
    }
}
