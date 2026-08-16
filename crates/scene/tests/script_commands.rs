//! The applier for the script commands that reach nothing but the scene.
//!
//! A host queues these from a builtin and the applier lands them a tick
//! later, so what a script wrote is only observable through the store, the
//! array signals, the external-property queue, or an element's own text.
//! Each case here writes the command a builtin would and reads back the one
//! place its effect is meant to show.

use bevy_ecs::message::{MessageRegistry, Messages};
use bevy_ecs::system::RunSystemOnce;
use bevy_ecs::world::World;
use lumen_core::components::{LumenId, TextContent, TextInput};
use lumen_core::property_store::{
    PropertyKey, PropertyStore, PropertyValue, commit_external_properties,
    drain_external_properties,
};
use lumen_core::signals::ArraySignals;
use lumen_scene::script_commands::apply_scene_script_commands;
use lumen_script::ScriptCommand;
use lumen_script::runtime::ScriptCommandEvent;

/// A world with the resources the applier reads and nothing else.
fn world() -> World {
    let mut world = World::new();
    MessageRegistry::register_message::<ScriptCommandEvent>(&mut world);
    world.insert_resource(PropertyStore::default());
    world.insert_resource(ArraySignals::default());
    world
}

fn queue(world: &mut World, command: ScriptCommand) {
    world
        .resource_mut::<Messages<ScriptCommandEvent>>()
        .write(ScriptCommandEvent(command));
}

#[test]
fn a_signal_write_reaches_the_store() {
    let mut world = world();
    queue(
        &mut world,
        ScriptCommand::SetSignal {
            name: "count".into(),
            value: "7".into(),
        },
    );

    world.run_system_once(apply_scene_script_commands).unwrap();

    let store = world.resource::<PropertyStore>();
    assert_eq!(store.get_global_str("count").as_deref(), Some("7"));
}

#[test]
fn an_array_write_reaches_the_array_signals() {
    let mut world = world();
    let mut row = std::collections::HashMap::new();
    row.insert("title".to_string(), "ship it".to_string());
    queue(
        &mut world,
        ScriptCommand::SetArray {
            name: "todos".into(),
            items: vec![row],
        },
    );

    world.run_system_once(apply_scene_script_commands).unwrap();

    let arrays = world.resource::<ArraySignals>();
    let items = arrays.get("todos").expect("the array was written");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get("title").map(String::as_str), Some("ship it"));
}

#[test]
fn setting_text_writes_the_element_the_command_names() {
    let mut world = world();
    let greeting = world
        .spawn((LumenId("greeting".into()), TextContent("hi".into())))
        .id();
    let other = world
        .spawn((LumenId("other".into()), TextContent("untouched".into())))
        .id();
    queue(
        &mut world,
        ScriptCommand::SetText {
            target_id: "greeting".into(),
            text: "hello".into(),
        },
    );

    world.run_system_once(apply_scene_script_commands).unwrap();

    assert_eq!(world.get::<TextContent>(greeting).unwrap().0, "hello");
    assert_eq!(world.get::<TextContent>(other).unwrap().0, "untouched");
}

/// The caret is clamped with the text, or the next keystroke inserts past
/// the end of the buffer the script just shortened.
#[test]
fn setting_text_clamps_the_caret_of_an_input() {
    let mut world = world();
    let input = TextInput {
        cursor: 11,
        ..TextInput::default()
    };
    let field = world
        .spawn((
            LumenId("field".into()),
            TextContent("long content".into()),
            input,
        ))
        .id();
    queue(
        &mut world,
        ScriptCommand::SetText {
            target_id: "field".into(),
            text: "sh".into(),
        },
    );

    world.run_system_once(apply_scene_script_commands).unwrap();

    assert_eq!(world.get::<TextInput>(field).unwrap().cursor, 2);
}

#[test]
fn a_typed_property_goes_out_through_the_external_queue() {
    let mut world = world();
    queue(
        &mut world,
        ScriptCommand::SetProperty {
            key: PropertyKey::global("theme"),
            value: PropertyValue::Str("dark".into()),
        },
    );

    world.run_system_once(apply_scene_script_commands).unwrap();
    // The queue is the cross-thread path; the systems that drain and commit
    // it are what put the value in the store.
    world.run_system_once(drain_external_properties).unwrap();
    world.run_system_once(commit_external_properties).unwrap();

    // `PropertyValue` carries a `Custom` variant and so is not comparable;
    // its string form is what a reader of this property would see.
    let store = world.resource::<PropertyStore>();
    assert_eq!(store.get_global_str("theme").as_deref(), Some("dark"));
}

/// A command another applier owns passes through here untouched, which is
/// what lets every applier read the same stream.
#[test]
fn a_command_this_applier_does_not_own_is_left_alone() {
    let mut world = world();
    queue(&mut world, ScriptCommand::AddClicks(3));

    world.run_system_once(apply_scene_script_commands).unwrap();

    assert!(world.resource::<PropertyStore>().dirty_peek().is_empty());
}
