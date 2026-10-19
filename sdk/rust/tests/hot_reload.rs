//! A debug build's reload: the app boots from its compiled UI, as a release
//! build does, and an edit to a source under `src/` (the entry or a part it
//! includes) swaps the tree in while the app runs.
//!
//! The test builds the runtime options a debug `lumenui::app!()` hands over
//! (compiled bytes, the crate directory, `watch_sources`) and drives the
//! runtime's headless build. `LUMEN_HOT_RELOAD_POLL=1` selects the
//! deterministic mtime-diff driver so the test doesn't ride on cross-platform
//! fs-event timing.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use lumenui::lumen_runtime::RunOptions;
use lumenui::prelude::{EcsApp, TextContent};
// `build_headless_app` via the compiler wrapper so the injected parser is
// wired: a reload parses the edited sources with it.
use lumenui::lumenc::build_headless_app;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_sdk_hot_reload_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src").join("parts")).unwrap();
    // Disable the MCP server so the test never binds a TCP port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    dir
}

fn texts(app: &mut EcsApp) -> Vec<String> {
    let mut q = app.world.query::<&TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

/// Tick past the poll throttle (300 ms) a few times, so an edit is seen.
fn tick_through_a_poll(app: &mut EcsApp) {
    std::thread::sleep(Duration::from_millis(400));
    for _ in 0..5 {
        app.tick();
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_compiled_ui_reloads_from_its_sources() {
    // Deterministic mtime-diff driver (no notify/fs-event flakiness).
    // SAFETY: single-threaded test setup, before any app thread starts.
    unsafe {
        std::env::set_var("LUMEN_HOT_RELOAD_POLL", "1");
    }

    let dir = scratch_dir();
    // No `id` on the labels: the runtime's hot reload preserves per-`id`
    // TextContent (so input values survive edits), which would mask a text
    // change. An anonymous label reflects the freshly parsed markup.
    let entry = dir.join("src").join("main.lmn");
    let part = dir.join("src").join("parts").join("side.lmn");
    write(
        &entry,
        r#"<root><label text="ALPHA"/><include src="parts/side.lmn"/></root>"#,
    );
    write(&part, r#"<label text="LEFT"/>"#);

    let compiled = lumenui::lumenc::compile_app_ui(&dir).expect("the UI compiles");
    let bytes = lumenui::lumenc::artifact::serialize(&compiled).expect("the UI encodes");

    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.hot_reload = true;
    opts.watch_sources = true;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..3 {
        app.tick();
    }
    let first = texts(&mut app);
    assert!(
        first.iter().any(|t| t == "ALPHA") && first.iter().any(|t| t == "LEFT"),
        "the app starts from its compiled UI, got {first:?}"
    );

    // An edit to the entry reloads it from source.
    write(
        &entry,
        r#"<root><label text="BETA"/><include src="parts/side.lmn"/></root>"#,
    );
    tick_through_a_poll(&mut app);
    let after = texts(&mut app);
    assert!(
        after.iter().any(|t| t == "BETA") && !after.iter().any(|t| t == "ALPHA"),
        "an edit to the entry reloads, got {after:?}"
    );

    // So does an edit to a part the entry includes.
    write(&part, r#"<label text="RIGHT"/>"#);
    tick_through_a_poll(&mut app);
    let after = texts(&mut app);
    assert!(
        after.iter().any(|t| t == "RIGHT") && !after.iter().any(|t| t == "LEFT"),
        "an edit to an included part reloads, got {after:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Booted from bytes without `watch_sources`, nothing reloads: a packaged
/// app beside a stray `src/` keeps the UI it shipped with.
#[test]
fn compiled_bytes_without_watch_never_reload() {
    unsafe {
        std::env::set_var("LUMEN_HOT_RELOAD_POLL", "1");
    }
    let dir = scratch_dir();
    let entry = dir.join("src").join("main.lmn");
    write(&entry, r#"<root><label text="SHIPPED"/></root>"#);
    let compiled = lumenui::lumenc::compile_app_ui(&dir).expect("the UI compiles");
    let bytes = lumenui::lumenc::artifact::serialize(&compiled).expect("the UI encodes");

    let opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    assert!(opts.hot_reload && !opts.watch_sources);
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..3 {
        app.tick();
    }
    write(&entry, r#"<root><label text="STRAY"/></root>"#);
    tick_through_a_poll(&mut app);
    let after = texts(&mut app);
    assert!(
        after.iter().any(|t| t == "SHIPPED") && !after.iter().any(|t| t == "STRAY"),
        "a packaged app keeps its UI, got {after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
