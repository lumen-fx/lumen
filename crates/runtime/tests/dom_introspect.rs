//! Headless proof of the low-level introspection read side (phase 5).
//!
//! Builds a small precompiled artifact by hand with a stylesheet + fixed
//! geometry, ticks it through the window-free headless app so
//! `publish_introspection` publishes a snapshot, then drives the
//! host-neutral introspection surface (`lumen_script::introspect`) that
//! every script host and the C-ABI share. No window is opened.

use lumen_ir::css::{Declaration, Origin, Rule, Stylesheet, parse_selector_list};
use lumen_ir::layout_ir::{Attributes, DisplaySpec, Edges, Element, LayoutIR, LengthSpec};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::introspect as ins;
use lumen_script::node_query;

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

fn rule(selector: &str, decls: &[(&str, &str)], order: usize) -> Rule {
    Rule {
        selectors: parse_selector_list(selector).unwrap(),
        declarations: decls
            .iter()
            .map(|(n, v)| Declaration {
                name: n.to_string(),
                value: v.to_string(),
                important: false,
            })
            .collect(),
        origin: Origin::Author,
        source_order: order,
        media: None,
        selector: Default::default(),
    }
}

// The introspection snapshot is a process-global singleton, so this test
// serializes against the other DOM-API headless tests via its own lock.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn introspection_read_side_headless() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = node_query::drain_external_dom_commands();

    let dir =
        std::env::temp_dir().join(format!("lumen_dom_introspect_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    std::fs::create_dir_all(&dir).unwrap();

    // root#app.app (400x300) > column.list > [ button#save.row (100x40, pad 4),
    // label#hidden (display:none) ].
    let mut save = el("button", Some("save"), &["row"], vec![]);
    save.attrs.width = Some(LengthSpec::Px(100.0));
    save.attrs.height = Some(LengthSpec::Px(40.0));
    save.attrs.padding = Some(Edges::all(4.0));

    let mut hidden = el("label", Some("hidden"), &[], vec![]);
    hidden.attrs.display = Some(DisplaySpec::None);

    let column = el("column", Some("list"), &["list"], vec![save, hidden]);
    let mut root = el("root", Some("app"), &["app"], vec![column]);
    root.attrs.width = Some(LengthSpec::Px(400.0));
    root.attrs.height = Some(LengthSpec::Px(300.0));

    let sheet = Stylesheet {
        rules: vec![
            rule(".row", &[("color", "#00ff00"), ("font-size", "18")], 0),
            rule("#save", &[("font-weight", "700")], 1),
        ],
    };
    let ir = LayoutIR {
        root,
        combined_stylesheet: Some(sheet),
        ..Default::default()
    };
    let bytes = lumen_ir::artifact::serialize(&lumen_ir::artifact::CompiledApp {
        ir,
        script_source: String::new(),
        ..Default::default()
    })
    .expect("serialize fixture artifact");

    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _winit) = build_headless_app(opts).expect("build headless app");
    for _ in 0..6 {
        app.tick();
    }

    let save = node_query::run_get_by_id("save").expect("#save resolves");
    let hidden = node_query::run_get_by_id("hidden").expect("#hidden resolves");
    let root = node_query::run_document().expect("document root");

    // --- Geometry (post-layout) ---
    let rect = ins::node_rect(save).expect("save has a rect");
    assert_eq!(rect.width, 100.0, "fixed width flows to rect");
    assert_eq!(rect.height, 40.0, "fixed height flows to rect");
    let content = ins::node_content_rect(save).expect("save has a content rect");
    assert_eq!(content.width, 92.0, "content width drops 2x4 padding");
    assert_eq!(content.height, 32.0, "content height drops 2x4 padding");
    // Content box origin is inset by the padding.
    assert_eq!(content.client_x, rect.client_x + 4.0);
    assert_eq!(content.client_y, rect.client_y + 4.0);

    // --- Visibility + stacking ---
    assert!(ins::node_is_visible(save), "save is visible");
    assert!(
        !ins::node_is_visible(hidden),
        "display:none reads as not visible"
    );
    assert_eq!(ins::node_z_index(save), 0, "default stacking is 0");

    // --- Computed style + provenance ---
    let cs: std::collections::HashMap<String, String> =
        ins::node_computed_style_map(save).into_iter().collect();
    assert_eq!(
        cs.get("text-color").map(String::as_str),
        Some("#00ff00"),
        "cascade color reaches computed_style"
    );
    assert_eq!(cs.get("font-size").map(String::as_str), Some("18px"));
    assert_eq!(cs.get("font-weight").map(String::as_str), Some("700"));

    let matched = ins::node_matched_rules(save);
    let selectors: Vec<&str> = matched.iter().map(|m| m.selector.as_str()).collect();
    assert!(selectors.contains(&".row"), "matched .row: {selectors:?}");
    assert!(selectors.contains(&"#save"), "matched #save: {selectors:?}");
    let save_rule = matched.iter().find(|m| m.selector == "#save").unwrap();
    assert_eq!(save_rule.source, "author");
    assert_eq!(save_rule.specificity, (1, 0, 0), "#id specificity (1,0,0)");
    let row_rule = matched.iter().find(|m| m.selector == ".row").unwrap();
    assert_eq!(
        row_rule.specificity,
        (0, 1, 0),
        ".class specificity (0,1,0)"
    );
    // Cascade order: the id rule sorts after the class rule (last wins).
    let row_pos = matched.iter().position(|m| m.selector == ".row").unwrap();
    let save_pos = matched.iter().position(|m| m.selector == "#save").unwrap();
    assert!(
        row_pos < save_pos,
        "#save sorts after .row in cascade order"
    );

    // --- Attributes / classes ---
    let attrs: std::collections::HashMap<String, String> =
        ins::node_attrs(save).into_iter().collect();
    assert_eq!(attrs.get("id").map(String::as_str), Some("save"));
    assert_eq!(attrs.get("class").map(String::as_str), Some("row"));
    assert_eq!(ins::node_classes(save), vec!["row".to_string()]);
    assert!(
        ins::node_inline_style(save).is_empty(),
        "no inline style yet"
    );

    // --- Inline style overlay ---
    node_query::push_external_dom_command(lumen_script::ScriptCommand::SetStyleProp {
        node: save,
        name: "color".into(),
        value: "#0000ff".into(),
    });
    for _ in 0..4 {
        app.tick();
    }
    let inline: std::collections::HashMap<String, String> =
        ins::node_inline_style(save).into_iter().collect();
    assert_eq!(
        inline.get("color").map(String::as_str),
        Some("#0000ff"),
        "inline color published"
    );
    let cs2: std::collections::HashMap<String, String> =
        ins::node_computed_style_map(save).into_iter().collect();
    assert_eq!(
        cs2.get("text-color").map(String::as_str),
        Some("#0000ff"),
        "inline overrides the cascade in computed_style"
    );

    // --- ECS introspection ---
    let comps = ins::node_components(save);
    assert!(
        comps.contains(&"LayoutBox".to_string()),
        "LayoutBox present: {comps:?}"
    );
    let layout_box: std::collections::HashMap<String, String> =
        ins::node_component(save, "LayoutBox")
            .expect("LayoutBox is whitelisted")
            .expect("save carries a LayoutBox")
            .into_iter()
            .collect();
    assert_eq!(layout_box.get("width").map(String::as_str), Some("100"));
    assert_eq!(layout_box.get("height").map(String::as_str), Some("40"));
    assert!(
        ins::node_component(save, "NotAComponent").is_err(),
        "unknown component name is an error"
    );

    // entity_id round-trips against the packed handle.
    let (index, generation) = ins::node_entity_id(save).expect("entity id");
    let unpacked = lumen_core::node::NodeHandle::unpack(save).unwrap();
    assert_eq!(index, unpacked.entity.to_bits() as u32);
    let _ = generation;

    // --- Tree serialization ---
    let markup = ins::outer_markup(root);
    assert!(markup.contains("id=\"save\""), "outer_markup: {markup}");
    assert!(markup.contains("class=\"row\""));
    let dump = ins::dump_tree();
    assert!(dump.contains("button#save.row"), "dump_tree: {dump}");
    assert!(dump.contains("app"), "dump_tree root: {dump}");

    // --- Global state (read without panic) ---
    let _ = ins::pointer_state();
    let frame = ins::frame_info();
    assert!(frame.frame > 0, "frame counter advances");
    let _ = ins::signals_all();

    // Null / stale handle never panics.
    assert!(ins::node_rect(0).is_none());
    assert!(!ins::node_is_visible(0));
    assert!(ins::node_computed_style_map(0).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
