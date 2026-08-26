//! Pins the encoding of the script surface.
//!
//! Every [`ScriptCommand`] and [`ScriptValue`] variant is built with fixed
//! field values, encoded, and compared against bytes checked in here. Appending
//! a variant leaves the earlier bytes alone and passes; inserting or reordering
//! one changes them and fails, which is the whole point: the encoding writes a
//! variant by its index, and a peer decoding an index it disagrees about reads
//! a different command with the same confidence.
//!
//! Regenerate the tables with
//! `cargo test -p lumen-script --test wire -- --ignored --nocapture`, and only
//! after deciding the change is intended and bumping
//! [`SCRIPT_WIRE_VERSION`].

use std::collections::HashMap;

use bevy_ecs::prelude::Entity;
use bincode::Options;
use lumen_core::components::Color;
use lumen_core::property_store::{PropertyKey, PropertyValue};
use lumen_script::{FileDialogKind, PluginEvent, SCRIPT_WIRE_VERSION, ScriptCommand, ScriptValue};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound on one decoded payload, matching the plugin boundary's codec.
const MAX_PAYLOAD: u64 = 512 * 1024 * 1024;

/// The encode half of the boundary codec: plain `bincode::serialize`.
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serialize(value).map_err(|e| e.to_string())
}

/// The decode half: `DefaultOptions` defaults to varint, so fixint has to be
/// asked for or it would mis-read what `encode` wrote.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PAYLOAD)
        .deserialize(bytes)
        .map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fixed entity, so the packed bits in the golden do not depend on a world.
fn entity() -> Entity {
    Entity::from_raw_u32(7).expect("7 is an entity index")
}

fn s(v: &str) -> String {
    v.to_string()
}

