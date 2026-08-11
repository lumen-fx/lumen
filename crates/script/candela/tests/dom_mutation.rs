//! The candela DOM write bindings are procedural (`lumen::node_*(h, ...)`),
//! not method-chained: the pinned candela dep predates user-struct impl
//! methods. Node handles are `int` ids; `node_spawn` returns a reserved-id
//! valid for the whole tick, and every mutation pushes one command into the
//! sink, the same commands the rhai / lua fluent form emits.
//!
//! The `window` / `history` namespaces do work natively on candela
//! (`window.set_title(..)` compiles), so they are bound directly.

use lumen_script::{ScriptCommand, ScriptHost};
use lumen_script_candela::CandelaHost;

const SRC: &str = r##"
host "lumen" {
    int node_spawn(string);
    node_set_class(int, string);
    node_class_add(int, string);
    node_set_attr(int, string, string);
    node_set_text(int, string);
    node_set_style(int, string, string);
}
host "window" {
    set_title(string);
}
host "history" {
    back();
}
fn build() {
    let n = lumen::node_spawn("div");
    lumen::node_set_class(n, "row");
    lumen::node_class_add(n, "active");
    lumen::node_set_attr(n, "role", "button");
    lumen::node_set_text(n, "Save");
    lumen::node_set_style(n, "color", "#ff0000");
    window::set_title("Hi");
    history::back();
}
fn main() {}
"##;

#[test]
fn candela_procedural_mutators_emit_commands() {
    let mut host = CandelaHost::new();
    host.load(SRC, "t.cdl").expect("script compiles");
    let out = host.call("build", &[]).expect("build runs");

    let kinds: Vec<&str> = out.commands.iter().map(kind).collect();
    assert_eq!(
        kinds,
        vec![
            "Spawn",
            "SetAttr",     // node_set_class -> class attr
            "ClassAdd",    // node_class_add
            "SetAttr",     // node_set_attr(role)
            "SetNodeText", // node_set_text
            "SetStyleProp",
            "WindowSetTitle",
        ],
        "procedural calls emit the same command sequence as the fluent hosts"
    );

    // Each mutation targets the one spawned node.
    let tok = match &out.commands[0] {
        ScriptCommand::Spawn { reserved, tag } => {
            assert_eq!(tag, "div");
            *reserved
        }
        other => panic!("first command is Spawn, got {other:?}"),
    };
    for c in &out.commands[1..6] {
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
