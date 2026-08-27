// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! End-to-end proof that the `counter` template runs as scaffolded.
//!
//! `lumenc new <dir> counter` writes a candela script, and the getting-started
//! walkthrough quotes it verbatim. This test writes the template to a temp dir
//! exactly as the scaffolder does, boots it through the headless plugin stack
//! (no window, no GPU), clicks `bump` and `reset`, and asserts the `bind-text`
//! label follows the `clicks` signal. The script looks each button up with
//! `get_by_id` from `on_ready` and binds its click handler on the node handle,
//! so a template whose script fails to compile, whose `on_ready` runs too early
//! to see the mounted tree, or whose bindings never reach the dispatcher leaves
//! the label on its markup default and fails here.

use bevy_ecs::prelude::Entity;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent};
use lumen_core::input::{ClickEvent, PointerButton};
use lumenc::{RunOptions, build_headless_app};

/// Write the `counter` template into a temp dir of this case's own, with the
/// MCP server disabled (`port = 0`) so the test never binds a TCP port.
///
/// `case` keeps two tests apart: they run on threads of one process, and each
/// removes its directory when it is done, so a shared name lets one delete the
/// app the other is still reading.
///
/// The files come from the copy of the gallery the toolchain ships, which
/// `tools/fetch-templates.sh` downloads. A checkout that has not run the
/// script has nothing to scaffold, so the case says what it needs and returns;
/// CI fetches before it tests.
fn scaffolded_counter(case: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "lumenc-counter-template-{}-{case}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(why) = lumenc::scaffold::write_template("counter", &dir) {
        eprintln!("skipping: {why}");
        return None;
    }
    // Same [app] / [window] config the template ships, plus the port pin.
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Counter\"\nsize = [480, 360]\n\n\
         [mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    Some(dir)
}

/// Every `TextContent` string currently in the world.
fn all_texts(app: &mut App) -> Vec<String> {
    let mut q = app.world.query::<&TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

/// Simulate a click on the entity carrying `LumenId(id)`.
fn click_on(app: &mut App, id: &str) {
    let target = {
        let mut q = app.world.query::<(Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, lid)| lid.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity with LumenId {id:?}"))
    };
    app.world.write_message(ClickEvent {
        entity: target,
        position: glam::Vec2::ZERO,
        button: PointerButton::Primary,
    });
}

#[test]
fn scaffolded_counter_counts_clicks() {
    // The full app build + cascade + taffy layout recurses deeper than a
    // default 2 MiB test-thread stack; run the case on a roomy stack (the
    // windowed path runs on the 8 MiB main thread).
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run_case)
        .expect("spawn test thread")
        .join()
        .expect("counter template case");
}

fn run_case() {
    let Some(dir) = scaffolded_counter("clicks") else {
        return;
    };
    let (mut app, _window) = build_headless_app(RunOptions::new(dir.clone())).expect("build app");

    // A few ticks so the tree mounts, the candela `on_ready` fires, its
    // `signal_set_int` and event-bind commands drain, and the `bind-text`
    // reader mirrors the signal into the label.
    for _ in 0..5 {
        app.tick();
    }

    // The candela host owns the script: a `.cdl` file selects it with no
    // `[script] engine` line in lumen.toml.
    assert!(
        app.world
            .get_resource::<lumen_script_candela::CandelaHost>()
            .is_some(),
        "the counter template's main.cdl did not select the candela host"
    );

    click_on(&mut app, "bump");
    app.tick();
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "1"),
        "clicking bump did not raise the clicks signal; TextContents = {texts:?}"
    );

    // A rapid second click on the same entity folds into a double-click, so
    // reset is the next thing to drive.
    click_on(&mut app, "reset");
    app.tick();
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "0"),
        "clicking reset did not clear the clicks signal; TextContents = {texts:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same click, on an app carrying extra systems, must still land in one
/// tick.
///
/// A `node.on("click", ...)` handler's signal write travels to the store as a
/// `ScriptCommandEvent`, and a message a reader misses waits a whole tick.
/// The applier's ordering against the DOM dispatcher used to be left to the
/// scheduler, so which side won depended on the shape of the system graph:
/// one more system anywhere in `TickStage::Systems` was enough to lose the
/// click, and nothing rescheduled a tick to recover it, because the write had
/// not reached the store for the end-of-tick wake to notice. Padding the
/// graph here keeps that edge honest.
#[test]
fn click_lands_in_one_tick_with_extra_systems_installed() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run_padded_case)
        .expect("spawn test thread")
        .join()
        .expect("padded counter case");
}

fn run_padded_case() {
    use lumen_core::prelude::TickStage;

    let Some(dir) = scaffolded_counter("padded") else {
        return;
    };
    let (mut app, _window) = build_headless_app(RunOptions::new(dir.clone())).expect("build app");
    for _ in 0..8 {
        app.add_systems(TickStage::Systems, || {});
        app.add_systems(TickStage::Systems, |_: bevy_ecs::prelude::Commands| {});
    }
    for _ in 0..5 {
        app.tick();
    }

    click_on(&mut app, "bump");
    app.tick();
    let texts = all_texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "1"),
        "the click needed more than one tick once the schedule grew; \
         TextContents = {texts:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The applier runs after the DOM dispatch because an edge says so, not
/// because the executor sorted it that way.
///
/// Padding the stage catches an order a bigger graph would flip, but only the
/// flips that padding happens to produce; an order that survives every
/// padding a test thought to try is still an order nobody asked for. So this
/// asks the schedule rather than the app. It offers a system that has to run
/// after the applier and before the dispatch, which the builder can only
/// refuse if it already knows a path the other way, and the refusal is that
/// path.
#[test]
fn the_dom_dispatch_reaches_the_applier_through_the_graph() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run_edge_case)
        .expect("spawn test thread")
        .join()
        .expect("applier edge case");
}

fn run_edge_case() {
    use bevy_ecs::prelude::{IntoScheduleConfigs, Schedules};
    use lumen_core::app::Tick;
    use lumen_core::prelude::TickStage;
    use lumen_scene::script_commands::apply_scene_script_commands;
    use lumen_script::ScriptSet;

    let Some(dir) = scaffolded_counter("edge") else {
        return;
    };
    let (mut app, _window) = build_headless_app(RunOptions::new(dir.clone())).expect("build app");
    // One tick so every plugin has registered and the graph is whole.
    app.tick();

    let mut schedules = app
        .world
        .remove_resource::<Schedules>()
        .expect("the Tick schedule is installed by App::new");
    let schedule = schedules.get_mut(Tick).expect("the Tick schedule");
    schedule.add_systems(
        (|| {})
            .after(apply_scene_script_commands)
            .before(ScriptSet::DomInput)
            .in_set(TickStage::Systems),
    );
    let outcome = schedule.initialize(&mut app.world);
    app.world.insert_resource(schedules);

    assert!(
        outcome.is_err(),
        "the scene applier is not ordered after ScriptSet::DomInput, so a \
         handler's signal write reaches the store whenever the executor \
         feels like it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
