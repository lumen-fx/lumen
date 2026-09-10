//! Translation in the browser: the catalogue the site carries, installed at
//! boot, and what an element says once the runtime owns it.
//!
//! A page arrives with its text already translated, and then the runtime
//! spawns the same tree over it: every element's text is resolved again, from
//! the world this time, and written into the node. Without a catalogue that
//! resolution answers in the language the app was authored in, so the first
//! frame writes English over a German document. Only a browser says which of
//! the two a visitor is left looking at.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```

#![cfg(target_arch = "wasm32")]

use lumen_core::prelude::App;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web::{PageSpec, SiteSpec, WebSpec};
use lumen_web_dom::WebDomPlugin;
use lumen_web_runtime::assemble;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element as DomElement;

wasm_bindgen_test_configure!(run_in_browser);

/// The catalogue the German tree of a site carries.
const GERMAN: &str = "greeting = Hallo\n";

/// A tree of one `translatable` label, showing the text the build resolved
/// for it.
fn tree(text: &str) -> LayoutIR {
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![Element {
                tag: "label".to_string(),
                attrs: Attributes {
                    classes: vec!["greeting".to_string()],
                    translatable: Some("greeting".to_string()),
                    text: Some(text.to_string()),
                    ..Attributes::default()
                },
                ..Element::default()
            }],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

/// Write the page the emitter writes for `ir`, and put it in the document.
fn page_of(ir: LayoutIR) -> DomElement {
    let spec = SiteSpec {
        pages: vec![PageSpec::new("index", ir)],
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
    let host = document.create_element("div").unwrap();
    host.set_inner_html(&html);
    document.body().unwrap().append_child(&host).unwrap();
    host.first_element_child().expect("the page root")
}

/// Boot `ir` into `root` the way the page's own boot does, with the
/// catalogues the manifest named, and tick it once.
fn boot(ir: LayoutIR, root: DomElement, catalogues: &[(String, String)]) -> App {
    let mut app = assemble::portable_app();
    assemble::install_i18n(&mut app.world, "de-DE", catalogues).expect("a valid catalogue");
    let compiled = CompiledApp {
        ir,
        ..CompiledApp::default()
    };
    let root_entity = compiled.spawn_into(&mut app.world);
    app.add_plugin(WebDomPlugin {
        root,
        root_entity,
        routes: None,
    });
    app.tick();
    app
}

/// The catalogue for the tree this document belongs to.
fn german() -> Vec<(String, String)> {
    vec![("de-DE".to_string(), GERMAN.to_string())]
}

/// The page a visitor is served: the build resolved the key, and the runtime
/// resolves it to the same string, so the text the document arrived with is
/// the text it keeps.
#[wasm_bindgen_test]
fn a_translated_document_keeps_its_language_once_the_runtime_owns_it() {
    let root = page_of(tree("Hallo"));
    let label = root
        .query_selector(".greeting")
        .unwrap()
        .expect("the build wrote the label");

    let _app = boot(tree("Hallo"), root, &german());

    assert_eq!(label.text_content().as_deref(), Some("Hallo"));
}

/// An element the document never carried, which is every row of a list a
/// script fills. It is built in the browser, so its key is resolved there.
#[wasm_bindgen_test]
fn an_element_the_runtime_builds_reads_in_the_document_s_language() {
    let root = page_of(LayoutIR {
        root: Element {
            tag: "root".to_string(),
            ..Element::default()
        },
        ..LayoutIR::default()
    });

    let _app = boot(tree("Hello"), root.clone(), &german());

    let label = root
        .query_selector(".greeting")
        .unwrap()
        .expect("the runtime built the label");
    assert_eq!(label.text_content().as_deref(), Some("Hallo"));
}

/// A locale the site carries no catalogue for. Every key falls back to the
/// text the app was authored with, which is what leaves such a page readable.
#[wasm_bindgen_test]
fn a_locale_with_no_catalogue_reads_in_the_source_language() {
    let root = page_of(tree("Hello"));

    let _app = boot(tree("Hello"), root.clone(), &[]);

    let label = root
        .query_selector(".greeting")
        .unwrap()
        .expect("the label");
    assert_eq!(label.text_content().as_deref(), Some("Hello"));
}