/// One sample per [`ScriptCommand`] variant, in declaration order, plus the
/// property shapes [`ScriptCommand::SetProperty`] carries.
fn command_samples() -> Vec<(&'static str, ScriptCommand)> {
    let mut item = HashMap::new();
    item.insert(s("f"), s("v"));
    vec![
        ("Print", ScriptCommand::Print(s("a"))),
        ("AddClicks", ScriptCommand::AddClicks(2)),
        (
            "SetString",
            ScriptCommand::SetString {
                key: s("k"),
                value: s("v"),
            },
        ),
        (
            "SetText",
            ScriptCommand::SetText {
                target_id: s("t"),
                text: s("x"),
            },
        ),
        (
            "SetSrc",
            ScriptCommand::SetSrc {
                target_id: s("t"),
                path: s("p"),
            },
        ),
        (
            "SetTimer",
            ScriptCommand::SetTimer {
                name: s("n"),
                millis: 5,
                repeat: true,
            },
        ),
        ("CancelTimer", ScriptCommand::CancelTimer { name: s("n") }),
        (
            "Fetch",
            ScriptCommand::Fetch {
                url: s("u"),
                tag: s("g"),
            },
        ),
        (
            "Http",
            ScriptCommand::Http {
                method: s("GET"),
                url: s("u"),
                headers: vec![(s("h"), s("v"))],
                body: Some(s("b")),
                timeout_ms: Some(9),
                tag: s("g"),
            },
        ),
        (
            "SetResponseStatus",
            ScriptCommand::SetResponseStatus { status: 404 },
        ),
        (
            "SetResponseHeader",
            ScriptCommand::SetResponseHeader {
                name: s("n"),
                value: s("v"),
            },
        ),
        ("Redirect", ScriptCommand::Redirect { location: s("/l") }),
        (
            "SetSignal",
            ScriptCommand::SetSignal {
                name: s("n"),
                value: s("v"),
            },
        ),
        (
            "SetProperty/global-str",
            ScriptCommand::SetProperty {
                key: PropertyKey::Global("n".into()),
                value: PropertyValue::Str("v".into()),
            },
        ),
        (
            "SetProperty/entity-bool",
            ScriptCommand::SetProperty {
                key: PropertyKey::Entity(entity(), "n".into()),
                value: PropertyValue::Bool(true),
            },
        ),
        (
            "SetProperty/i64",
            ScriptCommand::SetProperty {
                key: PropertyKey::Global("n".into()),
                value: PropertyValue::I64(-3),
            },
        ),
        (
            "SetProperty/f64",
            ScriptCommand::SetProperty {
                key: PropertyKey::Global("n".into()),
                value: PropertyValue::F64(0.5),
            },
        ),
        (
            "SetProperty/color",
            ScriptCommand::SetProperty {
                key: PropertyKey::Global("n".into()),
                value: PropertyValue::Color(Color::rgba(0.0, 0.25, 0.5, 1.0)),
            },
        ),
        (
            "SetProperty/vec2",
            ScriptCommand::SetProperty {
                key: PropertyKey::Global("n".into()),
                value: PropertyValue::Vec2(glam::Vec2::new(1.0, 2.0)),
            },
        ),
        (
            "SetArray",
            ScriptCommand::SetArray {
                name: s("n"),
                items: vec![item],
            },
        ),
        (
            "Notify",
            ScriptCommand::Notify {
                title: s("t"),
                body: s("b"),
            },
        ),
        (
            "NotifyEx",
            ScriptCommand::NotifyEx {
                id: s("i"),
                title: s("t"),
                body: s("b"),
                options: s("o"),
                actions: s("a"),
            },
        ),
        (
            "ClipboardWrite",
            ScriptCommand::ClipboardWrite { text: s("x") },
        ),
        (
            "ClipboardRead",
            ScriptCommand::ClipboardRead { tag: s("g") },
        ),
        ("OpenUrl", ScriptCommand::OpenUrl { url: s("u") }),
        ("OpenPath", ScriptCommand::OpenPath { path: s("p") }),
        ("RevealPath", ScriptCommand::RevealPath { path: s("p") }),
        (
            "KeepAwake",
            ScriptCommand::KeepAwake {
                name: s("n"),
                reason: s("r"),
            },
        ),
        ("AllowSleep", ScriptCommand::AllowSleep { name: s("n") }),
        (
            "CopyImageToClipboard",
            ScriptCommand::CopyImageToClipboard { path: s("p") },
        ),
        (
            "SaveClipboardImage",
            ScriptCommand::SaveClipboardImage { path: s("p") },
        ),
        (
            "RegisterTrayIcon",
            ScriptCommand::RegisterTrayIcon {
                id: s("i"),
                icon_path: s("p"),
                tooltip: Some(s("t")),
                menu: s("m"),
                template: false,
            },
        ),
        (
            "UnregisterTrayIcon",
            ScriptCommand::UnregisterTrayIcon { id: s("i") },
        ),
        (
            "SetClasses",
            ScriptCommand::SetClasses {
                target_id: s("t"),
                classes: s("c"),
            },
        ),
        (
            "SetColorScheme",
            ScriptCommand::SetColorScheme { name: s("dark") },
        ),
        (
            "OpenFileDialog",
            ScriptCommand::OpenFileDialog {
                kind: FileDialogKind::Save,
                tag: s("g"),
                filters: vec![(s("l"), vec![s("txt")])],
                default_name: Some(s("d")),
            },
        ),
        (
            "RegisterHotkey",
            ScriptCommand::RegisterHotkey {
                name: s("n"),
                accelerator: s("F11"),
            },
        ),
        (
            "UnregisterHotkey",
            ScriptCommand::UnregisterHotkey { name: s("n") },
        ),
        (
            "SetAttr",
            ScriptCommand::SetAttr {
                node: 1,
                name: s("n"),
                value: s("v"),
            },
        ),
        (
            "RemoveAttr",
            ScriptCommand::RemoveAttr {
                node: 1,
                name: s("n"),
            },
        ),
        (
            "SetNodeText",
            ScriptCommand::SetNodeText {
                node: 1,
                text: s("x"),
            },
        ),
        (
            "ClassAdd",
            ScriptCommand::ClassAdd {
                node: 1,
                class: s("c"),
            },
        ),
        (
            "ClassRemove",
            ScriptCommand::ClassRemove {
                node: 1,
                class: s("c"),
            },
        ),
        (
            "ClassToggle",
            ScriptCommand::ClassToggle {
                node: 1,
                class: s("c"),
            },
        ),
        (
            "SetStyleProp",
            ScriptCommand::SetStyleProp {
                node: 1,
                name: s("n"),
                value: s("v"),
            },
        ),
        (
            "RemoveStyleProp",
            ScriptCommand::RemoveStyleProp {
                node: 1,
                name: s("n"),
            },
        ),
        (
            "Spawn",
            ScriptCommand::Spawn {
                tag: s("div"),
                reserved: 2,
            },
        ),
        (
            "Insert",
            ScriptCommand::Insert {
                parent: 1,
                node: 2,
                before: 3,
            },
        ),
        ("ReplaceWith", ScriptCommand::ReplaceWith { old: 1, new: 2 }),
        ("RemoveNode", ScriptCommand::RemoveNode { node: 1 }),
        (
            "CloneNode",
            ScriptCommand::CloneNode {
                source: 1,
                reserved: 2,
            },
        ),
        (
            "SetInnerMarkup",
            ScriptCommand::SetInnerMarkup {
                node: 1,
                markup: s("<b/>"),
            },
        ),
        (
            "SpawnFragment",
            ScriptCommand::SpawnFragment {
                key: s("k"),
                args: vec![(s("a"), s("1"))],
                children: vec![(s("slot"), 3)],
                reserved: 2,
            },
        ),
        (
            "BindEvent",
            ScriptCommand::BindEvent {
                node: 1,
                event_type: s("click"),
                capture: true,
                token: 4,
            },
        ),
        ("UnbindEvent", ScriptCommand::UnbindEvent { token: 4 }),
        (
            "WindowSetTitle",
            ScriptCommand::WindowSetTitle { title: s("t") },
        ),
        (
            "WindowSetSize",
            ScriptCommand::WindowSetSize {
                width: 640.0,
                height: 480.0,
            },
        ),
    ]
}

