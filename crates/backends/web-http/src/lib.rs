//! The HTTP transport a Lumen app runs on in a browser: the page's own
//! `fetch`, behind the scripts' `fetch(url, tag)` and `http(#{...})` builtins.
//!
//! [`WebFetchDispatch`] implements [`lumen_script::HttpDispatch`] rather than
//! [`lumen_script::HttpClient`], which is the difference between this crate
//! and `lumen-http-ureq`. A desktop request finishes when a worker thread
//! returns from a blocking read; a browser request finishes when a promise
//! resolves, and a page has no thread to block in the first place. The
//! dispatcher is the seam where those two shapes differ, so a browser build
//! replaces it and leaves everything above it alone: the reply travels the
//! same channel, and `on_fetch` and `on_http` fire the way they do on the
//! desktop.
//!
//! ## What counts as an error
//!
//! Only failures to complete the exchange: a refused or blocked request, a
//! dead network, a request the browser would not construct, an abandoned read.
//! Any status that came back is a completed reply, so a 404 reaches the script
//! as `ok: false, status: 404` rather than as an error string. That is
//! web-`fetch` semantics, which is also what `lumen-http-ureq` implements, and
//! it is what lets a script branch on a status instead of parsing prose.
//!
//! ## What the browser decides
//!
//! Whether the request is allowed at all. A page may read a cross-origin
//! response only when that server sends `Access-Control-Allow-Origin` for the
//! page's origin, and the browser reports a refusal, a blocked request and an
//! unreachable host with one indistinguishable failure. The error a script
//! receives says so, because the page's console is the only place the reason
//! is written.
//!
//! The browser also owns the request headers it reserves for itself. A header
//! like `Host` or `Content-Length` is dropped on the way out whatever a script
//! asked for, and no error is raised.
//!
//! ## Bounded reads
//!
//! The runtime passes a byte cap with every request. The body is read a chunk
//! at a time and the read is abandoned at the cap, so an open-ended or
//! streaming endpoint fails with a clear message instead of growing until the
//! tab dies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use js_sys::{Array, Reflect, Uint8Array};
use lumen_script::{HttpDispatch, HttpDone, HttpRequest, HttpResponse};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{AbortSignal, Headers, ReadableStreamDefaultReader, Request, RequestInit, Response};

/// A browser-`fetch`-backed [`HttpDispatch`].
///
/// Stateless: a request carries everything the browser needs, and the page
/// owns the connection pool, the cache and the cookie jar.
#[derive(Clone, Copy, Debug, Default)]
pub struct WebFetchDispatch;

impl HttpDispatch for WebFetchDispatch {
    fn dispatch(&self, _label: &str, request: HttpRequest, body_limit: u64, done: HttpDone) {
        // The task resolves on the page's own queue, which is the thread the
        // world ticks on, so `done` hands the outcome to the runtime's channel
        // from where the runtime already drains it and no frame ever waits.
        spawn_local(async move {
            done(send(&request, body_limit).await);
        });
    }
}

/// Perform `request` through the page and read at most `body_limit` bytes of
/// the reply.
async fn send(request: &HttpRequest, body_limit: u64) -> Result<HttpResponse, String> {
    let window = web_sys::window().ok_or("there is no page to fetch from")?;

    let init = RequestInit::new();
    init.set_method(&request.method.to_ascii_uppercase());
    if let Some(body) = &request.body {
        init.set_body(&JsValue::from_str(body));
    }
    if !request.headers.is_empty() {
        let headers = Headers::new().map_err(|e| describe(&e))?;
        for (name, value) in &request.headers {
            headers.append(name, value).map_err(|e| describe(&e))?;
        }
        init.set_headers(&headers);
    }
    // A browser has no whole-request deadline to configure, so the deadline is
    // expressed as the thing it does have: a signal that aborts the fetch and
    // the body read together once the time is up.
    if let Some(ms) = request.timeout_ms {
        let deadline = AbortSignal::timeout_with_u32(u32::try_from(ms).unwrap_or(u32::MAX));
        init.set_signal(Some(&deadline));
    }

    let js_request =
        Request::new_with_str_and_init(&request.url, &init).map_err(|e| describe(&e))?;
    let response: Response = JsFuture::from(window.fetch_with_request(&js_request))
        .await
        .map_err(|e| refused(&request.url, &e))?
        .unchecked_into();

    Ok(HttpResponse {
        status: response.status(),
        headers: response_headers(&response),
        body: read_body(&response, body_limit).await?,
    })
}

/// The reply's headers, in the order the browser hands them over. A name it
/// withholds from a page, and a value that is not a string, are left out
/// rather than reported: the reply is complete either way.
fn response_headers(response: &Response) -> Vec<(String, String)> {
    response
        .headers()
        .entries()
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pair = Array::from(&entry);
            Some((pair.get(0).as_string()?, pair.get(1).as_string()?))
        })
        .collect()
}

/// Read the reply's body, stopping at `body_limit` bytes.
///
/// Chunk at a time through the response stream rather than `Response::text()`,
/// which buffers whatever arrives: the cap has to be able to refuse a body
/// before it is in memory, which is the whole reason the runtime passes one.
async fn read_body(response: &Response, body_limit: u64) -> Result<String, String> {
    // A 204, a 304 and a redirect the browser followed to nothing all arrive
    // with no stream at all.
    let Some(stream) = response.body() else {
        return Ok(String::new());
    };
    let reader = ReadableStreamDefaultReader::new(&stream).map_err(|e| describe(&e))?;
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read())
            .await
            .map_err(|e| format!("read body: {}", describe(&e)))?;
        if Reflect::get(&chunk, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true)
        {
            return Ok(String::from_utf8_lossy(&buffer).into_owned());
        }
        let bytes = Reflect::get(&chunk, &JsValue::from_str("value"))
            .and_then(|value| value.dyn_into::<Uint8Array>())
            .map_err(|_| "read body: the reply produced a chunk that is not bytes".to_string())?;
        if buffer.len() as u64 + u64::from(bytes.length()) > body_limit {
            // Cancelling releases the connection the rest of the body would
            // have arrived on; a page that keeps reading is a page that keeps
            // paying for what it already refused.
            let _ = JsFuture::from(reader.cancel()).await;
            return Err(format!(
                "read body (cap {body_limit} bytes): the reply is larger than the cap"
            ));
        }
        buffer.extend_from_slice(&bytes.to_vec());
    }
}

/// What a rejected `fetch` means, for a script that only ever sees the string.
///
/// A browser gives one failure for every reason a request did not happen, and
/// writes the reason to the console instead. Saying which reasons those are is
/// the difference between a developer looking at the console and a developer
/// looking at their server logs.
fn refused(url: &str, error: &JsValue) -> String {
    format!(
        "fetch {url}: {}; a browser reports a cross-origin refusal, a blocked \
         request and an unreachable host the same way. Check that the server \
         sends `Access-Control-Allow-Origin` for this page's origin, and read \
         the reason the browser logged to the console.",
        describe(error)
    )
}

/// The text of a JavaScript exception. An `Error` carries a message worth
/// reading; anything else is reported as it prints.
fn describe(error: &JsValue) -> String {
    if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}
