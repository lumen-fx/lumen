//! The HTTP seam behind the scripts' `fetch(url, tag)` and `http(#{...})`
//! builtins.
//!
//! [`HttpClient`] is the transport contract: one blocking call that turns an
//! [`HttpRequest`] into an [`HttpResponse`], with a byte cap on the body it
//! buffers. The client Lumen ships is `lumen-http-ureq`, selected by the
//! default-on `http-fetch` feature. An embedder that needs a different stack (a
//! proxy-aware client, a recording client in tests, an in-process fake)
//! implements the trait and installs it with
//! [`FetchRegistry::with_client`](crate::runtime::FetchRegistry::with_client)
//! before the script plugin builds.
//!
//! [`HttpDispatch`] is the concurrency contract next to it: who runs the
//! request and how its reply gets back. [`ThreadDispatch`] is the one every
//! desktop build uses, a worker thread per request blocked in
//! [`HttpClient::send`]. A platform with no threads to block supplies its own
//! and installs it with
//! [`FetchRegistry::with_dispatch`](crate::runtime::FetchRegistry::with_dispatch);
//! in a browser that is `lumen-web-http`, which runs the request on the page's
//! own `fetch` and needs no client behind it.
//!
//! The types are transport-only on purpose: the delivery tag, the
//! `fetch()`-versus-`http()` reply style, and the retry or logging policy all
//! stay on the runtime side of the seam, so a client implementation never has
//! to know what a script is.

use std::sync::Arc;

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

/// The completion callback a dispatcher owns for the life of one request. It
/// is called exactly once, from whatever thread or task the reply arrived on,
/// and it never touches the world: all it does is hand the outcome to the
/// channel the world thread drains.
pub type HttpDone = Box<dyn FnOnce(Result<HttpResponse, String>) + Send + 'static>;

/// How a request leaves the calling thread and how its reply comes back.
///
/// [`HttpClient`] says how to perform a request; this says who performs it and
/// when the answer arrives. They are separate because the answer is not always
/// a blocked thread waiting on a socket: a browser has no thread to block and
/// resolves the request from a promise instead.
pub trait HttpDispatch: Send + Sync + 'static {
    /// Start `request` and hand its outcome to `done` exactly once.
    ///
    /// This call returns immediately; it must never block on the request. The
    /// dispatcher forwards `body_limit` to the client unchanged - the cap is
    /// the caller's policy, not the transport's. `label` names the work for
    /// diagnostics (it becomes the worker thread's name) and carries no
    /// meaning a dispatcher may act on.
    fn dispatch(&self, label: &str, request: HttpRequest, body_limit: u64, done: HttpDone);
}

/// The dispatcher every desktop build runs: one worker thread per request,
/// blocked in [`HttpClient::send`] until the reply lands.
///
/// A thread per request suits the usual "a few API calls per UI action"
/// workload; a pool can replace it behind the same trait if that stops being
/// true.
///
/// A platform with no threads cannot run this one, and up front is the only
/// place to say so: [`std::thread::Builder::spawn`] consumes the completion
/// callback, so a failed spawn leaves nothing to report the failure through.
/// Such a platform gets an error naming what it should install instead, which
/// is what a browser build sees until `lumen-web-http` goes in.
pub struct ThreadDispatch(
    // Unreachable where there are no threads, since `dispatch` answers before
    // it would reach the client. It stays in place so both platforms build the
    // same type rather than a second one appearing for the smaller case.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] Arc<dyn HttpClient>,
);

impl ThreadDispatch {
    /// Run every dispatched request on `client`.
    pub fn new(client: Arc<dyn HttpClient>) -> Self {
        Self(client)
    }
}

impl HttpDispatch for ThreadDispatch {
    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch(&self, label: &str, request: HttpRequest, body_limit: u64, done: HttpDone) {
        let client = Arc::clone(&self.0);
        std::thread::Builder::new()
            .name(format!("lumen-http:{label}"))
            .spawn(move || done(client.send(&request, body_limit)))
            .expect("spawn http thread");
    }

    #[cfg(target_arch = "wasm32")]
    fn dispatch(&self, _label: &str, request: HttpRequest, _body_limit: u64, done: HttpDone) {
        done(Err(format!(
            "cannot request {}: this platform has no thread to run a request \
             on, so a build for it installs its own dispatcher with \
             FetchRegistry::with_dispatch",
            request.url
        )));
    }
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
