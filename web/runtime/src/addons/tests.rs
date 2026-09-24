//! The add-on binding in a real page: module objects built by the page's own
//! JavaScript, installed into an app the way [`crate::boot`] installs them.
//!
//! ```sh
//! cargo test -p lumen-web-runtime --lib --target wasm32-unknown-unknown
//! ```

use std::collections::BTreeMap;

use js_sys::{Function, Reflect};
use lumen_core::components::Color;
use lumen_core::plugin_events::{QueuedEvent, drain_plugin_events};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_html::contract::{AddonRef, ForeignElement, Manifest};
use lumen_ir::addon::{Addon, AddonElement, AddonFunction, AddonParam};
use lumen_script::{PluginEvent, ScriptFnRegistry, ScriptValue};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use super::install;
use crate::assemble::portable_app;

wasm_bindgen_test_configure!(run_in_browser);

/// The module the tests install: a namespace object the way a page's
/// `import * as echo from "./echo.js"` hands one over. `install` keeps the
/// host and the config on `globalThis` so a test can reach them afterwards.
const ECHO_MODULE: &str = r#"
    return {
        install(host, config) { globalThis.echoHost = host; globalThis.echoConfig = config; },
        shout(text) { return text.toUpperCase(); },
        pair(a, b) { return { a, b, list: [a, b] }; },
        nothing() { return undefined; },
        boom() { throw new Error("it broke"); },
        promised() { return Promise.resolve(1); },
        cyclic() { const o = {}; o.self = o; return o; },
        later(text) {
            return new Promise((resolve) => setTimeout(() => resolve(text + "!"), 0));
        },
        refuse() { return Promise.reject(new Error("no thanks")); },
        elements: {
            "echo-view": { mount() {}, update() {} },
        },
    };
"#;

fn module(source: &str) -> JsValue {
    Function::new_no_args(source)
        .call0(&JsValue::NULL)
        .expect("the module source evaluates")
}

fn function(
    name: &str,
    params: &[(&str, &str)],
    returns: &str,
    event: Option<&str>,
) -> AddonFunction {
    AddonFunction {
        name: name.to_string(),
        params: params
            .iter()
            .map(|(name, ty)| AddonParam {
                name: (*name).to_string(),
                ty: (*ty).to_string(),
            })
            .collect(),
        returns: returns.to_string(),
        event: event.map(str::to_string),
        doc: String::new(),
    }
}

/// What the compiled app says the echo add-on offers. `missing` is declared
/// and has no export behind it.
fn echo() -> Addon {
    Addon {
        name: "echo".to_string(),
        namespace: "echo".to_string(),
        functions: vec![
            function("shout", &[("text", "string")], "string", None),
            function("pair", &[("a", "int"), ("b", "string")], "any", None),
            function("nothing", &[], "null", None),
            function("boom", &[], "null", None),
            function("promised", &[], "int", None),
            function("cyclic", &[], "any", None),
            function("missing", &[], "null", None),
            function("later", &[("text", "string")], "null", Some("on_later")),
            function("refuse", &[], "null", Some("on_refuse")),
        ],
        elements: vec![AddonElement {
            tag: "echo-view".to_string(),
            html: "div".to_string(),
            void: false,
        }],
    }
}

fn manifest(addons: &[&str]) -> Manifest {
    let mut foreign = BTreeMap::new();
    foreign.insert(
        "echo-view".to_string(),
        ForeignElement {
            html: "div".to_string(),
            void: false,
        },
    );
    Manifest {
        addons: addons
            .iter()
            .map(|name| AddonRef {
                name: (*name).to_string(),
                module: format!("addons/{name}/{name}.js"),
                styles: Vec::new(),
                head: None,
                config: None,
            })
            .collect(),
        foreign,
        ..Manifest::default()
    }
}

