//! An element an add-on answers for, in a browser, against markup the emitter
//! wrote.
//!
//! ```sh
//! cargo test -p lumen-web-dom --target wasm32-unknown-unknown
//! ```

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use bevy_ecs::prelude::*;
use lumen_core::components::{LumenAttributes, LumenTag, TextContent};
use lumen_core::prelude::App;
use lumen_html::contract::{DATA_LM, DATA_LM_FOREIGN, ForeignElement};
use lumen_ir::layout_ir::{Attributes, Element as IrElement, LayoutIR};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{
    ForeignElements, ForeignHooks, ForeignTag, Navigation, NodeTable, Routes, WebDomPlugin,
};
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

wasm_bindgen_test_configure!(run_in_browser);

const TAG: &str = "echo-view";

fn element(tag: &str, text: Option<&str>, children: Vec<IrElement>) -> IrElement {
    IrElement {
        tag: tag.to_string(),
        attrs: Attributes {
            text: text.map(str::to_string),
            ..Attributes::default()
        },
        children,
        ..IrElement::default()
    }
}

/// A label, the add-on's element with a label of its own inside, and a label
/// after it.
fn tree() -> LayoutIR {
    LayoutIR {
        root: element(
            "root",
            None,
            vec![
                element("label", Some("top"), Vec::new()),
                element(
                    TAG,
                    Some("fallback"),
                    vec![element("label", Some("inside"), Vec::new())],
                ),
                element("label", Some("after"), Vec::new()),
            ],
        ),
        ..LayoutIR::default()
    }
}

/// Emit the tree the way `lumenc web` would, and put it in the document.
fn prerender() -> Element {
    let mut web = WebSpec {
        runtime: false,
        ..WebSpec::default()
    };
    web.foreign.insert(
        TAG.to_string(),
        ForeignElement {
            html: "section".to_string(),
            void: false,
        },
    );
    let spec = SiteSpec {
        pages: vec![PageSpec::new("index", tree())],
        web,
        ..SiteSpec::default()
    };
    let mut warnings = Vec::new();
    let html = lumen_web::html::emit_tree(&spec.pages[0], &spec, &mut warnings)
        .expect("the tree emits")
        .0;
    let document = web_sys::window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_inner_html(&html);
    document.body().unwrap().append_child(&host).unwrap();
    host.first_element_child().expect("the page root")
}

/// Every hook call, as `hook name value`.
type Heard = Rc<RefCell<Vec<String>>>;

/// Hooks that write down every call.
fn hooks(heard: &Heard) -> ForeignHooks {
    let object = js_sys::Object::new();
    let mount_heard = Rc::clone(heard);
    let mount = Closure::<dyn Fn(web_sys::Element)>::new(move |element: web_sys::Element| {
        mount_heard
            .borrow_mut()
            .push(format!("mount {}", element.tag_name().to_lowercase()));
        element.set_inner_html("<b>built by the add-on</b>");
    });
    let update_heard = Rc::clone(heard);
    let update = Closure::<dyn Fn(JsValue, String, String)>::new(
        move |_element: JsValue, name: String, value: String| {
            update_heard
                .borrow_mut()
                .push(format!("update {name} {value}"));
        },
    );
    let unmount_heard = Rc::clone(heard);
    let unmount = Closure::<dyn Fn(JsValue)>::new(move |_element: JsValue| {
        unmount_heard.borrow_mut().push("unmount".to_string());
    });
    for (name, closure) in [
        ("mount", mount.as_ref()),
        ("update", update.as_ref()),
        ("unmount", unmount.as_ref()),
    ] {
        js_sys::Reflect::set(&object, &JsValue::from_str(name), closure).unwrap();
    }
    mount.forget();
    update.forget();
    unmount.forget();
    ForeignHooks::from_object(&object)
}

