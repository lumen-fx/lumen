//! `[dependencies]` version sources at the compile seam: the resolutions
//! `with_default_compiler_plugins` records into `RunOptions` for the
//! runtime's loader, which never resolves a version itself.

#![cfg(feature = "dev-run")]

use std::path::{Path, PathBuf};

use lumenc::{RunOptions, with_default_compiler_plugins};

fn app_dir(tag: &str, lumen_toml: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenc-resolve-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the app directory is writable");
    write(&dir, "src/main.lmn", "<root><label>hi</label></root>\n");
    write(&dir, "lumen.toml", lumen_toml);
    dir
}

fn write(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write file");
}

#[test]
fn an_app_without_version_sources_records_nothing() {
    let dir = app_dir(
        "none",
        "[dependencies]\nlocal = { path = \"modules/local\" }\n",
    );
    let opts = with_default_compiler_plugins(RunOptions::new(&dir)).expect("options build");
    assert!(
        opts.resolved_modules.0.is_empty(),
        "a path source resolves at load, not here"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unresolvable_version_is_recorded_not_fatal() {
    let dir = app_dir(
        "unresolvable",
        "[dependencies]\nlumenc-test-never-cached = \"9.9\"\n",
    );
    let opts = with_default_compiler_plugins(RunOptions::new(&dir)).expect("options build");
    let outcome = opts
        .resolved_modules
        .0
        .get("lumenc-test-never-cached")
        .expect("the version source was recorded");
    let reason = outcome
        .as_ref()
        .expect_err("nothing is cached under that name");
    assert!(reason.contains("no cached version matches"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_lock_fails_every_version_entry_with_its_reason() {
    let dir = app_dir("broken-lock", "[dependencies]\na = \"1\"\nb = \"2\"\n");
    write(&dir, "lumen.lock", "not [ toml");
    let opts = with_default_compiler_plugins(RunOptions::new(&dir)).expect("options build");
    assert_eq!(opts.resolved_modules.0.len(), 2);
    for name in ["a", "b"] {
        let reason = opts.resolved_modules.0[name]
            .as_ref()
            .expect_err("a lock that does not parse fails the entry");
        assert!(reason.contains("lumen.lock"), "{reason}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
