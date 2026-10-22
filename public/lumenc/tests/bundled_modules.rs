//! Every first-party runtime module is compiled into the dev binary.
//!
//! An app declaring `lumen-audio = { bundled = true }` gets the module's
//! script functions from the registry its constructor writes to before
//! `main`, which only happens when the crate is on the link line. The same
//! goes for a script host, which an app's script implies rather than
//! declares. Cargo cannot catch a module added under `std/` and not named
//! here: an unnamed dependency links nothing, and the app then starts without
//! the functions it declared, so the check is this test rather than the
//! compiler.

#![cfg(all(feature = "runtime-parse", feature = "dev-run"))]

use std::path::{Path, PathBuf};

/// The workspace root, two levels above this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is readable")
}

/// The directories directly under `dir`.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("the directory is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.is_dir())
        .collect()
}

/// The crate identifier each module under `std/` builds as, which is the
/// spelling an anchor uses. A module is a crate that depends on
/// `lumen-module`; one nested in another module's directory
/// (`std/candela/dev`) counts too.
fn first_party_modules(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = subdirs(&root.join("std"))
        .into_iter()
        .flat_map(|dir| {
            let nested = subdirs(&dir);
            std::iter::once(dir).chain(nested)
        })
        .filter_map(|path| {
            let text = std::fs::read_to_string(path.join("Cargo.toml")).ok()?;
            let manifest: toml::Table = toml::from_str(&text).expect("Cargo.toml parses");
            manifest.get("dependencies")?.get("lumen-module")?;
            let package = manifest["package"]["name"]
                .as_str()
                .expect("a package name");
            Some(package.replace('-', "_"))
        })
        .collect();
    names.sort();
    names
}

/// The text of the dev binary's anchors: its own source and the library's,
/// which anchors what every in-process run through it needs.
fn anchors(root: &Path) -> String {
    ["public/lumenc/src/main.rs", "public/lumenc/src/lib.rs"]
        .iter()
        .map(|file| std::fs::read_to_string(root.join(file)).expect("the source is readable"))
        .collect()
}

#[test]
fn every_first_party_module_is_anchored() {
    let root = root();
    let anchors = anchors(&root);
    let missing: Vec<String> = first_party_modules(&root)
        .into_iter()
        .filter(|krate| !anchors.contains(&format!("use {krate} as _;")))
        .collect();
    assert!(
        missing.is_empty(),
        "these modules under std/ are not on the dev binary's link line, so an app \
         declaring one starts without it: {}",
        missing.join(", ")
    );
}
