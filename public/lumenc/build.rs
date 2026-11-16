//! Puts the candela standard library beside a `lumenc` installed from
//! crates.io.
//!
//! candela resolves `import "std/..."` against a `libs/` directory beside the
//! running executable. The candela host's build script stages that tree into
//! the build's profile directory, which is where it sits beside `lumenc` in a
//! checkout. `cargo install` keeps only the binary, so outside a checkout this
//! copies the staged tree to the directory the binary is installed into.
//!
//! The published `lumenc` runs the candela host's build script ahead of this
//! one (tools/release/bundle-crate.py), so the tree is staged by the time this
//! looks for it. In a checkout it does nothing.

use std::path::{Path, PathBuf};

/// Directory the script standard library sits in, beside the binaries that
/// read it. Matches `SCRIPT_LIBRARY_DIR` in `src/package/cli.rs`, the release
/// archives, and the Windows installer.
const SCRIPT_LIBRARY_DIR: &str = "libs";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_INSTALL_ROOT");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    if in_lumen_workspace(&manifest_dir) {
        return;
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    // OUT_DIR is <profile>/build/<package>-<hash>/out.
    let Some(profile) = out_dir.ancestors().nth(3) else {
        return;
    };
    let Some(bin_dir) = install_bin_dir() else {
        warn("cannot tell where lumenc is being installed. Set CARGO_INSTALL_ROOT");
        return;
    };
    let staged = profile.join(SCRIPT_LIBRARY_DIR);
    if let Err(message) = copy_tree(&staged, &bin_dir.join(SCRIPT_LIBRARY_DIR)) {
        warn(&format!(
            "{message}, so the candela standard library is not beside lumenc. \
             `import \"std/...\"` will not resolve until a {SCRIPT_LIBRARY_DIR} tree \
             is there or CANDELA_LIB_PATH names one"
        ));
    }
}

/// Whether this manifest is the one in a Lumen checkout rather than the
/// published package. The workspace root sits two directories up and lists
/// this crate as a member.
fn in_lumen_workspace(manifest_dir: &Path) -> bool {
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
fn install_bin_dir() -> Option<PathBuf> {
    let root = std::env::var_os("CARGO_INSTALL_ROOT")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        })?;
    Some(root.join("bin"))
}

/// Copy a directory whole, making what it needs on the way. The source is
/// read before the destination is made, so a build that staged nothing leaves
/// nothing behind.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
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

fn warn(message: &str) {
    println!("cargo:warning=lumenc: {message}");
}
