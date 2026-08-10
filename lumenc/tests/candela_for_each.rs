// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Proof that a candela app drives `<for each>` from an array signal.
//!
//! `apps/candela-for-each` fills the `rows` array signal from `on_start` and
//! renders it with `<for each="rows" key="id">`. The rendered labels are the
//! evidence: before array signals existed, a candela app had to build list
//! contents element by element through the DOM API.

use lumenc::{RunOptions, build_headless_app};

fn app_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apps/candela-for-each")
        .canonicalize()
        .expect("apps/candela-for-each fixture must exist")
}

/// Text of every element carrying the `row` class, in tree order.
fn row_texts(app: &mut lumen_core::prelude::App) -> Vec<String> {
    use lumen_core::components::{LumenClasses, TextContent};
    let mut q = app.world.query::<(&LumenClasses, &TextContent)>();
    q.iter(&app.world)
        .filter(|(classes, _)| classes.0.iter().any(|c| &**c == "row"))
        .map(|(_, t)| t.0.clone())
        .collect()
}

fn label_text(app: &mut lumen_core::prelude::App, id: &str) -> Option<String> {
    use lumen_core::components::{LumenId, TextContent};
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0.as_str() == id)
        .map(|(_, t)| t.0.clone())
}

#[test]
fn an_array_signal_renders_one_element_per_record() {
    let opts = RunOptions::new(app_dir());
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    // A few ticks so the on_start commands drain, the array lands in the
    // signal store, and the `<for>` block reconciles.
    for _ in 0..5 {
        app.tick();
    }

    let mut texts = row_texts(&mut app);
    texts.sort();
    assert_eq!(
        texts,
        ["Alpha", "Beta", "Gamma"],
        "one row element per record, each bound to its `title` field"
    );
    assert_eq!(
        label_text(&mut app, "count-label").as_deref(),
        Some("3"),
        "signal_array_len counted the records the script wrote"
    );
}

#[test]
fn removing_a_record_despawns_its_element() {
    let opts = RunOptions::new(app_dir());
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..5 {
        app.tick();
    }

    // Dispatch the script's `drop_first`, which removes index 0 through the
    // prelude's ArraySignal handle.
    {
        use lumen_script::ScriptHost;
        let mut host = app
            .world
            .resource_mut::<lumen_script_candela::CandelaHost>();
        let outcome = host.call("drop_first", &[]).expect("drop_first ok");
        assert!(outcome.found, "the fixture defines drop_first");
        host.push_commands(outcome.commands);
    }
    for _ in 0..5 {
        app.tick();
    }

    let mut texts = row_texts(&mut app);
    texts.sort();
    assert_eq!(
        texts,
        ["Beta", "Gamma"],
        "the removed record's element is gone"
    );
    assert_eq!(label_text(&mut app, "count-label").as_deref(), Some("2"));
}
