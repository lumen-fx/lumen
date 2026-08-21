//! System tests for the panel's interactive half: click routing (tabs,
//! rows, pick mode, and the inspect-pane edit actions that mutate the
//! running app), the tab and row state styling, the dock inset, the
//! element highlight with its tag chip, and the F12 toggle.

// `Snapshot` has ~30 fields; assigning the few relevant ones after
// `default()` reads clearer than a full struct literal.
#![allow(clippy::field_reassign_with_default)]

use std::sync::{Arc, RwLock};

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use glam::Vec2;
use lumen_core::components::{
    Color, DirtyLayout, LumenId, TextContent, TextStyle, Transform, Visible, Visuals,
};
use lumen_core::input::{ClickEvent, Hovered, Key, KeyPressed, Modifiers, PointerButton};
use lumen_core::render_world::DockInsets;
use lumen_devtools::{
    DevtoolsRoot, DevtoolsState, HighlightBox, HighlightTip, PANEL_ID, RowTarget, Tab,
    handle_clicks, style_rows, style_tabs, sync_dock_inset, toggle_devtools_on_f12,
    update_highlight,
};
use lumen_mcp::{EntityInspect, Snapshot, SnapshotHandle};

fn click_world() -> World {
    let mut world = World::new();
    world.init_resource::<Messages<ClickEvent>>();
    world.init_resource::<Messages<KeyPressed>>();
    world.insert_resource(DevtoolsState {
        visible: true,
        ..Default::default()
    });
    world
}

fn click(world: &mut World, entity: Entity) {
    // Each assertion runs a fresh schedule whose reader would replay the
    // whole ring; keep exactly one pending click.
    world.resource_mut::<Messages<ClickEvent>>().clear();
    world
        .resource_mut::<Messages<ClickEvent>>()
        .write(ClickEvent {
            entity,
            position: Vec2::ZERO,
            button: PointerButton::Primary,
        });
}

macro_rules! run {
    ($world:expr, $system:expr) => {{
        let mut schedule = Schedule::default();
        schedule.add_systems($system);
        schedule.run($world);
    }};
}

fn button(world: &mut World, id: &str) -> Entity {
    world.spawn(LumenId(id.into())).id()
}

#[test]
fn clicks_route_tabs_rows_pick_and_edit_actions() {
    let mut world = click_world();
    let tab_signals = button(&mut world, "dt-tab-signals");
    let tab_network = button(&mut world, "dt-tab-network");
    let tab_elements = button(&mut world, "dt-tab-elements");
    let pick = button(&mut world, "dt-pick");
    let hide = button(&mut world, "dt-act-hide");
    let del = button(&mut world, "dt-act-del");
    let apply = button(&mut world, "dt-act-apply");
    let edit = world
        .spawn((LumenId("dt-edit".into()), TextContent("typed".into())))
        .id();
    let _ = edit;
    let target = world.spawn(TextContent("original".into())).id();
    let row = world.spawn(RowTarget(target.to_bits())).id();

    // Tab clicks switch the active tab.
    for (tab, want) in [
        (tab_signals, Tab::Signals),
        (tab_network, Tab::Network),
        (tab_elements, Tab::Elements),
    ] {
        click(&mut world, tab);
        run!(&mut world, handle_clicks);
        assert_eq!(world.resource::<DevtoolsState>().tab, want);
    }

    // A row click selects its element.
    click(&mut world, row);
    run!(&mut world, handle_clicks);
    assert_eq!(
        world.resource::<DevtoolsState>().selected,
        Some(target.to_bits())
    );

    // Apply writes the edit input's text into the selected element.
    click(&mut world, apply);
    run!(&mut world, handle_clicks);
    assert_eq!(world.get::<TextContent>(target).unwrap().0, "typed");
    assert!(
        world.get::<DirtyLayout>(target).is_some(),
        "edit relays out"
    );

    // Hide toggles visibility, seeding the component when absent.
    click(&mut world, hide);
    run!(&mut world, handle_clicks);
    assert!(!world.get::<Visible>(target).unwrap().0, "hidden");
    click(&mut world, hide);
    run!(&mut world, handle_clicks);
    assert!(world.get::<Visible>(target).unwrap().0, "shown again");

    // Pick arms, and the next app-side click selects and disarms.
    click(&mut world, pick);
    run!(&mut world, handle_clicks);
    assert!(world.resource::<DevtoolsState>().picking);
    let other = world.spawn(TextContent("other".into())).id();
    click(&mut world, other);
    run!(&mut world, handle_clicks);
    let state = *world.resource::<DevtoolsState>();
    assert_eq!(state.selected, Some(other.to_bits()));
    assert!(!state.picking, "picking disarms after the pick");

    // Delete despawns the selection and clears it.
    click(&mut world, del);
    run!(&mut world, handle_clicks);
    assert!(world.get_entity(other).is_err(), "picked entity despawned");
    assert_eq!(world.resource::<DevtoolsState>().selected, None);

    // A stale selection (entity already gone) makes every action a no-op.
    let stale = world.spawn(TextContent("stale".into())).id();
    world.resource_mut::<DevtoolsState>().selected = Some(stale.to_bits());
    world.despawn(stale);
    for b in [hide, apply, del] {
        click(&mut world, b);
        run!(&mut world, handle_clicks);
    }
}

