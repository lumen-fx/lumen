//! Every first-party runtime module meant for the dev binary is compiled into
//! it, and the ones meant for nothing are not.
//!
//! An app declaring `lumen-audio = { bundled = true }` gets the module's
//! script functions from the registry its constructor writes to before
//! `main`, which only happens when the crate is on the link line. Cargo
//! cannot catch a module added under `std/` and not named here: an unnamed
//! dependency links nothing, and the app then starts without the functions
//! it declared, so the check is this test rather than the compiler.
//!
//! A module's manifest says where it is compiled in with
//! `[package.metadata.lumen] link`: absent or `"toolchain"` puts it in the
//! dev binary, `"none"` keeps it out of everything (it ships in the modules
//! archive alone).

#![cfg(all(feature = "runtime-parse", feature = "dev-run"))]

use std::path::{Path, PathBuf};

/// The workspace root, two levels above this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is readable")
}

/// Each crate under `std/`: the identifier it builds as, which is the
/// spelling an anchor uses, and the `link` value its manifest sets.
fn first_party_modules(root: &Path) -> Vec<(String, Option<String>)> {
    let mut modules: Vec<(String, Option<String>)> = std::fs::read_dir(root.join("std"))
        .expect("std/ is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .map(|path| {
            let dir = path
                .file_name()
                .expect("a named directory")
                .to_string_lossy()
                .to_string();
            let manifest: toml::Table = toml::from_str(
                &std::fs::read_to_string(path.join("Cargo.toml")).expect("a readable manifest"),
            )
            .expect("a manifest that parses");
            let link = manifest
                .get("package")
                .and_then(|p| p.get("metadata"))
                .and_then(|m| m.get("lumen"))
                .and_then(|l| l.get("link"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            (format!("lumen_{}", dir.replace('-', "_")), link)
        })
        .collect();
    modules.sort();
    modules
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
        .filter(|(_, link)| link.as_deref() != Some("none"))
        .map(|(krate, _)| krate)
        .filter(|krate| !anchors.contains(&format!("use {krate} as _;")))
        .collect();
    assert!(
        missing.is_empty(),
        "these modules under std/ are not on the dev binary's link line, so an app \
         declaring one starts without it: {}",
        missing.join(", ")
    );
}

#[test]
fn a_module_meant_for_nothing_is_not_anchored() {
    let root = root();
    let anchors = anchors(&root);
    let anchored: Vec<String> = first_party_modules(&root)
        .into_iter()
        .filter(|(_, link)| link.as_deref() == Some("none"))
        .map(|(krate, _)| krate)
        .filter(|krate| anchors.contains(&format!("use {krate} as _;")))
        .collect();
    assert!(
        anchored.is_empty(),
        "these modules say they are compiled into nothing, and the dev binary links \
         them in: {}",
        anchored.join(", ")
    );
}
