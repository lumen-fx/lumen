//! The request a document is being produced for.
//!
//! A page rendered on a server is rendered for somebody: a method, an address,
//! a query string, the headers that came with it. This is where that arrives,
//! in the two shapes an app reads it in.
//!
//! The parts every surface can read are reserved [`PropertyStore`] cells, the
//! same mechanism [`crate::nav`] publishes the active page through. Markup
//! binds to them by name, a derivation reads them, and every script host sees
//! them without a builtin of its own.
//!
//! The parts that are too large or too sensitive to publish as signals - the
//! headers, the cookies and the body - stay in a [`RequestContext`] the
//! renderer installs on its own thread for the length of one render. A script
//! asks for a named header or a named cookie and gets that one, so a page
//! never carries the whole `Cookie` header around in its state.
//!
//! Nothing here decides which headers an app may see. The renderer that builds
//! the context applies that policy, and what arrives here is what the app is
//! allowed to read.

use std::cell::RefCell;

use crate::property_store::PropertyStore;

/// Reserved global signal holding the request method, uppercased (`GET`).
pub const METHOD_SIGNAL: &str = "request.method";

/// Reserved global signal holding the requested path, without the query
/// string (`/user/42`).
pub const PATH_SIGNAL: &str = "request.path";

/// Reserved global signal holding the query string, without the leading `?`
/// (`page=2&sort=name`).
pub const QUERY_SIGNAL: &str = "request.query";

/// Reserved global signal holding the fragment, without the leading `#`.
///
/// A browser keeps the fragment to itself, so this is empty on a server
/// unless the caller knows it from somewhere else.
pub const HASH_SIGNAL: &str = "request.hash";

/// Reserved global signal that is true when the request arrived over TLS.
pub const SECURE_SIGNAL: &str = "request.secure";

/// What is known about the request being rendered for.
///
/// Header and cookie names are compared without regard to case, which is what
/// HTTP says they mean.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestContext {
    /// Request method, uppercased.
    pub method: String,
    /// Requested path, without the query string.
    pub path: String,
    /// Query string, without the leading `?`.
    pub query: String,
    /// Fragment, without the leading `#`.
    pub hash: String,
    /// Whether the request arrived over TLS.
    pub secure: bool,
    /// The headers the app may read, in the order they arrived.
    pub headers: Vec<(String, String)>,
    /// The cookies the app may read, in the order they arrived.
    pub cookies: Vec<(String, String)>,
    /// The request body, empty when there was none.
    pub body: String,
}

impl RequestContext {
    /// A context for a `GET` of `path`.
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET".to_string(),
            path: path.into(),
            ..Self::default()
        }
    }

    /// The value of a header, or `None` when it did not arrive or the app is
    /// not allowed to read it.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The value of a cookie, or `None` when it did not arrive.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Write the reserved cells, so markup and derivations can read them.
    ///
    /// Called before the app's first tick, which is what lets an `on_start`
    /// branch on the address it was asked for.
    pub fn publish(&self, store: &mut PropertyStore) {
        store.set_global_str(METHOD_SIGNAL, self.method.as_str());
        store.set_global_str(PATH_SIGNAL, self.path.as_str());
        store.set_global_str(QUERY_SIGNAL, self.query.as_str());
        store.set_global_str(HASH_SIGNAL, self.hash.as_str());
        store.set_global_bool(SECURE_SIGNAL, self.secure);
    }
}

