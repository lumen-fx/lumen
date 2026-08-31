//! Points this checkout at `.githooks` the first time the workspace is built.
//!
//! A hook that has to be installed by hand is a hook a fresh clone does not
//! have, and the gates it runs are the ones CI will fail on anyway. So the
//! registration rides along with the build every contributor already runs.
//!
//! It is deliberately timid. It writes one git config key, only in a checkout
//! of this repository, only when nothing else has claimed that key, and never
//! in CI. Anyone who has pointed `core.hooksPath` somewhere of their own keeps
//! it, and `LUMEN_NO_HOOK_SETUP=1` turns the whole thing off.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LUMEN_NO_HOOK_SETUP");

    if std::env::var_os("LUMEN_NO_HOOK_SETUP").is_some_and(|v| !v.is_empty()) {
        return;
    }
    // CI clones fresh every run and commits nothing, so a hook there is pure
    // cost. It would also write to a config file the runner throws away.
    if std::env::var_os("CI").is_some() {
        return;
    }

    let Some(root) = workspace_root() else {
        return;
    };
    println!("cargo:rerun-if-changed={}", root.join(HOOKS).display());

    // A repository, not a source tree someone vendored or a package cargo
    // unpacked. `.git` is a directory in a clone and a file in a worktree, so
    // both count.
    if !root.join(".git").exists() || !root.join(HOOKS).join("pre-commit").is_file() {
        return;
    }

    // A key that is already set is left exactly as it is, whether it names
    // these hooks or someone's own. Where a person points their hooks is a
    // preference, not a mistake to correct.
    if hooks_path(&root).is_none() {
        set_hooks_path(&root);
    }
}

/// Where the hooks live, relative to the top of the working tree, which is how
/// git resolves a relative `core.hooksPath`.
const HOOKS: &str = ".githooks";

/// The top of the repository: this crate sits two directories below it.
fn workspace_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

/// What `core.hooksPath` is set to now, or `None` when it is unset or git
/// cannot be reached at all.
fn hooks_path(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn set_hooks_path(root: &Path) {
    let done = Command::new("git")
        .args(["config", "core.hooksPath", HOOKS])
        .current_dir(root)
        .status()
        .is_ok_and(|s| s.success());
    if done {
        println!("cargo:warning=git hooks registered: core.hooksPath = {HOOKS}");
    }
}
