//! Integration tests for the ECS-first SDK surface: plugin-group composition,
//! bare-app assembly, and the typed `Signals` param.

use lumenui::ecs_app::{App as EcsApp, Plugin};
use lumenui::plugins::PluginGroupBuilder;
use lumenui::prelude::*;

// -- Dummy plugins that record their installation via marker resources --------

#[derive(Resource)]
struct PhysicsInstalled;
#[derive(Resource)]
struct AudioInstalled;

struct Physics;
impl Plugin for Physics {
    fn build(self, app: &mut EcsApp) {
        app.world.insert_resource(PhysicsInstalled);
    }
}

struct Audio;
impl Plugin for Audio {
    fn build(self, app: &mut EcsApp) {
        app.world.insert_resource(AudioInstalled);
    }
}

#[test]
fn plugin_group_installs_enabled_entries() {
    let mut app = EcsApp::new();
    PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Audio)
        .finish(&mut app);
    assert!(app.is_plugin_added::<Physics>());
    assert!(app.is_plugin_added::<Audio>());
    assert!(app.world.get_resource::<PhysicsInstalled>().is_some());
    assert!(app.world.get_resource::<AudioInstalled>().is_some());
}

#[test]
fn plugin_group_disable_skips_entry() {
    let mut app = EcsApp::new();
    PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Audio)
        .disable::<Audio>()
        .finish(&mut app);
    assert!(app.is_plugin_added::<Physics>());
    assert!(!app.is_plugin_added::<Audio>());
    assert!(app.world.get_resource::<AudioInstalled>().is_none());
}

#[test]
fn plugin_group_enabled_names_reflect_disable() {
    let names: Vec<_> = PluginGroupBuilder::new("test")
        .add(Physics)
        .add(Audio)
        .disable::<Physics>()
        .enabled_names()
        .collect();
    assert_eq!(names.len(), 1);
}

// -- build_bare: user systems + seeds on a real, tickable ECS app -------------

#[derive(Resource, Default)]
struct Observed(i64);

fn read_seed(signals: Signals, mut out: ResMut<Observed>) {
    out.0 = signals.get_or::<i64>("count", -1);
}

#[test]
fn build_bare_runs_user_systems_with_seeded_signals() {
    let mut app = lumenui::App::new()
        .add_plugin(RecordCountPlugin)
        .insert_signal("count", 41i64)
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
    let mut app = lumenui::App::new()
        .insert_signal("n", 10i64)
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

// -- Terse native handlers on the ECS-first App -------------------------------

/// `App::on_click(id, closure)` installs the dispatch pipeline; a click on the
/// matching id fires the closure and its signal write lands same-tick.
#[test]
fn on_click_closure_fires_and_writes_signal() {
    use lumenui::prelude::*;

    let mut app = lumenui::App::new()
        .insert_signal("count", 0i64)
        .on_click("go", |ctx| {
            let n = ctx.get_or::<i64>("count", 0) + 1;
            ctx.set("count", n);
        })
        .build_bare();

    let go = app.world.spawn(LumenId("go".to_string())).id();
    app.world.write_message(ClickEvent {
        entity: go,
        position: glam::Vec2::ZERO,
        button: lumenui::input::PointerButton::Primary,
    });
    app.tick();

    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("count").get(store), Some(1));
}

/// `on_any_click` is the wildcard fallback; a per-id `on_click` overrides it.
#[test]
fn on_any_click_is_wildcard_and_per_id_overrides() {
    use lumenui::prelude::*;

    let mut app = lumenui::App::new()
        .on_any_click(|ctx| ctx.set("hit", "wildcard"))
        .on_click("special", |ctx| ctx.set("hit", "special"))
        .build_bare();

    let special = app.world.spawn(LumenId("special".to_string())).id();
    app.world.write_message(ClickEvent {
        entity: special,
        position: glam::Vec2::ZERO,
        button: lumenui::input::PointerButton::Primary,
    });
    app.tick();
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("hit")
            .as_deref(),
        Some("special")
    );

    let plain = app.world.spawn_empty().id();
    app.world.write_message(ClickEvent {
        entity: plain,
        position: glam::Vec2::ZERO,
        button: lumenui::input::PointerButton::Primary,
    });
    app.tick();
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("hit")
            .as_deref(),
        Some("wildcard")
    );
}

// -- add_computed derives a signal from other signals -------------------------

#[test]
fn add_computed_recomputes_from_inputs() {
    let mut app = lumenui::App::new()
        .insert_signal("count", 3i64)
        .add_computed("doubled", |s| s.get_or::<i64>("count", 0) * 2)
        .build_bare();

    app.tick();
    let store = app.world.resource::<PropertyStore>();
    assert_eq!(Property::<i64>::new("doubled").get(store), Some(6));
}

// -- Signals ergonomic helpers: update / update_or / toggle -------------------

fn bump_update(mut signals: Signals) {
    signals.update::<i64>("n", |n| n + 1);
}

#[test]
fn signals_update_read_modify_writes() {
    let mut app = lumenui::App::new()
        .insert_signal("n", 5i64)
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
    let mut app = lumenui::App::new()
        .add_systems(TickStage::Systems, flip)
        .build_bare();

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
