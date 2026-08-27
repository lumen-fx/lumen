//! Process-global app location: the directory an app was loaded from, and
//! the per-user directory it keeps saved data in.
//!
//! A script function that names a file runs outside the world, so it cannot
//! look the app up through a `&World`. It reads this cache instead, the way
//! [`crate::i18n`] and [`crate::window_state`] carry their own process-global
//! seams. The runtime publishes both values with [`set_app`] while it builds
//! the app, before any script host loads, so `on_start` already sees them.
//!
//! Until then the app directory reads as the process working directory and
//! the id as `lumen-app`, which keeps a bare host in a test or in
//! `lumenc check` resolving paths instead of failing.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The id an app falls back to before the runtime publishes a real one.
const DEFAULT_APP_ID: &str = "lumen-app";

#[derive(Debug, Clone)]
struct AppPaths {
    dir: PathBuf,
    id: String,
}

impl Default for AppPaths {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            id: DEFAULT_APP_ID.to_string(),
        }
    }
}

fn cell() -> &'static Mutex<AppPaths> {
    static PATHS: OnceLock<Mutex<AppPaths>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(AppPaths::default()))
}

/// Publish the app's directory and its stable id (`[app] id` in
/// `lumen.toml`, else the app directory name).
pub fn set_app(dir: impl Into<PathBuf>, id: impl Into<String>) {
    let (dir, id) = (dir.into(), id.into());
    if let Ok(mut p) = cell().lock() {
        p.dir = dir;
        p.id = id;
    }
}

/// The directory the app was loaded from.
pub fn app_dir() -> PathBuf {
    cell()
        .lock()
        .map(|p| p.dir.clone())
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// The app's stable id.
pub fn app_id() -> String {
    cell()
        .lock()
        .map(|p| p.id.clone())
        .unwrap_or_else(|_| DEFAULT_APP_ID.to_string())
}

/// Resolve a path an app author wrote: a relative path against the app
/// directory, an absolute path unchanged. It is the rule the runtime already
/// applies to the paths its script commands carry, so
/// `"data/tasks.json"` names the same file wherever the app was
/// started from.
pub fn resolve(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_relative() {
        app_dir().join(path)
    } else {
        path.to_path_buf()
    }
}

/// The directory this app keeps saved data in, created when missing.
///
/// The location follows the platform convention for user data:
/// `$XDG_DATA_HOME` (else `~/.local/share`) on Linux,
/// `~/Library/Application Support` on macOS, `%APPDATA%` on Windows. Each
/// app gets `lumen/<app-id>` under that root, so two apps sharing a machine
/// keep their saves apart. A machine that offers no per-user data root at
/// all falls back to the app directory.
pub fn data_dir() -> PathBuf {
    let dir = data_dir_for(&app_id());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::warn_line!("data_dir({}): {e}", dir.display());
    }
    dir
}

/// The directory a given app id keeps saved data in, without touching (or
/// creating) it and without going through the process-global published app.
///
/// Same platform root and shape as [`data_dir`] (`<root>/lumen/<id>`), but
/// takes the id explicitly: an OS-capability crate that scopes storage to
/// an app id of its own - rather than the current process's published one -
/// resolves its storage through this, so there is one definition of the
/// shape and not a second copy of it.
pub fn data_dir_for(id: &str) -> PathBuf {
    data_dir_at(data_root(), id)
}

/// Join a resolved root and an app id into the one path shape every
/// per-app data directory uses. Exposed so a caller that already holds an
/// override root (a test, an embedder) can place a directory the same way
/// [`data_dir_for`] does, instead of reimplementing the join.
pub fn data_dir_under(root: &Path, id: &str) -> PathBuf {
    root.join("lumen").join(id)
}

/// Place one app's data under a resolved root. Split out so the fallback
/// arm is reachable without a machine that lacks a data root.
fn data_dir_at(root: Option<PathBuf>, id: &str) -> PathBuf {
    match root {
        Some(root) => data_dir_under(&root, id),
        None => app_dir(),
    }
}

fn data_root() -> Option<PathBuf> {
    data_root_from(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
}

/// Read the per-user data root out of `env`. Takes the lookup as an
/// argument so tests drive the set and unset arms without writing to the
/// process environment.
fn data_root_from(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(d) = env("XDG_DATA_HOME") {
            return Some(PathBuf::from(d));
        }
        env("HOME").map(|h| Path::new(&h).join(".local").join("share"))
    }
    #[cfg(target_os = "macos")]
    {
        env("HOME").map(|h| Path::new(&h).join("Library").join("Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        env("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = env;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test for the whole module: the cache is process-global, so
    /// separate `#[test]` functions would race over the published app.
    #[test]
    fn paths_follow_the_published_app() {
        let dir = std::env::temp_dir().join(format!("lumen-app-paths-{}", std::process::id()));
        set_app(&dir, "lumen-core-app-paths-test");
        assert_eq!(app_dir(), dir);
        assert_eq!(app_id(), "lumen-core-app-paths-test");

        // Relative goes to the app, absolute is left alone.
        assert_eq!(resolve("tasks.json"), dir.join("tasks.json"));
        let absolute = dir.join("elsewhere.json");
        assert_eq!(resolve(&absolute), absolute);

        // A data root scopes the app under it; no root falls back to the app.
        let root = dir.join("share");
        assert_eq!(
            data_dir_at(Some(root.clone()), "lumen-core-app-paths-test"),
            root.join("lumen").join("lumen-core-app-paths-test")
        );
        assert_eq!(data_dir_at(None, "lumen-core-app-paths-test"), dir);

        // `data_dir_under` is the one join every per-app data directory goes
        // through; an id-scoped crate (an OS capability that is not the
        // published app) reaches the same shape through it directly.
        assert_eq!(
            data_dir_under(&root, "other-app"),
            root.join("lumen").join("other-app")
        );

        // The live directory exists once asked for, and sits under the app id.
        let live = data_dir();
        assert!(live.is_dir(), "{} was not created", live.display());
        assert!(live.ends_with("lumen-core-app-paths-test") || live == dir);
        if live != dir {
            let _ = std::fs::remove_dir(&live);
        }

        // A directory that cannot be made is reported, not raised: the caller
        // still gets a path, and the write it was for fails on its own terms.
        // Both the id and the fallback name something that cannot be a
        // directory, so the failure holds with or without a data root.
        let blocked = dir.join("not-a-directory");
        std::fs::create_dir_all(&dir).expect("app dir");
        std::fs::write(&blocked, "").expect("blocking file");
        set_app(&blocked, "cannot\0be-a-directory");
        assert!(!data_dir().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_data_root_reads_the_platform_variables() {
        let set = data_root_from(|key| match key {
            "XDG_DATA_HOME" | "HOME" | "APPDATA" => Some("/home/tester/share".to_string()),
            _ => None,
        });
        assert!(set.is_some(), "a populated environment resolves a root");
        assert!(set.unwrap().starts_with("/home/tester"));
        // Without the XDG override the home directory carries the root.
        let home_only =
            data_root_from(|key| (key != "XDG_DATA_HOME").then(|| "/home/tester".to_string()));
        assert!(home_only.is_some_and(|d| d.starts_with("/home/tester")));
        assert_eq!(data_root_from(|_| None), None);
        // The process environment path, whatever this machine reports.
        let _ = data_root();
    }
}
