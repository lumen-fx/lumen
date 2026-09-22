//! Starting the app the page it is loaded into was emitted for.
//!
//! [`boot`] is the whole public surface a document needs, and every emitted
//! page calls it the same way. It knows nothing about any app: what to load,
//! where from, which engine runs the scripts and what state the markup shows
//! all come out of the document and the manifest beside it.

use std::sync::Once;

use lumen_core::request::RequestContext;
use lumen_html::contract::{DATA_LM, Manifest};
use lumen_scene::routing::Location;
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web_dom::{Navigation, Routes, WebDomPlugin};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use crate::assemble::{self, apply_node_seed, apply_seed, portable_app};
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
    // Translations go in before anything reads a key: a script's `on_start`
    // may call `t()`, and an element's text is resolved as it spawns. A
    // catalogue that will not load is reported and the app starts anyway,
    // reading in the language its source strings are written in. The
    // manifest names no fallback chain, so a page falls through to the one a
    // desktop app gets by default.
    if let Err(error) =
        assemble::install_i18n(&mut app.world, &page.locale, &loaded.catalogues, &[])
    {
        web_sys::console::error_1(&JsValue::from_str(&format!("lumen: {error}")));
    }
    // The host goes in before the scene: `on_start` publishes the signals the
    // markup binds to, and the spawner seeds only what nothing has written.
    for script in &loaded.scripts {
        hosts::install(&mut app, &script.engine, &script.bytes, &script.uri)
            .map_err(|e| BootError::Engine(e.to_string()))?;
    }
    // A document is served at its own address until a deep path is served
    // through the shell, so which page the app opens on is a question about
    // the address, not about the document. The manifest holds the map from
    // one to the other.
    //
    // Installed whichever way `[web] navigation` reads: `route.path` has to
    // seed correctly for the page this address names. What the setting
    // decides is what a navigation does once the app is running. Under soft
    // navigation a same-page `<a href>` click and a script's `page()` call
    // both swap the page in place and the address bar follows; under hard
    // navigation both load the target document. `listen` below decides the
    // click, because only there is a browser event still in hand to prevent,
    // and `WebDomPlugin` the rest.
    let address = lumen_core::request::current()
        .map(|request| request.path)
        .unwrap_or_default();
    let (path, segment) = manifest.page_at(&address);
    lumen_scene::routing::install_routing(
        &mut app,
        manifest.entry.clone(),
        manifest.page_keys(),
        Location { path, segment },
    );
    apply_seed(&mut app.world, &loaded.seed);
    let root_entity = loaded.artifact.spawn_into(&mut app.world);
    // What the page says the app wrote onto its nodes. Applied after the
    // spawn, because it names nodes and there are none before it.
    apply_node_seed(&mut app.world, root_entity, &loaded.seed);

    let root = page_root(&page)?;
    // A site emitted in more than one language holds a tree per language and
    // one manifest, at the root, for all of them. The document says which of
    // those trees it belongs to, and that is what its addresses, and the
    // canonical URL its head names, hang off.
    let routes = Routes::from_manifest(&manifest, &page.locale);
    let navigation = navigation(&manifest, loaded.artifact.pages.is_some());
    let soft = navigation == Navigation::Soft;
    lumen_web_dom::listen(&root, soft.then_some(&routes)).map_err(|_| BootError::Listeners)?;
    app.add_plugin(WebDomPlugin {
        root,
        root_entity,
        routes,
        navigation,
    });

    let app = LumenWebApp::from_parts(app, loaded.scripts.first().map(|s| s.engine.clone()));
    if let Some(error) = app.script_error() {
        web_sys::console::error_1(&JsValue::from_str(&format!("lumen: {error}")));
    }
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

/// How this document navigates: `[web] navigation`, unless the document has
/// nothing to swap in.
///
/// `has_pages` is whether the artifact carries the other pages' trees, which
/// is what a swap mounts from. Without them the app is a single-file one, and
/// a navigation swaps the one page in place with the rest of the path on
/// `route.segment`, whatever the setting says. Intercepting a click there
/// would leave a link that does nothing at all: the browser is stopped and
/// the page it named never arrives.
fn navigation(manifest: &Manifest, has_pages: bool) -> Navigation {
    if has_pages {
        manifest.navigation.into()
    } else {
        Navigation::InPlace
    }
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

#[cfg(test)]
mod tests {
    use super::navigation;
    use lumen_html::contract::{Manifest, NavigationMode};
    use lumen_web_dom::Navigation;

    fn manifest(mode: NavigationMode) -> Manifest {
        Manifest {
            navigation: mode,
            ..Manifest::default()
        }
    }

    /// `navigation = "soft"` (the default) means the runtime intercepts a
    /// same-page link click and swaps the page in; before this was wired
    /// up, `start` read the manifest and then discarded it
    /// (`let _ = manifest;`), so soft and hard navigation were
    /// indistinguishable no matter what the site declared.
    #[test]
    fn soft_navigation_swaps_pages_in() {
        assert_eq!(
            navigation(&manifest(NavigationMode::Soft), true),
            Navigation::Soft
        );
    }

    #[test]
    fn hard_navigation_loads_documents() {
        assert_eq!(
            navigation(&manifest(NavigationMode::Hard), true),
            Navigation::Hard
        );
    }

    #[test]
    fn a_single_file_app_swaps_in_place_whatever_the_key_says() {
        for mode in [NavigationMode::Soft, NavigationMode::Hard] {
            assert_eq!(
                navigation(&manifest(mode), false),
                Navigation::InPlace,
                "with no page set there is no other document to load and \
                 nothing to intercept a click for, so `{mode}` swaps the one \
                 page in place"
            );
        }
    }
}
