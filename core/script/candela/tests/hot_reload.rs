//! Hot reload on [`CandelaHost`] (parity with the Rhai and Lua `hot_reload`
//! suites, plus the `on_start` case all three share).
//!
//! A reload swaps the compiled program while the live host keeps its signal
//! mirror. Two contracts are under test:
//!
//! - Atomicity: a reload that fails to compile, or whose `main` errors while
//!   the module instantiates, leaves the previous registrations intact.
//! - Carry-forward: a reload that succeeds keeps the registrations the old
//!   program made, and the new program's registrations win on collision.
//!   Apps bind their handlers from `on_start`, which the runtime fires once at
//!   app construction and never re-fires on reload, so a reload that dropped
//!   those bindings would leave every click a silent no-op.

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The event-binding registry is process-global, and `replace()` walks it in
/// the carry-forward, so tests in this binary mutate it concurrently and can
/// wipe a binding another test registered. Serialise every test and start it
/// from an empty registry.
static GLOBAL_EVENT_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serialise() -> std::sync::MutexGuard<'static, ()> {
    let guard = GLOBAL_EVENT_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lumen_script::event::clear_all_bindings();
    guard
}

/// A program that registers its click handler from `main` (candela's module
/// body), so a reload re-runs the registration.
fn main_registered(handler: &str, clicks: i64) -> String {
    format!(
        r#"
import "lumen.cdl";
fn main() {{ lumen::on("click", "save", "{handler}"); }}
fn {handler}(id) {{ lumen::add_clicks({clicks}); }}
"#
    )
}

/// A program that registers its click handler from `on_start`, the way an app
/// template does.
fn on_start_registered(clicks: i64) -> String {
    format!(
        r#"
import "lumen.cdl";
fn on_start() {{ lumen::on("click", "bump", "handle_bump"); }}
fn handle_bump(id) {{ lumen::add_clicks({clicks}); }}
fn main() {{}}
"#
    )
}

#[test]
fn replace_with_compile_error_preserves_old_handlers() {
    let _serial = serialise();
    let mut host = CandelaHost::new();
    host.load(&main_registered("save_v1", 1), "app.cdl")
        .expect("initial load");
    assert!(host.handler_for("click", "save").is_some());

    let err = host.replace("fn @@@ broken syntax {{{", "app.cdl");
    assert!(err.is_err(), "malformed source must fail replace");

    assert_eq!(
        host.handler_for("click", "save"),
        Some("save_v1".to_owned()),
        "old handler survives a parse-time failure",
    );
}

#[test]
fn replace_with_runtime_error_preserves_old_handlers() {
    let _serial = serialise();
    let mut host = CandelaHost::new();
    host.load(&main_registered("save_v1", 1), "app.cdl")
        .expect("initial load");
    assert!(host.handler_for("click", "save").is_some());

    // Parses, then fails while `main` instantiates the module: the namespace
    // resolves lazily, so an undeclared one errors at run time.
    let err = host.replace(
        "fn main() { undeclared_namespace::whatever(); }\n",
        "app.cdl",
    );
    assert!(err.is_err(), "a failing `main` must fail replace");

    assert_eq!(
        host.handler_for("click", "save"),
        Some("save_v1".to_owned()),
        "old handler survives a failed replace",
    );
}

#[test]
fn replace_success_swaps_handlers() {
    let _serial = serialise();
    let mut host = CandelaHost::new();
    host.load(&main_registered("save_v1", 1), "app.cdl")
        .expect("initial load");
    assert_eq!(
        host.handler_for("click", "save"),
        Some("save_v1".to_owned())
    );

    host.replace(&main_registered("save_v2", 2), "app.cdl")
        .expect("successful replace");

    assert_eq!(
        host.handler_for("click", "save"),
        Some("save_v2".to_owned()),
        "a re-registered handler takes the new name",
    );
}

/// The reload regression: the app registers from `on_start`, which fires once
/// at app construction. `replace` must carry that registration forward, and
/// the carried handler must dispatch against the reloaded program.
#[test]
fn on_start_registered_handler_survives_reload() {
    let _serial = serialise();
    let mut host = CandelaHost::new();
    host.load(&on_start_registered(1), "counter.cdl")
        .expect("initial load");
    // Drive `on_start` exactly the way `ScriptPlugin::build` does.
    let outcome = host.call("on_start", &[]).expect("on_start ok");
    host.push_commands(outcome.commands);
    assert_eq!(
        host.handler_for("click", "bump"),
        Some("handle_bump".to_owned()),
        "on_start registered the handler"
    );

    host.replace(&on_start_registered(7), "counter.cdl")
        .expect("successful replace");

    assert_eq!(
        host.handler_for("click", "bump"),
        Some("handle_bump".to_owned()),
        "a reload carries the on_start registration forward",
    );

    let out = host
        .call("handle_bump", &[ScriptValue::Str("bump".to_owned())])
        .expect("handler dispatches");
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::AddClicks(7))),
        "the carried handler runs the reloaded body, got {:?}",
        out.commands
    );
}

/// The same contract for the dynamic-DOM event side, which dispatches through
/// the process-global binding table rather than the per-id handler map: an
/// `event_on` bind made from `on_start` still fires after a reload, and runs
/// the reloaded handler body.
///
/// This is the host-level contract, and it holds for any embedder that swaps
/// a script without touching the tree. `lumenc run` respawns the element tree
/// on every reload, so under it a carried binding names an entity that no
/// longer exists and goes inert rather than firing twice.
#[test]
fn on_start_event_binding_survives_reload() {
    let _serial = serialise();
    use bevy_ecs::world::World;
    use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
    use lumen_script::event::{self, EventData};

    event::clear_all_bindings();

    // Minimal two-node tree so `node_get_by_id("btn")` resolves.
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn_entity = w.spawn_empty().id();
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
        rec(root, "root", Some("app"), None, &[btn_entity]),
        rec(btn_entity, "button", Some("btn"), Some(root), &[]),
    ]));
    let btn = NodeHandle::new(btn_entity).pack();

    let src = |label: &str| {
        format!(
            r#"
import "lumen.cdl";
fn on_start() {{
    let b = lumen::node_get_by_id("btn");
    lumen::event_on(b, "click", "on_click");
}}
fn on_click(ev) {{ lumen::node_set_text(lumen::event_target(ev), "{label}"); }}
fn main() {{}}
"#
        )
    };

    let mut host = CandelaHost::new();
    host.load(&src("v1"), "app.cdl").expect("initial load");
    let outcome = host.call("on_start", &[]).expect("on_start ok");
    // Stand in for the runtime's `BindEvent` applier.
    for c in &outcome.commands {
        if let ScriptCommand::BindEvent {
            token,
            node,
            event_type,
            capture,
        } = c
        {
            event::register_host_binding(*token, *node, event_type.clone(), *capture);
        }
    }
    host.drain_commands();

    host.replace(&src("v2"), "app.cdl").expect("reload");

    let data = EventData {
        event_type: "click".into(),
        target: btn,
        ..Default::default()
    };
    event::dispatch(data, &[], true, |t| {
        host.dispatch_event_handler(t).expect("handler runs");
    });
    let texts: Vec<String> = host
        .drain_commands()
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::SetNodeText { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["v2".to_string()],
        "the carried binding dispatched once, into the reloaded body"
    );
    event::clear_all_bindings();
}
