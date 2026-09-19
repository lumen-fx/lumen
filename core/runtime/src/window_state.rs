//! Per-app window-state persistence.
//!
//! Opt-in via `[window] remember_state = true` in `lumen.toml`.
//! Stores `(position, size, maximized)` at
//! `<state_dir>/<app-id>/window-state.toml` between runs (`state_dir`
//! follows the XDG / macOS / Windows conventions:
//! `$XDG_STATE_HOME` -> `~/.local/state` on Linux,
//! `~/Library/Application Support` on macOS,
//! `%LOCALAPPDATA%` on Windows).
//!
//! Save happens on `LoopExiting` so a normal window close persists,
//! and a `SIGTERM` / crash doesn't (acceptable - apps usually want a
//! clean state if they crashed). Save also runs whenever `Resized` or
//! a future `Moved` event fires so kill-9 still keeps the most-recent
//! committed state.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// On-disk shape. Logical pixels for `size` so HiDPI machines don't
/// see the app come back at half-size; `position` is in physical
/// pixels (winit's `outer_position` units).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WindowState {
    /// Outer window position in physical pixels. `None` = let the OS
    /// pick (matches the bare-launch behaviour).
    pub position: Option<[i32; 2]>,
    /// Inner window size in logical pixels.
    pub size: Option<[u32; 2]>,
    /// Last maximized state.
    pub maximized: bool,
}

/// Resolve the path Lumen uses to persist a given app's window state.
/// Returns `None` when no per-user state dir is available (CI
/// containers, sandboxed runs without `$HOME`); callers should treat
/// that as "remember_state silently no-ops".
pub fn state_path(app_id: &str) -> Option<PathBuf> {
    let base = state_dir()?;
    Some(base.join("lumen").join(app_id).join("window-state.toml"))
}

fn state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(d) = std::env::var("XDG_STATE_HOME")
            && !d.is_empty()
        {
            return Some(PathBuf::from(d));
        }
        std::env::var("HOME").ok().map(|h| {
            let mut p = PathBuf::from(h);
            p.push(".local");
            p.push("state");
            p
        })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|h| {
            let mut p = PathBuf::from(h);
            p.push("Library");
            p.push("Application Support");
            p
        })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA").ok().map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Load the persisted state for `app_id`, returning a fresh default
/// when the file doesn't exist or fails to parse (corrupted state
/// shouldn't brick the app - just discard).
pub fn load(app_id: &str) -> WindowState {
    let Some(path) = state_path(app_id) else {
        return WindowState::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(src) => toml::from_str(&src).unwrap_or_default(),
        Err(_) => WindowState::default(),
    }
}

/// Save `state` for `app_id`. Best-effort: creates parent directories
/// on the fly; logs to stderr on failure and returns.
pub fn save(app_id: &str, state: &WindowState) {
    let Some(path) = state_path(app_id) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("lumenc: window-state mkdir failed: {e}");
        return;
    }
    let body = match toml::to_string_pretty(state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lumenc: window-state serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, body) {
        eprintln!("lumenc: window-state write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let app_id = "lumenc-window-state-test";
        let state = WindowState {
            position: Some([100, 200]),
            size: Some([1024, 768]),
            maximized: false,
        };
        save(app_id, &state);
        let loaded = load(app_id);
        assert_eq!(loaded.position, Some([100, 200]));
        assert_eq!(loaded.size, Some([1024, 768]));
        assert!(!loaded.maximized);
    }
}
