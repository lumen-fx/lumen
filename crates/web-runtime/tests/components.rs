//! Fragments in a real page: what the emitter wrote, what the runtime built,
//! and whether the two agree.
//!
//! A fragment reaches a page two ways. A component the build could not stand
//! in for is a marker in the document that the first tick replaces with what
//! the call returns; a block the app mounts is a subtree no document ever
//! carried. Both go through the browser backend, and only a browser says
//! whether the element that came out is the one the page needed.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use lumen_core::prelude::App;
use lumen_html::contract::DATA_LM;
use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_ir::fragment::{Fragment, FragmentKind, FragmentParam, FragmentTable};
use lumen_ir::layout_ir::{Attributes, Element, FragmentUse, InterpolationSlot, LayoutIR};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::{NodeTable, WebDomPlugin};
use lumen_web_runtime::{assemble, hosts};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element as DomElement;

wasm_bindgen_test_configure!(run_in_browser);

/// The program the build script compiled: a component that has to run, and an
/// `on_ready` that mounts a fragment by key.
const COMPONENTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/components.cdlb"));

/// A body of one label whose text is the fragment's parameter.
fn body(class: &str, param: &str) -> Vec<Element> {
    vec![Element {
        tag: "label".to_string(),
        attrs: Attributes {
            classes: vec![class.to_string()],
            text: Some(format!("{{{param}}}")),
            ..Attributes::default()
        },
        interpolations: vec![InterpolationSlot::Arg(param.to_string())],
        ..Element::default()
    }]
}

/// The fragments the artifact carries: what `Shout` builds, and what the app
/// mounts.
fn fragments() -> FragmentTable {
    let mut table = FragmentTable::new();
    for (key, class, param) in [("shout", "shout", "who"), ("card", "card", "title")] {
        table
            .insert(Fragment {
                key: key.to_string(),
                params: vec![FragmentParam {
                    name: param.to_string(),
                    default: None,
                }],
                body: body(class, param),
                origins: Vec::new(),
                kind: FragmentKind::Markup,
                components: Vec::new(),
            })
            .expect("distinct keys");
    }
    table
}

/// The tree the build emits: a label the build baked, and the marker it left
/// for the component that has to run.
fn tree() -> LayoutIR {
    let mut marker = Element {
        tag: "Shout".to_string(),
        ..Element::default()
    };
    marker.frag_use = Some(Box::new(FragmentUse {
        key: "Shout".to_string(),
        args: vec![("who".to_string(), "ann".to_string())],
        slot_children: false,
    }));
    let baked = Element {
        tag: "label".to_string(),
        attrs: Attributes {
            classes: vec!["baked".to_string()],
            text: Some("already here".to_string()),
            ..Attributes::default()
        },
        ..Element::default()
    };
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![baked, marker],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

fn compiled() -> CompiledApp {
    CompiledApp {
        ir: tree(),
        fragments: fragments(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(COMPONENTS.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

/// Write the page the emitter writes for this app, and put it in the document.
fn page() -> DomElement {
    let spec = SiteSpec {
        pages: vec![PageSpec::new("index", tree())],
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

/// Boot the app into `root`, the way the page's own boot does, and tick it
/// once.
fn boot(root: DomElement) -> App {
    let mut app = assemble::portable_app();
    hosts::install(&mut app, "candela", COMPONENTS, "components.cdlb")
        .expect("this build carries the candela host");
    let root_entity = compiled().spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin { root, root_entity });
    app.tick();
    app
}

#[wasm_bindgen_test]
fn a_component_the_build_left_a_marker_for_is_filled_in_the_page() {
    let root = page();
    assert!(
        root.query_selector(".lm-fragment").unwrap().is_some(),
        "the emitter wrote the marker: {}",
        root.outer_html()
    );

    let _app = boot(root.clone());

    let html = root.outer_html();
    assert!(html.contains("ann!"), "the call filled the marker: {html}");
    assert!(
        root.query_selector(".lm-fragment").unwrap().is_none(),
        "and the marker is gone: {html}"
    );
}

/// What the build already baked is adopted, not rebuilt: the element the
/// emitter wrote is the element the app ends up bound to, unchanged.
#[wasm_bindgen_test]
fn filling_a_marker_leaves_the_rest_of_the_page_alone() {
    let root = page();
    let baked = root
        .query_selector(&format!("[{DATA_LM}=\"0.0\"]"))
        .unwrap()
        .expect("the emitter wrote the baked label");

    let app = boot(root.clone());

    assert_eq!(
        baked.outer_html(),
        r#"<span class="lm-label baked" data-lm="0.0">already here</span>"#,
        "the baked element was adopted rather than replaced"
    );
    let report = app.world.non_send::<NodeTable>().report();
    assert!(
        report.adopted >= 2,
        "the page's own nodes were adopted: {report:?}"
    );
}

/// A block the app mounts is a subtree no document carried, so the backend
/// builds it and puts it where the app put it.
#[wasm_bindgen_test]
fn a_mounted_fragment_is_built_into_the_page() {
    let root = page();
    let _app = boot(root.clone());

    let html = root.outer_html();
    assert!(html.contains("mounted"), "{html}");
    let card = root
        .query_selector(".card")
        .unwrap()
        .expect("the mounted body is in the page");
    assert_eq!(card.tag_name(), "SPAN", "a label is a span: {html}");
}
