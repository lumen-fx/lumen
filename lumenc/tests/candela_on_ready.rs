// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Proof that the post-mount `on_ready` callback sees a populated DOM while
//! `on_start` (which runs before the first DOM index publish) does not.
//!
//! `apps/candela-on-ready` records, in two bound labels, the id
//! `node_get_by_id("playlist")` returned from each callback. `on_start` sees 0
//! (no index yet); `on_ready` sees a nonzero interned id. Without the
//! `fire_on_ready` dispatch a DOM app had to defer its initial build behind a
//! `set_timeout("boot", 0)` timer.

use lumenc::{RunOptions, build_headless_app};

fn on_ready_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apps/candela-on-ready")
        .canonicalize()
        .expect("apps/candela-on-ready fixture must exist")
}

fn label_text(app: &mut lumen_core::prelude::App, id: &str) -> Option<String> {
    use lumen_core::components::{LumenId, TextContent};
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0.as_str() == id)
        .map(|(_, t)| t.0.clone())
}

#[test]
fn on_ready_sees_populated_dom_on_start_does_not() {
    let opts = RunOptions::new(on_ready_dir());
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    // A few ticks so on_ready fires, both signals drain, and the bind-text
    // readers mirror them into the labels.
    for _ in 0..5 {
        app.tick();
    }

    let start = label_text(&mut app, "start-label").expect("start-label present");
    let ready = label_text(&mut app, "ready-label").expect("ready-label present");

    assert_eq!(
        start, "0",
        "on_start ran before the DOM index publish, so #playlist must be unqueryable (0)"
    );
    assert_ne!(
        ready, "0",
        "on_ready ran after the DOM index publish, so #playlist must resolve to a live id"
    );
    assert_ne!(ready, "waiting", "on_ready must have written ready_saw");
}
