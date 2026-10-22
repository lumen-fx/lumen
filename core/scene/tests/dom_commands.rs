//! What the DOM applier writes for an edit, and what it leaves alone.
//!
//! A write to an element's classes or inline style is a change to everything
//! watching them: the element restyles with its subtree, the accessibility
//! tree and the DOM index rebuild, the frame repaints. An edit that leaves
//! the element as it was has to write nothing.

use bevy_ecs::change_detection::DetectChanges;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::{MessageRegistry, Messages};
use bevy_ecs::system::RunSystemOnce;
use bevy_ecs::world::World;
use lumen_core::components::{InlineStyle, LumenClasses, LumenTag, StyleDirty, StyleVersion};
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

/// #539: an edit of the tree's shape restyles the element whose children
/// changed, with its subtree, and leaves the rest of the document alone.
#[test]
fn a_tree_edit_restyles_the_subtree_it_changed() {
    let mut world = world();
    world.insert_resource(StyleVersion::default());
    let root = world.spawn(LumenTag("root".into())).id();
    let list = world.spawn((LumenTag("column".into()), ChildOf(root))).id();
    let row = world.spawn((LumenTag("row".into()), ChildOf(list))).id();
    let panel = world.spawn((LumenTag("column".into()), ChildOf(root))).id();
    let added = world.spawn(LumenTag("row".into())).id();
    let pack = |e| NodeHandle::new(e).pack();

    apply(
        &mut world,
        vec![ScriptCommand::Insert {
            parent: pack(list),
            node: pack(added),
            before: 0,
        }],
    );
    assert!(
        world.get::<StyleDirty>(list).is_some(),
        "the list's children changed"
    );
    assert!(
        world.get::<StyleDirty>(panel).is_none(),
        "the panel did not"
    );
    assert!(world.get::<StyleDirty>(root).is_none());
    assert_eq!(
        *world.resource::<StyleVersion>(),
        StyleVersion::default(),
        "no full restyle"
    );

    world.entity_mut(list).remove::<StyleDirty>();
    apply(
        &mut world,
        vec![ScriptCommand::RemoveNode { node: pack(row) }],
    );
    assert!(
        world.get::<StyleDirty>(list).is_some(),
        "a removal reshapes the parent"
    );
    assert!(world.get::<StyleDirty>(panel).is_none());
    assert_eq!(*world.resource::<StyleVersion>(), StyleVersion::default());
}

/// A page's window: the app has its primary window, but no window driver
/// applies titles, so the title reads back at once.
#[test]
fn an_undriven_window_reports_its_new_title_at_once() {
    use lumen_core::window::{PrimaryWindow, Window};

    let mut world = world();
    let window = world.spawn((Window::default(), PrimaryWindow)).id();
    apply(
        &mut world,
        vec![ScriptCommand::WindowSetTitle {
            window: 0,
            title: "Paged".into(),
        }],
    );
    assert_eq!(world.get::<Window>(window).unwrap().title, "Paged");
    assert_eq!(
        lumen_core::window_state::title_of(window.to_bits()),
        "Paged"
    );
}

/// A world nothing drives a window for (a browser page, a bare app) still
/// answers `window.set_title` through the getter, and drops a close or quit
/// with no one to obey it instead of queueing a request nobody reads.
#[test]
fn window_commands_without_a_window_keep_to_the_getter() {
    use lumen_core::window::{QuitRequest, WindowRequest};

    let mut world = world();
    apply(
        &mut world,
        vec![
            ScriptCommand::WindowSetTitle {
                window: 0,
                title: "Unwindowed".into(),
            },
            ScriptCommand::WindowClose { window: 0 },
            ScriptCommand::AppQuit,
        ],
    );
    assert_eq!(lumen_core::window_state::title_of(0), "Unwindowed");
    assert!(!world.contains_resource::<Messages<WindowRequest>>());
    assert!(!world.contains_resource::<Messages<QuitRequest>>());
}
