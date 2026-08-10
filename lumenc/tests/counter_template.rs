// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! End-to-end proof that the `counter` template runs as scaffolded.
//!
//! `lumenc new <dir> counter` writes a candela script, and the top-level README and
//! the getting-started walkthrough both quote it verbatim. This test writes the
//! template to a temp dir exactly as the scaffolder does, boots it through
//! the headless plugin stack (no window, no GPU), clicks `bump` and `reset`,
//! and asserts the `bind-text` label follows the `clicks` signal. A template
//! whose script fails to compile, or whose per-id routes never register,
//! leaves the label on its markup default and fails here.

use bevy_ecs::prelude::Entity;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent};
use lumen_core::input::{ClickEvent, PointerButton};
use lumenc::{RunOptions, build_headless_app};

/// Write the `counter` template into an isolated temp dir, with the MCP
/// server disabled (`port = 0`) so the test never binds a TCP port.
fn scaffolded_counter() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenc-counter-template-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir temp app");
    for (path, body) in lumenc::scaffold::COUNTER {
        std::fs::write(dir.join(path), body).unwrap_or_else(|e| panic!("write {path}: {e}"));
    }
    // Same [app] / [window] config the template ships, plus the port pin.
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Counter\"\nsize = [480, 360]\n\n\
         [mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    dir
}

/// Every `TextContent` string currently in the world.
fn all_texts(app: &mut App) -> Vec<String> {
    let mut q = app.world.query::<&TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

/// Simulate a click on the entity carrying `LumenId(id)`.
fn click_on(app: &mut App, id: &str) {
    let target = {
        let mut q = app.world.query::<(Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, lid)| lid.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity with LumenId {id:?}"))
    };
    app.world.write_message(ClickEvent {
        entity: target,
        position: glam::Vec2::ZERO,
        button: PointerButton::Primary,
    });
}

#[test]
fn scaffolded_counter_counts_clicks() {
    // The full app build + cascade + taffy layout recurses deeper than a
    // default 2 MiB test-thread stack; run the case on a roomy stack (the
    // windowed path runs on the 8 MiB main thread).
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run_case)
        .expect("spawn test thread")
        .join()
        .expect("counter template case");
}

fn run_case() {
    let dir = scaffolded_counter();
    let (mut app, _winit) = build_headless_app(RunOptions::new(dir.clone())).expect("build app");

    // A few ticks so the candela `on_start` fires, its `signal_set_int`
    // command drains, and the `bind-text` reader mirrors it into the label.
    for _ in 0..5 {
        app.tick();
    }

    // The candela host owns the script: a `.cdl` file selects it with no
    // `[script] engine` line in lumen.toml.
    assert!(
        app.world
            .get_resource::<lumen_script_candela::CandelaHost>()
            .is_some(),
        "the counter template's main.cdl did not select the candela host"
    );

    click_on(&mut app, "bump");
    app.tick();
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "1"),
        "clicking bump did not raise the clicks signal; TextContents = {texts:?}"
    );

    // A rapid second click on the same entity folds into a double-click, so
    // reset is the next thing to drive.
    click_on(&mut app, "reset");
    app.tick();
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "0"),
        "clicking reset did not clear the clicks signal; TextContents = {texts:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