/// One sample per [`ScriptValue`] variant, in declaration order.
fn value_samples() -> Vec<(&'static str, ScriptValue)> {
    let mut map = HashMap::new();
    map.insert(s("k"), ScriptValue::I64(1));
    vec![
        ("Unit", ScriptValue::Unit),
        ("Bool", ScriptValue::Bool(true)),
        ("I64", ScriptValue::I64(-3)),
        ("F64", ScriptValue::F64(0.5)),
        ("Str", ScriptValue::Str(s("a"))),
        (
            "Array",
            ScriptValue::Array(vec![ScriptValue::Unit, ScriptValue::Bool(false)]),
        ),
        ("Map", ScriptValue::Map(map)),
    ]
}

const COMMAND_GOLDEN: &[(&str, &str)] = &[
    ("Print", "00000000010000000000000061"),
    ("AddClicks", "0100000002000000"),
    ("SetString", "0200000001000000000000006b010000000000000076"),
    ("SetText", "03000000010000000000000074010000000000000078"),
    ("SetSrc", "04000000010000000000000074010000000000000070"),
    ("SetTimer", "0500000001000000000000006e050000000000000001"),
    ("CancelTimer", "0600000001000000000000006e"),
    ("Fetch", "07000000010000000000000075010000000000000067"),
    (
        "Http",
        "080000000300000000000000474554010000000000000075010000000000000001000000000000006801000000000000007601010000000000000062010900000000000000010000000000000067",
    ),
    ("SetResponseStatus", "090000009401"),
    (
        "SetResponseHeader",
        "0a00000001000000000000006e010000000000000076",
    ),
    ("Redirect", "0b00000002000000000000002f6c"),
    ("SetSignal", "0c00000001000000000000006e010000000000000076"),
    (
        "SetProperty/global-str",
        "0d0000000000000001000000000000006e03000000010000000000000076",
    ),
    (
        "SetProperty/entity-bool",
        "0d00000001000000f8ffffff0000000001000000000000006e0000000001",
    ),
    (
        "SetProperty/i64",
        "0d0000000000000001000000000000006e01000000fdffffffffffffff",
    ),
    (
        "SetProperty/f64",
        "0d0000000000000001000000000000006e02000000000000000000e03f",
    ),
    (
        "SetProperty/color",
        "0d0000000000000001000000000000006e04000000000000000000803e0000003f0000803f",
    ),
    (
        "SetProperty/vec2",
        "0d0000000000000001000000000000006e050000000000803f00000040",
    ),
    (
        "SetArray",
        "0e00000001000000000000006e01000000000000000100000000000000010000000000000066010000000000000076",
    ),
    ("Notify", "0f000000010000000000000074010000000000000062"),
    (
        "NotifyEx",
        "1000000001000000000000006901000000000000007401000000000000006201000000000000006f010000000000000061",
    ),
    ("ClipboardWrite", "11000000010000000000000078"),
    ("ClipboardRead", "12000000010000000000000067"),
    ("OpenUrl", "13000000010000000000000075"),
    ("OpenPath", "14000000010000000000000070"),
    ("RevealPath", "15000000010000000000000070"),
    ("KeepAwake", "1600000001000000000000006e010000000000000072"),
    ("AllowSleep", "1700000001000000000000006e"),
    ("CopyImageToClipboard", "18000000010000000000000070"),
    ("SaveClipboardImage", "19000000010000000000000070"),
    (
        "RegisterTrayIcon",
        "1a0000000100000000000000690100000000000000700101000000000000007401000000000000006d00",
    ),
    ("UnregisterTrayIcon", "1b000000010000000000000069"),
    ("SetClasses", "1c000000010000000000000074010000000000000063"),
    ("SetColorScheme", "1d00000004000000000000006461726b"),
    (
        "OpenFileDialog",
        "1e00000002000000010000000000000067010000000000000001000000000000006c0100000000000000030000000000000074787401010000000000000064",
    ),
    (
        "RegisterHotkey",
        "1f00000001000000000000006e0300000000000000463131",
    ),
    ("UnregisterHotkey", "2000000001000000000000006e"),
    (
        "SetAttr",
        "21000000010000000000000001000000000000006e010000000000000076",
    ),
    ("RemoveAttr", "22000000010000000000000001000000000000006e"),
    ("SetNodeText", "230000000100000000000000010000000000000078"),
    ("ClassAdd", "240000000100000000000000010000000000000063"),
    ("ClassRemove", "250000000100000000000000010000000000000063"),
    ("ClassToggle", "260000000100000000000000010000000000000063"),
    (
        "SetStyleProp",
        "27000000010000000000000001000000000000006e010000000000000076",
    ),
    (
        "RemoveStyleProp",
        "28000000010000000000000001000000000000006e",
    ),
    ("Spawn", "2900000003000000000000006469760200000000000000"),
    (
        "Insert",
        "2a000000010000000000000002000000000000000300000000000000",
    ),
    ("ReplaceWith", "2b00000001000000000000000200000000000000"),
    ("RemoveNode", "2c0000000100000000000000"),
    ("CloneNode", "2d00000001000000000000000200000000000000"),
    (
        "SetInnerMarkup",
        "2e000000010000000000000004000000000000003c622f3e",
    ),
    (
        "SpawnFragment",
        "2f00000001000000000000006b010000000000000001000000000000006101000000000000003101000000000000000400000000000000736c6f7403000000000000000200000000000000",
    ),
    (
        "BindEvent",
        "3000000001000000000000000500000000000000636c69636b010400000000000000",
    ),
    ("UnbindEvent", "310000000400000000000000"),
    ("WindowSetTitle", "32000000010000000000000074"),
    ("WindowSetSize", "33000000000020440000f043"),
];

