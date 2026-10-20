//! The browser's own controls standing in for Lumen's: a color input whose
//! picker reports `input` and `change`, a number input with its range, and a
//! `contextmenu` a handler keeps the browser's menu from answering.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown --test controls
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(target_arch = "wasm32")]

use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumen_core::prelude::App;
use lumen_core::property_store::PropertyStore;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Attributes, BindKind, BindSpec, Element, InputType, LayoutIR};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_script::event;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{Navigation, Routes, WebDomPlugin};
use lumen_web_runtime::assemble;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    Element as DomElement, HtmlInputElement, InputEvent, InputEventInit, MouseEvent, MouseEventInit,
};

wasm_bindgen_test_configure!(run_in_browser);

/// A page with a color field bound to `tint`, a number field, and a pad to
/// right-click.
fn tree() -> LayoutIR {
    let mut color = Attributes {
        id: Some("tint".to_string()),
        input_type: Some(InputType::Color),
        text: Some("#3366ff".to_string()),
        ..Attributes::default()
    };
    color.bind = Some(BindSpec {
        kind: BindKind::Value,
        name: "tint".to_string(),
    });
    let number = Attributes {
        id: Some("qty".to_string()),
        input_type: Some(InputType::Number),
        text: Some("4".to_string()),
        min: Some(0.0),
        max: Some(10.0),
        step: Some(2.0),
        ..Attributes::default()
    };
    let veil = Attributes {
        id: Some("veil".to_string()),
        input_type: Some(InputType::Color),
        alpha: true,
        text: Some("color(srgb 0.2 0.4 1 / 0.5)".to_string()),
        ..Attributes::default()
    };
    let pad = Attributes {
        id: Some("pad".to_string()),
        ..Attributes::default()
    };
    let el = |tag: &str, attrs: Attributes| Element {
        tag: tag.to_string(),
        attrs: Box::new(attrs),
        ..Element::default()
    };
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![
                el("input", color),
                el("input", number),
                el("input", veil),
                el("column", pad),
            ],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

/// Write the page the emitter writes, boot the app into it with its
/// listeners, and let a listener that needs an answer at once run a tick.
fn boot() -> (Arc<Mutex<App>>, DomElement) {
    event::clear_all_bindings();
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
    let app = Arc::new(Mutex::new(app));
    let driven = Arc::clone(&app);
    lumen_web_dom::set_run_now(move || driven.try_lock().map(|mut a| a.tick()).is_ok());
    (app, root)
}

fn node(app: &Arc<Mutex<App>>, id: &str) -> u64 {
    let mut app = app.lock().unwrap();
    let mut q = app.world.query::<(Entity, &LumenId)>();
    let entity = q
        .iter(&app.world)
        .find(|(_, l)| l.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element #{id}"));
    lumen_core::node::NodeHandle::new(entity).pack()
}

/// Record every `kind` event delivered to `node` as `type value`.
fn watch(node: u64, kinds: &[&str], prevent: bool) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    for kind in kinds {
        let sink = Arc::clone(&seen);
        event::register_native_binding(
            node,
            (*kind).to_string(),
            false,
            Arc::new(move || {
                sink.lock().unwrap().push(format!(
                    "{} {}",
                    event::event_type(),
                    event::event_value()
                ));
                if prevent {
                    event::event_prevent_default();
                }
            }),
        );
    }
    seen
}

fn find(root: &DomElement, id: &str) -> DomElement {
    root.query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("the page has #{id}"))
}

fn raise(target: &DomElement, kind: &str) {
    let init = InputEventInit::new();
    init.set_bubbles(true);
    let event = InputEvent::new_with_event_init_dict(kind, &init).unwrap();
    target.dispatch_event(&event).unwrap();
}

fn tick(app: &Arc<Mutex<App>>) {
    for _ in 0..2 {
        app.lock().unwrap().tick();
    }
}

/// The browser's color input is the field: what its picker reports reaches
/// `input` and `change` handlers with the color, and the bound signal.
#[wasm_bindgen_test]
fn the_color_input_reports_input_change_and_its_binding() {
    let (app, root) = boot();
    let tint = find(&root, "tint");
    assert_eq!(tint.get_attribute("type").as_deref(), Some("color"));
    let input: HtmlInputElement = tint.clone().dyn_into().unwrap();
    assert_eq!(input.value(), "#3366ff");
    let seen = watch(node(&app, "tint"), &["input", "change"], false);

    input.set_value("#ff8800");
    raise(&tint, "input");
    tick(&app);
    raise(&tint, "change");
    tick(&app);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["input #ff8800".to_string(), "change #ff8800".to_string()]
    );
    let bound = app
        .lock()
        .unwrap()
        .world
        .resource::<PropertyStore>()
        .get_global_str("tint")
        .map(|s| s.to_string());
    assert_eq!(bound.as_deref(), Some("#ff8800"));
}

/// A color field with `alpha` carries the attribute into the page. Whatever
/// the browser reports, the field keeps it in the value form its `alpha`
/// asks for.
#[wasm_bindgen_test]
fn the_color_input_carries_alpha() {
    let (app, root) = boot();
    let veil = find(&root, "veil");
    assert_eq!(veil.get_attribute("alpha").as_deref(), Some(""));
    let input: HtmlInputElement = veil.clone().dyn_into().unwrap();
    // Chrome reads the `color()` form and, having no alpha, writes it back
    // as `#rrggbb`; a browser with `alpha` keeps the form it was given.
    let shown = input.value();
    assert!(
        shown == "#3366ff" || shown.starts_with("color(srgb 0.2 0.4 1"),
        "{shown}"
    );
    let seen = watch(node(&app, "veil"), &["input"], false);
    input.set_value("#ff8800");
    raise(&veil, "input");
    tick(&app);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].starts_with("input color(srgb 1 0.533333 0"),
        "{seen:?}"
    );
}

/// The number field is the browser's number input, with its range.
#[wasm_bindgen_test]
fn the_number_input_carries_its_range() {
    let (_app, root) = boot();
    let qty = find(&root, "qty");
    assert_eq!(qty.get_attribute("type").as_deref(), Some("number"));
    assert_eq!(qty.get_attribute("min").as_deref(), Some("0"));
    assert_eq!(qty.get_attribute("max").as_deref(), Some("10"));
    assert_eq!(qty.get_attribute("step").as_deref(), Some("2"));
    assert_eq!(
        qty.clone().unchecked_into::<HtmlInputElement>().value(),
        "4"
    );
    assert_eq!(qty.child_element_count(), 0, "the browser draws the steps");
}

/// A `contextmenu` handler that prevents the default keeps the browser's own
/// menu shut: the handler runs inside the browser's dispatch.
#[wasm_bindgen_test]
fn a_contextmenu_handler_keeps_the_browser_menu_shut() {
    let (app, root) = boot();
    let seen = watch(node(&app, "pad"), &["contextmenu"], true);
    let init = MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_button(2);
    let event = MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init).unwrap();
    find(&root, "pad").dispatch_event(&event).unwrap();
    assert!(event.default_prevented(), "the browser's menu stays shut");
    assert_eq!(seen.lock().unwrap().len(), 1);
}
