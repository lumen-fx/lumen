//! The browser smoke gate: a real page, a real wasm module, real candela
//! bytecode.
//!
//! Run with a headless browser, never a DOM shim. The web target's whole point
//! is that the browser is the layout and paint engine, so a fake DOM would let
//! the suite pass on things a page would reject.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```
//!
//! `wasm-bindgen-test-runner` drives Chrome through `chromedriver`; point
//! `CHROMEDRIVER` at the binary if it is not on `PATH`.

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use lumen_web_runtime::LumenWebApp;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The engine the fixtures are written in, as a manifest names it.
const ENGINE: &str = "candela";

/// The fixtures the build script compiled with the linked candela compiler.
const SMOKE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke.cdlb"));
const UNBOUND: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/unbound.cdlb"));

/// Boot a fixture the way a page boots an app: over the engine a manifest names.
fn boot(program: &[u8]) -> LumenWebApp {
    LumenWebApp::new(ENGINE, program).expect("this build carries the candela host")
}

#[wasm_bindgen_test]
fn the_artifact_loads_and_its_host_functions_bind() {
    let app = boot(SMOKE);

    assert_eq!(
        app.script_error(),
        None,
        "the prelude declares the whole builtin surface, so a clean load \
         proves every declaration found its closure"
    );
    assert_eq!(
        app.signal("greeting").as_deref(),
        Some("hello from candela"),
        "on_start ran and the builtin it called wrote through to the host"
    );
}

#[wasm_bindgen_test]
fn an_exported_handler_runs_and_its_writes_are_readable() {
    let mut app = boot(SMOKE);
    assert!(app.exports().iter().any(|e| e == "bump"));

    assert_eq!(
        app.call("bump").expect("the handler runs").as_deref(),
        Some("1")
    );
    assert_eq!(app.signal("count").as_deref(), Some("1"));

    assert_eq!(
        app.call("bump").expect("the handler runs").as_deref(),
        Some("2")
    );
    assert_eq!(
        app.signal("count").as_deref(),
        Some("2"),
        "the VM's state is resident between calls"
    );
}

#[wasm_bindgen_test]
fn a_tick_drives_the_script_systems() {
    let mut app = boot(SMOKE);
    assert_eq!(
        app.signal("shout").as_deref(),
        Some(""),
        "registering a derivation reserves its signal; nothing has computed it"
    );

    app.tick();

    assert_eq!(
        app.signal("shout").as_deref(),
        Some("hello from candela!"),
        "the tick ran the derivation pass, which called back into the script"
    );
}

#[wasm_bindgen_test]
fn calling_a_function_the_artifact_does_not_export_is_not_an_error() {
    let mut app = boot(SMOKE);
    assert_eq!(app.call("on_click").expect("a miss is not an error"), None);
}

#[wasm_bindgen_test]
fn a_broken_script_is_reported_rather_than_taking_the_page_with_it() {
    let mut app = LumenWebApp::with_uri(ENGINE, UNBOUND, "unbound.cdlb")
        .expect("this build carries the candela host");

    let error = app
        .script_error()
        .expect("a declaration with no closure behind it fails the load");
    assert!(
        error.contains("no_such_builtin"),
        "the report names what is missing: {error}"
    );
    // The module is still alive and the app still ticks; a dead script does not
    // take the page down with it.
    app.tick();
    assert_eq!(app.signal("greeting"), None);
}

#[wasm_bindgen_test]
fn an_engine_no_host_answers_for_is_named_in_the_refusal() {
    let refused = LumenWebApp::new("lua", SMOKE)
        .err()
        .expect("no host in this build runs lua");

    let message: String = js_sys::Error::from(JsValue::from(refused)).message().into();
    assert!(
        message.contains("lua") && message.contains("candela"),
        "the refusal names the engine asked for and the ones this build has: {message}"
    );
}
