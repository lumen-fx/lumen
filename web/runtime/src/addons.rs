//! Browser add-ons: the modules a page loads beside the runtime, bound to the
//! functions and elements the compiled app describes.
//!
//! The document imports every add-on's module and hands the modules to
//! [`crate::boot`] in the order the manifest lists the add-ons. For each one
//! the runtime calls its `install(host)` export, if it has one, then binds
//! every function the add-on describes to the export of the same name, and
//! every element it answers for to the hooks under its `elements` export.
//!
//! Values cross as JSON: what a script passes arrives as plain JavaScript
//! values, and what the module returns comes back the same way. A function
//! declared `async` returns a promise; the script's call returns at once, and
//! the settled value arrives as the event the add-on names.
//!
//! `install` is called with two arguments: the `host` object, and the
//! `config` table the app's dependency entry gave the add-on, as an object.
//! The `host` is how the module reaches the app outside a call: `emit(event, key, ...values)` calls a script handler with
//! up to six values after the key, `setSignal(name, value)` writes a signal,
//! and `getSignal(name)` reads one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use js_sys::{Array, Function, Object, Promise, Reflect};
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::{
    PropertyKey, PropertyStore, PropertyValue, push_external_property,
};
use lumen_html::contract::Manifest;
use lumen_ir::addon::Addon;
use lumen_script::addon::{error_event, script_fns};
use lumen_script::text_parse::{parse_json, script_value_to_json};
use lumen_script::{PluginEvent, ScriptFnAppExt, ScriptResult, ScriptValue, push_plugin_event};
use lumen_web_dom::{ForeignElements, ForeignHooks, ForeignTag};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// A function a script calls, bound to the module export that answers it.
struct Bound {
    /// `namespace::name`, for what a failure says.
    qualified: String,
    /// The export, or `None` when the module has none of that name.
    export: Option<Function>,
    /// The event an asynchronous function's result arrives as.
    event: Option<String>,
}

thread_local! {
    /// Every bound function, by the index its script function body carries.
    /// A body has to be `Send + Sync`, which a JavaScript function is not, so
    /// the body holds the index and the function stays here, on the one
    /// thread a page has.
    static BOUND: RefCell<Vec<Bound>> = const { RefCell::new(Vec::new()) };

    /// The app's global signals as of the last tick, for `host.getSignal`.
    /// A module runs while the app is mid-tick or between ticks, and in
    /// neither case can it reach the world, so it reads this copy.
    static SIGNALS: RefCell<HashMap<String, PropertyValue>> = RefCell::new(HashMap::new());
}

/// Why the page's add-ons could not be put together.
#[derive(Debug)]
pub(crate) struct AddonError(String);

impl std::fmt::Display for AddonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Install the page's add-ons into `app`, ahead of its script hosts, and
/// return the elements they answer for.
///
/// `modules` is what the document handed [`crate::boot`]: an array of module
/// namespace objects in manifest order, or nothing for a site with no add-on.
///
/// # Errors
///
/// The page handed over a different number of modules than the manifest
/// names, or the manifest names an add-on the compiled app does not describe.
/// Either means the document and the build disagree about the site.
pub(crate) fn install(
    app: &mut App,
    manifest: &Manifest,
    described: &[Addon],
    modules: &JsValue,
) -> Result<ForeignElements, AddonError> {
    let modules: Array = if modules.is_undefined() || modules.is_null() {
        Array::new()
    } else {
        modules
            .clone()
            .dyn_into()
            .map_err(|_| AddonError("the page handed over add-ons that are not a list".into()))?
    };
    if modules.length() as usize != manifest.addons.len() {
        return Err(AddonError(format!(
            "the page loaded {} add-on modules and the manifest names {}",
            modules.length(),
            manifest.addons.len()
        )));
    }

    let mut foreign = ForeignElements::new();
    for (tag, element) in &manifest.foreign {
        foreign.insert(
            tag.clone(),
            ForeignTag {
                html: element.html.clone(),
                void: element.void,
                hooks: ForeignHooks::default(),
            },
        );
    }
    if manifest.addons.is_empty() {
        return Ok(foreign);
    }

    let host = host_object();
    for (index, entry) in manifest.addons.iter().enumerate() {
        let module = modules.get(index as u32);
        let addon = described
            .iter()
            .find(|addon| addon.name == entry.name)
            .ok_or_else(|| {
                AddonError(format!(
                    "the manifest names the add-on '{}', which the compiled app does not describe",
                    entry.name
                ))
            })?;
        if let Some(install) = export(&module, "install") {
            // The `config` table the app's dependency entry gave the add-on,
            // as an object; an empty one when it gave none.
            let config = entry
                .config
                .as_deref()
                .and_then(|text| js_sys::JSON::parse(text).ok())
                .unwrap_or_else(|| Object::new().into());
            if let Err(error) = install.call2(&JsValue::NULL, &host, &config) {
                report(
                    &format!("the '{}' add-on's install threw", addon.name),
                    &error,
                );
            }
        }
        let fns = script_fns(addon, |_, function| {
            let slot = BOUND.with(|bound| {
                let mut bound = bound.borrow_mut();
                bound.push(Bound {
                    qualified: format!("{}::{}", addon.namespace, function.name),
                    export: export(&module, &function.name),
                    event: function.event.clone(),
                });
                bound.len() - 1
            });
            Arc::new(move |cx| call(slot, cx.args()))
        })
        .map_err(AddonError)?;
        app.add_script_fns(fns);

        let hooks = Reflect::get(&module, &JsValue::from_str("elements")).unwrap_or_default();
        for element in &addon.elements {
            let entry = Reflect::get(&hooks, &JsValue::from_str(&element.tag)).unwrap_or_default();
            foreign.insert(
                element.tag.clone(),
                ForeignTag {
                    html: element.html.clone(),
                    void: element.void,
                    hooks: ForeignHooks::from_object(&entry),
                },
            );
        }
    }

    // What `getSignal` reads, kept after every tick that changed a signal,
    // before the end of the tick clears what changed.
    app.add_systems(TickStage::LayoutSync, mirror_signals);
    Ok(foreign)
}

