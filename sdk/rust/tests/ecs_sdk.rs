//! Integration tests for the SDK surface on a bare app: plugin groups, systems
//! with seeded signals, the typed `Signals` param, and the handlers on ids.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use lumenui::plugins::PluginGroupBuilder;
use lumenui::prelude::*;

// -- Dummy plugins that record their installation via marker resources --------

#[derive(Resource)]
struct PhysicsInstalled;
#[derive(Resource)]
struct TelemetryInstalled;

struct Physics;
impl Plugin for Physics {
    fn build(self, app: &mut EcsApp) {
        app.world.insert_resource(PhysicsInstalled);
    }
}

struct Telemetry;
impl Plugin for Telemetry {
    fn build(self, app: &mut EcsApp) {
        app.world.insert_resource(TelemetryInstalled);
    }
}

#[test]
fn plugin_group_installs_enabled_entries() {
    let mut app = EcsApp::new();
    PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Telemetry)
        .finish(&mut app);
    assert!(app.is_plugin_added::<Physics>());
    assert!(app.is_plugin_added::<Telemetry>());
    assert!(app.world.get_resource::<PhysicsInstalled>().is_some());
    assert!(app.world.get_resource::<TelemetryInstalled>().is_some());
}

#[test]
fn plugin_group_disable_skips_entry() {
    let mut app = EcsApp::new();
    PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Telemetry)
        .disable::<Telemetry>()
        .finish(&mut app);
    assert!(app.is_plugin_added::<Physics>());
    assert!(!app.is_plugin_added::<Telemetry>());
    assert!(app.world.get_resource::<TelemetryInstalled>().is_none());
}

#[test]
fn plugin_group_enabled_names_reflect_disable() {
    let names: Vec<_> = PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Telemetry)
        .disable::<Physics>()
        .enabled_names()
        .collect();
    assert_eq!(names.len(), 1);
}

/// A bare app: what the tests below tick by hand.
fn bare() -> lumenui::App {
    lumenui::App::from_markup("<root/>")
}

// -- build_bare: user systems + seeds on a real, tickable ECS app -------------

#[derive(Resource, Default)]
struct Observed(i64);

fn read_seed(signals: Signals, mut out: ResMut<Observed>) {
    out.0 = signals.get_or::<i64>("count", -1);
}

#[test]
fn build_bare_runs_user_systems_with_seeded_signals() {
    let mut app = bare()
        .add_plugin(RecordCountPlugin)
        .signal("count", 41i64)
        .add_systems(TickStage::Systems, read_seed)
        .build_bare();

    app.tick();
    assert_eq!(app.world.resource::<Observed>().0, 41);
}

struct RecordCountPlugin;
impl Plugin for RecordCountPlugin {
    fn build(self, app: &mut EcsApp) {
        app.world.insert_resource(Observed::default());
    }
}

// -- Signals typed round-trip in a real system --------------------------------

fn write_then_bump(mut signals: Signals) {
    let n = signals.get_or::<i64>("n", 0);
    signals.set("n", n + 1);
}

#[test]
fn signals_param_reads_and_writes_typed() {
    let mut app = bare()
        .signal("n", 10i64)
        .add_systems(TickStage::Systems, write_then_bump)
        .build_bare();

    app.tick();
    app.tick();
    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("n").get(store), Some(12));
}

// -- The signals! handle-struct macro -----------------------------------------

signals! {
    /// Handles for the counter app.
    pub struct Counter {
        count: i64,
        label: String,
    }
}

#[test]
fn signals_macro_mints_typed_handles() {
    let mut store = PropertyStore::default();
    Counter::count().set(&mut store, 7);
    Counter::label().set(&mut store, "hi".to_string());
    assert_eq!(Counter::count().get(&store), Some(7));
    assert_eq!(Counter::label().get(&store).as_deref(), Some("hi"));
}

// -- Handlers on ids ------------------------------------------------------------

fn click(app: &mut EcsApp, entity: Entity) {
    app.world.write_message(ClickEvent {
        entity,
        position: Vec2::ZERO,
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
}

fn hit(app: &EcsApp) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str("hit")
        .map(|s| s.to_string())
}

/// A click on the id fires the handler, and its signal write lands on the
/// same tick.
#[test]
fn on_click_fires_and_writes_signal() {
    let mut app = bare()
        .signal("count", 0i64)
        .on_click("go", |ui, _| ui.update("count", |n: i64| n + 1))
        .build_bare();

    let go = app.world.spawn(LumenId("go".to_string())).id();
    click(&mut app, go);

    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("count").get(store), Some(1));
}