#[test]
fn tabs_and_rows_style_by_state() {
    let mut world = World::new();
    world.insert_resource(DevtoolsState {
        visible: true,
        tab: Tab::Signals,
        selected: Some(7),
        picking: true,
    });

    // Active tab (Signals), hovered tab with a Visuals to update, armed
    // Pick button, and one without Visuals (seeded by the system).
    let active = world
        .spawn((LumenId("dt-tab-signals".into()), TextStyle::default()))
        .id();
    let hovered = world
        .spawn((
            LumenId("dt-tab-network".into()),
            TextStyle::default(),
            Visuals::default(),
            Hovered,
        ))
        .id();
    let pick = world
        .spawn((LumenId("dt-pick".into()), TextStyle::default()))
        .id();
    run!(&mut world, style_tabs);
    // Seeded on the entities that had none; underline only on the active.
    assert!(world.get::<Visuals>(active).unwrap().border.is_some());
    assert!(world.get::<Visuals>(pick).unwrap().border.is_some());
    assert!(world.get::<Visuals>(hovered).unwrap().fill.is_some());
    assert!(world.get::<Visuals>(hovered).unwrap().border.is_none());
    run!(&mut world, style_tabs); // second pass: the write-only-on-change arms
    world.resource_mut::<DevtoolsState>().tab = Tab::Network;
    run!(&mut world, style_tabs); // active flips: borders rewrite both ways

    let selected_row = world.spawn((RowTarget(7), Visuals::default())).id();
    let hovered_row = world
        .spawn((RowTarget(8), Visuals::default(), Hovered))
        .id();
    let plain_row = world.spawn((RowTarget(9), Visuals::default())).id();
    run!(&mut world, style_rows);
    assert!(world.get::<Visuals>(selected_row).unwrap().fill.is_some());
    assert!(world.get::<Visuals>(hovered_row).unwrap().fill.is_some());
    assert!(world.get::<Visuals>(plain_row).unwrap().fill.is_none());
}

#[test]
fn dock_inset_follows_the_panel_and_visibility() {
    let mut world = World::new();
    world.insert_resource(DockInsets::default());
    world.insert_resource(DevtoolsState {
        visible: true,
        ..Default::default()
    });
    world.spawn((
        LumenId(PANEL_ID.into()),
        Transform::new(Vec2::ZERO, Vec2::new(470.0, 900.0)),
    ));
    run!(&mut world, sync_dock_inset);
    assert_eq!(world.resource::<DockInsets>().right, 470.0);

    world.resource_mut::<DevtoolsState>().visible = false;
    run!(&mut world, sync_dock_inset);
    assert_eq!(world.resource::<DockInsets>().right, 0.0);
}

