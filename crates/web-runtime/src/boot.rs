//! Starting the app the page it is loaded into was emitted for.
//!
//! [`boot`] is the whole public surface a document needs, and every emitted
//! page calls it the same way. It knows nothing about any app: what to load,
//! where from, which engine runs the scripts and what state the markup shows
//! all come out of the document and the manifest beside it.

use std::sync::Once;

use lumen_core::request::RequestContext;
use lumen_html::contract::DATA_LM;
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web_dom::WebDomPlugin;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use crate::assemble::{apply_seed, portable_app};
use crate::load::{LoadError, PageContext};
use crate::{LumenWebApp, hosts};

/// Start the Lumen app this document was emitted for.
///
/// Every page emitted by `lumenc web` ends with the same two lines, so this
/// takes no argument it cannot find for itself: the document says which page
/// it is and where the site root is, and the manifest at that root says the
/// rest. A page that keeps its manifest somewhere else may pass the URL.
///
/// The argument is a [`JsValue`] rather than a string because the shortest
/// way to write the call is `init().then(boot)`, and a promise hands its
/// result to whatever it resolves into. Anything that is not a string is
/// what the module initialiser returned, and means "find it yourself".
///
/// # Errors
///
/// The document was not emitted by `lumenc web`, it was emitted against a
/// contract this runtime does not implement, a file it names could not be
/// fetched, or the app declares an engine no host in this build answers for.
/// Every one of them is reported to the console as well, because a page that
/// silently does nothing is the failure this exists to make visible.
#[wasm_bindgen]
pub async fn boot(manifest: JsValue) -> Result<(), JsError> {
    report_panics();
    match start(manifest.as_string()).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = error.to_string();
            web_sys::console::error_1(&JsValue::from_str(&format!("lumen: {message}")));
            Err(JsError::new(&message))
        }
    }
}

/// Send Rust panics to the page's console.
///
/// A panic compiled to wasm reaches the browser as `unreachable`, with the
/// message nowhere: what went wrong is only knowable if the hook says so
/// before the trap. Installed once, whatever calls [`boot`].
fn report_panics() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            web_sys::console::error_1(&JsValue::from_str(&format!("lumen: {info}")));
            previous(info);
        }));
    });
}

/// Everything [`boot`] does, in terms that report their own failures.
async fn start(manifest_url: Option<String>) -> Result<(), BootError> {
    let page = PageContext::from_document()?;
    install_location();
    let url = manifest_url.unwrap_or_else(|| page.manifest_url());
    let (manifest, loaded) = page.load(&url).await?;

    let mut app = portable_app();
    // The host goes in before the scene: `on_start` publishes the signals the
    // markup binds to, and the spawner seeds only what nothing has written.
    for script in &loaded.scripts {
        hosts::install(&mut app, &script.engine, &script.bytes, &script.uri)
            .map_err(|e| BootError::Engine(e.to_string()))?;
    }
    if let Some(pages) = &loaded.artifact.pages {
        // The document is one page of the site, and which one is what it
        // says it is: a visitor who asked for `/settings` was served that
        // document, so the app opens on it rather than on the entry.
        lumen_scene::routing::install_routing(&mut app, page.page.clone(), pages.keys.clone());
    }
    let root_entity = loaded.artifact.ir.spawn_into(&mut app.world);
    apply_seed(&mut app.world, &loaded.seed);

    let root = page_root(&page)?;
    lumen_web_dom::listen(&root).map_err(|_| BootError::Listeners)?;
    app.add_plugin(WebDomPlugin { root, root_entity });

    let app = LumenWebApp::from_parts(app, loaded.scripts.first().map(|s| s.engine.clone()));
    if let Some(error) = app.script_error() {
        web_sys::console::error_1(&JsValue::from_str(&format!("lumen: {error}")));
    }
    let _ = manifest;
    app.start_frame_loop().map_err(|_| BootError::NoWindow)
}

/// Tell the app the address the page was opened at.
///
/// `window::location_query()` and `window::location_hash()` read the request
/// a document was produced for. On a server that is the request being
/// answered; here it is the address in the bar, which is the same question a
/// script is asking.
fn install_location() {
    let Some(location) = web_sys::window().map(|window| window.location()) else {
        return;
    };
    let strip = |value: Result<String, JsValue>, lead: char| {
        value
            .unwrap_or_default()
            .strip_prefix(lead)
            .unwrap_or_default()
            .to_string()
    };
    lumen_core::request::install(RequestContext {
        method: "GET".to_string(),
        path: location.pathname().unwrap_or_default(),
        query: strip(location.search(), '?'),
        hash: strip(location.hash(), '#'),
        secure: location.protocol().unwrap_or_default() == "https:",
        ..RequestContext::default()
    });
}

/// The element the app's root node is.
///
/// A prerendered page already has it, carrying the root node path. A page
/// emitted without one gets a container to build into, which is the body:
/// there is nothing else in such a document.
fn page_root(page: &PageContext) -> Result<Element, BootError> {
    if let Ok(Some(root)) = page.document.query_selector(&format!("[{DATA_LM}=\"0\"]")) {
        return Ok(root);
    }
    page.document
        .body()
        .map(JsCast::unchecked_into)
        .ok_or(BootError::NoDocument)
}

/// Why an app did not start.
#[derive(Debug)]
enum BootError {
    /// The page could not be read, or a file it names could not be fetched.
    Load(LoadError),
    /// There is no document to boot into.
    NoDocument,
    /// The app declares an engine no host in this build answers for.
    Engine(String),
    /// The browser refused the event listeners.
    Listeners,
    /// There is no window to request frames from.
    NoWindow,
}

impl From<LoadError> for BootError {
    fn from(error: LoadError) -> Self {
        BootError::Load(error)
    }
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::Load(error) => error.fmt(f),
            BootError::NoDocument => f.write_str("no document to boot into"),
            BootError::Engine(message) => f.write_str(message),
            BootError::Listeners => f.write_str("the page refused an event listener"),
            BootError::NoWindow => f.write_str("no window to run frames on"),
        }
    }
}

impl std::error::Error for BootError {}
