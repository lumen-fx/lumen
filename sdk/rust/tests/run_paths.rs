//! The `run` entry points end to end: the headless frame loop, the bounded
//! tick driver, and the fast failure a bad `[[plugins]]` declaration takes
//! before any window could exist. One app at a time, same as the FFI
//! headless suite: an app publishes process-global state.

use std::path::PathBuf;
use std::sync::Mutex;

use lumenui::prelude::*;

static APP_ISOLATION: Mutex<()> = Mutex::new(());

fn isolate() -> std::sync::MutexGuard<'static, ()> {
    APP_ISOLATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A minimal on-disk app; `plugins_toml` lands verbatim in its lumen.toml.
fn fixture(tag: &str, plugins_toml: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenui-run-paths-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label text=\"hi\"/></root>",
    )
    .unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[mcp]\nport = 0\n{plugins_toml}"),
    )
    .unwrap();
    dir
}

#[test]
fn a_headless_mode_run_completes() {
    let _guard = isolate();
    let dir = fixture("headless-mode", "");
    lumenui::App::new()
        .add_plugins(LumenDefaultPlugins.with_dir(&dir).build().headless())
        .run()
        .unwrap();
}

#[test]
fn a_bounded_headless_run_completes() {
    let _guard = isolate();
    let dir = fixture("bounded", "");
    lumenui::App::new()
        .add_plugins(LumenDefaultPlugins.with_dir(&dir))
        .run_headless(1)
        .unwrap();
}

#[test]
fn a_bad_plugin_declaration_fails_before_any_window() {
    let _guard = isolate();
    let dir = fixture(
        "bad-decl",
        "[[plugins]]\nname = \"x\"\ngit = \"https://example.com\"\n",
    );
    let err = lumenui::App::new()
        .add_plugins(LumenDefaultPlugins.with_dir(&dir))
        .run()
        .unwrap_err()
        .to_string();
    assert!(err.contains("not supported yet"), "{err}");

    // Same guarantee through the simple builder.
    let err = lumenui::simple::App::builder()
        .dir(&dir)
        .run()
        .unwrap_err()
        .to_string();
    assert!(err.contains("not supported yet"), "{err}");
}
