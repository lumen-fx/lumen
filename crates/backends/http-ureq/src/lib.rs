//! The HTTP client Lumen ships: [`ureq`] behind the scripts' `fetch(url, tag)`
//! and `http(#{...})` builtins.
//!
//! [`UreqHttpClient`] implements [`lumen_script::HttpClient`]. It performs one
//! blocking request per call on the worker thread the script runtime spawned,
//! and hands back status, headers, and body; the runtime owns everything above
//! that (queueing, tagging, delivery to handlers). An app selects it through
//! the runtime's `http-fetch` feature, which is on by default.
//!
//! ## What counts as an error
//!
//! Only transport failures: DNS, connect, TLS, timeout, a malformed method or
//! url. Any status that came back over the wire is a completed reply, so a 404
//! reaches the script as `ok: false, status: 404` rather than as an error
//! string. That is web-`fetch` semantics, and it is what lets a script branch
//! on a status instead of parsing prose.
//!
//! ## Bounded reads
//!
//! The runtime passes a byte cap with every request and this client enforces it
//! while reading the body, rather than trusting ureq's own default. A streaming
//! or open-ended endpoint therefore fails with a clear message instead of
//! growing until the process dies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use lumen_script::{HttpClient, HttpRequest, HttpResponse};
use ureq::Agent;
use ureq::http::{Method, Request};

/// A ureq-backed [`HttpClient`].
///
/// Stateless: each request builds its own agent, because the per-request
/// timeout is agent-level configuration in ureq 3.
#[derive(Clone, Copy, Debug, Default)]
pub struct UreqHttpClient;

impl HttpClient for UreqHttpClient {
    fn send(&self, request: &HttpRequest, body_limit: u64) -> Result<HttpResponse, String> {
        let method = Method::from_bytes(request.method.to_ascii_uppercase().as_bytes())
            .map_err(|_| format!("invalid HTTP method: {}", request.method))?;

        // `http_status_as_error(false)` = web-`fetch` semantics: a 4xx / 5xx
        // is a completed reply, not an `Err`. `timeout_global` applies the
        // per-request deadline (Qt `QNetworkRequest` transfer timeout).
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(request.timeout_ms.map(Duration::from_millis))
            .build();
        let agent: Agent = config.into();

        let mut builder = Request::builder().method(method).uri(request.url.as_str());
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }

        // GET-style requests with no body send `()`; anything with an
        // explicit body sends the string. Both `()` and `String` implement
        // `AsSendBody`, so `agent.run` accepts either.
        let reply = match &request.body {
            Some(b) => agent.run(builder.body(b.clone()).map_err(|e| e.to_string())?),
            None => agent.run(builder.body(()).map_err(|e| e.to_string())?),
        };
        let mut reply = reply.map_err(|e| e.to_string())?;

        let status = reply.status().as_u16();
        let headers = reply
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        // Bounded read: `Body::read_to_string()` would use ureq's implicit
        // default; applying the caller's cap explicitly makes a body past
        // `body_limit` fail deterministically (surfaced as the reply's
        // `error`) instead of depending on a transport default that could
        // change. `lossy_utf8(true)` replaces invalid bytes with `?` rather
        // than failing the whole read.
        let body = reply
            .body_mut()
            .with_config()
            .limit(body_limit)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(|e| format!("read body (cap {body_limit} bytes): {e}"))?;

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
