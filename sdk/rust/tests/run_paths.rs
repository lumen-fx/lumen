//! The `run` entry points end to end: a compiled app's bounded run, a
//! compiled app that runs with its sources gone, and the fast failure a bad
//! `[[plugins]]` declaration takes before any window could exist. One app at a
//! time, same as the FFI headless suite: an app publishes process-global
//! state.

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
fn a_bounded_headless_run_completes() {
    let _guard = isolate();
    let dir = fixture("bounded", "");
    App::new(CompiledUi::compile(&dir).expect("the UI compiles"))
        .run_headless(1)
        .unwrap();
}

/// The UI travels in the compiled artifact: with the app's `src/` gone (a
/// package that ships only the executable and its assets) and started from
/// an unrelated directory, an app whose stylesheet `@import`s another and
/// whose markup includes a part still runs, and shows the part.
#[test]
fn a_compiled_app_runs_without_its_sources() {
    let _guard = isolate();
    let dir = fixture("no-sources", "");
    std::fs::create_dir_all(dir.join("src").join("parts")).unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><include src=\"parts/panel.lmn\"/></root>",
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("parts").join("panel.lmn"),
        "<label id=\"from-part\" text=\"part\"/>",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.css"), "@import \"theme.css\";").unwrap();
    std::fs::write(
        dir.join("src").join("theme.css"),
        "label { color: #ff0000; }",
    )
    .unwrap();
    let ui = CompiledUi::compile(&dir).expect("the UI compiles");
    std::fs::remove_dir_all(dir.join("src")).unwrap();

    let elsewhere = std::env::temp_dir().join(format!(
        "lumenui-run-paths-{}-elsewhere",
        std::process::id()
    ));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let was = std::env::current_dir().unwrap();
    std::env::set_current_dir(&elsewhere).unwrap();

    static SEEN: Mutex<bool> = Mutex::new(false);
    fn record(ids: Query<&LumenId>) {
        if ids.iter().any(|id| id.0 == "from-part") {
            *SEEN.lock().unwrap() = true;
        }
    }
    let run = App::new(ui)
        .add_systems(TickStage::Systems, record)
        .run_headless(2);
    std::env::set_current_dir(was).unwrap();
    run.unwrap();
    assert!(*SEEN.lock().unwrap(), "the included part is in the tree");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_plugin_declaration_fails_before_any_window() {
    let _guard = isolate();
    let dir = fixture(
        "bad-decl",
        "[[plugins]]\nname = \"x\"\ngit = \"https://example.com\"\n",
    );
    let err = App::from_markup("<root/>")
        .app_dir(&dir)
        .run()
        .unwrap_err()
        .to_string();
    assert!(err.contains("not supported yet"), "{err}");

    // The UI compile refuses it the same way.
    let err = CompiledUi::compile(&dir).unwrap_err().to_string();
    assert!(err.contains("not supported yet"), "{err}");
}
