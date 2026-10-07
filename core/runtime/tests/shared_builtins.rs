//! The shared builtin table, called from a real app script.
//!
//! `lumen-script`'s own suite drives the bodies directly. This one goes through
//! the host: the script names the builtin, the engine resolves it, and the
//! command it queued reaches the property store the way it does in a running
//! app. That is the half a table of descriptions cannot prove on its own.

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

// The candela host, compiled in: the artifact names the module that runs its
// program, and a test binary has no shared engine to open it from.
use lumen_candela_dev as _;

/// An app publishes process-global registries, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build and tick a headless app whose candela script is `source`.
fn run(source: &str) -> EcsApp {
    let dir = std::env::temp_dir().join(format!(
        "lumen_shared_builtins_{}_{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).expect("temp app dir");
    let root = Element {
        tag: "root".to_string(),
        children: vec![Element {
            tag: "label".to_string(),
            attrs: Attributes {
                id: Some("out".to_string()),
                // `set_text` writes into an element that already carries text,
                // so the label starts with a placeholder the script replaces.
                text: Some("waiting".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root,
            ..Default::default()
        },
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            module: "lumen-candela-dev".to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    for _ in 0..3 {
        app.tick();
    }
    app
}

fn signal(app: &EcsApp, name: &str) -> Option<String> {
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

/// The text a `set_text` reached the element with, read back off the tree.
fn label_text(app: &mut EcsApp) -> Option<String> {
    let mut q = app.world.query::<(
        &lumen_core::components::LumenId,
        &lumen_core::components::TextContent,
    )>();
    let found: Vec<String> = q
        .iter(&app.world)
        .filter(|(id, _)| id.0 == "out")
        .map(|(_, text)| text.0.to_string())
        .collect();
    found.into_iter().next()
}

/// A command builtin reaches the element, and a value builtin reads back into
/// a signal, through the prelude's typed declarations. `node_get_by_id` proves the typed shape adapter binds an
/// int-returning builtin, not just the unit-returning ones.
#[test]
fn a_script_calls_the_shared_builtins() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = run(r#"
import "lumen.cdl";

fn on_start() {
    lumen::set_text("out", "candela says " + lumen::t("hello"));
    lumen::signal_set("page", lumen::page_current());
}

// The tree exists from `on_ready` on, so that is where a node lookup belongs.
// This walks the free-function DOM surface: a lookup, a read, a traversal, a
// list, and a write, each one a typed declaration in the prelude.
fn on_ready() {
    let n = lumen::node_get_by_id("out");
    if n != 0 {
        lumen::signal_set("found", "yes");
        lumen::signal_set("tag", lumen::node_get_attr(n, "id"));
        lumen::node_set_attr(n, "data-seen", "1");
        let parent = lumen::node_parent(n);
        let kids = lumen::node_children(parent);
        lumen::signal_set_int("kids", kids.len());
        lumen::signal_set("markup", lumen::node_outer_markup(n));
    }
}

fn main() {}
"#);

    assert_eq!(label_text(&mut app).as_deref(), Some("candela says hello"));
    assert!(signal(&app, "page").is_some());
    assert_eq!(signal(&app, "found").as_deref(), Some("yes"));
    assert_eq!(
        signal(&app, "tag").as_deref(),
        Some("out"),
        "a string-returning node read crossed back"
    );
    assert_eq!(
        signal(&app, "kids").as_deref(),
        Some("1"),
        "a node list crossed back as a candela array"
    );
    assert!(
        signal(&app, "markup").is_some_and(|m| m.contains("label")),
        "a markup read crossed back as a string"
    );
}
