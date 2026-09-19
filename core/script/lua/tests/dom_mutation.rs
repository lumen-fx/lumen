//! The lua DOM write bindings (phases 2 + 3, section 4.8) emit the right
//! `ScriptCommand`s. Lua `UserData` methods cannot capture the per-host
//! sink, so node mutations route through the process-global external DOM
//! bus; the fluent chain still returns the receiver so `n:a():b()` chains.

use lumen_script::ScriptCommand;
use lumen_script::ScriptHost;
use lumen_script::node_query::drain_external_dom_commands;
use lumen_script_lua::LuaHost;

const SRC: &str = r##"
function build()
    local n = create("div")
    n:set_class("row"):add_class("active"):set_attr("role", "button"):set_text("Save")
    n:set_style("color", "#ff0000")
    window.set_title("Hi")
    history.back()
end
"##;

#[test]
fn lua_fluent_mutators_emit_commands() {
    // Clear any residue on the shared bus.
    let _ = drain_external_dom_commands();

    let mut host = LuaHost::new();
    host.load(SRC).expect("script compiles");
    let _ = host.call("build", &[]).expect("build runs");

    let cmds = drain_external_dom_commands();
    let kinds: Vec<&str> = cmds.iter().map(kind).collect();
    assert_eq!(
        kinds,
        vec![
            "Spawn",
            "SetAttr",
            "ClassAdd",
            "SetAttr",
            "SetNodeText",
            "SetStyleProp",
            "WindowSetTitle",
        ],
        "fluent chain emits commands in issue order"
    );

    let tok = match &cmds[0] {
        ScriptCommand::Spawn { reserved, tag } => {
            assert_eq!(tag, "div");
            *reserved
        }
        other => panic!("first command is Spawn, got {other:?}"),
    };
    for c in &cmds[1..6] {
        assert_eq!(command_node(c), Some(tok));
    }
}

fn kind(c: &ScriptCommand) -> &'static str {
    match c {
        ScriptCommand::Spawn { .. } => "Spawn",
        ScriptCommand::SetAttr { .. } => "SetAttr",
        ScriptCommand::ClassAdd { .. } => "ClassAdd",
        ScriptCommand::SetNodeText { .. } => "SetNodeText",
        ScriptCommand::SetStyleProp { .. } => "SetStyleProp",
        ScriptCommand::WindowSetTitle { .. } => "WindowSetTitle",
        _ => "other",
    }
}

fn command_node(c: &ScriptCommand) -> Option<u64> {
    match c {
        ScriptCommand::SetAttr { node, .. }
        | ScriptCommand::ClassAdd { node, .. }
        | ScriptCommand::SetNodeText { node, .. }
        | ScriptCommand::SetStyleProp { node, .. } => Some(*node),
        _ => None,
    }
}
