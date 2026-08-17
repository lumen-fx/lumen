//! What a build gets out of running an app.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bevy_ecs::system::{Res, ResMut};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::render_world::AnimationsActive;
use lumen_core::tick::{TickStage, work_pending};
use lumen_html::contract::{Seed, SeedValue};
use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_portable::portable_app;
use lumen_prerender::{Budget, Settled, page, settle};

/// The program the build script compiled: an `on_start` that publishes a
/// global and a list.
const SETTLES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/settles.cdlb"));

/// A program that asks for data over the network.
const FETCHES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fetches.cdlb"));

/// The external property bus belongs to the process, and a run empties it on
/// the way in, so two runs at once would eat each other's writes. Tests take
/// this in turn for the same reason a server renders one request at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn in_turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// An app carrying `program` as its only script.
fn app_with(program: &[u8]) -> CompiledApp {
    CompiledApp {
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(program.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

#[test]
fn what_the_app_publishes_is_what_the_page_is_written_with() {
    let _turn = in_turn();
    let run = page(&app_with(SETTLES), "index", &Seed::new(), Budget::default());

    assert_eq!(
        run.state.signals.global("greeting"),
        Some("hello from the build")
    );
    // A script's signal write reaches the store as text, whichever typed
    // setter wrote it, so that is what the seed carries it as.
    assert_eq!(run.state.seed.globals["count"], SeedValue::Str("2".into()));
    let rows = run
        .state
        .signals
        .rows("todos")
        .expect("the script published a list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["title"], "write it down");
    assert_eq!(run.state.seed.arrays["todos"][1]["id"], "2");
    assert!(run.state.skipped.is_empty());

    // An app that publishes on start has said everything it has to say within
    // a few frames of saying it; a budget's worth of ticks means something
    // never converged.
    match run.settled {
        Settled::At(ticks) => assert!(ticks < 8, "took {ticks} ticks to settle"),
        Settled::Capped(ticks) => panic!("never settled, {ticks} ticks in"),
    }
}

#[test]
fn the_page_is_written_where_the_run_was_asked_for() {
    let _turn = in_turn();
    let run = page(
        &app_with(SETTLES),
        "settings",
        &Seed::new(),
        Budget::default(),
    );
    assert_eq!(run.state.signals.global("route.path"), Some("settings"));
}

#[test]
fn a_declared_value_starts_the_run_and_the_app_writes_over_it() {
    let _turn = in_turn();
    let mut seed = Seed::new();
    seed.globals.insert(
        "greeting".to_string(),
        SeedValue::Str("declared".to_string()),
    );
    seed.globals.insert(
        "subtitle".to_string(),
        SeedValue::Str("declared".to_string()),
    );
    let run = page(&app_with(SETTLES), "index", &seed, Budget::default());

    assert_eq!(run.state.signals.global("subtitle"), Some("declared"));
    assert_eq!(
        run.state.signals.global("greeting"),
        Some("hello from the build")
    );
}

#[test]
fn an_address_the_build_would_not_ask_for_is_reported() {
    let _turn = in_turn();
    let run = page(&app_with(FETCHES), "index", &Seed::new(), Budget::default());

    assert_eq!(run.denied, vec!["https://example.invalid/items.json"]);
    // The refusal reached the app on the tick after the request, so it is
    // rendered with what it does without the network rather than mid-request.
    assert_eq!(run.state.signals.global("status"), Some("refused"));
}

#[test]
fn two_runs_of_one_page_agree() {
    let _turn = in_turn();
    let compiled = app_with(SETTLES);
    let first = page(&compiled, "index", &Seed::new(), Budget::default());
    let second = page(&compiled, "index", &Seed::new(), Budget::default());
    assert_eq!(first.state, second.state);
    assert_eq!(
        first
            .state
            .seed
            .to_script_json()
            .expect("a seed serializes"),
        second
            .state
            .seed
            .to_script_json()
            .expect("a seed serializes")
    );
}

/// Keeps the frame loop awake for as long as the app exists.
fn always_animating(animations: Res<AnimationsActive>) {
    animations.request();
}

#[test]
fn an_app_that_never_stops_drawing_still_settles() {
    let _turn = in_turn();
    let mut app = portable_app();
    app.world.init_resource::<AnimationsActive>();
    app.add_systems(TickStage::Systems, always_animating);

    let (_, settled) = settle(&mut app, Budget::default());
    assert!(
        matches!(settled, Settled::At(_)),
        "an animation is not state, so it must not hold a page open: {settled}"
    );
    assert!(
        work_pending(&app.world),
        "the frame predicate is the thing settling must not be built on"
    );
}

/// Writes a different value every tick, which is an app whose state has no
/// answer to arrive at.
fn never_the_same_twice(mut store: ResMut<PropertyStore>) {
    let next = match store.get(&PropertyKey::global("tick")) {
        Some(PropertyValue::I64(n)) => n + 1,
        _ => 0,
    };
    store.set(PropertyKey::global("tick"), PropertyValue::I64(next));
}

#[test]
fn an_app_that_never_arrives_is_capped_and_says_so() {
    let _turn = in_turn();
    let mut app = portable_app();
    app.add_systems(TickStage::Systems, never_the_same_twice);

    let budget = Budget {
        ticks: 8,
        time: Duration::from_secs(2),
    };
    let (state, settled) = settle(&mut app, budget);
    assert_eq!(settled, Settled::Capped(8));
    // What it had reached is still what the page holds.
    assert_eq!(state.seed.globals["tick"], SeedValue::I64(7));
}

#[test]
fn a_run_is_bounded_in_time_as_well_as_in_ticks() {
    let _turn = in_turn();
    let mut app = portable_app();
    app.add_systems(TickStage::Systems, never_the_same_twice);

    let budget = Budget {
        ticks: u32::MAX,
        time: Duration::from_millis(50),
    };
    let (_, settled) = settle(&mut app, budget);
    assert!(matches!(settled, Settled::Capped(_)));
}

/// The app is built and dropped wherever its caller runs, so a build that
/// renders pages on a worker is not a special case.
#[test]
fn a_run_belongs_to_no_thread() {
    let _turn = in_turn();
    std::thread::spawn(|| {
        let run = page(&app_with(SETTLES), "index", &Seed::new(), Budget::default());
        assert_eq!(run.state.signals.global("count"), Some("2"));
    })
    .join()
    .expect("the run finished on the worker");
}

#[test]
fn an_engine_this_build_cannot_run_is_named() {
    let _turn = in_turn();
    let compiled = CompiledApp {
        scripts: vec![CompiledScript {
            engine: "elvish".to_string(),
            source: String::new(),
            bytecode: Some(vec![0]),
        }],
        ..CompiledApp::default()
    };
    let run = page(&compiled, "index", &Seed::new(), Budget::default());
    assert_eq!(run.unsupported_engines, vec!["elvish"]);
}
