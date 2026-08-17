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
use lumen_core::components::{LumenClasses, SliderValue, TextContent, Visible};
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::PropertyStore;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_html::contract::{DATA_LM, DATA_LM_HIDDEN};
use lumen_ir::layout_ir::{
    Attributes, Element as IrElement, IfModeSpec, InterpolationSlot, LayoutIR,
};
use lumen_scene::spawn;
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SignalEnv, SiteSpec, WebSpec};
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
    prerender_with(ir, SignalEnv::new())
}

/// The same, for a page rendered with state in it.
fn prerender_with(ir: LayoutIR, signals: SignalEnv) -> Element {
    let mut page = PageSpec::new("index", ir);
    page.signals = signals;
    let spec = SiteSpec {
        pages: vec![page],
        web: WebSpec {
            runtime: false,
            ..WebSpec::default()
        },
        ..SiteSpec::default()
    };
    let mut warnings = Vec::new();
    let html =
        lumen_web::html::emit_tree(&spec.pages[0], &spec, &mut warnings).expect("the tree emits");

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

/// What the walk took out of the page because nothing claimed it.
fn removed(app: &App) -> u32 {
    app.world.non_send::<NodeTable>().report().removed
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

#[wasm_bindgen_test]
fn a_slider_the_visitor_moves_moves_in_the_world_too() {
    let mut slider = element("slider", None, Vec::new());
    slider.attrs.min = Some(0.0);
    slider.attrs.max = Some(100.0);
    slider.attrs.value = Some(42.0);
    let ir = LayoutIR {
        root: element("root", None, vec![slider]),
        ..LayoutIR::default()
    };
    let root = prerender(ir.clone());
    let mut app = hydrate(ir, root.clone());
    lumen_web_dom::listen(&root).expect("the page takes listeners");

    let range: web_sys::HtmlInputElement = root
        .query_selector("input[type=range]")
        .unwrap()
        .expect("the emitter wrote a range input")
        .unchecked_into();
    range.set_value("75");
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    let moved_it = web_sys::Event::new_with_event_init_dict("input", &init).unwrap();
    range.dispatch_event(&moved_it).unwrap();
    app.tick();

    let moved = app
        .world
        .query::<&SliderValue>()
        .iter(&app.world)
        .map(|slider| slider.value)
        .next()
        .expect("the tree has a slider");
    assert_eq!(
        moved, 75.0,
        "where the visitor left the control is where the world has it"
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

/// A label whose text is the row's `name`, which is what a row template is
/// in nearly every list an app writes.
fn row_label(text: &str) -> IrElement {
    let mut label = element("label", Some(text), Vec::new());
    label.interpolations = vec![
        InterpolationSlot::Row("name".to_string()),
        InterpolationSlot::RowIndex,
    ];
    label
}

/// A page whose only content is a `<for>` over `items` with `body` as its
/// row template.
fn list_tree(body: Vec<IrElement>) -> LayoutIR {
    let mut block = element("for", None, body);
    block.attrs.each = Some("items".to_string());
    LayoutIR {
        root: element("root", None, vec![block]),
        ..LayoutIR::default()
    }
}

/// The rows of the `items` array, one `name` field each.
fn rows(names: &[&str]) -> Vec<ArrayItem> {
    names
        .iter()
        .map(|name| ArrayItem::from([("name".to_string(), (*name).to_string())]))
        .collect()
}

/// Spawn `ir` into an app holding `names` in `items`, reconcile its rows, and
/// settle it.
fn hydrate_list(ir: LayoutIR, root: Element, names: &[&str]) -> App {
    let mut app = App::new();
    app.extract_fns.clear();
    app.world.init_resource::<PropertyStore>();
    app.world.init_resource::<ArraySignals>();
    app.world
        .resource_mut::<ArraySignals>()
        .set("items", rows(names));
    let root_entity = ir.spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin { root, root_entity });
    app.add_systems(TickStage::Systems, spawn::reconcile_for_blocks);
    app.tick();
    app
}

/// The text of every element the `<for>` block holds, in page order.
fn row_texts(root: &Element) -> Vec<String> {
    let block = root
        .query_selector(".lm-for")
        .unwrap()
        .expect("the block is in the page");
    let mut texts = Vec::new();
    let mut child = block.first_element_child();
    while let Some(element) = child {
        texts.push(element.text_content().unwrap_or_default());
        child = element.next_element_sibling();
    }
    texts
}

#[wasm_bindgen_test]
fn the_rows_a_page_was_rendered_with_are_adopted_untouched() {
    let signals = SignalEnv::new().with_array("items", rows(&["one", "two", "three"]));
    let root = prerender_with(list_tree(vec![row_label("{row.name}")]), signals);
    let before = root.outer_html();

    let app = hydrate_list(
        list_tree(vec![row_label("{row.name}")]),
        root.clone(),
        &["one", "two", "three"],
    );

    assert_eq!(
        report(&app),
        (5, 0),
        "the root, the block and one label per row all bound to markup already in the page"
    );
    assert_eq!(removed(&app), 0, "and nothing was left over");
    assert_eq!(
        root.outer_html(),
        before,
        "adopting the rows changed nothing in the page"
    );
    assert_eq!(row_texts(&root), vec!["one", "two", "three"]);
}

#[wasm_bindgen_test]
fn a_shorter_list_takes_the_rows_the_page_has_too_many_of_out() {
    let signals = SignalEnv::new().with_array("items", rows(&["one", "two", "three"]));
    let root = prerender_with(list_tree(vec![row_label("{row.name}")]), signals);

    let app = hydrate_list(
        list_tree(vec![row_label("{row.name}")]),
        root.clone(),
        &["one", "two"],
    );

    assert_eq!(
        report(&app),
        (4, 0),
        "the two rows the app has were adopted"
    );
    assert_eq!(removed(&app), 1, "and the third was taken out of the page");
    assert_eq!(
        row_texts(&root),
        vec!["one", "two"],
        "no orphan is left showing a row the app does not have"
    );
}

#[wasm_bindgen_test]
fn a_longer_list_builds_the_rows_the_page_is_missing_in_order() {
    let signals = SignalEnv::new().with_array("items", rows(&["one", "two", "three"]));
    let root = prerender_with(list_tree(vec![row_label("{row.name}")]), signals);

    let app = hydrate_list(
        list_tree(vec![row_label("{row.name}")]),
        root.clone(),
        &["one", "two", "three", "four"],
    );

    assert_eq!(report(&app), (5, 1), "the fourth row is the one built");
    assert_eq!(removed(&app), 0);
    assert_eq!(
        row_texts(&root),
        vec!["one", "two", "three", "four"],
        "the built row goes after the last adopted one, not ahead of it"
    );
}

#[wasm_bindgen_test]
fn a_two_element_row_body_adopts_both_elements_of_every_row() {
    let body = vec![row_label("{row.name}"), row_label("#{$index}")];
    let signals = SignalEnv::new().with_array("items", rows(&["one", "two"]));
    let root = prerender_with(list_tree(body.clone()), signals);
    let before = root.outer_html();

    let app = hydrate_list(list_tree(body), root.clone(), &["one", "two"]);

    assert_eq!(
        report(&app),
        (6, 0),
        "a row is as many elements as its template, and every one of them was adopted"
    );
    assert_eq!(removed(&app), 0);
    assert_eq!(
        root.outer_html(),
        before,
        "which is what a row's identity being its flat slot means: 0.0::2 is row two's first \
         element, not row two"
    );
    assert_eq!(row_texts(&root), vec!["one", "#0", "two", "#1"]);
    assert_eq!(
        root.query_selector(&format!("[{DATA_LM}=\"0.0::2\"]"))
            .unwrap()
            .expect("the third slot")
            .text_content()
            .as_deref(),
        Some("two")
    );
}

/// A style written on an element outranks a rule that targets it, which is
/// what Lumen does and what a browser does not do on its own.
///
/// The whole point of writing these as rules rather than inline is that
/// `!important` stays free for the author, so this asserts the ranking in a
/// real browser rather than trusting the layer rules to read the way they
/// look. A styled row is built by the runtime, not by the emitter, so the
/// second assertion is that a class carried in the IR reaches a node the page
/// never contained.
#[wasm_bindgen_test]
fn a_style_on_an_element_beats_a_rule_that_targets_it() {
    let mut tile = element("tile", None, Vec::new());
    tile.attrs.classes = vec!["card".to_string()];
    tile.attrs.markup_styles = vec![("bg".to_string(), "#00ff00".to_string())];
    let mut ir = LayoutIR {
        root: element("root", None, vec![tile]),
        ..LayoutIR::default()
    };
    ir.combined_stylesheet = Some(authored_sheet());

    let markup = lumen_web::lift_markup_styles(&mut ir.root);
    let lifted = ir.root.children[0].attrs.classes.clone();
    assert_eq!(lifted.len(), 2, "the class the author wrote, then ours");

    let mut page = PageSpec::new("index", ir.clone());
    page.signals = SignalEnv::new();
    let spec = SiteSpec {
        pages: vec![page],
        web: WebSpec {
            runtime: false,
            ..WebSpec::default()
        },
        markup,
        ..SiteSpec::default()
    };
    let css = lumen_web::styles_css(
        ir.combined_stylesheet.as_ref(),
        &spec.markup,
        lumen_web::CssMode::Sheet,
    );

    let document = web_sys::window().unwrap().document().unwrap();
    let style = document.create_element("style").unwrap();
    style.set_text_content(Some(&css));
    document.body().unwrap().append_child(&style).unwrap();

    let mut warnings = Vec::new();
    let html =
        lumen_web::html::emit_tree(&spec.pages[0], &spec, &mut warnings).expect("the tree emits");
    let host = document.create_element("div").unwrap();
    host.set_inner_html(&html);
    document.body().unwrap().append_child(&host).unwrap();

    let tile = host
        .query_selector(".lm-tile")
        .unwrap()
        .expect("the emitter wrote the tile");
    assert_eq!(
        background_of(&tile),
        "rgb(0, 255, 0)",
        "the stylesheet's red should have lost to the green written on the element"
    );
    assert!(
        !css.contains("!important"),
        "the ranking comes from the layers, so important stays the author's to spend"
    );

    style.remove();
    host.remove();
}

/// A stylesheet that paints every tile red, which the element overrides.
fn authored_sheet() -> lumen_ir::css::Stylesheet {
    lumen_ir::css::Stylesheet {
        rules: vec![lumen_ir::css::Rule {
            selectors: lumen_ir::css::parse_selector_list("tile").expect("selectors parse"),
            declarations: vec![lumen_ir::css::Declaration {
                name: "bg".to_string(),
                value: "#ff0000".to_string(),
                important: false,
            }],
            origin: lumen_ir::css::Origin::Author,
            source_order: 0,
            media: None,
            selector: Default::default(),
        }],
    }
}

/// What the browser resolved an element's background to.
fn background_of(element: &Element) -> String {
    web_sys::window()
        .unwrap()
        .get_computed_style(element)
        .unwrap()
        .expect("the element is in the document")
        .get_property_value("background-color")
        .unwrap()
}
