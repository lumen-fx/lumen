//! The head of the page the app is showing.
//!
//! A navigation in a browser swaps the page in place and leaves everything
//! above the body alone: the `<title>`, the description, the canonical link
//! and the Open Graph tags a share card is built from all stay the ones the
//! document that was loaded was emitted with. A visitor who lands on Home
//! and clicks through to Settings would keep a tab, a bookmark and a preview
//! that all still say Home.
//!
//! So the head follows the swap. The emitter wrote what each page's own
//! document says about it into the manifest, and this writes that over the
//! head of the loaded document whenever the resolver settles on another
//! page. What is the same on every page of a site (`og:type`, the preview
//! image, the card kind, the stylesheet, the `hreflang` links) is left where
//! the emitter put it.

use bevy_ecs::prelude::*;
use lumen_core::nav;
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::PageInfo;
use web_sys::{Document, Element};

use crate::navigation::Routes;

/// Put the head of the page the resolver settled on into the document.
///
/// Runs after [`lumen_scene::routing::apply_navigation`], which is what puts
/// the settled page on `route.path`, and writes only when that changes.
///
/// The first tick writes the head of the page the app opened on, which is
/// what a deep path needs: a static host answers `/user/42` with the shell,
/// and the shell carries the entry page's head while the app shows `user`.
/// For a page reached by a real document load that first write puts back
/// what the emitter already wrote.
pub(crate) fn sync_head(
    store: Res<PropertyStore>,
    routes: Res<Routes>,
    mut shown: Local<Option<String>>,
) {
    let path = store
        .get_global_str(nav::PATH_SIGNAL)
        .unwrap_or_else(|| "".into());
    if shown.as_deref() == Some(&*path) {
        return;
    }
    *shown = Some(path.to_string());
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(page) = routes.head(&path) else {
        return;
    };
    write_head(&document, page, routes.canonical(&path).as_deref());
}

/// Write one page's head into `document`.
fn write_head(document: &Document, page: &PageInfo, canonical: Option<&str>) {
    document.set_title(&page.title);
    let description = page.description.as_deref();
    meta(document, "name", "description", description);
    meta(
        document,
        "name",
        "robots",
        (!page.index).then_some("noindex"),
    );
    link(document, "canonical", canonical);
    meta(document, "property", "og:title", Some(page.title.as_str()));
    meta(document, "property", "og:description", description);
    meta(document, "property", "og:url", canonical);
    meta(document, "name", "twitter:title", Some(page.title.as_str()));
    meta(document, "name", "twitter:description", description);
}

/// Put `content` on the `<meta>` the document identifies by `key`, which is
/// `name` for a plain tag and `property` for an Open Graph one.
fn meta(document: &Document, key: &str, id: &str, content: Option<&str>) {
    put(
        document,
        &format!("meta[{key}=\"{id}\"]"),
        "content",
        content,
        || build(document, "meta", key, id),
    );
}

/// Put `href` on the `<link>` of relation `rel`.
fn link(document: &Document, rel: &str, href: Option<&str>) {
    put(
        document,
        &format!("link[rel=\"{rel}\"]"),
        "href",
        href,
        || build(document, "link", "rel", rel),
    );
}

/// The element one of the selectors above would have found, for a document
/// emitted without it. A page that says nothing about itself is emitted with
/// no description tags at all, so a swap onto one that does have them has to
/// add them rather than fill them in.
fn build(document: &Document, tag: &str, key: &str, id: &str) -> Option<Element> {
    let element = document.create_element(tag).ok()?;
    element.set_attribute(key, id).ok()?;
    Some(element)
}

/// Write `value` into `attribute` on the one head element `selector` names,
/// adding the element when the document has none and taking it out when the
/// page swapped in has nothing to put there.
///
/// `make` builds the element the selector would have found, which is all
/// that differs between a `<meta>` and a `<link>`.
fn put(
    document: &Document,
    selector: &str,
    attribute: &str,
    value: Option<&str>,
    make: impl FnOnce() -> Option<Element>,
) {
    let found = document.query_selector(selector).ok().flatten();
    match (found, value) {
        (Some(element), Some(value)) => {
            let _ = element.set_attribute(attribute, value);
        }
        (Some(element), None) => element.remove(),
        (None, Some(value)) => {
            let Some(element) = make() else {
                return;
            };
            if element.set_attribute(attribute, value).is_err() {
                return;
            }
            if let Some(head) = document.head() {
                let _ = head.append_child(&element);
            }
        }
        (None, None) => {}
    }
}
