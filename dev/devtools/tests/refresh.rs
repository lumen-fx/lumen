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
use lumen_core::components::{Color, LumenId, TextContent, TextStyle, Visible};
use lumen_devtools::{
    BODY_ID, DevtoolsMarker, DevtoolsState, NetworkCapture, OverlayPalette, ROWS_ID, RowTarget,
    Tab, rebuild_element_rows, refresh_panes,
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
    // Distinct from the default fallback: proves a row label's color comes
    // from the resource (what a resolved `--dt-tag-color` would supply),
    // not a compiled-in constant.
    let palette = OverlayPalette {
        tag_color: Color::rgb(0.9, 0.1, 0.9),
        ..OverlayPalette::default()
    };
    world.insert_resource(palette);

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);

    let mut rows = world.query::<(&RowTarget, &ChildOf)>();
    let rows: Vec<(u64, Entity)> = rows.iter(&world).map(|(t, p)| (t.0, p.parent())).collect();
    assert_eq!(rows.len(), 2, "one row per app element: {rows:?}");
    assert!(rows.iter().all(|(_, p)| *p == container));

    // Label parts are children of the rows, colored per part.
    let mut labels = world.query_filtered::<(&TextContent, &TextStyle), With<DevtoolsMarker>>();
    let joined: String = labels
        .iter(&world)
        .map(|(t, _)| t.0.clone())
        .collect::<Vec<_>>()
        .join("|");
    assert!(joined.contains("<column>"), "got: {joined}");
    assert!(joined.contains("#app-root"), "got: {joined}");
    assert!(joined.contains("<text>"), "got: {joined}");
    let tag_label = labels
        .iter(&world)
        .find(|(t, _)| t.0.contains("<column>"))
        .expect("a tag label part");
    assert_eq!(tag_label.1.color, palette.tag_color);
    assert!(
        !joined.contains("dt-secret"),
        "overlay must not inspect itself: {joined}"
    );
}

#[test]
fn leaving_the_elements_tab_clears_rows_and_reflows_the_scroll() {
    use lumen_core::components::DirtyLayout;

    let mut world = World::new();
    let scroll = world.spawn(()).id();
    let container = world
        .spawn((LumenId(ROWS_ID.into()), Visible(true), ChildOf(scroll)))
        .id();
    let _ = container;

    let mut snap = Snapshot::default();
    snap.entities = vec![EntityView {
        id: 1,
        components: vec![],
    }];
    snap.inspect.insert(1, insp("root", None, None, vec![]));
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    world.insert_resource(state(true, Tab::Elements));
    world.insert_resource(OverlayPalette::default());

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);
    assert_eq!(world.query::<&RowTarget>().iter(&world).count(), 1);

    world.insert_resource(state(true, Tab::Signals));
    sched.run(&mut world);
    assert_eq!(
        world.query::<&RowTarget>().iter(&world).count(),
        0,
        "rows cleared off the Elements tab"
    );
    // The scroll parent relaid out, so siblings (the text body) reposition.
    assert!(world.get::<DirtyLayout>(scroll).is_some());
}

#[test]
fn rebuild_skips_unchanged_trees_replaces_changed_ones_and_needs_a_container() {
    // Snapshot is not Clone; build a fresh one per phase.
    let snap_of = |ids: &[u64]| {
        let mut snap = Snapshot::default();
        for &id in ids {
            snap.entities.push(EntityView {
                id,
                components: vec![],
            });
            snap.inspect.insert(id, insp("node", None, None, vec![]));
        }
        snap
    };
    let mut world = World::new();
    world.spawn((LumenId(ROWS_ID.into()), Visible(true)));
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap_of(&[1])))));
    world.insert_resource(state(true, Tab::Elements));
    world.insert_resource(OverlayPalette::default());

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);
    let first: Vec<Entity> = world
        .query_filtered::<Entity, With<RowTarget>>()
        .iter(&world)
        .collect();
    assert_eq!(first.len(), 1);

    // Unchanged tree: the same row entities survive (no rebuild).
    sched.run(&mut world);
    let again: Vec<Entity> = world
        .query_filtered::<Entity, With<RowTarget>>()
        .iter(&world)
        .collect();
    assert_eq!(first, again, "unchanged tree keeps its rows");

    // Changed tree: the old rows despawn and new ones replace them.
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap_of(&[1, 2])))));
    sched.run(&mut world);
    let replaced: Vec<Entity> = world
        .query_filtered::<Entity, With<RowTarget>>()
        .iter(&world)
        .collect();
    assert_eq!(replaced.len(), 2);
    assert!(replaced.iter().all(|e| !first.contains(e)), "rows replaced");

    // No rows container in the world: the rebuild has nowhere to spawn.
    let mut bare = World::new();
    bare.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap_of(&[1])))));
    bare.insert_resource(state(true, Tab::Elements));
    bare.insert_resource(OverlayPalette::default());
    let mut bare_sched = Schedule::default();
    bare_sched.add_systems(rebuild_element_rows);
    bare_sched.run(&mut bare);
    assert_eq!(bare.query::<&RowTarget>().iter(&bare).count(), 0);
}

