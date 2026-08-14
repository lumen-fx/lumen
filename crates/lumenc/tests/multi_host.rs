// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! An app can ship more than one script language. Each `<script src>` file
//! joins its extension's host, the hosts run side by side, and they reach each
//! other only through the shared signal bus.
//!
//! `fixtures/multi-host` pairs `model.cdl` (writes the `shared` signal) with
//! `report.lua` (derives `seen_by_lua` from it without ever writing it), so a
//! passing run proves both hosts loaded, both dispatched their lifecycle
//! callbacks, and a value crossed from one language to the other.

use lumen_core::prelude::App;
use lumenc::{RunOptions, build_headless_app};

fn app_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("fixtures/{name} must exist: {e}"))
}

fn run_ticks(dir: std::path::PathBuf, ticks: u32) -> App {
    let (mut app, _window) = build_headless_app(RunOptions::new(dir)).expect("build_headless_app");
    for _ in 0..ticks {
        app.tick();
    }
    app
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<lumen_core::property_store::PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

fn label_text(app: &mut App, id: &str) -> Option<String> {
    use lumen_core::components::{LumenId, TextContent};
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0.as_str() == id)
        .map(|(_, t)| t.0.clone())
}

/// Both hosts load, both run their `on_start` / `on_ready`, and a signal
/// written in candela is read from Lua.
#[test]
fn two_language_app_runs_both_hosts_over_one_signal_bus() {
    let mut app = run_ticks(app_dir("multi-host"), 5);

    assert_eq!(
        signal(&app, "shared").as_deref(),
        Some("candela"),
        "model.cdl's on_start must have run"
    );
    assert_eq!(
        signal(&app, "seen_by_lua").as_deref(),
        Some("candela+lua"),
        "report.lua's derivation must have recomputed from the signal candela wrote"
    );
    assert_eq!(
        signal(&app, "candela_ready").as_deref(),
        Some("1"),
        "the candela host must have dispatched on_ready"
    );
    assert_eq!(
        signal(&app, "lua_ready").as_deref(),
        Some("1"),
        "the lua host must have dispatched on_ready too, not just the first host"
    );
    assert_eq!(
        label_text(&mut app, "seen-label").as_deref(),
        Some("candela+lua"),
        "the cross-language value must reach a bind-text label"
    );
}

/// Every active host re-arms `on_ready` independently, the way hot reload does
/// after respawning the tree.
#[test]
fn on_ready_latch_is_per_host() {
    let mut app = run_ticks(app_dir("multi-host"), 5);

    let fired = app.world.resource::<lumen_script::OnReadyFired>();
    let mut langs: Vec<&str> = fired.0.iter().copied().collect();
    langs.sort_unstable();
    assert_eq!(
        langs,
        vec!["candela", "lua"],
        "each active host latches its own on_ready"
    );

    // Re-arm the way hot reload does; both hosts must dispatch again.
    app.world
        .resource_mut::<lumen_script::OnReadyFired>()
        .0
        .clear();
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("candela_ready", "0");
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("lua_ready", "0");
    for _ in 0..5 {
        app.tick();
    }

    assert_eq!(signal(&app, "candela_ready").as_deref(), Some("1"));
    assert_eq!(signal(&app, "lua_ready").as_deref(), Some("1"));
}

/// Hot reload swaps each host's program with its own language's source, and the
/// carry-forward of handlers and derivations survives per host.
#[test]
fn hot_reload_replaces_each_host_with_its_own_language() {
    let dir = app_dir("multi-host");
    let mut app = run_ticks(dir.clone(), 5);
    assert_eq!(signal(&app, "seen_by_lua").as_deref(), Some("candela+lua"));

    let candela_src = std::fs::read_to_string(dir.join("model.cdl")).expect("read model.cdl");
    let lua_src = std::fs::read_to_string(dir.join("report.lua")).expect("read report.lua");

    // Regrouping by language is what makes reload work at all: handing one host
    // the other language's source is a compile error, which is what a
    // single-blob reload produced for every mixed app.
    assert!(
        lumen_script::reload_script::<lumen_script_candela::CandelaHost>(
            &mut app.world,
            &lua_src,
            "<inline>",
        )
        .expect("the candela host is installed")
        .is_err(),
        "lua source must not compile under the candela host"
    );

    let reloads = [
        (
            "candela",
            lumen_script::reload_script::<lumen_script_candela::CandelaHost>(
                &mut app.world,
                &candela_src,
                "<inline>",
            ),
        ),
        (
            "lua",
            lumen_script::reload_script::<lumen_script_lua::LuaHost>(
                &mut app.world,
                &lua_src,
                "<inline>",
            ),
        ),
    ];
    for (name, result) in reloads {
        let outcome = result.unwrap_or_else(|| panic!("the {name} host is installed"));
        outcome.unwrap_or_else(|e| panic!("{name} reload failed: {e}"));
    }

    // Re-arm the way the hot-reload sweep does, then drive a fresh value
    // through the bus: the Lua derivation registered before the reload must
    // still recompute from the signal candela owns.
    app.world
        .resource_mut::<lumen_script::OnReadyFired>()
        .0
        .clear();
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("shared", "reloaded");
    for _ in 0..5 {
        app.tick();
    }

    assert_eq!(
        signal(&app, "seen_by_lua").as_deref(),
        Some("reloaded+lua"),
        "the lua derivation must carry forward across its own host's reload"
    );
    assert_eq!(
        signal(&app, "candela_ready").as_deref(),
        Some("1"),
        "on_ready must re-arm on the candela host"
    );
    assert_eq!(
        signal(&app, "lua_ready").as_deref(),
        Some("1"),
        "on_ready must re-arm on the lua host"
    );
}

/// `[script] engine` still collapses an app onto one host. `fixtures/lua-smoke`
/// keeps its script inline and declares `engine = "lua"`, so exactly the Lua
/// host runs and the single-host tick order is unchanged.
#[test]
fn engine_override_forces_one_host() {
    let mut app = run_ticks(app_dir("lua-smoke"), 5);

    let fired = app.world.resource::<lumen_script::OnReadyFired>();
    let langs: Vec<&str> = fired.0.iter().copied().collect();
    assert_eq!(
        langs,
        vec!["lua"],
        "the override installs the lua host alone"
    );
    assert_eq!(
        label_text(&mut app, "counter-label").as_deref(),
        Some("Lua host - clicks: 0"),
        "the single-host derivation path is unchanged"
    );
}

/// A single-language app with an external `.cdl` file behaves exactly as it did
/// under one-host-per-app selection.
#[test]
fn single_language_app_is_unchanged() {
    let mut app = run_ticks(app_dir("candela-smoke"), 5);

    let fired = app.world.resource::<lumen_script::OnReadyFired>();
    let langs: Vec<&str> = fired.0.iter().copied().collect();
    assert_eq!(langs, vec!["candela"]);
    assert_eq!(
        label_text(&mut app, "greeting-label").as_deref(),
        Some("candela host - ready"),
    );
}
