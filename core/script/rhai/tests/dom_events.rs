//! Phase-4 rhai event bindings: `n.on(type, handler)` delivers a real event
//! object to a closure, and the returned off token unbinds.
//!
//! These drive the same path the runtime does, headlessly: publish a DOM
//! snapshot so `get_by_id` resolves, bind through the rhai builtin, apply the
//! emitted `BindEvent` to the host-neutral registry, then run the propagation
//! driver invoking the host per token.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_script::event::{self, EventData};
use lumen_script::{ScriptCommand, ScriptHost};
use lumen_script_rhai::RhaiHost;

use bevy_ecs::world::World;

fn publish_fixture() -> (u64, u64) {
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
    (NodeHandle::new(root).pack(), NodeHandle::new(btn).pack())
}

/// Apply a `BindEvent` command to the registry the way the runtime applier
/// does, returning its token.
fn apply_bind(cmd: &ScriptCommand) -> u64 {
    match cmd {
        ScriptCommand::BindEvent {
            node,
            event_type,
            capture,
            token,
        } => {
            event::register_host_binding(*token, *node, event_type.clone(), *capture);
            *token
        }
        other => panic!("expected BindEvent, got {other:?}"),
    }
}

// Share the process-global binding registry, so run as one test.
#[test]
fn rhai_events_receive_and_unbind() {
    click_handler_receives_event();
    off_token_unbinds();
}

fn click_handler_receives_event() {
    event::clear_all_bindings();
    let (_root, btn) = publish_fixture();

    let src = r#"
        fn setup() {
            let b = get_by_id("btn");
            b.on("click", |ev| {
                print("target=" + ev.target().handle() + " key=" + ev.key() + " btn=" + ev.button());
            });
        }
    "#;
    let mut host = RhaiHost::new();
    host.load(src).expect("compiles");
    let out = host.call("setup", &[]).expect("setup runs");
    let bind = out
        .commands
        .iter()
        .find(|c| matches!(c, ScriptCommand::BindEvent { .. }))
        .expect("setup emits a BindEvent");
    apply_bind(bind);

    // Inject a synthetic click on the button.
    let data = EventData {
        event_type: "click".into(),
        target: btn,
        button: 0,
        ..Default::default()
    };
    event::dispatch(data, &[], event::event_bubbles("click"), |token| {
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
    assert_eq!(
        prints,
        vec![format!("target={} key= btn=0", btn as i64)],
        "handler saw the right target / key / button"
    );
    event::clear_all_bindings();
}

fn off_token_unbinds() {
    event::clear_all_bindings();
    let (_root, _btn) = publish_fixture();

    // Bind then immediately call the off token; the drained commands must
    // carry a BindEvent followed by a matching UnbindEvent.
    let src = r#"
        fn bind_and_off() {
            let b = get_by_id("btn");
            let off = b.on("click", |ev| { print("hit"); });
            off.call();
        }
    "#;
    let mut host = RhaiHost::new();
    host.load(src).expect("compiles");
    let out = host.call("bind_and_off", &[]).expect("runs");
    let mut bind_token = None;
    let mut unbind_token = None;
    for c in &out.commands {
        match c {
            ScriptCommand::BindEvent { token, .. } => bind_token = Some(*token),
            ScriptCommand::UnbindEvent { token } => unbind_token = Some(*token),
            _ => {}
        }
    }
    assert!(bind_token.is_some(), "on(...) emits BindEvent");
    assert_eq!(
        bind_token, unbind_token,
        "off.call() emits UnbindEvent for the same token"
    );

    // Applying both leaves no live binding: a dispatch delivers nothing.
    let token = bind_token.unwrap();
    let (_r, btn) = publish_fixture();
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
    assert_eq!(hits, 0, "unbound handler does not fire");
    event::clear_all_bindings();
}
