//! Phase-4 lua event bindings: `n:on(type, handler)` delivers a real event
//! object to a Lua closure, and the returned `off()` unbinds. Drives the same
//! path the runtime does, headlessly.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_script::event::{self, EventData};
use lumen_script::node_query::drain_external_dom_commands;
use lumen_script::{ScriptCommand, ScriptHost};
use lumen_script_lua::LuaHost;

use bevy_ecs::world::World;

fn publish_fixture() -> u64 {
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    let rec = |entity, tag: &str, id: Option<&str>, parent, children: &[_]| DomRecord {
        entity,
        generation: 0,
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: vec![],
        parent,
        children: children.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    };
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), None, &[btn]),
        rec(btn, "button", Some("btn"), Some(root), &[]),
    ]));
    NodeHandle::new(btn).pack()
}

fn apply_bind(cmds: &[ScriptCommand]) -> u64 {
    for c in cmds {
        if let ScriptCommand::BindEvent {
            node,
            event_type,
            capture,
            token,
        } = c
        {
            event::register_host_binding(*token, *node, event_type.clone(), *capture);
            return *token;
        }
    }
    panic!("no BindEvent emitted");
}

// Both scenarios share the process-global external DOM bus, so they run as
// one test to avoid a cross-test drain race.
#[test]
fn lua_events_receive_and_unbind() {
    click_handler_receives_event();
    off_unbinds();
}

fn click_handler_receives_event() {
    event::clear_all_bindings();
    let _ = drain_external_dom_commands();
    let btn = publish_fixture();

    let src = r#"
        function setup()
            local b = get_by_id("btn")
            b:on("click", function(ev)
                print("target=" .. ev:target():handle() .. " key=" .. ev:key() .. " btn=" .. ev:button())
            end)
        end
    "#;
    let mut host = LuaHost::new();
    host.load(src).expect("compiles");
    host.call("setup", &[]).expect("setup runs");
    apply_bind(&drain_external_dom_commands());

    let data = EventData {
        event_type: "click".into(),
        target: btn,
        button: 0,
        ..Default::default()
    };
    event::dispatch(data, &[], true, |token| {
        host.dispatch_event_handler(token).expect("handler runs");
    });

    let prints: Vec<String> = host
        .drain_commands()
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(prints, vec![format!("target={} key= btn=0", btn as i64)]);
    event::clear_all_bindings();
}

fn off_unbinds() {
    event::clear_all_bindings();
    let _ = drain_external_dom_commands();
    let btn = publish_fixture();

    // Bind, then call the returned off() immediately.
    let src = r#"
        function bind_and_off()
            local b = get_by_id("btn")
            local off = b:on("click", function(ev) print("hit") end)
            off()
        end
    "#;
    let mut host = LuaHost::new();
    host.load(src).expect("compiles");
    host.call("bind_and_off", &[]).expect("runs");
    let cmds = drain_external_dom_commands();
    let mut bind_token = None;
    let mut unbind_token = None;
    for c in &cmds {
        match c {
            ScriptCommand::BindEvent { token, .. } => bind_token = Some(*token),
            ScriptCommand::UnbindEvent { token } => unbind_token = Some(*token),
            _ => {}
        }
    }
    assert!(bind_token.is_some(), "on(...) emits BindEvent");
    assert_eq!(bind_token, unbind_token, "off() emits UnbindEvent");

    // With both applied, no live binding remains.
    let token = bind_token.unwrap();
    event::register_host_binding(token, btn, "click".into(), false);
    event::unregister_binding(token);
    host.drop_event_handler(token);
    let data = EventData {
        event_type: "click".into(),
        target: btn,
        ..Default::default()
    };
    event::dispatch(data, &[], true, |t| {
        host.dispatch_event_handler(t).ok();
    });
    let hits = host
        .drain_commands()
        .into_iter()
        .filter(|c| matches!(c, ScriptCommand::Print(_)))
        .count();
    assert_eq!(hits, 0);
    event::clear_all_bindings();
}
