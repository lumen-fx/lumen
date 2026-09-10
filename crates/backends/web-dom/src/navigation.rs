//! The address bar as the app's history.
//!
//! Navigating is one thing everywhere: a write to the reserved request
//! signal, resolved by [`lumen_scene::routing::apply_navigation`]. What a
//! browser adds is that the app also lives at an address, and a visitor
//! expects that address to be the page they are looking at - so a reload
//! brings the same page back, a copied link opens it, and the browser's own
//! back button returns to where they were.
//!
//! So this translates between the one navigation mechanism and the address
//! bar, in both directions. A navigation the app resolves pushes a history
//! entry; a history entry the visitor steps to raises a navigation. Nothing
//! in [`lumen_scene::routing`] changes, and nothing there learns that a
//! browser is what is running it.
//!
//! Back and forward are handed to the browser rather than answered from the
//! in-memory stack. A site has one history, and it is the browser's: letting
//! both step would give a visitor two that disagree, one behind the button
//! in the page and one behind the button in the chrome.

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use lumen_core::nav::{self, NavOp};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::Manifest;
use lumen_html::urls::{join, normalize_base};
use wasm_bindgen::JsValue;

/// What the addresses of this site look like, as the manifest describes
/// them.
///
/// The document names come out of the manifest rather than being worked out
/// again here, so the emitter stays the only thing that decides what a page
/// is written as.
#[derive(Clone, Debug, Resource)]
pub struct Routes {
    /// URL prefix every address hangs off, with a slash at each end.
    base: String,
    /// Page key the site opens on.
    entry: String,
    /// Page key to the document that page was emitted as.
    documents: BTreeMap<String, String>,
    /// The page keys, which is what a path resolves against.
    keys: Vec<String>,
}

impl Routes {
    /// Read the site's addresses out of its manifest.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            base: normalize_base(&manifest.base_path),
            entry: manifest.entry.clone(),
            documents: manifest.pages.clone(),
            keys: manifest.pages.keys().cloned().collect(),
        }
    }

    /// The address a navigation to `path` leaves in the bar.
    ///
    /// This is the rule the emitter applied to the same link at build time
    /// (`lumen_web::urls::page_href`), which is what makes the address after
    /// a swap the one the anchor already named: a path a page answers for
    /// whole becomes that page's document (`/settings.html`), and a deeper
    /// path stays as the visitor asked for it (`/user/42`), because that is
    /// the URL the page's own `route.segment` is read from.
    pub fn address_of(&self, path: &str) -> String {
        let (key, segment) = nav::resolve_path(path, &self.keys, &self.entry);
        if segment.is_empty() {
            let document = self
                .documents
                .get(&key)
                .map_or(key.as_str(), String::as_str);
            return join(&self.base, document);
        }
        join(&self.base, path)
    }

    /// The path a navigation to `url_path` asks for: the reverse of
    /// [`Self::address_of`], for an address the visitor arrived at rather
    /// than one the app produced.
    ///
    /// A document of this site answers with the page it was emitted for.
    /// Anything else answers with the address as written, which is what a
    /// deep path like `/user/42` needs: no document was written for it, and
    /// the resolver is what turns it into a page and a segment.
    pub fn path_at(&self, url_path: &str) -> String {
        let rest = url_path
            .strip_prefix(&self.base)
            .unwrap_or_else(|| url_path.trim_start_matches('/'));
        for (key, document) in &self.documents {
            if document == rest {
                return key.clone();
            }
        }
        rest.to_string()
    }

    /// Whether the address `url_path` already names the page `path` asks
    /// for.
    ///
    /// Answered through [`Self::path_at`] rather than by comparing the two
    /// addresses as text, because one page has more than one address: a
    /// server serves the entry page at the site root as well as at the
    /// document it was written as, so a visitor at `/` is on the page
    /// `/index.html` names.
    pub fn is_at(&self, url_path: &str, path: &str) -> bool {
        self.address_of(&self.path_at(url_path)) == self.address_of(path)
    }
}

/// What a navigation the app resolved asks of the browser's history.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HistoryStep {
    /// Put this address in the bar, as an entry of its own.
    Push(String),
    /// Step the browser's own history one entry back.
    Back,
    /// Step it one entry forward.
    Forward,
}

/// What `request` asks of the address bar, for a document currently at
/// `here`.
///
/// `last` is the sequence number this already acted on. The request cell
/// holds its value until the next request replaces it, so without that a
/// single page swap would push an entry every tick.
fn history_step(routes: &Routes, here: &str, request: &str, last: &mut u64) -> Option<HistoryStep> {
    let (seq, op) = nav::parse_request(request)?;
    if seq <= *last {
        return None;
    }
    *last = seq;
    match op {
        // The bar already naming this page is a navigation the browser
        // raised: a popstate for an entry the visitor stepped to. It needs
        // no entry of its own, and needs no flag to be told apart. Pushing
        // one anyway would truncate the forward history and leave the
        // forward button dead.
        NavOp::Navigate(path) => {
            (!routes.is_at(here, &path)).then(|| HistoryStep::Push(routes.address_of(&path)))
        }
        NavOp::Back => Some(HistoryStep::Back),
        NavOp::Forward => Some(HistoryStep::Forward),
    }
}

