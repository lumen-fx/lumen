//! Headless proof that the structural pseudo-classes still select the right
//! sibling after the runtime re-runs the cascade.
//!
//! The load-time cascade walks the whole tree and knows where every element
//! sits. The runtime re-resolver rebuilds one element at a time from its ECS
//! identity, and used to hand the cascade a `1 of 1` position for all of
//! them: `:first-child` and `:last-child` then matched every element, and the
//! later rule painted the whole row.

use lumen_core::components::{Fill, Visuals};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::css::{Declaration, Origin, Rule, Stylesheet};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query;

/// The DOM snapshot is process-global, so the headless apps that read it run
/// one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn el(tag: &str, id: &str, classes: &[&str], children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Attributes {
            id: Some(id.to_string()),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        children,
        ..Default::default()
    }
}

/// One rule from a selector string and `(property, value)` pairs.
fn rule(selector: &str, decls: &[(&str, &str)], source_order: usize) -> Rule {
    Rule {
        selectors: lumen_ir::css::parse_selector_list(selector).expect("selector parses"),
        declarations: decls
            .iter()
            .map(|(name, value)| Declaration {
                name: (*name).to_string(),
                value: (*value).to_string(),
                important: false,
            })
            .collect(),
        origin: Origin::Author,
        source_order,
        media: None,
        selector: Default::default(),
    }
}

struct Harness {
    app: lumen_core::app::App,
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    /// A row of three `.cell` tiles under `#row`, styled by `rules`.
    fn three_cells(rules: Vec<Rule>) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumen_structural_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        // Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

        let mut ir = LayoutIR {
            root: el(
                "root",
                "app",
                &[],
                vec![el(
                    "row",
                    "row",
                    &[],
                    vec![
                        el("tile", "c1", &["cell"], vec![]),
                        el("tile", "c2", &["cell"], vec![]),
                        el("tile", "c3", &["cell"], vec![]),
                    ],
                )],
            ),
            combined_stylesheet: Some(Stylesheet { rules }),
            ..Default::default()
        };
        // Cascade once up front, the way `lumenc build` does.
        let sheet = ir.combined_stylesheet.clone().expect("stylesheet");
        lumen_ir::css::apply_css(&mut ir, &sheet).expect("cascade");

        let bytes = artifact::serialize(&CompiledApp {
            ir,
            ..Default::default()
        })
        .unwrap();
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _window) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness {
            app,
            dir,
            _guard: guard,
        };
        // The first post-window tick re-resolves the whole tree against the
        // real media context, which is the pass this test is about.
        for _ in 0..4 {
            h.app.tick();
        }
        h
    }

    /// The solid fill currently on the element with that id.
    fn fill(&self, id: &str) -> lumen_core::components::Color {
        let handle = node_query::run_get_by_id(id).expect("id is in the DOM index");
        let entity = lumen_core::node::NodeHandle::unpack(handle)
            .expect("live handle")
            .entity;
        self.app
            .world
            .get::<Visuals>(entity)
            .and_then(|v| v.fill.as_ref())
            .and_then(Fill::as_solid)
            .expect("cascaded fill")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether a fill matches an `#rrggbb` literal, with the tolerance the
/// byte-to-f32 channel conversion needs.
fn near(a: lumen_core::components::Color, hex: &str) -> bool {
    let byte = |i: usize| {
        u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).expect("#rrggbb literal") as f32 / 255.0
    };
    (a.r - byte(0)).abs() < 0.01 && (a.g - byte(1)).abs() < 0.01 && (a.b - byte(2)).abs() < 0.01
}

#[test]
fn first_and_last_child_select_the_ends_of_a_row() {
    let h = Harness::three_cells(vec![
        rule(".cell", &[("bg", "#ffffff")], 0),
        rule(".cell:first-child", &[("bg", "#0000ff")], 1),
        rule(".cell:last-child", &[("bg", "#ff0000")], 2),
    ]);
    assert!(near(h.fill("c1"), "#0000ff"), "first cell is :first-child");
    assert!(near(h.fill("c2"), "#ffffff"), "middle cell is neither end");
    assert!(near(h.fill("c3"), "#ff0000"), "last cell is :last-child");
}

#[test]
fn nth_child_selects_by_position() {
    let h = Harness::three_cells(vec![
        rule(".cell", &[("bg", "#ffffff")], 0),
        rule(".cell:nth-child(2)", &[("bg", "#00ff00")], 1),
    ]);
    assert!(near(h.fill("c1"), "#ffffff"), "cell 1 is not nth-child(2)");
    assert!(near(h.fill("c2"), "#00ff00"), "cell 2 is nth-child(2)");
    assert!(near(h.fill("c3"), "#ffffff"), "cell 3 is not nth-child(2)");
}

#[test]
fn only_child_needs_an_only_child() {
    // Three siblings: none of them is an only child, so the rule that would
    // repaint one must not reach any of them.
    let h = Harness::three_cells(vec![
        rule(".cell", &[("bg", "#ffffff")], 0),
        rule(".cell:only-child", &[("bg", "#ff00ff")], 1),
    ]);
    for id in ["c1", "c2", "c3"] {
        assert!(near(h.fill(id), "#ffffff"), "{id} is one of three siblings");
    }
}

/// `computed_style` resolves the same cascade the paint path does, so it has
/// to agree about which sibling a structural rule selected.
#[test]
fn computed_style_reports_the_same_match() {
    // The harness owns the running app; the reads below go through the
    // process-global DOM snapshot it publishes.
    let _h = Harness::three_cells(vec![
        rule(".cell", &[("bg", "#ffffff")], 0),
        rule(".cell:last-child", &[("bg", "#ff0000")], 1),
    ]);
    let read = |id: &str| {
        let handle = node_query::run_get_by_id(id).expect("id is in the DOM index");
        node_query::node_computed_style(handle, "bg")
    };
    assert_eq!(read("c1").as_deref(), Some("#ffffff"));
    assert_eq!(read("c3").as_deref(), Some("#ff0000"));
}
