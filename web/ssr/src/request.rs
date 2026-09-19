//! The request a document is rendered for, and how much of it the app sees.

use std::collections::BTreeSet;

use lumen_core::request::{RequestContext, parse_cookies};

/// One request to render a document for.
///
/// Build it from whatever your server hands you. Nothing here parses HTTP:
/// the method, the target and the headers have already been read by the time
/// a renderer is called.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SsrRequest {
    /// Request method. Uppercased on the way in.
    pub method: String,
    /// Requested path, without the query string.
    pub path: String,
    /// Query string, without the leading `?`.
    pub query: String,
    /// Fragment, without the leading `#`. A browser keeps the fragment to
    /// itself, so this is normally empty.
    pub hash: String,
    /// Whether the request arrived over TLS.
    pub secure: bool,
    /// The headers as they arrived, in order. What the app may read of them
    /// is [`HeaderPolicy`]'s decision.
    pub headers: Vec<(String, String)>,
    /// The request body, empty when there was none.
    pub body: String,
    /// The locale to answer in, when something in front of the renderer has
    /// already decided. Empty leaves the choice to the request: a locale
    /// prefix on the path, then `Accept-Language`, then the site's default.
    pub locale: String,
}

impl SsrRequest {
    /// A `GET` of `target`.
    ///
    /// `target` is the request line's target, so `/user/42?tab=posts` splits
    /// into a path and a query the way a browser splits it.
    pub fn get(target: &str) -> Self {
        Self::new("GET", target)
    }

    /// A request of `method` for `target`. See [`Self::get`].
    pub fn new(method: &str, target: &str) -> Self {
        let (target, hash) = split_once_or_all(target, '#');
        let (path, query) = split_once_or_all(target, '?');
        Self {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            query: query.to_string(),
            hash: hash.to_string(),
            ..Self::default()
        }
    }

    /// Add a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Answer in `locale`, whatever the path and the headers ask for.
    ///
    /// This is where a language cookie, a query parameter or a proxy that has
    /// already picked goes. A tag the site holds no tree for is answered in
    /// the site's default locale, with a warning naming it.
    pub fn with_locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    /// Set the request body.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Say the request arrived over TLS.
    pub fn secure(mut self) -> Self {
        self.secure = true;
        self
    }

    /// What the app reads of this request, under `policy`.
    pub fn context(&self, policy: &HeaderPolicy) -> RequestContext {
        let mut cookies = Vec::new();
        let mut headers = Vec::new();
        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case(COOKIE) {
                cookies.extend(parse_cookies(value));
            }
            if policy.allows(name) {
                headers.push((name.to_ascii_lowercase(), value.clone()));
            }
        }
        RequestContext {
            method: self.method.clone(),
            path: self.path.clone(),
            query: self.query.clone(),
            hash: self.hash.clone(),
            secure: self.secure,
            headers,
            cookies,
            body: self.body.clone(),
        }
    }
}

/// Split at the first `separator`, or take the whole string when there is
/// none.
fn split_once_or_all(text: &str, separator: char) -> (&str, &str) {
    text.split_once(separator).unwrap_or((text, ""))
}

/// The header whose value is a cookie jar.
const COOKIE: &str = "cookie";

/// Which request headers the app may read.
///
/// Headers are allowed by name, not refused by name. An app runs somebody
/// else's code against somebody else's request, and the headers that carry
/// credentials are the ones a page has the least reason to want: an app that
/// does want them says so, once, here.
///
/// Cookies are not headers to an app. It reads one by name through
/// `request_cookie`, which is the granularity worth having, so the `Cookie`
/// header itself stays out of reach unless it is named here as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPolicy {
    allowed: BTreeSet<String>,
}

/// The headers an app may read without being given them: the ones that say
/// what the visitor's browser is and where the request came through, and none
/// that say who the visitor is.
const ORDINARY: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "host",
    "referer",
    "user-agent",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-request-id",
];

impl Default for HeaderPolicy {
    fn default() -> Self {
        Self {
            allowed: ORDINARY.iter().map(|name| name.to_string()).collect(),
        }
    }
}

impl HeaderPolicy {
    /// A policy that allows nothing.
    pub fn none() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    /// Also allow `name`.
    pub fn allow(mut self, name: &str) -> Self {
        self.allowed.insert(name.trim().to_ascii_lowercase());
        self
    }

    /// Whether the app may read `name`.
    pub fn allows(&self, name: &str) -> bool {
        self.allowed.contains(&name.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_splits_the_way_a_browser_splits_it() {
        let request = SsrRequest::get("/user/42?tab=posts#top");
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/user/42");
        assert_eq!(request.query, "tab=posts");
        assert_eq!(request.hash, "top");

        let plain = SsrRequest::new("post", "/submit");
        assert_eq!(plain.method, "POST");
        assert_eq!(plain.path, "/submit");
        assert_eq!(plain.query, "");
    }

    #[test]
    fn a_credential_header_needs_naming_and_the_ordinary_ones_do_not() {
        let request = SsrRequest::get("/")
            .with_header("Accept-Language", "en-GB")
            .with_header("Authorization", "Bearer swordfish");

        let guarded = request.context(&HeaderPolicy::default());
        assert_eq!(guarded.header("accept-language"), Some("en-GB"));
        assert_eq!(guarded.header("authorization"), None);

        let asked_for = request.context(&HeaderPolicy::default().allow("Authorization"));
        assert_eq!(asked_for.header("authorization"), Some("Bearer swordfish"));
    }

    #[test]
    fn a_cookie_is_read_by_name_while_the_header_stays_out_of_reach() {
        let request = SsrRequest::get("/").with_header("Cookie", "session=abc; theme=dark");
        let context = request.context(&HeaderPolicy::default());
        assert_eq!(context.cookie("session"), Some("abc"));
        assert_eq!(context.cookie("theme"), Some("dark"));
        assert_eq!(context.header("cookie"), None);

        let asked_for = request.context(&HeaderPolicy::default().allow("cookie"));
        assert_eq!(asked_for.header("cookie"), Some("session=abc; theme=dark"));
    }

    #[test]
    fn a_policy_that_allows_nothing_allows_nothing() {
        let request = SsrRequest::get("/").with_header("accept", "text/html");
        assert!(request.context(&HeaderPolicy::none()).headers.is_empty());
    }
}
