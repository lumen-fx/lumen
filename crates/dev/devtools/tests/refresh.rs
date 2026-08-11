//! End-to-end system test: `refresh_body` reads the shared snapshot +
//! network ring and rewrites the `dt-body` text, while excluding the
//! overlay's own (`DevtoolsMarker`) entities from the Elements tree.

// `Snapshot` has ~30 fields; assigning the few relevant ones after
// `default()` reads clearer than a full struct literal.
#![allow(clippy::field_reassign_with_default)]

use std::sync::{Arc, RwLock};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use lumen_core::components::{LumenId, TextContent};
use lumen_devtools::{BODY_ID, DevtoolsMarker, DevtoolsState, NetworkCapture, Tab, refresh_body};
use lumen_mcp::{EntityInspect, EntityView, Snapshot, SnapshotHandle};

fn insp(tag: &str, id: Option<&str>, parent: Option<u64>, children: Vec<u64>) -> EntityInspect {
    EntityInspect {
        tag: Some(tag.to_string()),
        lumen_id: id.map(str::to_string),
        parent,
        children,
        ..Default::default()
    }
}

#[test]
fn refresh_populates_body_and_excludes_overlay() {
    let mut world = World::new();

    // Spawn the overlay body text entity and a devtools-marked entity, plus
    // two "app" entities the Elements tree should surface.
    let body = world
        .spawn((LumenId(BODY_ID.into()), TextContent(String::new())))
        .id();
    let app_root = world.spawn(()).id();
    let app_child = world.spawn(()).id();
    let dt_node = world.spawn(DevtoolsMarker).id();

    // Build a snapshot keyed by the real entity bits.
    let mut snap = Snapshot::default();
    snap.entities = vec![
        EntityView {
            id: app_root.to_bits(),
            components: vec![],
        },
        EntityView {
            id: app_child.to_bits(),
            components: vec![],
        },
        EntityView {
            id: dt_node.to_bits(),
            components: vec![],
        },
    ];
    snap.inspect.insert(
        app_root.to_bits(),
        insp("column", Some("app-root"), None, vec![app_child.to_bits()]),
    );
    snap.inspect.insert(
        app_child.to_bits(),
        insp("text", None, Some(app_root.to_bits()), vec![]),
    );
    snap.inspect.insert(
        dt_node.to_bits(),
        insp("column", Some("dt-secret"), None, vec![]),
    );

    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    world.insert_resource(NetworkCapture::default());
    world.insert_resource(DevtoolsState {
        visible: true,
        tab: Tab::Elements,
    });

    let mut sched = Schedule::default();
    sched.add_systems(refresh_body);
    sched.run(&mut world);

    let text = world.get::<TextContent>(body).unwrap().0.clone();
    assert!(text.contains("<column>#app-root"), "got: {text}");
    assert!(text.contains("<text>"), "got: {text}");
    assert!(
        !text.contains("dt-secret"),
        "overlay must not inspect itself: {text}"
    );
}

#[test]
fn refresh_noop_when_hidden() {
    let mut world = World::new();
    let body = world
        .spawn((LumenId(BODY_ID.into()), TextContent("untouched".into())))
        .id();
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(Snapshot::default()))));
    world.insert_resource(NetworkCapture::default());
    world.insert_resource(DevtoolsState {
        visible: false,
        tab: Tab::Elements,
    });

    let mut sched = Schedule::default();
    sched.add_systems(refresh_body);
    sched.run(&mut world);

    assert_eq!(world.get::<TextContent>(body).unwrap().0, "untouched");
}
