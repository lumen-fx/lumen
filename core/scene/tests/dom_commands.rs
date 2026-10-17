//! What the DOM applier writes for an edit, and what it leaves alone.
//!
//! A write to an element's classes or inline style is a change to everything
//! watching them: the element restyles with its subtree, the accessibility
//! tree and the DOM index rebuild, the frame repaints. An edit that leaves
//! the element as it was has to write nothing.

use bevy_ecs::change_detection::DetectChanges;
use bevy_ecs::message::{MessageRegistry, Messages};
use bevy_ecs::system::RunSystemOnce;
use bevy_ecs::world::World;
use lumen_core::components::{InlineStyle, LumenClasses, LumenTag, StyleDirty};
use lumen_core::node::NodeHandle;
use lumen_scene::dom::{PendingDomCommands, apply_dom_commands, collect_dom_commands};
use lumen_script::ScriptCommand;
use lumen_script::runtime::ScriptCommandEvent;

fn world() -> World {
    let mut world = World::new();
    MessageRegistry::register_message::<ScriptCommandEvent>(&mut world);
    world.init_resource::<PendingDomCommands>();
    world
}

/// Run one tick's worth of `commands` through the applier.
fn apply(world: &mut World, commands: Vec<ScriptCommand>) {
    for command in commands {
        world
            .resource_mut::<Messages<ScriptCommandEvent>>()
            .write(ScriptCommandEvent(command));
    }
    world.run_system_once(collect_dom_commands).unwrap();
    apply_dom_commands(world);
    world.clear_trackers();
}

/// #538: removing a class the element lacks, adding one it has, setting an
/// inline property to the value it holds or removing one it lacks marks
/// nothing for restyle and writes nothing; a real edit does both.
#[test]
fn an_edit_that_changes_nothing_writes_nothing() {
    let mut world = world();
    let mut inline = InlineStyle::default();
    inline.set("color", "#ff0000");
    let row = world
        .spawn((
            LumenTag("row".into()),
            LumenClasses(vec!["hot".into()]),
            inline,
        ))
        .id();
    let node = NodeHandle::new(row).pack();
    world.clear_trackers();
    let classes_at = world
        .entity(row)
        .get_ref::<LumenClasses>()
        .unwrap()
        .last_changed();
    let style_at = world
        .entity(row)
        .get_ref::<InlineStyle>()
        .unwrap()
        .last_changed();

    apply(
        &mut world,
        vec![
            ScriptCommand::ClassRemove {
                node,
                class: "cold".into(),
            },
            ScriptCommand::ClassAdd {
                node,
                class: "hot".into(),
            },
            ScriptCommand::SetStyleProp {
                node,
                name: "color".into(),
                value: "#ff0000".into(),
            },
            ScriptCommand::RemoveStyleProp {
                node,
                name: "width".into(),
            },
        ],
    );
    assert!(world.get::<StyleDirty>(row).is_none(), "nothing to restyle");
    let entity = world.entity(row);
    assert_eq!(
        entity.get_ref::<LumenClasses>().unwrap().last_changed(),
        classes_at
    );
    assert_eq!(
        entity.get_ref::<InlineStyle>().unwrap().last_changed(),
        style_at
    );

    apply(
        &mut world,
        vec![ScriptCommand::ClassToggle {
            node,
            class: "cold".into(),
        }],
    );
    assert!(
        world.get::<StyleDirty>(row).is_some(),
        "a real edit restyles"
    );
    let classes: Vec<&str> = world
        .get::<LumenClasses>(row)
        .unwrap()
        .0
        .iter()
        .map(|c| c.as_ref())
        .collect();
    assert_eq!(classes, ["hot", "cold"]);
}
