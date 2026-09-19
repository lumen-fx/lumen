//! What the dispatcher and the browser have to agree about, checked in a real
//! browser against a real server.
//!
//! Nothing short of a browser catches this. A shimmed `fetch` would answer
//! whatever the shim was written to answer, and the questions here are the
//! browser's own: whether a reply comes back on the page's task queue, what a
//! status that is not 2xx counts as, and what a request the browser refuses
//! outright does to the app.
//!
//! ```sh
//! cargo test -p lumen-web-http --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use lumen_script::{HttpDispatch, HttpDone, HttpRequest, HttpResponse};
use lumen_web_http::WebFetchDispatch;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The cap the runtime passes with every request, big enough that nothing here
/// meets it.
const BODY_LIMIT: u64 = 16 * 1024 * 1024;

/// A GET of `url` with no headers and no body, the shape `fetch(url, tag)`
/// builds.
fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        ..HttpRequest::default()
    }
}

/// The page this suite is running in, which the test runner is serving.
fn own_url() -> String {
    web_sys::window()
        .expect("the suite runs in a page")
        .location()
        .href()
        .expect("the page has an address")
}

/// Dispatch `request` and wait for its outcome the way the app does: the
/// dispatcher returns at once, and the reply arrives on the page's task queue
/// some frames later.
async fn outcome(request: HttpRequest) -> Result<HttpResponse, String> {
    let (tx, rx): (_, Receiver<Result<HttpResponse, String>>) = channel();
    let done: HttpDone = Box::new(move |result| {
        tx.send(result).expect("the receiver outlives the request");
    });
    let dispatch: Arc<dyn HttpDispatch> = Arc::new(WebFetchDispatch);
    dispatch.dispatch("test", request, BODY_LIMIT, done);

    for _ in 0..250 {
        if let Ok(result) = rx.try_recv() {
            return result;
        }
        yield_to_the_page().await;
    }
    panic!("the request never came back");
}

/// Give the page a turn, so a pending fetch can resolve before the next look.
async fn yield_to_the_page() {
    let window = web_sys::window().expect("the suite runs in a page");
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 20)
            .expect("the page grants a timer");
    });
    let _ = JsFuture::from(promise).await;
}

#[wasm_bindgen_test]
async fn a_reply_comes_back_through_the_completion_callback() {
    let reply = outcome(get(&own_url()))
        .await
        .expect("the server this page came from answers for it");

    assert_eq!(reply.status, 200);
    assert!(
        !reply.body.is_empty(),
        "the body is read off the response stream, not left behind"
    );
    assert!(
        reply
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type")),
        "the reply carries the headers the browser let the page read: {:?}",
        reply.headers
    );
}

#[wasm_bindgen_test]
async fn a_status_that_is_not_2xx_is_a_completed_reply() {
    let reply = outcome(get("nothing-is-served-here"))
        .await
        .expect("a 404 is an answer, not a transport failure");

    assert_eq!(reply.status, 404);
}

#[wasm_bindgen_test]
async fn a_request_the_browser_refuses_is_reported_rather_than_taking_the_page() {
    // Port 1 is on every browser's blocked list, so this is refused before a
    // packet is sent and needs no server to be down.
    let error = outcome(get("http://127.0.0.1:1/"))
        .await
        .expect_err("the browser will not make this request");

    assert!(
        error.contains("http://127.0.0.1:1/"),
        "the report names what was asked for: {error}"
    );
    assert!(
        error.contains("Access-Control-Allow-Origin"),
        "a browser gives one failure for every reason, so the report says \
         where to look: {error}"
    );
}
