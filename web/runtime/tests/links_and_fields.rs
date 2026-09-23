//! A text field and two links in a browser page: the browser's own `input`
//! event reaching a candela handler bound with `event_on`, and a link click
//! whose handler calls `event_prevent_default` keeping the browser from
//! following the link.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use std::cell::RefCell;
use std::rc::Rc;

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
    Element as DomElement, HtmlInputElement, InputEvent, InputEventInit, MouseEvent, MouseEventInit,
};

wasm_bindgen_test_configure!(run_in_browser);

/// The program the build script compiled: an `input` handler on the field
/// with id `field`, and click handlers on the links `stay` and `go`.
const PROGRAM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/link_and_field.cdlb"));

/// An element with `id`, `text`, and `href` when it is a link.
fn element(tag: &str, id: &str, href: Option<&str>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Attributes {
            id: Some(id.to_string()),
            text: Some(id.to_string()),
            href: href.map(str::to_string),
            ..Attributes::default()
        },
        ..Element::default()
    }
}

/// A page of one text field and two links to places on the same document,
/// so a link the browser follows leaves the test where it is.
fn tree() -> LayoutIR {
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![
                element("input", "field", None),
                element("a", "stay", Some("#stay")),
                element("a", "go", Some("#go")),
            ],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

/// Write the page the emitter writes, put it in the document, boot the app
/// into it with its listeners, and let the listeners run its ticks the way
/// the frame loop does.
fn boot() -> (Rc<RefCell<App>>, ScriptHostAccess, DomElement) {
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
    let host = hosts::install(&mut app, "candela", PROGRAM, "link_and_field.cdlb")
        .expect("this build carries the candela host");
    let compiled = CompiledApp {
        ir,
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(PROGRAM.to_vec()),
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
    let app = Rc::new(RefCell::new(app));
    let driven = Rc::clone(&app);
    lumen_web_dom::set_run_now(move || driven.try_borrow_mut().map(|mut a| a.tick()).is_ok());
    (app, host, root)
}

fn signal(app: &Rc<RefCell<App>>, host: &ScriptHostAccess, name: &str) -> Option<ScriptValue> {
    (host.signal)(&app.borrow().world, name)
}

fn count(app: &Rc<RefCell<App>>, host: &ScriptHostAccess, name: &str) -> i64 {
    match signal(app, host, name) {
        Some(ScriptValue::I64(v)) => v,
        Some(ScriptValue::F64(v)) => v as i64,
        _ => 0,
    }
}

fn find(root: &DomElement, id: &str) -> DomElement {
    root.query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("the page has #{id}"))
}

/// A primary click, raised the way the browser raises one: bubbling, and
/// cancelable, so a listener can keep the browser from acting on it.
fn click(target: &DomElement) -> MouseEvent {
    let init = MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_button(0);
    let event = MouseEvent::new_with_mouse_event_init_dict("click", &init).unwrap();
    target.dispatch_event(&event).unwrap();
    event
}

/// Each edit the visitor makes in a field raises `input`, carrying the text
/// the field holds after it, as it does on the desktop.
#[wasm_bindgen_test]
fn an_edit_in_a_field_raises_input_with_the_field_s_text() {
    let (app, host, root) = boot();
    let field = find(&root, "field");
    let input = field
        .query_selector("input")
        .unwrap()
        .map(|el| el.unchecked_into::<HtmlInputElement>())
        .or_else(|| field.clone().dyn_into::<HtmlInputElement>().ok())
        .expect("the field is an <input>");

    for (typed, expected) in [("h", 1), ("hi", 2)] {
        input.set_value(typed);
        let init = InputEventInit::new();
        init.set_bubbles(true);
        init.set_data(Some(&typed[typed.len() - 1..]));
        let event = InputEvent::new_with_event_init_dict("input", &init).unwrap();
        input.dispatch_event(&event).unwrap();
        for _ in 0..2 {
            app.borrow_mut().tick();
        }
        assert_eq!(count(&app, &host, "input_count"), expected);
        assert_eq!(
            signal(&app, &host, "input_value"),
            Some(ScriptValue::Str(typed.into()))
        );
    }
}

/// A handler that calls `event_prevent_default` on a link's click keeps the
/// browser from following the link. The handler runs before the browser's
/// own dispatch of the click ends, since the browser acts on the link as soon
/// as it does.
#[wasm_bindgen_test]
fn a_handler_that_prevents_a_link_click_keeps_the_browser_from_following_it() {
    let (app, host, root) = boot();

    let event = click(&find(&root, "stay"));
    assert!(
        event.default_prevented(),
        "the browser was told not to follow the link"
    );
    assert_eq!(
        count(&app, &host, "stay_count"),
        1,
        "the handler ran inside the click, with no frame in between"
    );
}

/// A link whose handler does not prevent the click is followed as usual.
#[wasm_bindgen_test]
fn a_link_whose_handler_lets_the_click_through_is_followed() {
    let (app, host, root) = boot();

    let event = click(&find(&root, "go"));
    assert!(!event.default_prevented(), "the browser follows the link");
    assert_eq!(count(&app, &host, "go_count"), 1);
}