/// Put what the app navigated to in the address bar, and hand back and
/// forward to the browser.
///
/// Runs before the resolver rather than off the click listener: a click only
/// becomes a request once
/// [`lumen_scene::routing::navigate_on_anchor_click`] has honoured a
/// handler's `prevent_default()`, so a cancelled navigation leaves no
/// history entry, and a script's own `page("settings")` reaches the address
/// bar through the same path a click does.
pub(crate) fn sync_history(
    mut store: ResMut<PropertyStore>,
    routes: Res<Routes>,
    mut last: Local<u64>,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let here = window.location().pathname().unwrap_or_default();
    let request = store
        .get_global_str(nav::REQUEST_SIGNAL)
        .unwrap_or_else(|| "".into());
    let Some(step) = history_step(&routes, &here, &request, &mut last) else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    match step {
        HistoryStep::Push(address) => {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(&address));
            // A document the browser loads starts at the top, and a page
            // swapped in place is the same arrival. Stepping back is left
            // alone: the browser restores the scroll of an entry it pushed.
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
        // The browser answers a step with a `popstate`, and that is what
        // navigates. Clearing the request keeps the resolver from stepping
        // the in-memory stack as well, which would leave the site's two
        // histories one entry apart.
        step => {
            let _ = if step == HistoryStep::Back {
                history.back()
            } else {
                history.forward()
            };
            store.set_global_str(nav::REQUEST_SIGNAL, "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_ir::layout_ir::LayoutIR;
    use lumen_web::markup::MarkupSheet;
    use lumen_web::spec::{LocaleSpec, PageSpec, SiteSpec, WebSpec};
    use lumen_web::urls::page_href;

    /// A site of three pages, one of which answers for deeper paths too.
    fn spec(base: &str) -> SiteSpec {
        SiteSpec {
            pages: vec![
                PageSpec::new("index", LayoutIR::default()),
                PageSpec::new("settings", LayoutIR::default()),
                PageSpec::new("user", LayoutIR::default()),
            ],
            web: WebSpec {
                base_path: base.into(),
                entry: "index".into(),
                ..WebSpec::default()
            },
            locale: LocaleSpec::new("en-US"),
            assets: Vec::new(),
            markup: MarkupSheet::default(),
        }
    }

    /// The addresses of that site, read out of the manifest the real emitter
    /// writes for it. Naming the documents here instead would be a copy of
    /// what a build produces, and a copy agrees with itself while the two
    /// halves drift apart.
    fn routes(base: &str) -> Routes {
        Routes::from_manifest(&lumen_web::site::manifest(&spec(base)))
    }

    #[test]
    fn a_page_is_addressed_as_the_document_it_was_emitted_as() {
        let routes = routes("/");
        assert_eq!(routes.address_of("settings"), "/settings.html");
        assert_eq!(routes.address_of("/settings"), "/settings.html");
        // The entry page is the document it was written as whatever it is
        // keyed as, and the empty path names it.
        assert_eq!(routes.address_of(""), "/index.html");
        assert_eq!(routes.address_of("/"), "/index.html");
    }

    #[test]
    fn a_deeper_path_is_addressed_as_the_visitor_asked_for_it() {
        let routes = routes("/");
        assert_eq!(routes.address_of("user/42"), "/user/42");
        assert_eq!(routes.address_of("/user/42"), "/user/42");
        // A path no page answers for is left as written too: the resolver
        // lands it on the entry page with the whole path as its segment,
        // and that page is what says there is no such thing.
        assert_eq!(routes.address_of("/nowhere"), "/nowhere");
    }

    #[test]
    fn every_address_hangs_off_the_base_path() {
        let routes = routes("/docs");
        assert_eq!(routes.address_of("settings"), "/docs/settings.html");
        assert_eq!(routes.address_of("user/42"), "/docs/user/42");
        assert_eq!(routes.path_at("/docs/settings.html"), "settings");
        assert_eq!(routes.path_at("/docs/user/42"), "user/42");
    }

    #[test]
    fn a_document_of_this_site_is_the_page_it_was_emitted_for() {
        let routes = routes("/");
        assert_eq!(routes.path_at("/settings.html"), "settings");
        assert_eq!(routes.path_at("/index.html"), "index");
        // The site root carries no document name, and resolves to the
        // entry page the same way an empty path does.
        assert_eq!(routes.path_at("/"), "");
    }

    #[test]
    fn an_address_no_document_was_written_for_stays_as_written() {
        let routes = routes("/");
        assert_eq!(routes.path_at("/user/42"), "user/42");
        assert_eq!(routes.path_at("/nowhere"), "nowhere");
    }

    #[test]
    fn the_site_root_is_the_entry_page_s_own_address() {
        let routes = routes("/");
        // A server serves the entry page at the root as well as at the
        // document it was written as, so a visitor who stepped back to the
        // root is already on the page a navigation to `index` asks for.
        // Pushing an entry for it would bury the one they stepped off.
        assert!(routes.is_at("/", "index"));
        assert!(routes.is_at("/index.html", ""));
        assert!(routes.is_at("/user/42", "user/42"));
        assert!(!routes.is_at("/", "settings"));
        assert!(!routes.is_at("/user/42", "user/7"));
    }

    #[test]
    fn an_address_survives_a_round_trip_through_the_bar() {
        let routes = routes("/docs");
        for path in ["settings", "user/42", "index"] {
            let address = routes.address_of(path);
            assert_eq!(
                routes.address_of(&routes.path_at(&address)),
                address,
                "{path} addresses the same place after being read back"
            );
        }
    }

    #[test]
    fn a_page_the_app_navigated_to_becomes_an_entry_of_its_own() {
        let routes = routes("/");
        let mut last = 0;
        let request = nav::encode_request(&NavOp::Navigate("settings".into()));
        assert_eq!(
            history_step(&routes, "/index.html", &request, &mut last),
            Some(HistoryStep::Push("/settings.html".to_string()))
        );
        // The cell holds its value until the next navigation replaces it,
        // so a swap that took one tick would otherwise push an entry on
        // every tick after it.
        assert_eq!(
            history_step(&routes, "/settings.html", &request, &mut last),
            None
        );
    }

    #[test]
    fn a_deeper_path_is_pushed_as_the_visitor_asked_for_it() {
        let routes = routes("/docs");
        let mut last = 0;
        let request = nav::encode_request(&NavOp::Navigate("/user/42".into()));
        assert_eq!(
            history_step(&routes, "/docs/index.html", &request, &mut last),
            Some(HistoryStep::Push("/docs/user/42".to_string()))
        );
    }

    #[test]
    fn the_page_the_address_already_names_is_not_pushed_again() {
        // This is the navigation a `popstate` raises: the browser has
        // already moved the address, and pushing there would truncate the
        // forward history, leaving the button the visitor just used dead.
        let routes = routes("/");
        let mut last = 0;
        let request = nav::encode_request(&NavOp::Navigate("settings".into()));
        assert_eq!(
            history_step(&routes, "/settings.html", &request, &mut last),
            None
        );
        // The entry page is served at the site root as well, so a visitor
        // who stepped back to `/` is already where a navigation to `index`
        // asks for.
        let request = nav::encode_request(&NavOp::Navigate("index".into()));
        assert_eq!(history_step(&routes, "/", &request, &mut last), None);
    }

    #[test]
    fn back_and_forward_are_the_browser_s_to_answer() {
        let routes = routes("/");
        let mut last = 0;
        assert_eq!(
            history_step(
                &routes,
                "/settings.html",
                &nav::encode_request(&NavOp::Back),
                &mut last
            ),
            Some(HistoryStep::Back)
        );
        assert_eq!(
            history_step(
                &routes,
                "/settings.html",
                &nav::encode_request(&NavOp::Forward),
                &mut last
            ),
            Some(HistoryStep::Forward)
        );
    }

    #[test]
    fn a_request_that_names_no_navigation_asks_for_nothing() {
        // The cell is empty before the first navigation, and `sync_history`
        // empties it again after handing a step to the browser.
        let routes = routes("/");
        let mut last = 0;
        assert_eq!(history_step(&routes, "/", "", &mut last), None);
        assert_eq!(
            history_step(&routes, "/", "7\u{1f}dance\u{1f}", &mut last),
            None
        );
        assert_eq!(last, 0, "nothing was acted on, so nothing is remembered");
    }

    /// Everything an author writes into an `href` on a site of that shape: a
    /// page, a page under a leading slash, the entry page, the site root, a
    /// path deeper than a page, and one no page answers for.
    const HREFS: [&str; 9] = [
        "settings",
        "/settings",
        "user",
        "user/42",
        "/user/42",
        "index",
        "",
        "/",
        "nowhere",
    ];

    #[test]
    fn a_swapped_page_ends_at_the_address_the_link_named() {
        // The two halves that write a page's address: the emitter puts it in
        // an `<a href>` at build time, and this puts it in the bar when the
        // page is swapped in. If they disagree, following a link and
        // reloading where it landed are different pages.
        for base in ["/", "/docs"] {
            let manifest = lumen_web::site::manifest(&spec(base));
            let keys = spec(base).keys();
            let routes = routes(base);
            for href in HREFS {
                assert_eq!(
                    routes.address_of(href),
                    page_href(href, &manifest.base_path, &keys, &manifest.entry),
                    "`{href}` under base `{base}`"
                );
            }
        }
    }

    #[test]
    fn every_document_the_build_wrote_reads_back_as_its_own_page() {
        // The other direction, which is what a reload and the browser's back
        // button both need: an address the visitor arrived at has to name the
        // page the emitter wrote that document for.
        for base in ["/", "/docs"] {
            let manifest = lumen_web::site::manifest(&spec(base));
            let routes = routes(base);
            for (key, document) in &manifest.pages {
                let address = join(&manifest.base_path, document);
                assert_eq!(routes.path_at(&address), *key, "{address} is {key}");
                assert!(routes.is_at(&address, key), "{address} already shows {key}");
            }
        }
    }
}
