//! Headless proof of the dynamic DOM read side (phase 1).
//!
//! Builds a small precompiled artifact by hand (root > column > three
//! buttons), ticks it through the window-free headless app so
//! `build_dom_index` publishes a snapshot, then drives the host-neutral
//! query surface (`lumen_script::node_query`) that every script host and
//! the C-ABI share. No window is ever opened.

use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query;

fn el(tag: &str, id: Option<&str>, classes: &[&str], children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Box::new(Attributes {
            id: id.map(str::to_string),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }),
        children,
        ..Default::default()
    }
}

#[test]
fn dom_query_read_side_headless() {
    let dir = std::env::temp_dir().join(format!("lumen_dom_query_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();

    let root = el(
        "root",
        Some("app"),
        &[],
        vec![el(
            "column",
            None,
            &["list"],
            vec![
                el("button", Some("save"), &["row"], vec![]),
                el("button", Some("cancel"), &["row"], vec![]),
                el("button", Some("reset"), &["row"], vec![]),
            ],
        )],
    );
    let ir = LayoutIR {
        root,
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        ..Default::default()
    })
    .expect("serialize fixture artifact");

    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    for _ in 0..3 {
        app.tick();
    }

    // get_by_id fast path.
    let save = node_query::run_get_by_id("save").expect("#save resolves");
    let cancel = node_query::run_get_by_id("cancel").expect("#cancel resolves");

    // query("#save").single() returns exactly that entity.
    let q_save = node_query::run_query("#save").unwrap();
    assert_eq!(q_save.single().unwrap(), save);

    // query(".row").len() == fixture row count.
    let rows = node_query::run_query(".row").unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.single().is_err(), "3 matches is not a single");

    // Descendant selector reuses the cascade matcher.
    assert_eq!(node_query::run_query(".list button").unwrap().len(), 3);
    assert_eq!(node_query::run_query(".list > .row").unwrap().len(), 3);

    // Traversal.
    let column = node_query::run_query(".list").unwrap().single().unwrap();
    assert_eq!(node_query::node_parent(save), Some(column));
    assert_eq!(node_query::node_next(save), Some(cancel));
    assert_eq!(node_query::node_prev(cancel), Some(save));
    assert_eq!(node_query::node_children(column).len(), 3);
    assert_eq!(node_query::node_first_child(column), Some(save));

    // document() is the root; closest walks up to a matching ancestor.
    let doc = node_query::run_document().expect("document root");
    assert_eq!(node_query::run_get_by_id("app"), Some(doc));
    assert_eq!(
        node_query::node_closest(save, ".list").unwrap(),
        Some(column)
    );
    assert_eq!(node_query::node_closest(save, "#app").unwrap(), Some(doc));

    // Liveness + stale handle: a fabricated / null handle never panics and
    // resolves to nothing.
    assert!(node_query::node_valid(save));
    assert!(!node_query::node_valid(0));
    assert_eq!(node_query::node_parent(0), None);
    assert_eq!(node_query::node_closest(0, ".list").unwrap(), None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A headless app whose tree is a root with one button carrying `id`.
fn app_with_button(id: &str) -> (lumen_core::prelude::App, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lumen_dom_query_{}_{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = LayoutIR {
        root: el("root", None, &[], vec![el("button", Some(id), &[], vec![])]),
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        ..Default::default()
    })
    .expect("serialize fixture artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (app, _window) = build_headless_app(opts).expect("build headless app");
    (app, dir)
}

/// Two apps ticking at once in one process each look elements up in their
/// own tree, never in the one the other app published last.
#[test]
fn concurrent_apps_each_query_their_own_tree() {
    use std::sync::{Arc, Barrier};

    let published = Arc::new(Barrier::new(2));
    let run = |own: &'static str, other: &'static str| {
        let published = Arc::clone(&published);
        std::thread::spawn(move || {
            let (mut app, dir) = app_with_button(own);
            for _ in 0..3 {
                app.tick();
            }
            // Both apps have published their tree before either looks.
            published.wait();
            app.tick();
            let found = (
                node_query::run_get_by_id(own).is_some(),
                node_query::run_get_by_id(other).is_some(),
                node_query::run_query(&format!("#{own}"))
                    .map(|q| q.len())
                    .ok(),
            );
            let _ = std::fs::remove_dir_all(&dir);
            found
        })
    };
    let a = run("only-in-a", "only-in-b");
    let b = run("only-in-b", "only-in-a");
    for (name, handle) in [("a", a), ("b", b)] {
        let (own, other, query) = handle.join().expect("app thread");
        assert!(own, "app {name} finds its own element");
        assert!(!other, "app {name} does not see the other app's element");
        assert_eq!(query, Some(1), "app {name} queries its own tree");
    }
}