#[test]
fn highlight_box_and_tag_chip_track_the_hovered_row_target() {
    let mut world = World::new();
    world.insert_resource(DevtoolsState {
        visible: true,
        ..Default::default()
    });

    let target = world
        .spawn(Transform::new(
            Vec2::new(40.0, 300.0),
            Vec2::new(200.0, 30.0),
        ))
        .id();
    // The hovered row pointing at it.
    world.spawn((RowTarget(target.to_bits()), Hovered));
    let mut snap = Snapshot::default();
    let mut inspect = EntityInspect::default();
    inspect.tag = Some("label".into());
    inspect.lumen_id = Some("status".into());
    inspect.classes = vec!["big".into()];
    snap.inspect.insert(target.to_bits(), inspect);
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));

    let hl = world
        .spawn((HighlightBox, Visible(false), Transform::default()))
        .id();
    let tip = world
        .spawn((
            HighlightTip,
            Visible(false),
            Transform::default(),
            TextContent(String::new()),
        ))
        .id();

    run!(&mut world, update_highlight);
    let box_t = *world.get::<Transform>(hl).unwrap();
    assert!(world.get::<Visible>(hl).unwrap().0);
    assert_eq!(box_t.absolute, Vec2::new(40.0, 300.0));
    assert_eq!(box_t.size, Vec2::new(200.0, 30.0));
    let tip_text = world.get::<TextContent>(tip).unwrap().0.clone();
    assert!(tip_text.contains("<label>#status.big"), "{tip_text}");
    assert!(tip_text.contains("200x30"), "{tip_text}");
    let tip_t = *world.get::<Transform>(tip).unwrap();
    assert!(tip_t.absolute.y < 300.0, "chip sits above the box");

    // Closing the panel hides both.
    world.resource_mut::<DevtoolsState>().visible = false;
    run!(&mut world, update_highlight);
    assert!(!world.get::<Visible>(hl).unwrap().0);
    assert!(!world.get::<Visible>(tip).unwrap().0);
}

#[test]
fn highlight_corrects_for_scroll_and_flips_the_chip_below_at_the_top_edge() {
    use lumen_core::input::ScrollOffset;

    let mut world = World::new();
    world.insert_resource(DevtoolsState {
        visible: true,
        ..Default::default()
    });

    // A target near the top edge, inside a scrolled container: the box
    // subtracts the ancestor scroll, and the chip flips below the box.
    let scroller = world.spawn(ScrollOffset(Vec2::new(0.0, 50.0))).id();
    let target = world
        .spawn((
            Transform::new(Vec2::new(10.0, 60.0), Vec2::new(80.0, 20.0)),
            ChildOf(scroller),
        ))
        .id();
    world.insert_resource(DevtoolsState {
        visible: true,
        tab: Tab::Elements,
        selected: Some(target.to_bits()),
        ..Default::default()
    });
    let mut snap = Snapshot::default();
    snap.inspect
        .insert(target.to_bits(), EntityInspect::default());
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));

    let hl = world
        .spawn((HighlightBox, Visible(false), Transform::default()))
        .id();
    let tip = world
        .spawn((
            HighlightTip,
            Visible(false),
            Transform::default(),
            TextContent(String::new()),
        ))
        .id();

    run!(&mut world, update_highlight);
    let box_t = *world.get::<Transform>(hl).unwrap();
    assert_eq!(box_t.absolute, Vec2::new(10.0, 10.0), "scroll-corrected");
    let tip_t = *world.get::<Transform>(tip).unwrap();
    assert!(
        tip_t.absolute.y > box_t.absolute.y,
        "chip flips below near the top edge"
    );

    // No selection, no hover: the highlight hides.
    world.resource_mut::<DevtoolsState>().selected = None;
    run!(&mut world, update_highlight);
    assert!(!world.get::<Visible>(hl).unwrap().0);

    // Pick mode targets whatever app entity is under the pointer.
    world.resource_mut::<DevtoolsState>().picking = true;
    let picked = world
        .spawn((
            Transform::new(Vec2::new(5.0, 100.0), Vec2::new(30.0, 30.0)),
            Hovered,
        ))
        .id();
    let mut snap = Snapshot::default();
    snap.inspect
        .insert(picked.to_bits(), EntityInspect::default());
    world.insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    run!(&mut world, update_highlight);
    assert!(world.get::<Visible>(hl).unwrap().0, "pick hover highlights");
}

#[test]
fn f12_toggles_visibility_and_mirrors_the_root() {
    let mut world = World::new();
    world.init_resource::<Messages<KeyPressed>>();
    world.insert_resource(DevtoolsState::default());
    let root = world.spawn((DevtoolsRoot, Visible(false))).id();

    world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Character("F12".into()),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    run!(&mut world, toggle_devtools_on_f12);
    assert!(world.resource::<DevtoolsState>().visible);
    assert!(world.get::<Visible>(root).unwrap().0);
}

#[test]
fn color_palette_round_trips_through_bytes() {
    // The panel's const palette is compile-time evaluated; exercise the
    // runtime pair once so the conversions stay honest.
    let c = Color::from_rgba8([0x5d, 0xb0, 0xd7, 0xff]);
    assert_eq!(c.to_rgba8(), [0x5d, 0xb0, 0xd7, 0xff]);
}