/// Call the script function `namespace::name` the install registered.
fn call(
    app: &lumen_core::prelude::App,
    name: &str,
    args: &[ScriptValue],
) -> Result<ScriptValue, String> {
    let registry = app.world.resource::<ScriptFnRegistry>();
    let f = registry
        .fns()
        .iter()
        .rev()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("`{name}` is registered"));
    f.invoke(args).0
}

/// Let the page's queue run, so a promise the module returned settles.
async fn settle() {
    let wait = js_sys::Promise::new(&mut |resolve, _| {
        Function::new_with_args("resolve", "setTimeout(resolve, 20)")
            .call1(&JsValue::NULL, &resolve)
            .unwrap();
    });
    JsFuture::from(wait).await.unwrap();
}

/// The handler calls waiting on the plugin bus, as `(event, key, args)`.
fn calls() -> Vec<(String, String, Vec<ScriptValue>)> {
    drain_plugin_events()
        .into_iter()
        .filter_map(|queued| match queued {
            QueuedEvent::Value(value) => value.downcast::<PluginEvent>().ok(),
            QueuedEvent::Bytes(_) => None,
        })
        .filter_map(|event| match *event {
            PluginEvent::Call {
                event, key, args, ..
            } => Some((event, key, args)),
            _ => None,
        })
        .collect()
}

fn host() -> JsValue {
    Reflect::get(&js_sys::global(), &"echoHost".into()).expect("install kept the host")
}

fn host_fn(name: &str) -> Function {
    Reflect::get(&host(), &name.into())
        .expect("the host member")
        .dyn_into()
        .expect("a function")
}

#[wasm_bindgen_test]
fn a_site_with_no_addons_takes_the_elements_from_the_manifest_alone() {
    let mut app = portable_app();
    let foreign = install(&mut app, &manifest(&[]), &[], &JsValue::UNDEFINED).unwrap();
    assert!(foreign.contains("echo-view"));
    let foreign = install(&mut app, &manifest(&[]), &[], &JsValue::NULL).unwrap();
    assert_eq!(
        foreign.get("echo-view").map(|t| t.html.as_str()),
        Some("div")
    );
}

#[wasm_bindgen_test]
fn a_page_and_a_manifest_that_disagree_are_refused() {
    let mut app = portable_app();
    let not_a_list = install(&mut app, &manifest(&[]), &[], &JsValue::from_str("echo"))
        .err()
        .expect("a string is not a list of modules");
    assert!(
        not_a_list.to_string().contains("not a list"),
        "{not_a_list}"
    );

    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    let miscounted = install(&mut app, &manifest(&["echo", "other"]), &[echo()], &modules)
        .err()
        .expect("two add-ons named, one module loaded");
    assert!(
        miscounted
            .to_string()
            .contains("loaded 1 add-on modules and the manifest names 2"),
        "{miscounted}"
    );

    let undescribed = install(&mut app, &manifest(&["other"]), &[echo()], &modules)
        .err()
        .expect("the manifest names an add-on the app does not describe");
    assert!(undescribed.to_string().contains("'other'"), "{undescribed}");
}

#[wasm_bindgen_test]
fn an_addon_whose_signature_does_not_bind_is_refused() {
    let mut app = portable_app();
    let mut addon = echo();
    addon.functions[0].params[0].ty = "text".to_string();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    let refused = install(&mut app, &manifest(&["echo"]), &[addon], &modules)
        .err()
        .expect("a parameter type no host reads");
    assert!(refused.to_string().contains("echo::shout"), "{refused}");
}