/// `on_any_click` takes what no handler on an id takes, and reads the
/// nearest id; a handler on an id wins for clicks inside it.
#[test]
fn on_any_click_takes_the_rest_and_an_id_handler_wins_inside_it() {
    let mut app = bare()
        .on_any_click(|ui, ev| ui.set("hit", format!("any {}", ev.id().unwrap_or("-"))))
        .on_click("special", |ui, _| ui.set("hit", "special"))
        .build_bare();

    let special = app.world.spawn(LumenId("special".to_string())).id();
    let icon = app.world.spawn(ChildOf(special)).id();
    click(&mut app, icon);
    assert_eq!(hit(&app).as_deref(), Some("special"));

    let panel = app.world.spawn(LumenId("panel".to_string())).id();
    click(&mut app, panel);
    assert_eq!(hit(&app).as_deref(), Some("any panel"));

    let plain = app.world.spawn_empty().id();
    click(&mut app, plain);
    assert_eq!(hit(&app).as_deref(), Some("any -"));
}

/// The app's resources are a handler's state.
#[test]
fn a_handler_changes_the_apps_resources() {
    #[derive(Resource, Default)]
    struct Tool(&'static str);

    let mut app = bare()
        .insert_resource(Tool::default())
        .on_click("pen", |ui, _| ui.resource_mut::<Tool>().0 = "pen")
        .build_bare();
    let pen = app.world.spawn(LumenId("pen".to_string())).id();
    click(&mut app, pen);
    assert_eq!(app.world.resource::<Tool>().0, "pen");
}

/// A system added with `add_systems` runs after the handlers, so it sees on
/// the same tick what a handler changed.
#[test]
fn systems_see_what_a_handler_changed_on_its_tick() {
    #[derive(Resource, Default)]
    struct Tool(&'static str);

    fn show(tool: Res<Tool>, mut signals: Signals) {
        signals.set("tool_label", tool.0);
    }

    let mut app = bare()
        .insert_resource(Tool("brush"))
        .add_systems(TickStage::Systems, show)
        .on_click("eraser", |ui, _| ui.resource_mut::<Tool>().0 = "eraser")
        .build_bare();
    let eraser = app.world.spawn(LumenId("eraser".to_string())).id();
    click(&mut app, eraser);
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("tool_label")
            .as_deref(),
        Some("eraser")
    );
}

/// A list a handler writes is the array signal `<for each>` renders.
#[test]
fn a_handler_writes_a_list() {
    let mut app = bare()
        .on_click("fill", |ui, _| {
            ui.set_list(
                "rows",
                (1..=3).map(|i| Record::new().with("id", i).with("name", format!("row {i}"))),
            );
            ui.push_row("rows", Record::new().with("id", 4));
            let n = ui.list_len("rows") as i64;
            ui.set("rows_len", n);
        })
        .build_bare();
    let fill = app.world.spawn(LumenId("fill".to_string())).id();
    click(&mut app, fill);
    let arrays = app
        .world
        .resource::<lumenui::lumen_core::signals::ArraySignals>();
    let rows = arrays.get("rows").expect("the list was written");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[1].get("name").map(String::as_str), Some("row 2"));
    assert_eq!(
        Property::<i64>::new("rows_len").get(app.world.resource::<PropertyStore>()),
        Some(4)
    );
}

/// A menu item's handler is keyed on the item's id.
#[test]
fn on_menu_takes_the_items_id() {
    let mut app = bare()
        .on_menu("file-save", |ui, ev| {
            ui.set("hit", ev.id().unwrap_or("-"));
        })
        .build_bare();
    app.world.write_message(MenuClicked {
        id: "file-save".to_string(),
    });
    app.tick();
    assert_eq!(hit(&app).as_deref(), Some("file-save"));
}

// -- add_computed derives a signal from other signals -------------------------

#[test]
fn add_computed_recomputes_from_inputs() {
    let mut app = bare()
        .signal("count", 3i64)
        .add_computed("doubled", |s| s.get_or::<i64>("count", 0) * 2)
        .build_bare();

    app.tick();
    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("doubled").get(store), Some(6));
}

// -- Signals ergonomic helpers: update / toggle --------------------------------

fn bump_update(mut signals: Signals) {
    signals.update::<i64>("n", |n| n + 1);
}

#[test]
fn signals_update_read_modify_writes() {
    let mut app = bare()
        .signal("n", 5i64)
        .add_systems(TickStage::Systems, bump_update)
        .build_bare();

    app.tick();
    app.tick();
    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("n").get(store), Some(7));
}

fn flip(mut signals: Signals) {
    signals.toggle("on");
}

#[test]
fn signals_toggle_flips_bool() {
    let mut app = bare().add_systems(TickStage::Systems, flip).build_bare();

    app.tick();
    assert_eq!(
        app.world.resource::<PropertyStore>().get_global_bool("on"),
        Some(true)
    );
    app.tick();
    assert_eq!(
        app.world.resource::<PropertyStore>().get_global_bool("on"),
        Some(false)
    );
}
