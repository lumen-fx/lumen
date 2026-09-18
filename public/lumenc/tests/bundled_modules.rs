//! Every first-party runtime module is compiled into the dev binary.
//!
//! An app declaring `lumen-audio = { bundled = true }` gets the module's
//! script functions from the registry its constructor writes to before
//! `main`, which only happens when the crate is on the link line. Cargo
//! cannot catch a module added under `std/` and not named here: an unnamed
//! dependency links nothing, and the app then starts without the functions
//! it declared, so the check is this test rather than the compiler.

#![cfg(all(feature = "runtime-parse", feature = "dev-run"))]

use std::path::{Path, PathBuf};

/// The workspace root, two levels above this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is readable")
}

/// The crate identifier each directory under `std/` builds as, which is the
/// spelling an anchor uses.
fn first_party_modules(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("std"))
        .expect("std/ is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .map(|path| {
            let dir = path
                .file_name()
                .expect("a named directory")
                .to_string_lossy()
                .to_string();
            format!("lumen_{}", dir.replace('-', "_"))
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_first_party_module_is_anchored() {
    let root = root();
    let main = std::fs::read_to_string(root.join("public/lumenc/src/main.rs"))
        .expect("the binary's source is readable");
    let missing: Vec<String> = first_party_modules(&root)
        .into_iter()
        .filter(|krate| !main.contains(&format!("use {krate} as _;")))
        .collect();
    assert!(
        missing.is_empty(),
        "these modules under std/ are not on the dev binary's link line, so an app \
         declaring one starts without it: {}",
        missing.join(", ")
    );
}
