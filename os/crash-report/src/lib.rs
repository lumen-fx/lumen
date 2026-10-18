//! What a Lumen app and its `lumen-crash` helper agree on.
//!
//! The app sends the helper a few short messages while it runs (who it is,
//! which GPU it drew on, its recent log lines, any panic it hit), and asks it
//! for a minidump when it faults. The helper turns what it was told into the
//! readable `.txt` report written beside the dump. When no helper is running,
//! the app writes the same report itself. Both sides use the types here, so
//! the two reports cannot drift apart.
//!
//! The crate carries nothing of the engine: the helper links it, and the
//! helper has to start fast and sit small while it waits.

#![warn(missing_docs)]

pub mod os;
pub mod protocol;
mod report;
pub mod time;

pub use report::{Fault, Frame, Panic, Report};

use std::path::{Path, PathBuf};

/// The folder crash reports go in, under an app's data directory.
pub const CRASH_DIR: &str = "crashes";

/// How many log lines a report keeps, newest last.
pub const LOG_LINES: usize = 50;

/// The name of the helper executable, without the platform's extension.
pub const HELPER_NAME: &str = "lumen-crash";

/// The helper's file name on this platform: `lumen-crash`, or
/// `lumen-crash.exe` on Windows.
pub fn helper_file_name() -> String {
    format!("{HELPER_NAME}{}", std::env::consts::EXE_SUFFIX)
}

/// The crash folder of the app whose data directory is `data_dir`.
pub fn crash_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CRASH_DIR)
}

/// A file stem in `dir` no report uses yet, from the time of the crash:
/// `2026-10-18_14-03-22`, then `2026-10-18_14-03-22-2` and up when a second
/// crash lands in the same second. Checks both report extensions, so a
/// `.txt` and its `.dmp` always share one stem.
pub fn free_stem(dir: &Path, at: std::time::SystemTime) -> String {
    let base = time::file_stamp(at);
    let taken = |stem: &str| {
        dir.join(format!("{stem}.txt")).exists() || dir.join(format!("{stem}.dmp")).exists()
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|stem| !taken(stem))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_crash_in_the_same_second_gets_its_own_stem() {
        let dir = std::env::temp_dir().join(format!("lumen-crash-stem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_760_796_202);
        let first = free_stem(&dir, at);
        assert_eq!(first, "2025-10-18_14-03-22");
        std::fs::write(dir.join(format!("{first}.dmp")), b"").unwrap();
        assert_eq!(free_stem(&dir, at), "2025-10-18_14-03-22-2");
        std::fs::write(dir.join("2025-10-18_14-03-22-2.txt"), b"").unwrap();
        assert_eq!(free_stem(&dir, at), "2025-10-18_14-03-22-3");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_helper_is_named_for_the_platform() {
        let name = helper_file_name();
        assert!(name.starts_with("lumen-crash"));
        assert_eq!(cfg!(windows), name.ends_with(".exe"));
    }
}
