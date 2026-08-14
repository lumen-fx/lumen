//! The Rust SDK `dom` wrappers marshal the dynamic DOM surface end to end:
//! query -> mutate -> read back, plus guarded `set_inner_markup` against a
//! real markup front-end. Drives a window-free headless app built through the
//! compiler wrapper (so the injected parser is present) and reads back through
//! the same `Node` handles a caller would use.

use lumenc::build_headless_app;
use lumenui::dom::{self, Node};
use lumenui::ecs_app::App as EcsApp;
use lumenui::runtime::RunOptions;
use std::path::PathBuf;

// The DOM snapshot + external command bus are process-global; the app that
// reads and writes them runs one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn app_dir(markup: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_sdk_dom_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(dir.join("main.lmn"), markup).unwrap();
    dir
}

fn settle(app: &mut EcsApp) {
    for _ in 0..4 {
        app.tick();
    }
}

#[test]
fn query_mutate_read_back_through_node_handles() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir(r#"<root id="app"><column id="list" class="list"></column></root>"#);
    let opts = RunOptions::new(&dir);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    settle(&mut app);

    // query(".list").single() finds the column.
    let column = dom::query(".list").single().expect("one .list");
    assert!(column.is_valid());
    assert_eq!(dom::get_by_id("list"), Some(column));

    // spawn + fluent chain, then append under the column.
    let button = dom::spawn("button");
    button
        .set_id("save")
        .set_text("Save")
        .add_class("primary")
        .set_style("color", "#ff0000");
    column.append(button);
    settle(&mut app);

    // Read the mutations back through fresh handles.
    let save = dom::get_by_id("save").expect("spawned node is queryable");
    assert_eq!(save.text().as_deref(), Some("Save"));
    assert!(save.has_class("primary"));
    assert_eq!(save.style_get("color").as_deref(), Some("#ff0000"));
    assert_eq!(save.parent(), Some(column));
    assert_eq!(
        save.computed_style_of("color").as_deref(),
        Some("#ff0000"),
        "inline style resolves through computed_style"
    );

    // Traversal + introspection surface.
    assert_eq!(column.first_child(), Some(save));
    assert_eq!(column.children(), vec![save]);
    assert!(save.attrs().contains_key("id"));
    assert!(save.entity_id().is_some());
    assert!(!dom::dump_tree().is_empty());

    // outer_markup / inner_markup round-trip.
    assert!(column.outer_markup().contains("<button"));
    assert!(column.inner_markup().contains("<button"));
    assert!(
        !column.inner_markup().contains("class=\"list\""),
        "inner_markup omits the node itself"
    );
}

#[test]
fn set_inner_markup_parses_and_replaces_children() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = lumen_script::node_query::drain_external_dom_commands();
    let dir = app_dir(r#"<root id="app"><column id="host" class="host"></column></root>"#);
    let opts = RunOptions::new(&dir);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    settle(&mut app);

    let host = dom::query(".host").single().expect("one .host");
    // Seed a child that set_inner_markup must replace.
    let stale = dom::spawn("label").set_id("stale");
    host.append(stale);
    settle(&mut app);
    let stale: Node = dom::get_by_id("stale").expect("seed child present");

    // Guarded markup injection: the real front-end parses the fragment and
    // the applier spawns it as the host's new children.
    host.set_inner_markup(r#"<button id="injected" class="made">Hi</button>"#);
    settle(&mut app);

    assert!(!stale.is_valid(), "prior children were replaced");
    let injected = dom::get_by_id("injected").expect("parsed child spawned");
    assert_eq!(injected.parent(), Some(host));
    assert!(injected.has_class("made"));
    assert_eq!(injected.text().as_deref(), Some("Hi"));
    assert_eq!(host.children(), vec![injected]);
}

#[test]
fn window_document_history_namespaces() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir(r#"<root id="app"><column id="list" class="list"></column></root>"#);
    let opts = RunOptions::new(&dir);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    settle(&mut app);

    dom::window::set_title("Docs");
    dom::window::set_size(800.0, 600.0);
    settle(&mut app);
    assert_eq!(dom::window::title(), "Docs");
    assert_eq!(dom::window::size(), (800.0, 600.0));

    // document.root() reaches the tree; navigation + history never panic.
    assert!(dom::document::root().is_some());
    dom::window::set_href("settings");
    dom::history::back();
    dom::history::go(-1);
    settle(&mut app);
}