/// The export `name` of `module`, when it is a function.
fn export(module: &JsValue, name: &str) -> Option<Function> {
    Reflect::get(module, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.dyn_into().ok())
}

/// Call the function bound at `slot` with what the script passed.
fn call(slot: usize, args: &[ScriptValue]) -> ScriptResult {
    // The borrow ends before the module runs: a module that calls back into
    // the host must not find the table held.
    let (qualified, export, event) = BOUND.with(|bound| {
        let bound = bound.borrow();
        let entry = &bound[slot];
        (
            entry.qualified.clone(),
            entry.export.clone(),
            entry.event.clone(),
        )
    });
    let Some(export) = export else {
        return Err(format!(
            "{qualified}: the add-on's module exports no such function"
        ));
    };
    match event {
        None => call_sync(&qualified, &export, args),
        Some(event) => {
            call_async(&export, args, event);
            Ok(ScriptValue::Unit)
        }
    }
}

/// A function that answers before it returns.
fn call_sync(qualified: &str, export: &Function, args: &[ScriptValue]) -> ScriptResult {
    let returned = export
        .apply(&JsValue::NULL, &to_js_args(args))
        .map_err(|error| format!("{qualified}: {}", reason(&error)))?;
    if returned.is_instance_of::<Promise>() {
        return Err(format!(
            "{qualified} returned a promise; declare it with `async` in its lumen-addon.toml"
        ));
    }
    to_script(&returned).map_err(|why| format!("{qualified}: {why}"))
}

/// A function whose result arrives as an event: the script's last argument is
/// the tag it arrives with, and the rest are the function's own.
fn call_async(export: &Function, args: &[ScriptValue], event: String) {
    let (tag, args) = match args.split_last() {
        Some((ScriptValue::Str(tag), rest)) => (tag.clone(), rest),
        Some((tag, rest)) => (tag.stringify(), rest),
        None => (String::new(), args),
    };
    let settled = match export.apply(&JsValue::NULL, &to_js_args(args)) {
        Ok(returned) => Promise::resolve(&returned),
        Err(error) => Promise::reject(&error),
    };
    wasm_bindgen_futures::spawn_local(async move {
        let settled = wasm_bindgen_futures::JsFuture::from(settled)
            .await
            .map_err(|error| reason(&error))
            .and_then(|value| to_script(&value));
        let (event, value) = match settled {
            Ok(value) => (event, value),
            Err(why) => (error_event(&event), ScriptValue::Str(why)),
        };
        emit(event, tag, vec![value]);
    });
}

/// Call the script's handler for `event`, with `key` first.
fn emit(event: String, key: String, args: Vec<ScriptValue>) {
    push_plugin_event(PluginEvent::Call {
        fallback: event.clone(),
        event,
        key,
        args,
    });
}

/// The arguments a script passed, as the JavaScript values they stand for.
fn to_js_args(args: &[ScriptValue]) -> Array {
    js_sys::JSON::parse(&script_value_to_json(&ScriptValue::Array(args.to_vec())))
        .ok()
        .and_then(|value| value.dyn_into().ok())
        .unwrap_or_default()
}

/// A JavaScript value as a script sees it, read the way JSON writes it.
/// `undefined` and a function arrive as nothing.
///
/// # Errors
///
/// JSON cannot write the value: it contains itself, or holds a `BigInt`.
fn to_script(value: &JsValue) -> Result<ScriptValue, String> {
    if value.is_undefined() {
        return Ok(ScriptValue::Unit);
    }
    let text = js_sys::JSON::stringify(value).map_err(|error| reason(&error))?;
    Ok(text
        .as_string()
        .map(|text| parse_json(&text))
        .unwrap_or(ScriptValue::Unit))
}

