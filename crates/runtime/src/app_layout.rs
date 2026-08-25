//! Where an app keeps its code.
//!
//! An app is `lumen.toml` at its root and every code file - the markup, the
//! stylesheet and the scripts - under `src/`, the same split a Cargo package
//! uses. Assets, `locale/`, build outputs and anything a `[[hooks]]` command
//! reads or writes stay at the root; `src/` is only what the compiler front-end
//! opens. One root directory has a meaning of its own: `lib/`, where a script's
//! `dylib` import finds its shared library.
//!
//! [`AppLayout::resolve`] is the one place that turns an app directory plus its
//! `lumen.toml` into the paths the rest of the runtime reads, and the one place
//! that rejects an app whose code is still at its root.

use std::path::{Path, PathBuf};

use crate::config::LumenToml;

/// Directory name an app keeps its code in.
pub const SRC: &str = "src";

/// Directory name an app keeps its native shared libraries in.
pub const LIB: &str = "lib";

/// Default markup entry, used when `[app] entry` is absent.
const DEFAULT_ENTRY: &str = "main.lmn";

/// The app stylesheet, always beside the entry.
const STYLESHEET: &str = "main.css";

/// Extensions that make a file code rather than an asset.
const CODE_EXTS: [&str; 5] = ["lmn", "css", "rhai", "lua", "cdl"];

/// How many offending names the flat-layout message lists before it stops.
const LISTED_FILES: usize = 3;

/// The code paths of one app directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLayout {
    /// `<app>/src`, holding the markup, the stylesheet and the scripts.
    pub src_dir: PathBuf,
    /// The markup entry: `[app] entry` (default `main.lmn`) under `src/`.
    pub entry_path: PathBuf,
    /// The app stylesheet, `src/main.css`. Optional on disk.
    pub css_path: PathBuf,
    /// `<app>/lib`, where a script's `dylib` import finds its shared library
    /// and where a `[[hooks]]` command that builds one writes it. Optional on
    /// disk.
    pub lib_dir: PathBuf,
}

impl AppLayout {
    /// The paths for `dir`, with no look at what is on disk.
    ///
    /// For sources that never came from the app directory: in-memory markup
    /// handed in by an SDK or a test, and a precompiled artifact, which carries
    /// its code inside itself. Everything that reads the directory calls
    /// [`Self::resolve`] instead.
    pub fn of(dir: &Path, cfg: &LumenToml) -> Self {
        let src_dir = src_dir(dir);
        Self {
            entry_path: src_dir.join(cfg.app.entry.as_deref().unwrap_or(DEFAULT_ENTRY)),
            css_path: src_dir.join(STYLESHEET),
            lib_dir: dir.join(LIB),
            src_dir,
        }
    }

    /// The paths for `dir`, rejecting an app whose code is still at its root.
    ///
    /// A directory with no code in it at all resolves fine: a packaged app has
    /// its code compiled into the executable, and only its assets travel.
    pub fn resolve(dir: &Path, cfg: &LumenToml) -> Result<Self, FlatLayout> {
        let layout = Self::of(dir, cfg);
        let stray = code_files(dir);
        if !stray.is_empty() && code_files(&layout.src_dir).is_empty() {
            return Err(FlatLayout {
                dir: dir.to_path_buf(),
                files: stray,
            });
        }
        Ok(layout)
    }
}

/// The directory `dir`'s code lives in.
pub fn src_dir(dir: &Path) -> PathBuf {
    dir.join(SRC)
}

/// The app's code sits at its root instead of under `src/`.
///
/// The message is the one migration hint: every path that resolves a layout
/// reports this text, so an author sees the same instruction from `run`,
/// `check`, `build`, `package` and the C ABI alike.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{}: found code files at the app root ({}). Lumen apps keep their code \
     under src/. Move the .lmn, .css and script files into src/; assets and \
     locale/ stay at the app root.",
    .dir.display(),
    .files.join(", ")
)]
pub struct FlatLayout {
    /// The app directory the code was found in.
    pub dir: PathBuf,
    /// The offending file names, sorted, up to the handful the message lists.
    pub files: Vec<String>,
}

/// The code files directly in `dir`, sorted, capped at what the message lists.
///
/// Non-recursive on purpose: a nested directory of markup is what `src/` is,
/// and an asset tree can be arbitrarily deep.
fn code_files(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| CODE_EXTS.contains(&e))
        })
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    found.sort();
    found.truncate(LISTED_FILES);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-app-layout-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, body).expect("write file");
    }

    #[test]
    fn the_entry_and_the_stylesheet_resolve_under_src() {
        let dir = scratch("under-src");
        write(&dir, "src/main.lmn", "<root/>");
        let layout = AppLayout::resolve(&dir, &LumenToml::default()).expect("src layout resolves");
        assert_eq!(layout.src_dir, dir.join("src"));
        assert_eq!(layout.entry_path, dir.join("src").join("main.lmn"));
        assert_eq!(layout.css_path, dir.join("src").join("main.css"));
        // Native libraries are a build output, so they sit at the root beside
        // the sources a hook builds them from.
        assert_eq!(layout.lib_dir, dir.join("lib"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_configured_entry_joins_src() {
        let dir = scratch("entry-key");
        write(&dir, "src/home.lmn", "<root/>");
        let cfg: LumenToml = toml::from_str("[app]\nentry = \"home.lmn\"\n").expect("parse config");
        let layout = AppLayout::resolve(&dir, &cfg).expect("src layout resolves");
        assert_eq!(layout.entry_path, dir.join("src").join("home.lmn"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_at_the_root_is_an_error_naming_the_files() {
        let dir = scratch("flat");
        write(&dir, "main.lmn", "<root/>");
        write(&dir, "main.css", "root { bg: #000; }");
        let err =
            AppLayout::resolve(&dir, &LumenToml::default()).expect_err("flat app is rejected");
        assert_eq!(err.files, vec!["main.css", "main.lmn"]);
        let msg = err.to_string();
        assert!(msg.contains("main.lmn"), "{msg}");
        assert!(msg.contains("under src/"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_with_no_code_resolves() {
        // A packaged app: its code is inside the executable, its assets
        // travel beside it.
        let dir = scratch("packaged");
        write(&dir, "icons/app.png", "");
        write(&dir, "lumen.toml", "[app]\nid = \"demo\"\n");
        AppLayout::resolve(&dir, &LumenToml::default()).expect("an asset-only directory resolves");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_migrated_app_may_keep_stray_code_at_its_root() {
        // Once `src/` holds the app's code, a leftover file at the root is
        // something the app does not load rather than a broken layout.
        let dir = scratch("migrated");
        write(&dir, "src/main.lmn", "<root/>");
        write(&dir, "notes.css", "");
        AppLayout::resolve(&dir, &LumenToml::default()).expect("the src layout wins");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_sdk_source_tree_alone_does_not_count_as_migrated() {
        // `src/main.rs` is the SDK app's own build source. The markup beside
        // it is still at the root, so the app is flat.
        let dir = scratch("sdk-flat");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "main.lmn", "<root/>");
        AppLayout::resolve(&dir, &LumenToml::default()).expect_err("flat app is rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
