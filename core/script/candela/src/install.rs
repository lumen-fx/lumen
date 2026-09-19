//! Installs a staged tree of files one rename at a time.
//!
//! This is the build script's install step. The script includes the file by
//! path; the library compiles it only under test, because cargo runs a build
//! script but never tests one. What the tests pin is the contract the script
//! leans on: an installed file is absent or complete, never half-written, and
//! a failed install leaves no temporary copy behind.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

/// Put every file under `from` at the same place under `to`.
///
/// Each file is copied under a temporary name beside its destination and
/// renamed over it, so another run of the build script installing the same
/// tree at the same moment never finds a file half-written, and a reader
/// never does either.
pub fn install_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            install_tree(&path, &target)?;
        } else {
            install_file(&path, &target)?;
        }
    }
    Ok(())
}

/// Put the file at `from` at `to`, replacing whatever is there.
///
/// A failed rename leaves nothing behind: the temporary copy is removed and
/// `to` is whatever it was before.
pub fn install_file(from: &Path, to: &Path) -> Result<(), String> {
    let Some(name) = to.file_name() else {
        return Err(format!("{} names no file", to.display()));
    };
    let mut staging = OsString::from(".");
    staging.push(name);
    staging.push(format!(".{}.tmp", std::process::id()));
    let staging = to.with_file_name(staging);
    fs::copy(from, &staging).map_err(|e| {
        format!(
            "cannot copy {} to {}: {e}",
            from.display(),
            staging.display()
        )
    })?;
    fs::rename(&staging, to).map_err(|e| {
        let _ = fs::remove_file(&staging);
        format!(
            "cannot move {} into place at {}: {e}",
            staging.display(),
            to.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{install_file, install_tree};

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-script-candela-install-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory");
        }
        fs::write(path, contents).expect("write");
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Every name under `dir`, recursively, relative to it.
    fn names(dir: &Path) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).expect("read_dir").flatten() {
                let path = entry.path();
                let relative = path.strip_prefix(root).expect("under root");
                out.push(relative.to_string_lossy().replace('\\', "/"));
                if path.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn installs_every_file_at_the_same_place_under_the_destination() {
        let dir = scratch("tree");
        let from = dir.join("from");
        let to = dir.join("to");
        write(&from.join("std/list.cdl"), "list");
        write(&from.join("std_src/math/math.so"), "math");
        write(&from.join("LICENSE.txt"), "license");

        install_tree(&from, &to).expect("install");

        assert_eq!(read(&to.join("std/list.cdl")), "list");
        assert_eq!(read(&to.join("std_src/math/math.so")), "math");
        assert_eq!(read(&to.join("LICENSE.txt")), "license");
        assert_eq!(
            names(&to),
            [
                "LICENSE.txt",
                "std",
                "std/list.cdl",
                "std_src",
                "std_src/math",
                "std_src/math/math.so",
            ],
            "no temporary copy survives the install"
        );
    }

    #[test]
    fn replaces_what_an_earlier_install_left() {
        let dir = scratch("replace");
        let from = dir.join("from");
        let to = dir.join("to");
        write(&from.join("std/list.cdl"), "first");
        install_tree(&from, &to).expect("first install");
        write(&from.join("std/list.cdl"), "second");

        install_tree(&from, &to).expect("second install");

        assert_eq!(read(&to.join("std/list.cdl")), "second");
        assert_eq!(names(&to), ["std", "std/list.cdl"]);
    }

    #[test]
    fn a_source_that_cannot_be_read_is_an_error() {
        let dir = scratch("unreadable-source");
        let error = install_tree(&dir.join("absent"), &dir.join("to")).expect_err("no source");
        assert!(error.starts_with("cannot read "), "{error}");
    }

    #[test]
    fn a_destination_without_a_file_name_is_an_error() {
        let dir = scratch("no-file-name");
        let file = dir.join("file");
        write(&file, "contents");
        let error = install_file(&file, Path::new("..")).expect_err("no file name");
        assert!(error.ends_with(" names no file"), "{error}");
    }

    #[test]
    fn a_missing_source_file_is_an_error_that_leaves_nothing_behind() {
        let dir = scratch("missing-source");
        let to = dir.join("to");
        fs::create_dir_all(&to).expect("destination");
        let error = install_file(&dir.join("absent"), &to.join("file")).expect_err("no source");
        assert!(error.starts_with("cannot copy "), "{error}");
        assert!(names(&to).is_empty(), "{:?}", names(&to));
    }

    #[test]
    fn a_destination_that_cannot_be_replaced_is_an_error_that_leaves_nothing_behind() {
        let dir = scratch("unreplaceable-destination");
        let file = dir.join("file");
        write(&file, "contents");
        let to = dir.join("to");
        let occupied = to.join("taken");
        write(&occupied.join("inner"), "keep");

        // A file cannot be renamed over a directory on any supported platform.
        let error = install_file(&file, &occupied).expect_err("rename over a directory");

        assert!(error.starts_with("cannot move "), "{error}");
        assert_eq!(read(&occupied.join("inner")), "keep");
        assert_eq!(
            names(&to),
            ["taken", "taken/inner"],
            "the temporary copy is gone"
        );
    }
}