/// Spawn the tree into an app bound to `root`, with the add-on's hooks.
fn hydrate(root: Element, heard: &Heard) -> App {
    let mut app = App::new();
    let root_entity = tree().spawn_into(&mut app.world);
    let mut foreign = ForeignElements::new();
    foreign.insert(
        TAG,
        ForeignTag {
            html: "section".to_string(),
            void: false,
            hooks: hooks(heard),
        },
    );
    app.world.insert_non_send(foreign);
    app.add_plugin(WebDomPlugin {
        root,
        root_entity,
        routes: Routes::default(),
        navigation: Navigation::InPlace,
    });
    app.tick();
    app
}

fn foreign_entity(app: &mut App) -> Entity {
    let mut query = app.world.query::<(Entity, &LumenTag)>();
    query
        .iter(&app.world)
        .find(|(_, tag)| &*tag.0 == TAG)
        .map(|(entity, _)| entity)
        .expect("the add-on's element was spawned")
}

#[wasm_bindgen_test]
fn the_emitter_writes_the_element_it_names_with_the_markup_inside() {
    let root = prerender();
    let foreign = root
        .query_selector(&format!("[{DATA_LM_FOREIGN}=\"{TAG}\"]"))
        .unwrap()
        .expect("the element is in the page");
    assert_eq!(foreign.tag_name().to_lowercase(), "section");
    assert_eq!(foreign.get_attribute(DATA_LM).as_deref(), Some("0.1"));
    // The content inside is a reader's fallback, not nodes the runtime binds.
    assert!(foreign.inner_html().contains("inside"));
    assert!(
        foreign
            .query_selector(&format!("[{DATA_LM}]"))
            .unwrap()
            .is_none()
    );
}

#[wasm_bindgen_test]
fn the_add_on_is_handed_its_element_and_nothing_inside_is_bound() {
    let root = prerender();
    let heard: Heard = Rc::default();
    let mut app = hydrate(root.clone(), &heard);

    let table = app.world.non_send::<NodeTable>();
    assert_eq!(
        (table.report().adopted, table.report().created),
        (4, 0),
        "the root, the two labels and the add-on's element; nothing inside it"
    );
    assert_eq!(
        heard.borrow().first().map(String::as_str),
        Some("mount section")
    );
    let foreign = root
        .query_selector(&format!("[{DATA_LM_FOREIGN}=\"{TAG}\"]"))
        .unwrap()
        .expect("the element is in the page");
    assert_eq!(foreign.inner_html(), "<b>built by the add-on</b>");

    // Text is the add-on's to show; it hears it rather than having it written.
    let entity = foreign_entity(&mut app);
    heard.borrow_mut().clear();
    app.world
        .entity_mut(entity)
        .insert(TextContent("changed".to_string()));
    let mut attributes = LumenAttributes::default();
    attributes.set("data-mood", "calm".to_string());
    app.world.entity_mut(entity).insert(attributes);
    app.tick();
    assert_eq!(foreign.inner_html(), "<b>built by the add-on</b>");
    assert_eq!(foreign.get_attribute("data-mood").as_deref(), Some("calm"));
    assert!(
        heard.borrow().contains(&"update text changed".to_string()),
        "{:?}",
        heard.borrow()
    );
    assert!(
        heard
            .borrow()
            .contains(&"update data-mood calm".to_string()),
        "{:?}",
        heard.borrow()
    );

    // Leaving the page is the add-on's to hear about first.
    app.world.entity_mut(entity).despawn();
    app.tick();
    assert_eq!(heard.borrow().last().map(String::as_str), Some("unmount"));
    assert!(
        root.query_selector(&format!("[{DATA_LM_FOREIGN}=\"{TAG}\"]"))
            .unwrap()
            .is_none()
    );
}

#[wasm_bindgen_test]
fn an_element_built_after_the_page_loaded_is_the_one_the_add_on_named() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&root).unwrap();
    let heard: Heard = Rc::default();
    let _app = hydrate(root.clone(), &heard);

    let foreign = root
        .query_selector(&format!("[{DATA_LM_FOREIGN}=\"{TAG}\"]"))
        .unwrap()
        .expect("the element was built");
    assert_eq!(foreign.tag_name().to_lowercase(), "section");
    assert_eq!(
        heard.borrow().first().map(String::as_str),
        Some("mount section")
    );
    assert_eq!(foreign.inner_html(), "<b>built by the add-on</b>");
}
