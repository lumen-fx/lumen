//! The HTTP seam behind the scripts' `fetch(url, tag)` and `http(#{...})`
//! builtins.
//!
//! [`HttpClient`] is the whole transport contract: one blocking call that
//! turns an [`HttpRequest`] into an [`HttpResponse`], with a byte cap on the
//! body it buffers. The client Lumen ships is `lumen-http-ureq`, selected by
//! the default-on `http-fetch` feature. An embedder that needs a different
//! stack (a proxy-aware client, a recording client in tests, an in-process
//! fake) implements the trait and installs it with
//! [`FetchRegistry::with_client`](crate::runtime::FetchRegistry::with_client)
//! before the script plugin builds.
//!
//! The types are transport-only on purpose: the delivery tag, the
//! `fetch()`-versus-`http()` reply style, and the retry or logging policy all
//! stay on the runtime side of the seam, so a client implementation never has
//! to know what a script is.

/// A request handed to a client on a per-request worker thread. Method, url,
/// headers, and body in: the input half of the Qt `QNetworkRequest` shape.
#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    /// HTTP method. Case-insensitive; clients uppercase it.
    pub method: String,
    /// Absolute request url, as the script wrote it.
    pub url: String,
    /// Request headers, in the order the script supplied them.
    pub headers: Vec<(String, String)>,
    /// Request body, sent verbatim. `None` sends no body at all.
    pub body: Option<String>,
    /// Whole-request deadline. `None` leaves the client's own default in
    /// place.
    pub timeout_ms: Option<u64>,
}

/// The reply half of the Qt `QNetworkReply` shape: status, headers, and body
/// out.
///
/// A 4xx or 5xx belongs here, not in the error channel: it is a completed
/// reply, and the runtime decides what a non-2xx means for each builtin.
#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    /// Numeric status line code.
    pub status: u16,
    /// Response headers as received. The runtime lowercases names before
    /// handing them to a script.
    pub headers: Vec<(String, String)>,
    /// Response body, decoded as UTF-8 with invalid bytes replaced.
    pub body: String,
}

/// The transport a script's HTTP builtins run on.
///
/// Implementations are called from a per-request worker thread and may block
/// for the length of the request, so they must not touch the world.
pub trait HttpClient: Send + Sync + 'static {
    /// Perform `request` and return the reply.
    ///
    /// `Err` is reserved for transport failures (DNS, connect, TLS, timeout,
    /// a malformed method or url); any status that came back over the wire is
    /// `Ok`. The error string reaches the script verbatim as the reply's
    /// `error` field, so it should read as a sentence.
    ///
    /// `body_limit` is a hard cap, in bytes, on the response body buffered
    /// into memory. Reading past it must fail rather than allocate: an
    /// open-ended or streaming endpoint must not be able to exhaust memory on
    /// the worker thread.
    fn send(&self, request: &HttpRequest, body_limit: u64) -> Result<HttpResponse, String>;
}

/// The client a build without the `http-fetch` feature gets: every request
/// resolves to an error carrying the rebuild hint, so a size-trimmed binary
/// says why `fetch()` did nothing instead of silently dropping it.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledHttpClient;

impl HttpClient for DisabledHttpClient {
    fn send(&self, _request: &HttpRequest, _body_limit: u64) -> Result<HttpResponse, String> {
        Err(
            "the script runtime was built without the `http-fetch` feature; \
             fetch() / http() are disabled in this binary"
                .to_string(),
        )
    }
}
