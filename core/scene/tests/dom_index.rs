//! The DOM index the scripting layer resolves against: which entities it
//! records, and which it must never see. `DomHidden` keeps tooling
//! subtrees (the devtools panel) out of it - without the filter, a tooling
//! root spawned before the app's sorts first among the index roots and
//! becomes the document root scripts build into.

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use lumen_core::components::{DomHidden, LumenId, LumenTag};
use lumen_core::node::dom_index_snapshot;
use lumen_scene::dom::build_dom_index;

#[test]
fn dom_hidden_subtree_never_reaches_the_index() {
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
