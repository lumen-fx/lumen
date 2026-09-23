//! The pointer in a real page: the browser's own `PointerEvent` and
//! `WheelEvent`, dispatched at an element, reaching a candela handler bound to
//! that element with `event_on`, carrying what the browser reported.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use lumen_core::prelude::App;
use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_script::{ScriptValue, event};
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{Navigation, Routes, WebDomPlugin};
use lumen_web_runtime::hosts::ScriptHostAccess;
use lumen_web_runtime::{assemble, hosts};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    Element as DomElement, EventTarget, PointerEvent, PointerEventInit, WheelEvent, WheelEventInit,
};

wasm_bindgen_test_configure!(run_in_browser);

/// The program the build script compiled: handlers for every pointer event on
/// the element with id `pad`.
const POINTER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pointer.cdlb"));

/// A page of one element the script binds.
fn tree() -> LayoutIR {
    let pad = Element {
        tag: "label".to_string(),
        attrs: Attributes {
            id: Some("pad".to_string()),
            text: Some("pad".to_string()),
            ..Attributes::default()
        },
        ..Element::default()
    };
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![pad],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

/// Write the page the emitter writes, put it in the document, and boot the
/// app into it the way the page's own boot does, listeners included.
fn boot() -> (App, ScriptHostAccess, DomElement) {
    // Bindings live in one registry per page, and every test here boots the
    // same tree, so a binding an earlier test made would answer for this
    // test's element too.
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
    let host = hosts::install(&mut app, "candela", POINTER, "pointer.cdlb")
        .expect("this build carries the candela host");
    let compiled = CompiledApp {
        ir,
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(POINTER.to_vec()),
        }],
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
    // Mount, then `on_ready`, then the bindings it made.
    for _ in 0..3 {
        app.tick();
    }
    let pad = root
        .query_selector("#pad")
        .unwrap()
        .expect("the pad element");
    (app, host, pad)
}

/// Raise a pointer event of `kind` at `target`, bubbling, as the browser
/// raises one for a mouse.
fn pointer(target: &EventTarget, kind: &str, x: i32, y: i32, button: i16) {
    let init = PointerEventInit::new();
    init.set_bubbles(true);
    init.set_client_x(x);
    init.set_client_y(y);
    init.set_button(button);
    init.set_pointer_type("mouse");
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    target.dispatch_event(&event).unwrap();
}

/// A float signal the handler wrote.
fn float(app: &App, host: &ScriptHostAccess, name: &str) -> f64 {
    match (host.signal)(&app.world, name) {
        Some(ScriptValue::F64(v)) => v,
        Some(ScriptValue::I64(v)) => v as f64,
        other => panic!("{name}: the handler never wrote it ({other:?})"),
    }
}

/// How many times the handler ran for `kind`.
fn count(app: &App, host: &ScriptHostAccess, kind: &str) -> i64 {
    match (host.signal)(&app.world, &format!("{kind}_count")) {
        Some(ScriptValue::I64(v)) => v,
        Some(ScriptValue::F64(v)) => v as i64,
        _ => 0,
    }
}

fn tick(app: &mut App) {
    for _ in 0..2 {
        app.tick();
    }
}

#[wasm_bindgen_test]
fn press_move_and_release_reach_the_handler_with_their_coordinates() {
    let (mut app, host, pad) = boot();

    pointer(&pad, "pointermove", 11, 21, -1);
    tick(&mut app);
    assert_eq!(count(&app, &host, "pointermove"), 1, "one move, one call");
    assert_eq!(float(&app, &host, "pointermove_x"), 11.0);
    assert_eq!(float(&app, &host, "pointermove_y"), 21.0);
    assert_eq!(
        count(&app, &host, "pointerenter"),
        1,
        "the pointer arriving over the element is its pointerenter"
    );

    pointer(&pad, "pointerdown", 12, 22, 0);
    tick(&mut app);
    assert_eq!(count(&app, &host, "pointerdown"), 1);
    assert_eq!(float(&app, &host, "pointerdown_x"), 12.0);
    assert_eq!(float(&app, &host, "pointerdown_y"), 22.0);
    assert_eq!(float(&app, &host, "pointerdown_button"), 0.0);

    pointer(&pad, "pointerup", 13, 23, 0);
    tick(&mut app);
    assert_eq!(count(&app, &host, "pointerup"), 1);
    assert_eq!(float(&app, &host, "pointerup_x"), 13.0);
    assert_eq!(float(&app, &host, "pointerup_y"), 23.0);
}

