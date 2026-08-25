//! Proves the SDK's *debug* UI-source path is genuinely disk-backed and
//! live-reloaded by the runtime's own watcher - not a compile-time-frozen
//! copy.
//!
//! `lumen_source!` in a debug build produces a [`Source::Disk`], which the
//! builder turns into a `RunOptions` with `markup: None`, `dir: <app dir>`,
//! `hot_reload: true`. This test reconstructs exactly that shape, drives the
//! runtime's headless build (the same `build_headless_app` the golden suite
//! uses), and confirms:
//!
//! 1. startup reads `src/main.lmn` from disk (the label text comes from the
//!    file),
//! 2. editing `src/main.lmn` on disk while the app is live swaps the tree in -
//!    reusing the runtime's `hot_reload` system, no watcher of our own.
//!
//! `LUMEN_HOT_RELOAD_POLL=1` selects the deterministic mtime-diff driver so the
//! test doesn't ride on cross-platform fs-event timing.

use lumenui::components::TextContent;
use lumenui::ecs_app::App as EcsApp;
use lumenui::runtime::RunOptions;
// `build_headless_app` via the compiler wrapper so the injected parser is
// wired (the bare `lumen_runtime` entry links no parser and would return
// `ParserDisabled` on a from-source load).
use lumenui::lumenc::build_headless_app;
use std::path::PathBuf;
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
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // Disable the MCP server so the test never binds a TCP port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    dir
}

fn texts(app: &mut EcsApp) -> Vec<String> {
    let mut q = app.world.query::<&TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

#[test]
fn debug_source_reads_from_disk_and_hot_reloads() {
    // Deterministic mtime-diff driver (no notify/fs-event flakiness).
    // SAFETY: single-threaded test setup, before any app thread starts.
    unsafe {
        std::env::set_var("LUMEN_HOT_RELOAD_POLL", "1");
    }

    let dir = scratch_dir();
    // No `id` on the label: the runtime's hot reload preserves per-`id`
    // TextContent (so input values survive edits), which would mask a text
    // change. An anonymous label reflects the freshly parsed markup.
    let entry = dir.join("src").join("main.lmn");
    std::fs::write(&entry, r#"<root><label text="ALPHA"/></root>"#).unwrap();

    // Exactly the RunOptions shape `Source::Disk` yields: dir set, no in-memory
    // markup, hot reload on.
    let opts = RunOptions::new(&dir);
    assert!(opts.markup.is_none());
    assert!(opts.hot_reload);
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");

    for _ in 0..3 {
        app.tick();
    }
    assert!(
        texts(&mut app).iter().any(|t| t == "ALPHA"),
        "startup must load markup from disk, got {:?}",
        texts(&mut app)
    );

    // Edit the file on disk. The poll throttle is 300 ms wall-clock; wait it
    // out so the next tick's mtime sweep is due, then tick.
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(&entry, r#"<root><label text="BETA"/></root>"#).unwrap();
    for _ in 0..5 {
        app.tick();
        std::thread::sleep(Duration::from_millis(80));
    }

    let after = texts(&mut app);
    assert!(
        after.iter().any(|t| t == "BETA"),
        "hot reload must pick up the on-disk edit, got {after:?}"
    );
    assert!(
        !after.iter().any(|t| t == "ALPHA"),
        "stale markup must be gone after reload, got {after:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