#[wasm_bindgen_test]
fn install_is_handed_the_config_the_dependency_gave_or_an_empty_one() {
    let config_of = |config: Option<&str>| -> JsValue {
        let mut app = portable_app();
        let mut manifest = manifest(&["echo"]);
        manifest.addons[0].config = config.map(str::to_string);
        let modules = js_sys::Array::of1(&module(ECHO_MODULE));
        install(&mut app, &manifest, &[echo()], &modules).unwrap();
        Reflect::get(&js_sys::global(), &"echoConfig".into()).unwrap()
    };
    let given = config_of(Some(r#"{"loud":true}"#));
    assert_eq!(
        Reflect::get(&given, &"loud".into()).unwrap().as_bool(),
        Some(true)
    );
    let none = config_of(None);
    assert!(none.is_object());
    assert_eq!(
        js_sys::Object::keys(none.unchecked_ref::<js_sys::Object>()).length(),
        0
    );
}

#[wasm_bindgen_test]
fn a_throwing_install_is_reported_and_the_functions_still_bind() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(
        r#"return { install() { throw new Error("no"); }, shout(t) { return t; } };"#,
    ));
    let mut addon = echo();
    addon.functions.truncate(1);
    install(&mut app, &manifest(&["echo"]), &[addon], &modules).unwrap();
    assert_eq!(
        call(&app, "shout", &[ScriptValue::Str("same".into())]),
        Ok(ScriptValue::Str("same".into()))
    );
}

#[wasm_bindgen_test]
fn a_synchronous_call_crosses_as_json_both_ways() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    let foreign = install(&mut app, &manifest(&["echo"]), &[echo()], &modules).unwrap();
    assert!(foreign.contains("echo-view"));

    assert_eq!(
        call(&app, "shout", &[ScriptValue::Str("hi".into())]),
        Ok(ScriptValue::Str("HI".into()))
    );
    let Ok(ScriptValue::Map(pair)) = call(
        &app,
        "pair",
        &[ScriptValue::I64(2), ScriptValue::Str("b".into())],
    ) else {
        panic!("an object comes back as a map");
    };
    assert_eq!(pair.get("a"), Some(&ScriptValue::I64(2)));
    assert_eq!(
        pair.get("list"),
        Some(&ScriptValue::Array(vec![
            ScriptValue::I64(2),
            ScriptValue::Str("b".into())
        ]))
    );
    assert_eq!(call(&app, "nothing", &[]), Ok(ScriptValue::Unit));
}

#[wasm_bindgen_test]
fn a_call_that_fails_says_which_function_and_why() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    install(&mut app, &manifest(&["echo"]), &[echo()], &modules).unwrap();

    let thrown = call(&app, "boom", &[]).unwrap_err();
    assert_eq!(thrown, "echo::boom: it broke");
    let promised = call(&app, "promised", &[]).unwrap_err();
    assert!(promised.contains("declare it with `async`"), "{promised}");
    let cyclic = call(&app, "cyclic", &[]).unwrap_err();
    assert!(cyclic.starts_with("echo::cyclic: "), "{cyclic}");
    let missing = call(&app, "missing", &[]).unwrap_err();
    assert!(missing.contains("exports no such function"), "{missing}");
}

#[wasm_bindgen_test]
async fn an_async_call_answers_later_as_its_event() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    install(&mut app, &manifest(&["echo"]), &[echo()], &modules).unwrap();
    let _ = calls();

    assert_eq!(
        call(
            &app,
            "later",
            &[
                ScriptValue::Str("hey".into()),
                ScriptValue::Str("t1".into())
            ]
        ),
        Ok(ScriptValue::Unit)
    );
    // A tag that is not text is written as the text it prints as.
    call(&app, "refuse", &[ScriptValue::I64(7)]).unwrap();
    // With no tag at all the event arrives under an empty key.
    call(&app, "refuse", &[]).unwrap();
    settle().await;

    let mut seen = calls();
    seen.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        seen,
        [
            (
                "on_refuse_error".to_string(),
                String::new(),
                vec![ScriptValue::Str("no thanks".into())]
            ),
            (
                "on_refuse_error".to_string(),
                "7".to_string(),
                vec![ScriptValue::Str("no thanks".into())]
            ),
            (
                "on_later".to_string(),
                "t1".to_string(),
                vec![ScriptValue::Str("hey!".into())]
            ),
        ]
    );
}

