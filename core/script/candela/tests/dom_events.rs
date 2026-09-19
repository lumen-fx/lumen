//! Phase-4 candela event bindings are PROCEDURAL: `lumen::event_on(node,
//! type, handler_name)` returns a token, the handler is a named function that
//! reads the event through `lumen::event_*(ev)` free calls, and
//! `lumen::event_off(token)` unbinds. The pinned candela dep predates
//! user-struct methods, so there is no `ev.target()` sugar.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_script::event::{self, EventData};
use lumen_script::{ScriptCommand, ScriptHost};
use lumen_script_candela::CandelaHost;

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

const SRC: &str = r##"
host "lumen" {
    int node_get_by_id(string);
    int event_on(int, string, string);
    event_off(int);
    int event_target(int);
    string event_key(int);
    node_set_text(int, string);
}
fn setup() {
    let b = lumen::node_get_by_id("btn");
    lumen::event_on(b, "click", "on_click");
}
fn on_click(ev) {
    let t = lumen::event_target(ev);
    let k = lumen::event_key(ev);
    lumen::node_set_text(t, k);
}
fn main() {}
"##;

#[test]
fn candela_procedural_event_fires() {
    event::clear_all_bindings();
    let btn = publish_fixture();

    let mut host = CandelaHost::new();
    host.load(SRC, "t.cdl").expect("compiles");
    let out = host.call("setup", &[]).expect("setup runs");

    let (token, node) = out
        .commands
        .iter()
        .find_map(|c| match c {
            ScriptCommand::BindEvent { token, node, .. } => Some((*token, *node)),
            _ => None,
        })
        .expect("event_on emits BindEvent");
    assert_eq!(node, btn, "bound against the button's real handle");
    event::register_host_binding(token, node, "click".into(), false);

    // Inject a synthetic keydown-less click.
    let data = EventData {
        event_type: "click".into(),
        target: btn,
        key: "".into(),
        ..Default::default()
    };
    event::dispatch(data, &[], true, |t| {
        host.dispatch_event_handler(t).expect("handler runs");
    });

    // The handler wrote text back to the target node.
    let texts: Vec<(u64, String)> = host
        .drain_commands()
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::SetNodeText { node, text } => Some((node, text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec![(btn, "".to_string())],
        "handler saw the event target and (empty) key"
    );

    // Unbind: a second injection delivers nothing.
    event::unregister_binding(token);
    host.drop_event_handler(token);
    let data2 = EventData {
        event_type: "click".into(),
        target: btn,
        ..Default::default()
    };
    event::dispatch(data2, &[], true, |t| {
        host.dispatch_event_handler(t).ok();
    });
    let after = host
        .drain_commands()
        .into_iter()
        .filter(|c| matches!(c, ScriptCommand::SetNodeText { .. }))
        .count();
    assert_eq!(after, 0, "unbound handler does not fire");
    event::clear_all_bindings();
}
