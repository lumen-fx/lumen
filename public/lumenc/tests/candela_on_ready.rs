// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Proof that the post-mount `on_ready` callback sees a populated DOM while
//! `on_start` (which runs before the first DOM index publish) does not.
//!
//! `fixtures/candela-on-ready` records, in two bound labels, the id
//! `node_get_by_id("playlist")` returned from each callback. `on_start` sees 0
//! (no index yet); `on_ready` sees a nonzero interned id. Without the
//! `fire_on_ready` dispatch a DOM app had to defer its initial build behind a
//! `set_timeout("boot", 0)` timer.

use lumenc::{RunOptions, build_headless_app};

/// The DOM index cache is process-global (`lumen_core::node`), so two apps in
/// one test process observe each other's published index: this fixture's ids
/// leak from one test's ticking app into the other's `on_start`, which must
/// see an empty index. Serialize the tests and reset the cache before each
/// build.
static DOM_INDEX_ISOLATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn isolate() -> std::sync::MutexGuard<'static, ()> {
    let guard = DOM_INDEX_ISOLATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lumen_core::node::publish_dom_index(lumen_core::node::DomIndex::default());
    guard
}

fn on_ready_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/candela-on-ready")
        .canonicalize()
        .expect("fixtures/candela-on-ready must exist")
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
    let _isolation = isolate();
    let opts = RunOptions::new(on_ready_dir());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");

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

#[test]
fn rearmed_latch_fires_on_ready_again() {
    let _isolation = isolate();
    let opts = RunOptions::new(on_ready_dir());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..5 {
        app.tick();
    }
    let runs = label_text(&mut app, "runs-label").expect("runs-label present");
    assert_eq!(runs, "1", "on_ready must have run exactly once");

    // Re-arm the latch the way hot reload does after respawning the tree;
    // the dispatch counter moving to 2 proves the second run.
    app.world
        .resource_mut::<lumen_script::OnReadyFired>()
        .0
        .clear();
    for _ in 0..5 {
        app.tick();
    }

    assert!(
        app.world
            .resource::<lumen_script::OnReadyFired>()
            .0
            .contains("candela"),
        "fire_on_ready must have run again and re-latched after the reset"
    );
    let store = app
        .world
        .resource::<lumen_core::property_store::PropertyStore>();
    let read_back = store.get_global_str("ready_read");
    let store_runs = store.get_global_str("ready_runs");
    assert_eq!(
        (store_runs.as_deref(), read_back.as_deref()),
        (Some("2"), Some("1")),
        "the second on_ready dispatch must read 1 and write 2"
    );
    let runs = label_text(&mut app, "runs-label").expect("runs-label present");
    assert_eq!(
        runs, "2",
        "re-arming the latch must dispatch on_ready exactly once more"
    );
}

/// Write `files` (path relative to the app root, contents) into a fresh app
/// directory named after `tag`.
fn write_app(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_candela_on_ready_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    for (path, body) in files {
        std::fs::write(dir.join(path), body).unwrap();
    }
    dir
}

/// `on_ready` finds the elements inside an `<if>` that `on_start` opened, in
/// either mode, and the rows of a `<for>` over an array it set, and a node it
/// appends to one attaches there.
#[test]
fn on_ready_finds_the_body_of_an_if_on_start_opened() {
    let _isolation = isolate();
    let dir = write_app(
        "if_body",
        &[
            (
                "src/main.lmn",
                r#"<root>
  <if signal="page" eq="a" mode="hide"><row id="chips"/></if>
  <if signal="flag"><row id="chips3"/></if>
  <column id="list">
    <for each="rows" key="id"><label class="row" text="{title}" /></for>
  </column>
  <label id="seen" bind-text="seen" text="waiting" />
  <script src="main.cdl"/>
</root>
"#,
            ),
            (
                "src/main.cdl",
                r#"import "lumen.cdl";
fn on_start() {
    signal<string>("page").set("a");
    signal<bool>("flag").set(true);
    lumen::signal_array_set("rows", [{"id": "a", "title": "Alpha"}, {"id": "b", "title": "Beta"}]);
}
fn on_ready() {
    let a = lumen::node_get_by_id("chips");
    let b = lumen::node_get_by_id("chips3");
    let rows = lumen::node_query(".row");
    lumen::signal_set("seen", str(a != 0) + " " + str(b != 0) + " " + str(rows.len()));
    let chip = create("label");
    chip.set_id("chip");
    chip.set_text("first chip");
    get_by_id("chips").append(chip);
}
fn main() {}
"#,
            ),
        ],
    );
    let (mut app, _window) = build_headless_app(RunOptions::new(&dir)).expect("build_headless_app");
    for _ in 0..5 {
        app.tick();
    }
    assert_eq!(
        label_text(&mut app, "seen").as_deref(),
        Some("true true 2"),
        "on_ready resolved both gated elements and the rows"
    );
    assert_eq!(
        label_text(&mut app, "chip").as_deref(),
        Some("first chip"),
        "the node on_ready appended attached under the gated row"
    );
    let chip_parent = {
        use lumen_core::components::LumenId;
        let mut q = app
            .world
            .query::<(&LumenId, &bevy_ecs::hierarchy::ChildOf)>();
        q.iter(&app.world)
            .find(|(lid, _)| lid.0.as_str() == "chip")
            .map(|(_, c)| c.parent())
    };
    let chips = {
        use lumen_core::components::LumenId;
        let mut q = app.world.query::<(bevy_ecs::entity::Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, lid)| lid.0.as_str() == "chips")
            .map(|(e, _)| e)
    };
    assert_eq!(chip_parent, chips, "the chip sits under #chips");
}
