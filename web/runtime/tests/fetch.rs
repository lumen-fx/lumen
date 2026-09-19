//! A script reaching the network from a page, end to end.
//!
//! The dispatcher's own suite proves a browser request comes back; this proves
//! the app is wired to it. A page assembles its app through `portable_app`,
//! which is where the browser's transport is installed, and everything above
//! the transport is the desktop's: the same builtin queues the request, the
//! same channel carries the reply, and the same `on_fetch` runs.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use lumen_web_runtime::{assemble, hosts};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The engine the fixture is written in, as a manifest names it.
const ENGINE: &str = "candela";

/// The fixture the build script compiled with the linked candela compiler. Its
/// `on_start` fetches the page the runner is serving.
const FETCH: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fetch.cdlb"));

#[wasm_bindgen_test]
async fn a_reply_reaches_the_handler_the_script_declared() {
    let mut app = assemble::portable_app();
    let host = hosts::install(&mut app, ENGINE, FETCH, "fetch.cdlb")
        .expect("this build carries the candela host");

    // The request left during `on_start`; ticking is what carries it to the
    // browser and what delivers the reply once the browser has it.
    for _ in 0..250 {
        app.tick();
        if let Some(tag) = (host.signal)(&app.world, "fetched") {
            assert_eq!(
                tag.stringify(),
                "page",
                "the handler is called with the tag the request was made under"
            );
            let body = (host.signal)(&app.world, "body").expect("the reply carried a body");
            assert!(
                !body.stringify().is_empty(),
                "the body the browser read reaches the script whole"
            );
            return;
        }
        if let Some(failure) = (host.signal)(&app.world, "failed") {
            panic!("the request failed: {}", failure.stringify());
        }
        yield_to_the_page().await;
    }
    panic!("the reply never reached the script");
}

/// Give the page a turn, so a pending fetch can resolve before the next tick.
async fn yield_to_the_page() {
    let window = web_sys::window().expect("the suite runs in a page");
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 20)
            .expect("the page grants a timer");
    });
    let _ = JsFuture::from(promise).await;
}
