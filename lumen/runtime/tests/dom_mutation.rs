//! Headless proof of the dynamic DOM write side (phases 2 + 3) and the
//! `window` setters (section 4.8).
//!
//! Drives the host-neutral external DOM command bus, the same seam the
//! C-ABI and SDKs use, against a window-free headless app, then reads the
//! published snapshot back to assert the tree / components. Every mutation
//! materializes through the runtime's command applier; no window is opened.

use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::ScriptCommand;
use lumen_script::node_query::{self, build_clone, build_spawn, push_external_dom_command};

fn el(tag: &str, id: Option<&str>, classes: &[&str], children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Attributes {
            id: id.map(str::to_string),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        children,
        interpolations: Vec::new(),
    }
}

/// The DOM snapshot, external command bus, and nav / window state are all
/// process-global singletons, so the headless apps that read + write them
/// must run one at a time. Each `Harness` holds this lock for its lifetime.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: lumen_core::app::App,
    _dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new() -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // Drain any commands a prior test left on the bus so this app starts
        // from a clean slate.
        let _ = node_query::drain_external_dom_commands();
        let dir = std::env::temp_dir().join(format!("lumen_dom_mut_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
        std::fs::create_dir_all(&dir).unwrap();
        let root = el(
            "root",
            Some("app"),
            &["app"],
            vec![el("column", None, &["list"], vec![])],
        );
        let ir = LayoutIR {
            root,
            ..Default::default()
        };
        let bytes = artifact::serialize(&CompiledApp {
            ir,
            script_source: String::new(),
            ..Default::default()
        })
        .unwrap();
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _winit) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness {
            app,
            _dir: dir,
            _guard: guard,
        };
        h.settle();
        h
    }

    /// Tick enough for a pushed batch to apply AND for the next snapshot to
    /// reflect it (apply runs mid-tick, publish runs at the start).
    fn settle(&mut self) {
        for _ in 0..4 {
            self.app.tick();
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

#[test]
fn spawn_chain_append_reads_back() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();

    // spawn("button") + set id / text / class, then append under the column.
    let (btn, spawn) = build_spawn("button");
    push_external_dom_command(spawn);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: btn,
        name: "id".into(),
        value: "created".into(),
    });
    push_external_dom_command(ScriptCommand::SetNodeText {
        node: btn,
        text: "Save".into(),
    });
    push_external_dom_command(ScriptCommand::ClassAdd {
        node: btn,
        class: "made".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: btn,
        before: 0,
    });
    h.settle();

    // The node is reachable by its new id, carries its text / class, and
    // sits under the column.
    let created = node_query::run_get_by_id("created").expect("spawned node is queryable");
    assert_eq!(node_query::node_text(created).as_deref(), Some("Save"));
    assert!(node_query::node_class_contains(created, "made"));
    assert_eq!(node_query::node_parent(created), Some(column));
    assert_eq!(
        node_query::node_get_attr(created, "id").as_deref(),
        Some("created")
    );
}

#[test]
fn class_list_add_remove_toggle() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    let (n, spawn) = build_spawn("label");
    push_external_dom_command(spawn);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: n,
        name: "id".into(),
        value: "cl".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: n,
        before: 0,
    });
    push_external_dom_command(ScriptCommand::ClassAdd {
        node: n,
        class: "a".into(),
    });
    push_external_dom_command(ScriptCommand::ClassToggle {
        node: n,
        class: "b".into(),
    });
    h.settle();
    let node = node_query::run_get_by_id("cl").unwrap();
    assert!(node_query::node_class_contains(node, "a"));
    assert!(node_query::node_class_contains(node, "b"));

    push_external_dom_command(ScriptCommand::ClassRemove {
        node,
        class: "a".into(),
    });
    push_external_dom_command(ScriptCommand::ClassToggle {
        node,
        class: "b".into(),
    });
    h.settle();
    assert!(!node_query::node_class_contains(node, "a"));
    assert!(!node_query::node_class_contains(node, "b"));
}

#[test]
fn inline_style_shows_in_computed_style() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    let (n, spawn) = build_spawn("label");
    push_external_dom_command(spawn);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: n,
        name: "id".into(),
        value: "styled".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: n,
        before: 0,
    });
    push_external_dom_command(ScriptCommand::SetStyleProp {
        node: n,
        name: "color".into(),
        value: "#ff0000".into(),
    });
    h.settle();
    let node = node_query::run_get_by_id("styled").unwrap();
    assert_eq!(
        node_query::node_style_get(node, "color").as_deref(),
        Some("#ff0000")
    );
    assert_eq!(
        node_query::node_computed_style(node, "color").as_deref(),
        Some("#ff0000"),
        "inline style is reflected in computed_style"
    );
}

#[test]
fn insert_before_orders_children() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    // Append `first`, then insert `second` BEFORE it.
    let (first, s1) = build_spawn("button");
    push_external_dom_command(s1);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: first,
        name: "id".into(),
        value: "first".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: first,
        before: 0,
    });
    h.settle();
    let first = node_query::run_get_by_id("first").unwrap();

    let (second, s2) = build_spawn("button");
    push_external_dom_command(s2);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: second,
        name: "id".into(),
        value: "second".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: second,
        before: first,
    });
    h.settle();
    let second = node_query::run_get_by_id("second").unwrap();
    let kids = node_query::node_children(column);
    assert_eq!(kids, vec![second, first], "second precedes first");
}

