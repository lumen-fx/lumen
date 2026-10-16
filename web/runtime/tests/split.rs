//! `<split>` in a real page: the browser lays the panes out, and the page's
//! app drags and steps the gutter, measuring the panes off the browser's
//! boxes and writing the `flex-basis` it holds onto them.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown --test split
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(target_arch = "wasm32")]

use lumen_core::prelude::App;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR, WidgetPart};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{Navigation, Routes, WebDomPlugin};
use lumen_web_runtime::assemble;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    Element as DomElement, HtmlElement, KeyboardEvent, KeyboardEventInit, PointerEvent,
    PointerEventInit,
};

wasm_bindgen_test_configure!(run_in_browser);

/// The rules the site's stylesheet carries for a split, from the reset and
/// the user-agent sheet, plus a fixed place for this one.
const CSS: &str = "
.lm-split { display: flex; flex-direction: row; position: fixed; left: 0; top: 0; width: 406px; height: 200px; }
.lm-column { display: flex; flex-direction: column; }
.split-pane { min-width: 0; min-height: 0; flex-grow: 1; flex-basis: 0; }
#left { min-width: 100px; }
.split-gutter { flex: 0 0 6px; user-select: none; touch-action: none; }
";

/// A split of two panes, as the parser writes it: the panes marked, a
/// gutter between them.
fn tree() -> LayoutIR {
    let pane = |id: &str| Element {
        tag: "column".to_string(),
        attrs: Attributes {
            id: Some(id.to_string()),
            classes: vec!["split-pane".to_string()],
            ..Attributes::default()
        },
        ..Element::default()
    };
    let gutter = Element {
        tag: "div".to_string(),
        attrs: Attributes {
            id: Some("gutter".to_string()),
            part: Some(WidgetPart::SplitGutter),
            classes: vec!["split-gutter".to_string()],
            tab_index: Some(0),
            ..Attributes::default()
        },
        ..Element::default()
    };
    let split = Element {
        tag: "split".to_string(),
        children: vec![pane("left"), gutter, pane("right")],
        ..Element::default()
    };
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![split],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

fn boot() -> (App, DomElement) {
    let ir = tree();
    let spec = SiteSpec {
        pages: vec![PageSpec::new("index", ir.clone())],
        web: WebSpec {
            runtime: false,
            ..WebSpec::default()
        },
        ..SiteSpec::default()
    };
    let mut warnings = Vec::new();
    let html = lumen_web::html::emit_tree(&spec.pages[0], &spec, &mut warnings)
        .expect("the tree emits")
        .0;
    let document = web_sys::window().unwrap().document().unwrap();
    let style = document.create_element("style").unwrap();
    style.set_text_content(Some(CSS));
    document.body().unwrap().append_child(&style).unwrap();
    let container = document.create_element("div").unwrap();
    container.set_inner_html(&html);
    document.body().unwrap().append_child(&container).unwrap();
    let root = container.first_element_child().expect("the page root");

    let mut app = assemble::portable_app();
    let compiled = CompiledApp {
        ir,
        ..CompiledApp::default()
    };
    let root_entity = compiled.spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin {
        root: root.clone(),
        root_entity,
        routes: Routes::default(),
        navigation: Navigation::InPlace,
    });
    lumen_web_dom::listen(&root, None).expect("the page takes listeners");
    for _ in 0..3 {
        app.tick();
    }
    (app, root)
}

fn find(root: &DomElement, id: &str) -> DomElement {
    root.query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("the page has #{id}"))
}

fn width(el: &DomElement) -> f64 {
    el.get_bounding_client_rect().width()
}

fn pointer(target: &DomElement, kind: &str, x: i32, y: i32) {
    let init = PointerEventInit::new();
    init.set_bubbles(true);
    init.set_client_x(x);
    init.set_client_y(y);
    init.set_button(if kind == "pointermove" { -1 } else { 0 });
    init.set_pointer_type("mouse");
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    target.dispatch_event(&event).unwrap();
}

fn tick(app: &mut App) {
    for _ in 0..2 {
        app.tick();
    }
}

/// The gutter is a focusable separator; a drag on it moves the boundary
/// with the pointer, written as the panes' `flex-basis`; the arrow keys on
/// the focused gutter step it, without scrolling the page, and Home takes
/// the pane before it to its `min-width`.
#[wasm_bindgen_test]
fn the_page_drags_and_steps_the_gutter() {
    let (mut app, root) = boot();
    let gutter = find(&root, "gutter");
    let left = find(&root, "left");
    let right = find(&root, "right");
    assert_eq!(gutter.get_attribute("role").as_deref(), Some("separator"));
    assert_eq!(gutter.get_attribute("tabindex").as_deref(), Some("0"));
    assert_eq!(width(&left), 200.0);
    assert_eq!(width(&right), 200.0);
    assert_eq!(
        gutter.get_attribute("aria-valuenow").as_deref(),
        Some("50"),
        "the app measured the panes once they were in the page"
    );
    assert_eq!(
        gutter.get_attribute("aria-orientation").as_deref(),
        Some("vertical")
    );

    pointer(&gutter, "pointerdown", 203, 100);
    tick(&mut app);
    pointer(&gutter, "pointermove", 263, 120);
    tick(&mut app);
    pointer(&gutter, "pointerup", 263, 120);
    tick(&mut app);
    assert_eq!(width(&left), 260.0);
    assert_eq!(width(&right), 140.0);
    let basis = left
        .unchecked_ref::<HtmlElement>()
        .style()
        .get_property_value("flex-basis")
        .unwrap();
    assert_eq!(basis, "260px");
    assert_eq!(gutter.get_attribute("aria-valuenow").as_deref(), Some("65"));

    gutter.unchecked_ref::<HtmlElement>().focus().unwrap();
    tick(&mut app);
    let key = |name: &str| {
        let init = KeyboardEventInit::new();
        init.set_key(name);
        init.set_bubbles(true);
        init.set_cancelable(true);
        let event = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
        gutter.dispatch_event(&event).unwrap();
        event
    };
    let event = key("ArrowLeft");
    assert!(event.default_prevented(), "the page does not scroll for it");
    tick(&mut app);
    assert_eq!(width(&left), 250.0);
    key("Home");
    tick(&mut app);
    assert_eq!(width(&left), 100.0);
    assert_eq!(width(&right), 300.0);
}