const VALUE_GOLDEN: &[(&str, &str)] = &[
    ("Unit", "00000000"),
    ("Bool", "0100000001"),
    ("I64", "02000000fdffffffffffffff"),
    ("F64", "03000000000000000000e03f"),
    ("Str", "04000000010000000000000061"),
    ("Array", "050000000200000000000000000000000100000000"),
    (
        "Map",
        "06000000010000000000000001000000000000006b020000000100000000000000",
    ),
];

/// One sample per [`PluginEvent`] variant, in declaration order.
fn event_samples() -> Vec<(&'static str, PluginEvent)> {
    vec![
        (
            "Call",
            PluginEvent::Call {
                event: s("on_gpio"),
                key: s("pin4"),
                fallback: s("on_any"),
                args: vec![ScriptValue::I64(-7)],
            },
        ),
        (
            "Commands",
            PluginEvent::Commands(vec![ScriptCommand::Print(s("a"))]),
        ),
    ]
}

const EVENT_GOLDEN: &[(&str, &str)] = &[
    (
        "Call",
        "0000000007000000000000006f6e5f6770696f040000000000000070696e3406000000000000006f6e5f616e79010000000000000002000000f9ffffffffffffff",
    ),
    (
        "Commands",
        "01000000010000000000000000000000010000000000000061",
    ),
];

#[test]
fn wire_version_is_pinned() {
    assert_eq!(SCRIPT_WIRE_VERSION, 2);
}

#[test]
fn plugin_event_encoding_is_pinned() {
    let samples = event_samples();
    assert_eq!(
        samples.len(),
        EVENT_GOLDEN.len(),
        "every event variant needs a golden; regenerate with --ignored"
    );
    for ((name, event), (golden_name, golden)) in samples.iter().zip(EVENT_GOLDEN) {
        assert_eq!(name, golden_name, "sample order drifted from the golden");
        let bytes = encode(event).expect("sample encodes");
        assert_eq!(&hex(&bytes), golden, "encoding of {name} changed");
    }
}

#[test]
fn plugin_events_round_trip() {
    for (name, event) in event_samples() {
        let bytes = encode(&event).expect("sample encodes");
        let back: PluginEvent = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format!("{event:?}"), format!("{back:?}"), "{name} changed");
    }
}

