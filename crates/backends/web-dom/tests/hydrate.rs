//! What the two halves of the web target have to agree about, checked in a
//! real browser against markup the real emitter wrote.
//!
//! The emitter and the runtime derive a node's path from the same IR by two
//! different walks, and everything rests on those walks agreeing. Nothing
//! short of running both catches a disagreement, which is why these tests
//! emit the page here rather than asserting against a fixture string.
//!
//! ```sh
//! cargo test -p lumen-web-dom --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(target_arch = "wasm32")]

use bevy_ecs::prelude::*;
use lumen_core::components::{LumenClasses, TextContent, Visible};
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::{DATA_LM, DATA_LM_HIDDEN};
use lumen_ir::layout_ir::{Attributes, Element as IrElement, IfModeSpec, LayoutIR};
use lumen_scene::spawn;
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{NodeTable, WebDomPlugin};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

wasm_bindgen_test_configure!(run_in_browser);

/// An IR element with `tag`, `text` and `children`.
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

/// The tree every test here hydrates: a root with a label, a row of two
/// labels, and a class on the row.
fn tree() -> LayoutIR {
    let mut row = element(
        "row",
        None,
        vec![
            element("label", Some("left"), Vec::new()),
            element("label", Some("right"), Vec::new()),
        ],
    );
    row.attrs.classes = vec!["pair".to_string()];
    LayoutIR {
        root: element(
            "root",
            None,
            vec![element("label", Some("top"), Vec::new()), row],
        ),
        ..LayoutIR::default()
    }
}

/// Emit `ir` the way `lumenc web` would, and put the result in the document.
fn prerender(ir: LayoutIR) -> Element {
    let spec = SiteSpec {
        pages: vec![PageSpec::new("index", ir)],
        web: WebSpec {
            runtime: false,
            ..WebSpec::default()
        },
        ..SiteSpec::default()
    };
    let html = lumen_web::html::emit_tree(&spec.pages[0], &spec).expect("the tree emits");

    let document = web_sys::window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_inner_html(&html);
    document.body().unwrap().append_child(&host).unwrap();
    host.first_element_child().expect("the page root")
}

/// Spawn `ir` into an app bound to `root`, and run one tick.
fn hydrate(ir: LayoutIR, root: Element) -> App {
    let mut app = App::new();
    app.extract_fns.clear();
    let root_entity = ir.spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin { root, root_entity });
    app.tick();
    app
}

/// What the node table ended up holding.
fn report(app: &App) -> (u32, u32) {
    let table = app.world.non_send::<NodeTable>();
    (table.report().adopted, table.report().created)
}

#[wasm_bindgen_test]
fn a_prerendered_page_is_adopted_without_being_touched() {
    let root = prerender(tree());
    let before = root.outer_html();

    let app = hydrate(tree(), root.clone());

    assert_eq!(
        report(&app),
        (5, 0),
        "every node of the tree bound to the element the emitter wrote for it"
    );
    assert_eq!(
        root.outer_html(),
        before,
        "and adopting the page changed nothing in it"
    );
}

#[wasm_bindgen_test]
fn a_node_the_page_is_missing_is_built_instead() {
    let root = prerender(tree());
    // Take out the second label of the row, which is `0.1.1`.
    root.query_selector(&format!("[{DATA_LM}=\"0.1.1\"]"))
        .unwrap()
        .expect("the emitter wrote it")
        .remove();

    let app = hydrate(tree(), root.clone());

    assert_eq!(report(&app), (4, 1), "the one missing node was built");
    let rebuilt = root
        .query_selector(&format!("[{DATA_LM}=\"0.1.1\"]"))
        .unwrap()
        .expect("and it is in the page again");
    assert_eq!(rebuilt.text_content().as_deref(), Some("right"));
    assert_eq!(
        rebuilt.previous_element_sibling().map(|e| e.outer_html()),
        root.query_selector(&format!("[{DATA_LM}=\"0.1.0\"]"))
            .unwrap()
            .map(|e| e.outer_html()),
        "in the place the walk says it goes, not appended at the end"
    );
}

#[wasm_bindgen_test]
fn a_page_with_no_markup_at_all_is_built_from_the_tree() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&root).unwrap();

    let app = hydrate(tree(), root.clone());

    assert_eq!(report(&app), (1, 4), "the root is given; the rest is built");
    assert_eq!(
        root.query_selector_all(&format!("[{DATA_LM}]"))
            .unwrap()
            .length(),
        4,
        "every node below the root is in the page"
    );
    assert_eq!(
        root.query_selector(".lm-row.pair")
            .unwrap()
            .expect("a built element carries its tag class and its own")
            .child_element_count(),
        2
    );
}

#[wasm_bindgen_test]
fn what_the_world_changes_lands_in_the_page() {
    let root = prerender(tree());
    let mut app = hydrate(tree(), root.clone());

    let label = find(&mut app, "top");
    app.world
        .entity_mut(label)
        .insert(TextContent("bottom".into()));
    app.world
        .entity_mut(label)
        .insert(LumenClasses::from(vec!["loud".to_string()]));
    app.world.entity_mut(label).insert(Visible(false));
    app.tick();

    let element = root
        .query_selector(&format!("[{DATA_LM}=\"0.0\"]"))
        .unwrap()
        .expect("the label");
    assert_eq!(element.text_content().as_deref(), Some("bottom"));
    assert_eq!(
        element.get_attribute("class").as_deref(),
        Some("lm-label loud")
    );
    assert!(element.has_attribute(DATA_LM_HIDDEN));

    app.world.entity_mut(label).insert(Visible(true));
    app.tick();
    assert!(!element.has_attribute(DATA_LM_HIDDEN));
}