#[test]
fn remove_despawns_and_replace_swaps() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    let (a, sa) = build_spawn("button");
    push_external_dom_command(sa);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: a,
        name: "id".into(),
        value: "gone".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: a,
        before: 0,
    });
    h.settle();
    let a = node_query::run_get_by_id("gone").unwrap();
    assert!(node_query::node_valid(a));

    // replace_with a fresh node, which despawns `a`.
    let (b, sb) = build_spawn("button");
    push_external_dom_command(sb);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: b,
        name: "id".into(),
        value: "kept".into(),
    });
    push_external_dom_command(ScriptCommand::ReplaceWith { old: a, new: b });
    h.settle();
    assert!(!node_query::node_valid(a), "replaced node is despawned");
    let b = node_query::run_get_by_id("kept").unwrap();
    assert_eq!(node_query::node_parent(b), Some(column));

    // remove() despawns the whole subtree.
    push_external_dom_command(ScriptCommand::RemoveNode { node: b });
    h.settle();
    assert!(node_query::run_get_by_id("kept").is_none());
}

#[test]
fn clone_deep_duplicates_subtree() {
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    // Build a small subtree: card > label("Hi").
    let (card, sc) = build_spawn("div");
    push_external_dom_command(sc);
    push_external_dom_command(ScriptCommand::ClassAdd {
        node: card,
        class: "orig".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: card,
        before: 0,
    });
    let (label, sl) = build_spawn("label");
    push_external_dom_command(sl);
    push_external_dom_command(ScriptCommand::SetNodeText {
        node: label,
        text: "Hi".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: card,
        node: label,
        before: 0,
    });
    h.settle();
    let card = node_query::run_query(".orig").unwrap().single().unwrap();

    // Clone the card and append the clone under the column.
    let (clone, cc) = build_clone(card);
    push_external_dom_command(cc);
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: clone,
        before: 0,
    });
    h.settle();

    // Two `.orig` cards now exist, each with a label child whose text is Hi.
    let cards = node_query::run_query(".orig").unwrap();
    assert_eq!(cards.len(), 2, "clone_deep duplicated the card");
    for c in cards.collect() {
        let child = node_query::node_first_child(c).expect("clone kept its child");
        assert_eq!(node_query::node_text(child).as_deref(), Some("Hi"));
    }
}

#[test]
fn set_inner_markup_no_ops_without_a_parser() {
    // The artifact run path links no markup front-end, so `set_inner_markup`
    // is a guarded no-op there: it must not despawn the existing children or
    // panic. (The parser-present spawn/replace path is proven end-to-end in
    // the Rust SDK, which links the real front-end.)
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    let (child, s) = build_spawn("label");
    push_external_dom_command(s);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: child,
        name: "id".into(),
        value: "keep".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: column,
        node: child,
        before: 0,
    });
    h.settle();
    let child = node_query::run_get_by_id("keep").unwrap();

    push_external_dom_command(ScriptCommand::SetInnerMarkup {
        node: column,
        markup: "<row/>".into(),
    });
    h.settle();

    // No parser -> no replacement: the original child is still there.
    assert!(node_query::node_valid(child));
    assert_eq!(node_query::node_children(column), vec![child]);
}

#[test]
fn inner_markup_read_serializes_children() {
    // The read half needs no parser: spawn two children under a node and
    // assert `inner_markup` serializes them (and not the node itself).
    let mut h = Harness::new();
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    for id in ["a", "b"] {
        let (n, s) = build_spawn("row");
        push_external_dom_command(s);
        push_external_dom_command(ScriptCommand::SetAttr {
            node: n,
            name: "id".into(),
            value: id.into(),
        });
        push_external_dom_command(ScriptCommand::Insert {
            parent: column,
            node: n,
            before: 0,
        });
    }
    h.settle();

    let markup = lumen_script::introspect::inner_markup(column);
    assert!(markup.contains("<row"), "children serialized: {markup}");
    assert!(markup.contains("id=\"a\""));
    assert!(markup.contains("id=\"b\""));
    assert!(
        !markup.contains("class=\"list\""),
        "inner_markup omits the node itself"
    );
}

#[test]
fn window_setters_apply_headlessly() {
    let mut h = Harness::new();
    // window.set_href routes onto the shared nav bus (page resolution needs
    // a registered page set, exercised elsewhere); it must never panic.
    assert!(lumen_core::nav::navigate("settings"));
    h.settle();

    // window.set_title / set_size flow through the DOM command applier and
    // land in the state cache the getters read.
    push_external_dom_command(ScriptCommand::WindowSetTitle {
        title: "Docs".into(),
    });
    push_external_dom_command(ScriptCommand::WindowSetSize {
        width: 800.0,
        height: 600.0,
    });
    h.settle();
    assert_eq!(lumen_core::window_state::title(), "Docs");
    assert_eq!(lumen_core::window_state::size(), (800.0, 600.0));
}