#[test]
fn command_encoding_is_pinned() {
    let samples = command_samples();
    assert_eq!(
        samples.len(),
        COMMAND_GOLDEN.len(),
        "every command variant needs a golden; regenerate with --ignored"
    );
    for ((name, cmd), (golden_name, golden)) in samples.iter().zip(COMMAND_GOLDEN) {
        assert_eq!(name, golden_name, "sample order drifted from the golden");
        let bytes = encode(cmd).expect("sample encodes");
        assert_eq!(&hex(&bytes), golden, "encoding of {name} changed");
    }
}

#[test]
fn commands_round_trip() {
    for (name, cmd) in command_samples() {
        let bytes = encode(&cmd).expect("sample encodes");
        let back: ScriptCommand = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format!("{cmd:?}"), format!("{back:?}"), "{name} changed");
    }
}

#[test]
fn value_encoding_is_pinned() {
    let samples = value_samples();
    assert_eq!(
        samples.len(),
        VALUE_GOLDEN.len(),
        "every value variant needs a golden; regenerate with --ignored"
    );
    for ((name, value), (golden_name, golden)) in samples.iter().zip(VALUE_GOLDEN) {
        assert_eq!(name, golden_name, "sample order drifted from the golden");
        let bytes = encode(value).expect("sample encodes");
        assert_eq!(&hex(&bytes), golden, "encoding of {name} changed");
    }
}

#[test]
fn values_round_trip() {
    for (name, value) in value_samples() {
        let bytes = encode(&value).expect("sample encodes");
        let back: ScriptValue = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(value, back, "{name} changed");
    }
}

#[test]
fn custom_property_values_refuse_to_encode() {
    let cmd = ScriptCommand::SetProperty {
        key: PropertyKey::Global("n".into()),
        value: PropertyValue::Custom(std::sync::Arc::new(7u8)),
    };
    let err = encode(&cmd).expect_err("Custom has no encoding");
    assert!(
        err.contains("PropertyValue::Custom cannot cross the plugin boundary"),
        "unhelpful error: {err}"
    );
}

#[test]
fn an_impossible_entity_is_refused() {
    // A zero index is the one bit pattern no entity's packed form has.
    let bytes = encode(&ScriptCommand::SetProperty {
        key: PropertyKey::Entity(entity(), "n".into()),
        value: PropertyValue::Bool(true),
    })
    .expect("sample encodes");
    let mut broken = bytes.clone();
    let at = broken
        .windows(8)
        .position(|w| w == entity().to_bits().to_le_bytes())
        .expect("the entity bits are in there");
    broken[at..at + 8].copy_from_slice(&0u64.to_le_bytes());
    let err = decode::<ScriptCommand>(&broken).expect_err("not an entity");
    assert!(err.contains("not an entity id"), "unhelpful error: {err}");
}

/// The in-process push wakes a parked event loop, exactly like a dlopened
/// plugin's push does: an engine-locked module delivering `on_audio_end`
/// while the app idles in `Wait` must trigger the tick that drains it, not
/// wait for an unrelated wake.
#[test]
fn an_in_process_push_wakes_a_parked_loop() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    lumen_core::plugin_events::discard_plugin_events();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    lumen_core::plugin_events::set_plugin_event_waker(lumen_core::app::EventLoopWaker(Arc::new(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
        },
    )));

    let event = PluginEvent::Call {
        event: "on_audio_end".into(),
        key: "player".into(),
        fallback: String::new(),
        args: Vec::new(),
    };
    assert!(lumen_script::push_plugin_event(&event));
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "the push must nudge the installed waker"
    );
    assert!(
        lumen_core::plugin_events::plugin_events_pending(),
        "the event must be on the bus for the woken tick to drain"
    );
    lumen_core::plugin_events::discard_plugin_events();
}

/// Prints the golden tables. Run with `--ignored --nocapture` and paste the
/// output over the constants above.
#[test]
#[ignore = "regenerates the golden tables"]
fn print_goldens() {
    println!("const COMMAND_GOLDEN: &[(&str, &str)] = &[");
    for (name, cmd) in command_samples() {
        println!("    (\"{name}\", \"{}\"),", hex(&encode(&cmd).unwrap()));
    }
    println!("];");
    println!("const VALUE_GOLDEN: &[(&str, &str)] = &[");
    for (name, value) in value_samples() {
        println!("    (\"{name}\", \"{}\"),", hex(&encode(&value).unwrap()));
    }
    println!("];");
    println!("const EVENT_GOLDEN: &[(&str, &str)] = &[");
    for (name, event) in event_samples() {
        println!("    (\"{name}\", \"{}\"),", hex(&encode(&event).unwrap()));
    }
    println!("];");
}