/// What a JavaScript exception or rejection says.
fn reason(error: &JsValue) -> String {
    error
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

/// Report a module's failure without stopping the app.
fn report(what: &str, error: &JsValue) {
    web_sys::console::error_2(&JsValue::from_str(&format!("lumen: {what}")), error);
}

/// The object every add-on is installed with.
fn host_object() -> JsValue {
    let host = Object::new();
    let set = |name: &str, function: &JsValue| {
        let _ = Reflect::set(&host, &JsValue::from_str(name), function);
    };
    // `emit(event, key, ...values)`. A closure the page calls has a fixed
    // arity, so it takes the most a handler is written with; a value the
    // caller left off arrives as `undefined` and is not passed on.
    type Emit = dyn Fn(JsValue, JsValue, JsValue, JsValue, JsValue, JsValue, JsValue, JsValue);
    let emit_fn = Closure::<Emit>::new(
        |event: JsValue,
         key: JsValue,
         a: JsValue,
         b: JsValue,
         c: JsValue,
         d: JsValue,
         e: JsValue,
         f: JsValue| {
            let mut values = vec![a, b, c, d, e, f];
            while values.last().is_some_and(JsValue::is_undefined) {
                values.pop();
            }
            let event = event.as_string().unwrap_or_default();
            let read = values
                .iter()
                .map(to_script)
                .collect::<Result<Vec<_>, _>>()
                .and_then(|values| {
                    let key = match key.as_string() {
                        Some(key) => key,
                        None => to_script(&key)?.stringify(),
                    };
                    Ok((key, values))
                });
            match read {
                Ok((key, values)) => emit(event, key, values),
                Err(why) => report(
                    &format!("host.emit(\"{event}\") was handed a value JSON cannot write"),
                    &JsValue::from_str(&why),
                ),
            }
        },
    );
    set("emit", emit_fn.as_ref());
    let set_signal = Closure::<dyn Fn(JsValue, JsValue)>::new(|name: JsValue, value: JsValue| {
        let Some(name) = name.as_string() else {
            return;
        };
        let value = to_property(&value);
        SIGNALS.with(|signals| {
            signals.borrow_mut().insert(name.clone(), value.clone());
        });
        push_external_property(PropertyKey::global(name.as_str()), value);
    });
    set("setSignal", set_signal.as_ref());
    let get_signal = Closure::<dyn Fn(JsValue) -> JsValue>::new(|name: JsValue| {
        let Some(name) = name.as_string() else {
            return JsValue::UNDEFINED;
        };
        SIGNALS.with(|signals| {
            signals
                .borrow()
                .get(&name)
                .map(from_property)
                .unwrap_or(JsValue::UNDEFINED)
        })
    });
    set("getSignal", get_signal.as_ref());
    // The page keeps the host for as long as it is open.
    emit_fn.forget();
    set_signal.forget();
    get_signal.forget();
    host.into()
}

/// A JavaScript value as a signal holds it: text, a whole number, a number,
/// a boolean, or anything else as its JSON text.
fn to_property(value: &JsValue) -> PropertyValue {
    if let Some(text) = value.as_string() {
        return PropertyValue::Str(text.into());
    }
    if let Some(flag) = value.as_bool() {
        return PropertyValue::Bool(flag);
    }
    if let Some(number) = value.as_f64() {
        let whole = number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0;
        return if whole {
            PropertyValue::I64(number as i64)
        } else {
            PropertyValue::F64(number)
        };
    }
    let text = js_sys::JSON::stringify(value)
        .ok()
        .and_then(|text| text.as_string())
        .unwrap_or_default();
    PropertyValue::Str(text.into())
}

/// A signal's value as a JavaScript value.
fn from_property(value: &PropertyValue) -> JsValue {
    match value {
        PropertyValue::Bool(flag) => JsValue::from_bool(*flag),
        PropertyValue::I64(number) => JsValue::from_f64(*number as f64),
        PropertyValue::F64(number) => JsValue::from_f64(*number),
        PropertyValue::Str(text) => JsValue::from_str(text),
        PropertyValue::Color(c) => JsValue::from_str(&format!(
            "rgba({}, {}, {}, {})",
            (c.r * 255.0).round(),
            (c.g * 255.0).round(),
            (c.b * 255.0).round(),
            c.a
        )),
        PropertyValue::Vec2(v) => Array::of2(&v.x.into(), &v.y.into()).into(),
        PropertyValue::Custom(_) => JsValue::UNDEFINED,
    }
}

/// Keep [`SIGNALS`] in step with the store: everything on the first tick, and
/// what changed on every tick after it.
fn mirror_signals(store: Res<PropertyStore>, mut filled: Local<bool>) {
    SIGNALS.with(|signals| {
        let mut signals = signals.borrow_mut();
        let mut copy = |key: &PropertyKey| {
            if let (PropertyKey::Global(name), Some(value)) = (key, store.get(key)) {
                signals.insert(name.to_string(), value.clone());
            }
        };
        if *filled {
            for key in store.dirty_peek() {
                copy(key);
            }
        } else {
            *filled = true;
            let keys: Vec<PropertyKey> = store.iter().map(|(key, _)| key.clone()).collect();
            for key in &keys {
                copy(key);
            }
        }
    });
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests;
