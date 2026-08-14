//! Headless proof that a runtime style change reaches what paints.
//!
//! Two writes are covered, both of which used to be inert:
//!
//! * `set_style` stored an [`InlineStyle`] the cascade re-resolver never read
//!   back, so the element kept painting its stylesheet colour.
//! * the global `set_class(id, classes)` rewrote a non-root element's class
//!   list without bumping `StyleVersion`, so nothing re-cascaded and no
//!   transition ran.
//!
//! Each test builds a window-free app from a hand-assembled artifact (IR plus
//! stylesheet, no parser involved), drives it, and reads the resulting ECS
//! components.

use lumen_core::components::{Fill, Visuals};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::css::{Declaration, Origin, Rule, Stylesheet};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query::{self, push_external_dom_command};

/// The DOM snapshot and the external command bus are process-global, so the
/// headless apps that read and write them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    /// Build a two-element app (`root` > `tile#box`) styled by `rules`, with
    /// `script` baked in as the app's Rhai source.
    fn new(rules: Vec<Rule>, script: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _ = node_query::drain_external_dom_commands();
        let dir = std::env::temp_dir().join(format!("lumen_restyle_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
        std::fs::create_dir_all(&dir).unwrap();
        // Pin the engine: the baked source below is Rhai, and the default is
        // candela. Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();

        let mut ir = LayoutIR {
            root: el(
                "root",
                Some("app"),
                &[],
                vec![el("tile", Some("box"), &["cold"], vec![])],
            ),
            script_source: script.to_string(),
            combined_stylesheet: (!rules.is_empty()).then_some(Stylesheet { rules }),
            ..Default::default()
        };
        // Cascade once up front, the way `lumenc build` does: an artifact
        // carries attributes the stylesheet already resolved, and the spawn
        // pass reads `transition` from them.
        if let Some(sheet) = ir.combined_stylesheet.clone() {
            lumen_ir::css::apply_css(&mut ir, &sheet).expect("cascade");
        }
        let bytes = artifact::serialize(&CompiledApp {
            ir,
            script_source: script.to_string(),
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
        h.settle();
        h
    }

    /// Tick enough for a pushed batch to apply and for the next DOM snapshot
    /// to reflect it (apply runs mid-tick, publish runs at the start).
    fn settle(&mut self) {
        for _ in 0..4 {
            self.app.tick();
        }
    }

    /// The packed node handle of `#box`.
    fn box_handle(&self) -> u64 {
        node_query::run_get_by_id("box").expect("#box is in the DOM index")
    }

    /// The solid fill currently on `#box`.
    fn box_fill(&mut self) -> Option<lumen_core::components::Color> {
        let entity = lumen_core::node::NodeHandle::unpack(self.box_handle())
            .expect("live handle")
            .entity;
        self.app
            .world
            .get::<Visuals>(entity)
            .and_then(|v| v.fill.as_ref())
            .and_then(Fill::as_solid)
    }

    fn box_entity(&self) -> bevy_ecs::entity::Entity {
        lumen_core::node::NodeHandle::unpack(self.box_handle())
            .expect("live handle")
            .entity
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether a fill matches an `#rrggbb` literal. Comparison has a tolerance:
/// the cascade divides each byte by 255 into f32 channels, so an exact bit
/// compare would be brittle.
fn near(a: lumen_core::components::Color, hex: &str) -> bool {
    let byte = |i: usize| {
        u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).expect("#rrggbb literal") as f32 / 255.0
    };
    (a.r - byte(0)).abs() < 0.01 && (a.g - byte(1)).abs() < 0.01 && (a.b - byte(2)).abs() < 0.01
}

/// `set_style` writes an inline value that beats the author rule and reaches
/// `Visuals`. Before the inline tier was wired into the re-resolver, the tile
/// kept the stylesheet's colour forever.
#[test]
fn set_style_repaints_over_the_stylesheet() {
    let mut h = Harness::new(vec![rule("#box", &[("bg", "#112233")], 0)], "");
    assert!(
        near(h.box_fill().expect("stylesheet fill"), "#112233"),
        "the stylesheet rule should paint first"
    );

    push_external_dom_command(lumen_script::ScriptCommand::SetStyleProp {
        node: h.box_handle(),
        name: "bg".to_string(),
        value: "#ff0000".to_string(),
    });
    h.settle();

    assert!(
        near(h.box_fill().expect("inline fill"), "#ff0000"),
        "set_style must override the author rule"
    );
    assert_eq!(
        node_query::node_computed_style(h.box_handle(), "bg").as_deref(),
        Some("#ff0000"),
        "computed_style must report the inline value"
    );
}

/// An app with no stylesheet at all still applies its inline layer, so
/// `set_style` works on a tree that was never styled by CSS.
#[test]
fn set_style_applies_without_a_stylesheet() {
    let mut h = Harness::new(Vec::new(), "");
    push_external_dom_command(lumen_script::ScriptCommand::SetStyleProp {
        node: h.box_handle(),
        name: "bg".to_string(),
        value: "#00ff00".to_string(),
    });
    h.settle();

    assert!(
        near(h.box_fill().expect("inline fill"), "#00ff00"),
        "an app with no CSS must still honour set_style"
    );
}

/// A `set_style` write to an animatable property tweens when the element
/// declares a matching transition, exactly as a class flip does: the fill
/// stays at the old colour and a `BackgroundTransition` drives it.
#[test]
fn set_style_tweens_when_a_transition_is_declared() {
    let mut h = Harness::new(
        vec![rule(
            "#box",
            &[("bg", "#112233"), ("transition", "bg 400ms linear")],
            0,
        )],
        "",
    );
    push_external_dom_command(lumen_script::ScriptCommand::SetStyleProp {
        node: h.box_handle(),
        name: "bg".to_string(),
        value: "#ff0000".to_string(),
    });
    h.settle();

    let entity = h.box_entity();
    assert!(
        h.app
            .world
            .get::<lumen_primitives::BackgroundTransition>(entity)
            .is_some(),
        "an animatable inline change must start a tween, not snap"
    );
    let fill = h.box_fill().expect("fill");
    assert!(
        !near(fill, "#ff0000"),
        "the tween must start from the current colour, not jump to the target"
    );
}

/// `style_remove` hands the property back to the stylesheet.
#[test]
fn style_remove_restores_the_stylesheet_value() {
    let mut h = Harness::new(vec![rule("#box", &[("bg", "#112233")], 0)], "");
    push_external_dom_command(lumen_script::ScriptCommand::SetStyleProp {
        node: h.box_handle(),
        name: "bg".to_string(),
        value: "#ff0000".to_string(),
    });
    h.settle();
    push_external_dom_command(lumen_script::ScriptCommand::RemoveStyleProp {
        node: h.box_handle(),
        name: "bg".to_string(),
    });
    h.settle();

    assert!(
        near(h.box_fill().expect("fill"), "#112233"),
        "clearing the inline value must fall back to the author rule"
    );
}

/// The global `set_class(id, classes)` re-cascades a non-root element. The
/// Node-method form always did; this form silently changed the class list and
/// left the paint alone.
#[test]
fn global_set_class_recascades_a_non_root_element() {
    let mut h = Harness::new(
        vec![
            rule(".cold", &[("bg", "#112233")], 0),
            rule(".hot", &[("bg", "#ff0000")], 1),
        ],
        r#"fn on_start() { set_class("box", "hot"); }"#,
    );
    h.settle();

    assert!(
        near(h.box_fill().expect("fill"), "#ff0000"),
        "set_class must re-resolve the element against the stylesheet"
    );
}
