//! End-to-end system tests: `rebuild_element_rows` turns the shared
//! snapshot into row entities under the `dt-rows` container (excluding the
//! overlay's own `DevtoolsMarker` entities), and `refresh_panes` routes the
//! text-blob body between the Signals / Network tabs and the Elements
//! empty-state hint.

// `Snapshot` has ~30 fields; assigning the few relevant ones after
// `default()` reads clearer than a full struct literal.
#![allow(clippy::field_reassign_with_default)]

use std::sync::{Arc, RwLock};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use lumen_core::components::{LumenId, TextContent, Visible};
use lumen_devtools::{
    BODY_ID, DevtoolsMarker, DevtoolsState, NetworkCapture, ROWS_ID, RowTarget, Tab,
    rebuild_element_rows, refresh_panes,
};
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

fn state(visible: bool, tab: Tab) -> DevtoolsState {
    DevtoolsState {
        visible,
        tab,
        ..Default::default()
    }
}

#[test]
fn rebuild_spawns_rows_and_excludes_overlay() {
    let mut world = World::new();

    let container = world.spawn((LumenId(ROWS_ID.into()), Visible(true))).id();
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
    world.insert_resource(state(true, Tab::Elements));

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);

    let mut rows = world.query::<(&RowTarget, &TextContent, &ChildOf)>();
    let rows: Vec<(u64, String, Entity)> = rows
        .iter(&world)
        .map(|(t, text, p)| (t.0, text.0.clone(), p.parent()))
        .collect();
    assert_eq!(rows.len(), 2, "one row per app element: {rows:?}");
    assert!(rows.iter().all(|(.., p)| *p == container));
    assert!(rows.iter().any(|(_, l, _)| l.contains("<column>#app-root")));
    assert!(rows.iter().any(|(_, l, _)| l.contains("<text>")));
    assert!(
        !rows.iter().any(|(_, l, _)| l.contains("dt-secret")),
        "overlay must not inspect itself: {rows:?}"
    );
}

#[test]
fn rebuild_noop_when_hidden() {
    let mut world = World::new();
    world.spawn((LumenId(ROWS_ID.into()), Visible(true)));
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(Snapshot::default()))));
    world.insert_resource(state(false, Tab::Elements));

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);

    let mut rows = world.query::<&RowTarget>();
    assert_eq!(rows.iter(&world).count(), 0);
}

#[test]
fn refresh_routes_signals_text_into_body() {
    let mut world = World::new();
    let body = world
        .spawn((
            LumenId(BODY_ID.into()),
            TextContent(String::new()),
            Visible(false),
        ))
        .id();

    let mut snap = Snapshot::default();
    snap.frame = 7;
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    world.insert_resource(NetworkCapture::default());
    world.insert_resource(state(true, Tab::Signals));

    let mut sched = Schedule::default();
    sched.add_systems(refresh_panes);
    sched.run(&mut world);

    let text = world.get::<TextContent>(body).unwrap().0.clone();
    assert!(text.contains("frame 7"), "got: {text}");
    assert!(world.get::<Visible>(body).unwrap().0, "body shown");
}