/// Split a `Cookie` header, or a browser's `document.cookie`, into pairs.
///
/// Both spell a cookie jar the same way: `name=value` pairs separated by
/// `;`. A pair with no `=` is skipped rather than guessed at.
pub fn parse_cookies(header: &str) -> Vec<(String, String)> {
    header
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

thread_local! {
    /// The request this thread is rendering for. One render at a time per
    /// thread is the whole of the bookkeeping: a renderer holds a thread for
    /// the length of a render, and a browser has one thread and one page.
    static CURRENT: RefCell<Option<RequestContext>> = const { RefCell::new(None) };
}

/// The request installed on this thread for as long as this value lives.
///
/// Dropping it puts back whatever was installed before, so a nested render
/// (a renderer used from inside a request handler) leaves the outer one in
/// place.
#[derive(Debug)]
pub struct Scope(Option<RequestContext>);

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

/// Install `context` on this thread until the returned [`Scope`] is dropped.
pub fn enter(context: RequestContext) -> Scope {
    Scope(install(context))
}

/// Install `context` on this thread for good, and give back whatever was
/// there.
///
/// This is for a surface whose thread answers one address for its whole life:
/// a page in a browser is loaded from an address and keeps it until it is
/// navigated away from. A renderer answering one request after another uses
/// [`enter`] instead, so each render gets its own.
pub fn install(context: RequestContext) -> Option<RequestContext> {
    CURRENT.with(|current| current.borrow_mut().replace(context))
}

/// Read something out of the request installed on this thread.
fn with_current<T>(read: impl FnOnce(&RequestContext) -> T) -> Option<T> {
    CURRENT.with(|current| current.borrow().as_ref().map(read))
}

/// The whole request installed on this thread, if there is one.
pub fn current() -> Option<RequestContext> {
    with_current(Clone::clone)
}

/// The value of a request header, empty when there is none to read or the app
/// is not allowed to read it.
pub fn header(name: &str) -> String {
    with_current(|request| request.header(name).unwrap_or_default().to_string()).unwrap_or_default()
}

/// The value of a request cookie, empty when there is none to read.
pub fn cookie(name: &str) -> String {
    with_current(|request| request.cookie(name).unwrap_or_default().to_string()).unwrap_or_default()
}

/// The request body, empty when there was none.
pub fn body() -> String {
    with_current(|request| request.body.clone()).unwrap_or_default()
}

/// The query string of the address being rendered for, without the leading
/// `?`.
pub fn query() -> String {
    with_current(|request| request.query.clone()).unwrap_or_default()
}

/// The fragment of the address being rendered for, without the leading `#`.
pub fn hash() -> String {
    with_current(|request| request.hash.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RequestContext {
        RequestContext {
            method: "POST".to_string(),
            path: "/user/42".to_string(),
            query: "tab=posts".to_string(),
            hash: String::new(),
            secure: true,
            headers: vec![("accept-language".to_string(), "en-GB".to_string())],
            cookies: vec![("session".to_string(), "abc".to_string())],
            body: "{\"name\":\"ada\"}".to_string(),
        }
    }

    #[test]
    fn the_reserved_cells_carry_the_address() {
        let mut store = PropertyStore::default();
        context().publish(&mut store);
        assert_eq!(
            store.get_global_str(PATH_SIGNAL).as_deref(),
            Some("/user/42")
        );
        assert_eq!(
            store.get_global_str(QUERY_SIGNAL).as_deref(),
            Some("tab=posts")
        );
        assert_eq!(store.get_global_str(METHOD_SIGNAL).as_deref(), Some("POST"));
        assert_eq!(store.get_global_bool(SECURE_SIGNAL), Some(true));
    }

    #[test]
    fn a_header_is_found_whatever_case_it_is_asked_for_in() {
        let request = context();
        assert_eq!(request.header("Accept-Language"), Some("en-GB"));
        assert_eq!(request.header("authorization"), None);
    }

    #[test]
    fn a_cookie_jar_splits_into_pairs() {
        let jar = parse_cookies("session=abc; theme=dark; broken");
        assert_eq!(
            jar,
            vec![
                ("session".to_string(), "abc".to_string()),
                ("theme".to_string(), "dark".to_string())
            ]
        );
    }

    #[test]
    fn nothing_is_readable_outside_a_render() {
        assert_eq!(header("accept-language"), "");
        assert_eq!(cookie("session"), "");
        assert_eq!(body(), "");
        assert_eq!(query(), "");
        assert!(current().is_none());
    }

    #[test]
    fn a_scope_reaches_the_render_and_ends_with_it() {
        {
            let _scope = enter(context());
            assert_eq!(header("accept-language"), "en-GB");
            assert_eq!(cookie("session"), "abc");
            assert_eq!(body(), "{\"name\":\"ada\"}");
            assert_eq!(query(), "tab=posts");
            assert_eq!(hash(), "");
        }
        assert_eq!(header("accept-language"), "");
    }

    #[test]
    fn an_inner_render_leaves_the_outer_one_in_place() {
        let _outer = enter(RequestContext::get("/outer"));
        {
            let _inner = enter(RequestContext::get("/inner"));
            assert_eq!(current().map(|r| r.path), Some("/inner".to_string()));
        }
        assert_eq!(current().map(|r| r.path), Some("/outer".to_string()));
    }
}