/// The browser's own click reaches a handler bound with `event_on`, through
/// the same dispatch the pointer events take.
#[wasm_bindgen_test]
fn a_click_reaches_the_handler_bound_to_the_element() {
    let (mut app, host, pad) = boot();

    pad.unchecked_ref::<web_sys::HtmlElement>().click();
    tick(&mut app);

    assert_eq!(count(&app, &host, "click"), 1);
}

/// Many moves between two frames are one move at the last position: what a
/// fast mouse costs is one handler call per frame.
#[wasm_bindgen_test]
fn moves_between_two_frames_arrive_as_one() {
    let (mut app, host, pad) = boot();
    let before = count(&app, &host, "pointermove");

    for x in 1..=10 {
        pointer(&pad, "pointermove", x, 5, -1);
    }
    tick(&mut app);

    assert_eq!(count(&app, &host, "pointermove") - before, 1);
    assert_eq!(float(&app, &host, "pointermove_x"), 10.0);
}

/// A wheel that reports lines is converted to pixels the way the window
/// backend converts it, so one handler reads the same distance on both.
#[wasm_bindgen_test]
fn a_wheel_reaches_the_handler_in_pixels() {
    let (mut app, host, pad) = boot();

    let init = WheelEventInit::new();
    init.set_bubbles(true);
    init.set_client_x(30);
    init.set_client_y(40);
    init.set_delta_y(3.0);
    init.set_delta_mode(WheelEvent::DOM_DELTA_LINE);
    let wheel = WheelEvent::new_with_event_init_dict("wheel", &init).unwrap();
    pad.dispatch_event(&wheel).unwrap();
    tick(&mut app);

    assert_eq!(count(&app, &host, "wheel"), 1);
    assert_eq!(float(&app, &host, "wheel_x"), 30.0);
    assert_eq!(float(&app, &host, "wheel_delta_y"), 96.0);
    assert!(
        !wheel.default_prevented(),
        "the page still scrolls: nothing cancels a wheel"
    );
}

/// The pointer leaving the document takes the hover with it.
#[wasm_bindgen_test]
fn leaving_the_page_is_the_element_s_pointerleave() {
    let (mut app, host, pad) = boot();
    pointer(&pad, "pointermove", 5, 5, -1);
    tick(&mut app);
    let left_before = count(&app, &host, "pointerleave");

    // No `relatedTarget`: nothing in the page is being entered.
    pointer(&pad, "pointerout", 5, 5, -1);
    tick(&mut app);

    assert_eq!(count(&app, &host, "pointerleave") - left_before, 1);
}

/// A touch lifts off and its pointer goes away in the same frame. The
/// release still reaches the element it happened on, rather than being aimed
/// at nothing because the pointer had already gone by the time it was
/// delivered.
#[wasm_bindgen_test]
fn a_tap_s_release_reaches_the_element_before_the_pointer_goes() {
    let (mut app, host, pad) = boot();
    let up_before = count(&app, &host, "pointerup");

    for kind in ["pointerdown", "pointerup", "pointerout"] {
        let init = PointerEventInit::new();
        init.set_bubbles(true);
        init.set_client_x(7);
        init.set_client_y(8);
        init.set_button(if kind == "pointerout" { -1 } else { 0 });
        init.set_pointer_type("touch");
        let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
        pad.dispatch_event(&event).unwrap();
    }
    for _ in 0..4 {
        app.tick();
    }

    assert_eq!(count(&app, &host, "pointerup") - up_before, 1);
    assert_eq!(float(&app, &host, "pointerup_x"), 7.0);
}