#[wasm_bindgen_test]
fn the_host_emits_events_and_reads_and_writes_signals() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    install(&mut app, &manifest(&["echo"]), &[echo()], &modules).unwrap();
    let _ = calls();

    let emit = host_fn("emit");
    let args = js_sys::Array::new();
    for value in [
        JsValue::from_str("on_tick"),
        JsValue::from_str("k"),
        JsValue::from_f64(1.0),
        JsValue::from_str("two"),
    ] {
        args.push(&value);
    }
    emit.apply(&JsValue::NULL, &args).unwrap();
    // A key that is not text is written as its JSON.
    emit.call2(&JsValue::NULL, &"on_tick".into(), &JsValue::from_f64(3.0))
        .unwrap();
    // A value JSON cannot write is reported, and nothing is emitted.
    let cyclic = module("const o = {}; o.self = o; return o;");
    let bad = js_sys::Array::of3(&"on_tick".into(), &"k".into(), &cyclic);
    emit.apply(&JsValue::NULL, &bad).unwrap();
    assert_eq!(
        calls(),
        [
            (
                "on_tick".to_string(),
                "k".to_string(),
                vec![ScriptValue::I64(1), ScriptValue::Str("two".into())]
            ),
            ("on_tick".to_string(), "3".to_string(), Vec::new()),
        ]
    );

    let set = host_fn("setSignal");
    let get = host_fn("getSignal");
    let round_trip = |name: &str, value: JsValue| -> JsValue {
        set.call2(&JsValue::NULL, &name.into(), &value).unwrap();
        get.call1(&JsValue::NULL, &name.into()).unwrap()
    };
    assert_eq!(
        round_trip("s", "text".into()).as_string().as_deref(),
        Some("text")
    );
    assert_eq!(round_trip("b", JsValue::TRUE).as_bool(), Some(true));
    assert_eq!(round_trip("i", JsValue::from_f64(4.0)).as_f64(), Some(4.0));
    assert_eq!(round_trip("f", JsValue::from_f64(0.5)).as_f64(), Some(0.5));
    let object = module("return { a: 1 };");
    assert_eq!(
        round_trip("o", object).as_string().as_deref(),
        Some(r#"{"a":1}"#)
    );
    // A name that is not text writes nothing and reads nothing.
    set.call2(&JsValue::NULL, &JsValue::from_f64(1.0), &JsValue::TRUE)
        .unwrap();
    assert!(
        get.call1(&JsValue::NULL, &JsValue::from_f64(1.0))
            .unwrap()
            .is_undefined()
    );
    assert!(
        get.call1(&JsValue::NULL, &"unset".into())
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn get_signal_reads_what_the_app_wrote_as_of_the_last_tick() {
    let mut app = portable_app();
    let modules = js_sys::Array::of1(&module(ECHO_MODULE));
    install(&mut app, &manifest(&["echo"]), &[echo()], &modules).unwrap();
    {
        let mut store = app.world.resource_mut::<PropertyStore>();
        store.set(PropertyKey::global("count"), PropertyValue::I64(3));
        store.set(
            PropertyKey::global("tint"),
            PropertyValue::Color(Color {
                r: 1.0,
                g: 0.5,
                b: 0.0,
                a: 1.0,
            }),
        );
        store.set(
            PropertyKey::global("opaque"),
            PropertyValue::Custom(std::sync::Arc::new(())),
        );
    }
    // The first tick copies every signal; later ones copy what changed.
    app.tick();
    let get = host_fn("getSignal");
    let read = |name: &str| get.call1(&JsValue::NULL, &name.into()).unwrap();
    assert_eq!(read("count").as_f64(), Some(3.0));
    assert_eq!(
        read("tint").as_string().as_deref(),
        Some("rgba(255, 128, 0, 1)")
    );
    assert!(read("opaque").is_undefined());

    app.world
        .resource_mut::<PropertyStore>()
        .set(PropertyKey::global("count"), PropertyValue::I64(4));
    app.tick();
    assert_eq!(read("count").as_f64(), Some(4.0));
}
