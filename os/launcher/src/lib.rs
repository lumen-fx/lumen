//! URL / file launcher for Lumen.
//!
//! Wraps the cross-platform `opener` crate. Mirrors Qt's
//! `QDesktopServices::openUrl(QUrl)` and GTK 4's `gtk_show_uri` /
//! `g_app_info_launch_default_for_uri`.
//!
//! Scripts open URLs in the default browser, files in the default viewer,
//! or directories in the file manager.
//!
//! The implementation is intentionally thin: every platform's "open
//! this thing" call boils down to `xdg-open` / `start` / `open` and
//! `opener` ships exactly that. The XDG `OpenURI` portal is not used, so a
//! sandboxed app falls back to whatever helper the sandbox exposes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// The optional-subsystem entry: how the run loop installs this crate. The
// constructor that registers it lives in the `-capability` crate beside
// this one, in an object of its own.
#[cfg(not(target_arch = "wasm32"))]
pub mod capability;

use bevy_ecs::prelude::*;
use std::path::Path;

/// Outcome of a launcher call.
///
/// Errors flatten to a `String` because `opener::OpenError` isn't
/// `'static` on every backend (the Linux variant carries an
/// `io::Error` reference inside an enum).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenResult {
    /// The platform reported success. Note: success means the helper
    /// process exited zero, not necessarily that the user actually
    /// saw the resource - `xdg-open` exits zero on success even when
    /// the user immediately closes the spawned app.
    Launched,
    /// The platform helper failed (no handler, bad URL, sandbox
    /// denied, ...). The string is the OS-side error message.
    Failed(String),
}

impl OpenResult {
    /// True when the result is [`Self::Launched`].
    pub fn is_ok(&self) -> bool {
        matches!(self, OpenResult::Launched)
    }
}

impl From<Result<(), opener::OpenError>> for OpenResult {
    fn from(r: Result<(), opener::OpenError>) -> Self {
        match r {
            Ok(()) => OpenResult::Launched,
            Err(e) => OpenResult::Failed(e.to_string()),
        }
    }
}

/// Launcher resource - stateless. Resource-shaped so the ECS systems
/// can request it as `Res<Launcher>` and so a follow-up that adds
/// permission gating (`lumen.toml [permissions] open_external`) has a
/// place to stash policy state.
#[derive(Resource, Default, Clone)]
pub struct Launcher {
    _priv: (),
}

impl Launcher {
    /// Empty launcher.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a URL in the user's default browser (or mail client for
    /// `mailto:`, etc).
    pub fn open_url(&self, url: &str) -> OpenResult {
        opener::open(url).into()
    }

    /// Open a filesystem path with the platform's default handler.
    /// Equivalent to `QDesktopServices::openUrl(QUrl::fromLocalFile)`.
    pub fn open_path(&self, path: &Path) -> OpenResult {
        opener::open(path).into()
    }

    /// Reveal a file in the platform's file manager
    /// (`Finder` / `Explorer` / `Files`). On Linux falls back to
    /// opening the containing directory because `dbus
    /// org.freedesktop.FileManager1.ShowItems` isn't universally
    /// implemented.
    pub fn reveal_in_file_manager(&self, path: &Path) -> OpenResult {
        #[cfg(target_os = "macos")]
        {
            // macOS: -R reveals the file in Finder.
            match std::process::Command::new("open")
                .arg("-R")
                .arg(path)
                .status()
            {
                Ok(s) if s.success() => OpenResult::Launched,
                Ok(s) => OpenResult::Failed(format!("open -R exit {s}")),
                Err(e) => OpenResult::Failed(e.to_string()),
            }
        }
        #[cfg(target_os = "windows")]
        {
            // `explorer /select,<path>` wants the switch and path as one
            // argument, and - critically - returns exit code 1 even on
            // success, so we must not gate on the exit status: a clean spawn
            // is the only success signal available here.
            let select = {
                let mut s = std::ffi::OsString::from("/select,");
                s.push(path);
                s
            };
            match std::process::Command::new("explorer.exe")
                .arg(select)
                .status()
            {
                Ok(_) => OpenResult::Launched,
                Err(e) => OpenResult::Failed(e.to_string()),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            // Linux fallback: open the containing directory.
            let target = path.parent().unwrap_or(path);
            opener::open(target).into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_constructs() {
        let _l = Launcher::new();
    }

    #[test]
    fn open_result_is_ok_only_on_launched() {
        assert!(OpenResult::Launched.is_ok());
        assert!(!OpenResult::Failed("nope".to_string()).is_ok());
    }
}
