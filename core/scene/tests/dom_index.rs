//! The DOM index the scripting layer resolves against: which entities it
//! records, and which it must never see. `DomHidden` keeps tooling
//! subtrees (the devtools panel) out of it - without the filter, a tooling
//! root spawned before the app's sorts first among the index roots and
//! becomes the document root scripts build into.

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use lumen_core::components::{DomHidden, LumenClasses, LumenId, LumenTag};
use lumen_core::node::dom_index_snapshot;
use lumen_scene::dom::build_dom_index;

/// The index is process-global, so the cases here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn dom_hidden_subtree_never_reaches_the_index() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut world = World::new();

    // A tooling root spawned FIRST (lower entity bits), then the app root.
    let tool_root = world
        .spawn((
            LumenTag("row".into()),
            LumenId("dt-panel".into()),
            DomHidden,
        ))
        .id();
    let tool_child = world
        .spawn((LumenTag("label".into()), DomHidden, ChildOf(tool_root)))
        .id();
    let app_root = world
        .spawn((LumenTag("root".into()), LumenId("app".into())))
        .id();
    let app_child = world
        .spawn((LumenTag("label".into()), ChildOf(app_root)))
        .id();
    let _ = tool_child;

    let mut schedule = Schedule::default();
    schedule.add_systems(build_dom_index);
    schedule.run(&mut world);

    let index = dom_index_snapshot();
    assert_eq!(
        index.roots(),
        &[app_root],
        "the app root is the only index root"
    );
    assert_eq!(index.document(), Some(app_root));
    assert!(index.record(app_child).is_some(), "app content is indexed");
    assert!(
        index.record(tool_root).is_none() && index.get_by_id("dt-panel").is_none(),
        "the DomHidden subtree is invisible to selectors"
    );
}

/// #535: a tick that changed nothing the index holds publishes nothing, so
/// an idle tick of a large tree does not walk it; a change to what it holds,
/// and an element going, publish again.
#[test]
fn the_index_is_rebuilt_only_when_what_it_holds_changes() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut world = World::new();
    let root = world
        .spawn((LumenTag("root".into()), LumenId("app".into())))
        .id();
    let child = world.spawn((LumenTag("label".into()), ChildOf(root))).id();
    let mut schedule = Schedule::default();
    schedule.add_systems(build_dom_index);

    schedule.run(&mut world);
    let first = dom_index_snapshot();
    assert!(first.record(child).is_some());
    schedule.run(&mut world);
    schedule.run(&mut world);
    assert!(
        std::sync::Arc::ptr_eq(&first, &dom_index_snapshot()),
        "idle ticks publish no new index"
    );

    world
        .entity_mut(child)
        .insert(LumenClasses(vec!["hot".into()]));
    schedule.run(&mut world);
    let classed = dom_index_snapshot();
    assert!(!std::sync::Arc::ptr_eq(&first, &classed));
    assert_eq!(
        classed.record(child).map(|r| r.classes.clone()),
        Some(vec!["hot".to_string()])
    );

    world.entity_mut(child).despawn();
    schedule.run(&mut world);
    assert!(
        dom_index_snapshot().record(child).is_none(),
        "a despawned element leaves the index"
    );
}
