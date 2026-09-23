//! The clipboard builtins in a real page: `clipboard_write` and
//! `clipboard_read` run on the page's `navigator.clipboard`.
//!
//! A headless browser grants a page no clipboard permission and has no
//! visitor to ask, so the page's clipboard methods are replaced with
//! recording stand-ins before the app runs. What this checks is the wiring,
//! from the builtin to the browser API and back to `on_clipboard`; the
//! browser's own clipboard is the browser's.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
//! ```

#![cfg(all(target_arch = "wasm32", feature = "host-candela"))]

use js_sys::{Function, Reflect};
use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_web_runtime::{assemble, hosts};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The program the build script compiled: an `on_ready` that writes the
/// clipboard and reads it back, and an `on_clipboard` that records the read.
const CLIPBOARD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/clipboard.cdlb"));

/// Replace the page clipboard's `writeText` and `readText` with stand-ins that
/// record what was written and answer reads with `pasted`.
fn stand_in_for_the_clipboard() -> JsValue {
    let navigator = web_sys::window().unwrap().navigator();
    let clipboard = Reflect::get(&navigator, &"clipboard".into()).unwrap();
    assert!(
        !clipboard.is_undefined(),
        "the test page is served from localhost, which is a secure context"
    );
    let write = Function::new_with_args(
        "text",
        "globalThis.__lumenWritten = text; return Promise.resolve();",
    );
    let read = Function::new_no_args("return Promise.resolve('pasted');");
    Reflect::set(&clipboard, &"writeText".into(), &write).unwrap();
    Reflect::set(&clipboard, &"readText".into(), &read).unwrap();
    clipboard
}

#[wasm_bindgen_test]
async fn a_script_writes_and_reads_the_page_clipboard() {
    stand_in_for_the_clipboard();

    let mut app = assemble::portable_app();
    let host = hosts::install(&mut app, "candela", CLIPBOARD, "clipboard.cdlb")
        .expect("this build carries the candela host");
    let compiled = CompiledApp {
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(CLIPBOARD.to_vec()),
        }],
        ..CompiledApp::default()
    };
    compiled.spawn_into(&mut app.world);

    for _ in 0..100 {
        app.tick();
        if let Some(text) = (host.signal)(&app.world, "clipboard_text") {
            let written = Reflect::get(&js_sys::global(), &"__lumenWritten".into()).unwrap();
            assert_eq!(
                written.as_string().as_deref(),
                Some("written by lumen"),
                "clipboard_write reached navigator.clipboard.writeText"
            );
            assert_eq!(
                (host.signal)(&app.world, "clipboard_tag").map(|t| t.stringify()),
                Some("paste".to_string()),
                "the read answers under the tag it was asked with"
            );
            assert_eq!(
                text.stringify(),
                "pasted",
                "what navigator.clipboard.readText resolved to reaches on_clipboard"
            );
            return;
        }
        yield_to_the_page().await;
    }
    panic!("on_clipboard never ran");
}

/// The browser's own clipboard, with nothing standing in for it. A headless
/// browser has no visitor to grant the read, so the text it answers with is
/// the browser's business; what the app owes the script is an answer, so a
/// handler waiting on `on_clipboard` is never left hanging.
#[wasm_bindgen_test]
async fn a_read_the_browser_answers_or_refuses_still_reaches_on_clipboard() {
    let navigator = web_sys::window().unwrap().navigator();
    let clipboard = Reflect::get(&navigator, &"clipboard".into()).unwrap();
    // Whatever another test stood in, the page's own methods are back.
    for name in ["writeText", "readText"] {
        Reflect::delete_property(clipboard.unchecked_ref::<js_sys::Object>(), &name.into())
            .unwrap();
    }

    let mut app = assemble::portable_app();
    let host = hosts::install(&mut app, "candela", CLIPBOARD, "clipboard.cdlb")
        .expect("this build carries the candela host");
    CompiledApp {
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(CLIPBOARD.to_vec()),
        }],
        ..CompiledApp::default()
    }
    .spawn_into(&mut app.world);

    for _ in 0..250 {
        app.tick();
        if let Some(text) = (host.signal)(&app.world, "clipboard_text") {
            web_sys::console::log_1(
                &format!(
                    "the browser's own clipboard answered {:?}",
                    text.stringify()
                )
                .into(),
            );
            return;
        }
        yield_to_the_page().await;
    }
    panic!("on_clipboard never ran");
}

/// Give the page a turn, so a pending promise can settle before the next tick.
async fn yield_to_the_page() {
    let window = web_sys::window().expect("the suite runs in a page");
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 20)
            .expect("the page grants a timer");
    });
    let _ = JsFuture::from(promise).await;
}
