//! The rhai DOM write bindings (phases 2 + 3, section 4.8) emit the right
//! `ScriptCommand`s. A fluent chain builds a node and each mutator returns
//! the receiver so the chain keeps going; the commands land in the host
//! sink in issue order.

use lumen_script::{ScriptCommand, ScriptHost};
use lumen_script_rhai::RhaiHost;

const SRC: &str = r##"
fn build() {
    let n = create("div");
    n.set_class("row").add_class("active").set_attr("role", "button").set_text("Save");
    n.set_style("color", "#ff0000");
    window.set_title("Hi");
    history.back();
}
"##;

#[test]
fn rhai_fluent_mutators_emit_commands() {
    let mut host = RhaiHost::new();
    host.load(SRC).expect("script compiles");
    let out = host.call("build", &[]).expect("build runs");
    let cmds = out.commands;

    // spawn + the four chained edits + inline style + window title. (back()
    // routes onto the process-global nav bus, not the command sink.)
    let kinds: Vec<&str> = cmds.iter().map(kind).collect();
    assert_eq!(
        kinds,
        vec![
            "Spawn",
            "SetAttr",     // set_class -> class attr
            "ClassAdd",    // add_class
            "SetAttr",     // set_attr(role)
            "SetNodeText", // set_text
            "SetStyleProp",
            "WindowSetTitle",
        ],
        "fluent chain emits commands in issue order"
    );

    // The whole chain addresses ONE reserved node.
    let spawn_tok = match &cmds[0] {
        ScriptCommand::Spawn { reserved, tag } => {
            assert_eq!(tag, "div");
            *reserved
        }
        other => panic!("first command is Spawn, got {other:?}"),
    };
    assert!(lumen_core::node::is_reserved_token(spawn_tok));
    for c in &cmds[1..6] {
        assert_eq!(
            command_node(c),
            Some(spawn_tok),
            "every edit targets the spawned node"
        );
    }
}

#[test]
fn rhai_set_inner_markup_emits_command() {
    let mut host = RhaiHost::new();
    host.load(r#"fn build() { create("div").set_inner_markup("<row/>"); }"#)
        .expect("script compiles");
    let out = host.call("build", &[]).expect("build runs");
    let markup = out.commands.iter().find_map(|c| match c {
        ScriptCommand::SetInnerMarkup { markup, .. } => Some(markup.clone()),
        _ => None,
    });
    assert_eq!(markup.as_deref(), Some("<row/>"));
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
