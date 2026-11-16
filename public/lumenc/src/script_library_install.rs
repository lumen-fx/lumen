//! Where a `lumenc` installed from crates.io gets the candela standard
//! library.
//!
//! candela resolves `import "std/..."` against a `libs/` directory beside the
//! running executable. The candela host's build script stages that tree into
//! the build's profile directory, which is where it sits beside `lumenc` in a
//! checkout. `cargo install` keeps only the binary, so outside a checkout the
//! build script copies the staged tree to the directory the binary is
//! installed into.
//!
//! The build script includes this file by path; the library compiles it only
//! under test, because cargo runs a build script but never tests one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Directory the script standard library sits in, beside the binaries that
/// read it. Matches `SCRIPT_LIBRARY_DIR` in `src/package/cli.rs`, the release
/// archives, and the Windows installer.
pub const SCRIPT_LIBRARY_DIR: &str = "libs";

/// Copy the staged library beside the installed binary. `var` reads the
/// environment. Returns where it went, or `None` in a checkout, where there
/// is nothing to do.
pub fn place(
    manifest_dir: &Path,
    out_dir: &Path,
    var: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    if in_lumen_workspace(manifest_dir) {
        return Ok(None);
    }
    // OUT_DIR is <profile>/build/<package>-<hash>/out.
    let profile = out_dir
        .ancestors()
        .nth(3)
        .ok_or_else(|| format!("{} is not inside a build profile", out_dir.display()))?;
    let bin_dir = install_bin_dir(var)
        .ok_or("cannot tell where lumenc is being installed. Set CARGO_INSTALL_ROOT")?;
    let dest = bin_dir.join(SCRIPT_LIBRARY_DIR);
    copy_tree(&profile.join(SCRIPT_LIBRARY_DIR), &dest)?;
    Ok(Some(dest))
}

/// The warning for a library that did not land beside `lumenc`.
pub fn not_placed(cause: &str) -> String {
    format!(
        "{cause}, so the candela standard library is not beside lumenc. \
         `import \"std/...\"` will not resolve until a {SCRIPT_LIBRARY_DIR} tree \
         is there or CANDELA_LIB_PATH names one"
    )
}

/// Whether this manifest is the one in a Lumen checkout rather than the
/// published package. The workspace root sits two directories up and lists
/// this crate as a member.
pub fn in_lumen_workspace(manifest_dir: &Path) -> bool {
    let Some(root) = manifest_dir.parent().and_then(Path::parent) else {
        return false;
    };
    let Ok(manifest) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return false;
    };
    manifest.contains("[workspace]") && manifest.contains("public/lumenc")
}

/// Where the installed `lumenc` will land. `cargo install --root` is not
/// visible to a build script, but the environment variable that does the same
/// thing is, and `CARGO_HOME` (or its default) covers the rest.
pub fn install_bin_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |key: &str| var(key).filter(|v| !v.is_empty());
    let root = set("CARGO_INSTALL_ROOT")
        .or_else(|| set("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| {
            set("HOME")
                .or_else(|| set("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        })?;
    Some(root.join("bin"))
}

/// Copy a directory whole, making what it needs on the way. The source is
/// read before the destination is made, so a build that staged nothing leaves
/// nothing behind.
pub fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))?;
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", from.display()))?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &dest)?;
        } else {
            std::fs::copy(&source, &dest)
                .map_err(|e| format!("copy {} -> {}: {e}", source.display(), dest.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_script_library_install_{}_{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_constant_matches_the_packager() {
        assert_eq!(SCRIPT_LIBRARY_DIR, "libs");
    }

    #[test]
    fn this_checkout_is_a_lumen_workspace() {
        assert!(in_lumen_workspace(Path::new(env!("CARGO_MANIFEST_DIR"))));
    }

    #[test]
    fn an_unpacked_package_is_not() {
        let dir = scratch("unpacked").join("registry").join("lumenc-0.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!in_lumen_workspace(&dir));
        assert!(!in_lumen_workspace(Path::new("/")));
    }

    #[test]
    fn the_install_root_wins_over_cargo_home_and_home() {
        let bin = install_bin_dir(env(&[
            ("CARGO_INSTALL_ROOT", "/root"),
            ("CARGO_HOME", "/cargo"),
            ("HOME", "/home/me"),
        ]));
        assert_eq!(bin, Some(PathBuf::from("/root/bin")));
        let bin = install_bin_dir(env(&[("CARGO_INSTALL_ROOT", ""), ("CARGO_HOME", "/cargo")]));
        assert_eq!(bin, Some(PathBuf::from("/cargo/bin")));
        let bin = install_bin_dir(env(&[("USERPROFILE", "C:/Users/me")]));
        assert_eq!(bin, Some(PathBuf::from("C:/Users/me/.cargo/bin")));
        assert_eq!(install_bin_dir(env(&[])), None);
    }

    #[test]
    fn a_staged_library_lands_beside_the_installed_binary() {
        let root = scratch("place");
        let profile = root.join("target").join("release");
        let out_dir = profile.join("build").join("lumenc-abc").join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let staged = profile.join(SCRIPT_LIBRARY_DIR).join("std");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("math.cdl"), "fn sqrt() {}").unwrap();
        let manifest_dir = root.join("pkg");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        let install = root.join("install").display().to_string();

        let placed = place(
            &manifest_dir,
            &out_dir,
            env(&[("CARGO_INSTALL_ROOT", &install)]),
        )
        .unwrap()
        .expect("placed outside a checkout");
        assert_eq!(placed, root.join("install").join("bin").join("libs"));
        assert_eq!(
            std::fs::read_to_string(placed.join("std").join("math.cdl")).unwrap(),
            "fn sqrt() {}"
        );
    }

    #[test]
    fn a_checkout_places_nothing() {
        let out = scratch("checkout");
        let placed = place(Path::new(env!("CARGO_MANIFEST_DIR")), &out, env(&[])).unwrap();
        assert_eq!(placed, None);
    }

    #[test]
    fn nothing_staged_is_an_error_that_leaves_no_directory() {
        let root = scratch("unstaged");
        let out_dir = root.join("release").join("build").join("x").join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let install = root.join("install").display().to_string();
        let err = place(&root, &out_dir, env(&[("CARGO_INSTALL_ROOT", &install)])).unwrap_err();
        assert!(err.contains("libs"), "{err}");
        assert!(!root.join("install").join("bin").join("libs").exists());
        let warning = not_placed(&err);
        assert!(warning.contains("CANDELA_LIB_PATH"), "{warning}");
    }

    #[test]
    fn no_install_root_is_reported() {
        let root = scratch("no-root");
        let out_dir = root.join("release").join("build").join("x").join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let err = place(&root, &out_dir, env(&[])).unwrap_err();
        assert!(err.contains("CARGO_INSTALL_ROOT"), "{err}");
    }

    #[test]
    fn a_shallow_out_dir_is_reported() {
        let root = scratch("shallow");
        let err = place(&root, Path::new("out"), env(&[])).unwrap_err();
        assert!(err.contains("build profile"), "{err}");
    }
}