#[wasm_bindgen_test]
fn an_entity_that_goes_takes_its_element_with_it() {
    let root = prerender(tree());
    let mut app = hydrate(tree(), root.clone());

    let label = find(&mut app, "left");
    app.world.entity_mut(label).despawn();
    app.tick();

    assert!(
        root.query_selector(&format!("[{DATA_LM}=\"0.1.0\"]"))
            .unwrap()
            .is_none(),
        "the element the entity stood for is out of the page"
    );
}

#[wasm_bindgen_test]
fn a_click_reaches_the_entity_the_element_stands_for() {
    let root = prerender(tree());
    let mut app = hydrate(tree(), root.clone());
    app.add_message::<lumen_core::input::ClickEvent>();
    app.add_systems(TickStage::Systems, record_clicks);
    app.world.init_resource::<Clicked>();

    lumen_web_dom::listen(&root).expect("the page takes listeners");
    let target: web_sys::HtmlElement = root
        .query_selector(&format!("[{DATA_LM}=\"0.1.1\"]"))
        .unwrap()
        .expect("the label")
        .unchecked_into();
    // The browser's own click, which bubbles to the one listener on the root
    // exactly as a visitor's would.
    target.click();
    app.tick();

    let expected = find(&mut app, "right");
    assert_eq!(
        app.world.resource::<Clicked>().0,
        Some(expected),
        "the click landed on the entity whose element it was dispatched on"
    );
}

/// A page whose only content is a dialog gated on `dialog_open`.
fn dialog_tree() -> LayoutIR {
    let mut dialog = element(
        "dialog",
        None,
        vec![element("label", Some("sure?"), vec![])],
    );
    dialog.attrs.if_signal = Some("dialog_open".to_string());
    dialog.attrs.if_mode = IfModeSpec::Hide;
    LayoutIR {
        root: element("root", None, vec![dialog]),
        ..LayoutIR::default()
    }
}

/// Spawn `ir` into an app that also reconciles its branches, and settle it.
fn hydrate_reactive(ir: LayoutIR, root: Element) -> App {
    let mut app = App::new();
    app.extract_fns.clear();
    app.world.init_resource::<PropertyStore>();
    let root_entity = ir.spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin { root, root_entity });
    app.add_systems(TickStage::Systems, spawn::reconcile_if_blocks);
    app.tick();
    app
}

/// Write a global signal and run a tick.
fn set_signal(app: &mut App, name: &str, value: &str) {
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str(name, value);
    app.tick();
}

#[wasm_bindgen_test]
fn a_dialog_opens_and_closes_as_the_browser_s_own() {
    let root = prerender(dialog_tree());
    let mut app = hydrate_reactive(dialog_tree(), root.clone());
    let dialog: web_sys::HtmlDialogElement = root
        .query_selector("dialog")
        .unwrap()
        .expect("the emitter wrote a dialog")
        .unchecked_into();

    assert!(
        !dialog.open(),
        "a dialog whose signal is false starts closed"
    );

    set_signal(&mut app, "dialog_open", "1");
    assert!(dialog.open(), "the signal turning true showed it");
    assert!(
        dialog.matches(":modal").unwrap(),
        "and showed it modally, which is what puts it over the page"
    );
    assert!(
        !dialog.has_attribute(DATA_LM_HIDDEN),
        "whether it shows is `open` alone"
    );

    set_signal(&mut app, "dialog_open", "");
    assert!(!dialog.open(), "the signal turning false closed it");
}

#[wasm_bindgen_test]
fn a_dialog_the_browser_dismisses_takes_its_signal_with_it() {
    let root = prerender(dialog_tree());
    let mut app = hydrate_reactive(dialog_tree(), root.clone());
    lumen_web_dom::listen(&root).expect("the page takes listeners");
    let dialog: web_sys::HtmlDialogElement = root
        .query_selector("dialog")
        .unwrap()
        .expect("the emitter wrote a dialog")
        .unchecked_into();

    set_signal(&mut app, "dialog_open", "1");
    assert!(dialog.open());

    // What Escape on a showing dialog does: the browser closes the element
    // and says so.
    dialog.close();
    dialog
        .dispatch_event(&web_sys::Event::new("cancel").unwrap())
        .unwrap();
    app.tick();

    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("dialog_open")
            .as_deref(),
        Some(""),
        "the signal the dialog hangs off followed it closed"
    );
    app.tick();
    assert!(!dialog.open(), "and nothing showed it again");
}

/// The entity whose text is `text`.
fn find(app: &mut App, text: &str) -> Entity {
    app.world
        .query::<(Entity, &TextContent)>()
        .iter(&app.world)
        .find(|(_, t)| t.0 == text)
        .map(|(e, _)| e)
        .expect("the tree has that label")
}

/// The entity the last click reached.
#[derive(Resource, Default)]
struct Clicked(Option<Entity>);

fn record_clicks(
    mut clicks: bevy_ecs::message::MessageReader<lumen_core::input::ClickEvent>,
    mut seen: ResMut<Clicked>,
) {
    for click in clicks.read() {
        seen.0 = Some(click.entity);
    }
}
