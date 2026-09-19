//! Lifecycle-callback dispatch on [`CandelaHost`].
//!
//! Every lifecycle callback the runtime fires reaches the host through the one
//! host-neutral [`ScriptHost::call`] path: the generic `dispatch_*::<H>` systems
//! in `lumen-script` / `lumen-runtime` build the argument list and call the
//! handler by name, identically for every host. So the only candela-specific
//! risk is whether [`CandelaHost::call`] marshals each argument SHAPE the
//! dispatchers use into a candela function call. The distinct shapes are:
//!
//! | shape          | callbacks using it                                        |
//! |----------------|-----------------------------------------------------------|
//! | `[]`           | `on_start`                                                |
//! | `[Str]`        | `on_click`, `on_long_press`, `on_hotkey`                   |
//! | `[Str, Str]`   | `on_text_input`, `on_file_picked`, `on_file_dropped`,     |
//! |                | `on_drop`, `on_fetch`, `on_fetch_error`                    |
//! | `[Str, F64]`   | `on_slider`                                               |
//! | `[Str, Bool]`  | `on_toggle`                                               |
//!
//! Each test drives a callback with the exact `ScriptValue` argument types the
//! matching dispatcher passes (see `lumen_script::runtime::route_event*`)
//! and asserts the handler ran
//! and its arguments crossed the boundary intact.
//!
//! Note: Lumen's runtime is reactive-only; there is no per-frame `on_tick`
//! dispatcher for any host. The `dispatches_per_frame_float_arg` test proves the
//! host would dispatch such a float-argument callback if one were wired, but
//! nothing in the runtime calls it today.

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// Load one candela program that opts into the whole builtin surface via the
/// prelude import, then defines every lifecycle handler the tests exercise.
fn host_with_handlers() -> CandelaHost {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn on_start()                { lumen::signal_set("fired_start", "yes"); }

fn on_click(id)              { lumen::signal_set("clicked_id", id); }
fn on_long_press(id)         { lumen::signal_set("long_pressed_id", id); }
fn on_hotkey(name)           { lumen::signal_set("hotkey_name", name); }

fn on_text_input(id, text)   { lumen::signal_set("text_value", text); }
fn on_file_picked(tag, path) { lumen::signal_set("picked_path", path); }
fn on_file_dropped(id, path) { lumen::signal_set("dropped_path", path); }
fn on_drop(target, payload)  { lumen::signal_set("drop_payload", payload); }
fn on_fetch(tag, body)       { lumen::signal_set("fetch_body", body); }
fn on_fetch_error(tag, msg)  { lumen::signal_set("fetch_error", msg); }

fn on_slider(id, value)      { lumen::signal_set_float("slider_value", value); }
fn on_toggle(id, checked)    { lumen::signal_set_bool("toggle_checked", checked); }

fn on_tick(dt)               { lumen::signal_set_float("tick_dt", dt); }

fn main() {}
"#;
    host.load(src, "lifecycle.cdl")
        .unwrap_or_else(|e| panic!("lifecycle script should compile: {e}"));
    host
}

/// Assert `fn_name(args)` dispatched and queued a `SetSignal { name, value }`.
fn assert_str_signal(
    host: &mut CandelaHost,
    fn_name: &str,
    args: &[ScriptValue],
    name: &str,
    value: &str,
) {
    let outcome = host
        .call(fn_name, args)
        .unwrap_or_else(|e| panic!("{fn_name} dispatch failed: {e}"));
    assert!(outcome.found, "{fn_name} exists so found must be true");
    assert!(
        outcome.commands.iter().any(|c| matches!(
            c,
            ScriptCommand::SetSignal { name: n, value: v } if n == name && v == value
        )),
        "{fn_name} must queue SetSignal {{ {name} = {value} }}; got {:?}",
        outcome.commands
    );
}

#[test]
fn zero_arg_callbacks_dispatch() {
    let mut host = host_with_handlers();
    assert_str_signal(&mut host, "on_start", &[], "fired_start", "yes");
}

#[test]
fn single_string_arg_callbacks_dispatch() {
    let mut host = host_with_handlers();
    let id = |s: &str| vec![ScriptValue::Str(s.to_owned())];
    assert_str_signal(&mut host, "on_click", &id("save"), "clicked_id", "save");
    assert_str_signal(
        &mut host,
        "on_long_press",
        &id("row-3"),
        "long_pressed_id",
        "row-3",
    );
    assert_str_signal(
        &mut host,
        "on_hotkey",
        &id("CmdOrCtrl+S"),
        "hotkey_name",
        "CmdOrCtrl+S",
    );
}

#[test]
fn two_string_arg_callbacks_dispatch() {
    let mut host = host_with_handlers();
    let two = |a: &str, b: &str| {
        vec![
            ScriptValue::Str(a.to_owned()),
            ScriptValue::Str(b.to_owned()),
        ]
    };
    assert_str_signal(
        &mut host,
        "on_text_input",
        &two("field", "hello world"),
        "text_value",
        "hello world",
    );
    assert_str_signal(
        &mut host,
        "on_file_picked",
        &two("open", "/tmp/a.txt"),
        "picked_path",
        "/tmp/a.txt",
    );
    assert_str_signal(
        &mut host,
        "on_file_dropped",
        &two("zone", "/tmp/b.png"),
        "dropped_path",
        "/tmp/b.png",
    );
    assert_str_signal(
        &mut host,
        "on_drop",
        &two("bin", "item-7"),
        "drop_payload",
        "item-7",
    );
    assert_str_signal(
        &mut host,
        "on_fetch",
        &two("weather", "{\"ok\":1}"),
        "fetch_body",
        "{\"ok\":1}",
    );
    assert_str_signal(
        &mut host,
        "on_fetch_error",
        &two("weather", "timeout"),
        "fetch_error",
        "timeout",
    );
}

#[test]
fn slider_float_arg_dispatches_and_marshals() {
    let mut host = host_with_handlers();
    // route_event_id_f64 passes [Str(id), F64(value)].
    let outcome = host
        .call(
            "on_slider",
            &[ScriptValue::Str("vol".to_owned()), ScriptValue::F64(0.75)],
        )
        .expect("on_slider dispatch ok");
    assert!(outcome.found);
    // signal_set_float writes the typed mirror entry: the f64 crossed intact.
    assert_eq!(
        host.mirror_get("slider_value"),
        Some(ScriptValue::F64(0.75))
    );
}

#[test]
fn toggle_bool_arg_dispatches_and_marshals() {
    let mut host = host_with_handlers();
    // route_event_id_bool passes [Str(id), Bool(value)].
    let outcome = host
        .call(
            "on_toggle",
            &[ScriptValue::Str("dark".to_owned()), ScriptValue::Bool(true)],
        )
        .expect("on_toggle dispatch ok");
    assert!(outcome.found);
    assert_eq!(
        host.mirror_get("toggle_checked"),
        Some(ScriptValue::Bool(true))
    );
}

/// The host dispatches a per-frame-style float-argument callback (`on_tick(dt)`)
/// correctly. Lumen's runtime is reactive-only and does not fire `on_tick`, so
/// this proves host-boundary capability, not that any tick loop calls it.
#[test]
fn dispatches_per_frame_float_arg() {
    let mut host = host_with_handlers();
    let outcome = host
        .call("on_tick", &[ScriptValue::F64(16.0)])
        .expect("on_tick dispatch ok");
    assert!(outcome.found);
    assert_eq!(host.mirror_get("tick_dt"), Some(ScriptValue::F64(16.0)));
}
