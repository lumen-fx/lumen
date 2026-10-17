//! The `run` entry points end to end: the headless frame loop, the bounded
//! tick driver, and the fast failure a bad `[[plugins]]` declaration takes
//! before any window could exist. One app at a time, same as the FFI
//! headless suite: an app publishes process-global state.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

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

/// #525: a release build embeds the entry markup and stylesheet and reads the
/// rest of the app from its app directory, not from wherever it was started.
/// Started from an unrelated directory, the stylesheet's `@import` still
/// resolves.
#[test]
fn embedded_sources_read_imports_from_the_app_not_the_working_dir() {
    let _guard = isolate();
    let dir = fixture("embedded-imports", "");
    std::fs::write(
        dir.join("src").join("theme.css"),
        "label { color: #ff0000; }",
    )
    .unwrap();
    let elsewhere = std::env::temp_dir().join(format!(
        "lumenui-run-paths-{}-elsewhere",
        std::process::id()
    ));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let was = std::env::current_dir().unwrap();
    std::env::set_current_dir(&elsewhere).unwrap();
    let run = lumenui::App::new()
        .add_plugins(LumenDefaultPlugins.with_source(Source::embedded_from(
            dir.join("src").join("main.lmn"),
            "<root><label text=\"hi\"/></root>",
            Some("@import \"theme.css\";"),
        )))
        .run_headless(1);
    std::env::set_current_dir(was).unwrap();
    run.unwrap();
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