#[test]
fn rebuild_noop_when_hidden() {
    let mut world = World::new();
    world.spawn((LumenId(ROWS_ID.into()), Visible(true)));
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(Snapshot::default()))));
    world.insert_resource(state(false, Tab::Elements));
    world.insert_resource(OverlayPalette::default());

    let mut sched = Schedule::default();
    sched.add_systems(rebuild_element_rows);
    sched.run(&mut world);

    let mut rows = world.query::<&RowTarget>();
    assert_eq!(rows.iter(&world).count(), 0);
}

#[test]
fn refresh_routes_network_text_and_the_inspect_pane() {
    use lumen_devtools::{INSPECT_BODY_ID, INSPECT_ID};

    let mut world = World::new();
    let body = world
        .spawn((
            LumenId(BODY_ID.into()),
            TextContent(String::new()),
            Visible(false),
        ))
        .id();
    // Panes without a Visible component: refresh seeds one.
    let pane = world
        .spawn((LumenId(INSPECT_ID.into()), TextContent(String::new())))
        .id();
    let pane_body = world
        .spawn((LumenId(INSPECT_BODY_ID.into()), TextContent(String::new())))
        .id();

    let mut snap = Snapshot::default();
    snap.inspect
        .insert(9, insp("label", Some("who"), None, vec![]));
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    world.insert_resource(NetworkCapture::default());
    world.insert_resource(DevtoolsState {
        visible: true,
        tab: Tab::Network,
        selected: Some(9),
        ..Default::default()
    });

    let mut sched = Schedule::default();
    sched.add_systems(refresh_panes);
    sched.run(&mut world);

    let text = world.get::<TextContent>(body).unwrap().0.clone();
    assert!(text.contains("no requests captured"), "got: {text}");
    // On a non-Elements tab the inspect pane is hidden, but its body text
    // still tracks the selection.
    assert!(!world.get::<Visible>(pane).unwrap().0, "pane hidden");
    let inspect_text = world.get::<TextContent>(pane_body).unwrap().0.clone();
    assert!(inspect_text.contains("<label>#who"), "got: {inspect_text}");

    // Back on Elements with a selection, the pane shows.
    world.resource_mut::<DevtoolsState>().tab = Tab::Elements;
    sched.run(&mut world);
    assert!(world.get::<Visible>(pane).unwrap().0, "pane shown");
}

#[test]
fn refresh_survives_a_poisoned_snapshot() {
    let mut world = World::new();
    let body = world
        .spawn((
            LumenId(BODY_ID.into()),
            TextContent("kept".into()),
            Visible(true),
        ))
        .id();
    let handle = SnapshotHandle(Arc::new(RwLock::new(Snapshot::default())));
    let poison = handle.0.clone();
    std::thread::spawn(move || {
        let _guard = poison.write().unwrap();
        panic!("poison the lock");
    })
    .join()
    .ok();
    world.insert_resource(handle);
    world.insert_resource(NetworkCapture::default());
    world.insert_resource(state(true, Tab::Signals));

    let mut sched = Schedule::default();
    sched.add_systems(refresh_panes);
    sched.run(&mut world);
    assert_eq!(world.get::<TextContent>(body).unwrap().0, "kept");
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
