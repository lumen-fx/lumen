// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! An app can ship more than one script language. Each `<script src>` file
//! joins its extension's host, the hosts run side by side, and they reach each
//! other only through the shared signal bus.
//!
//! The two-language app pairs `model.cdl` (writes the `shared` signal) with
//! `report.lua` (derives `seen_by_lua` from it without ever writing it), so a
//! passing run proves both hosts loaded, both dispatched their lifecycle
//! callbacks, and a value crossed from one language to the other. Lua is not
//! compiled into anything by default; this file links its module so the
//! second host is there to load.

use lumen_core::prelude::App;
use lumen_lua as _;
use lumenc::{RunOptions, build_headless_app};

/// The two-language app: a candela program that owns the `shared` signal and
/// a Lua program that reads it without ever writing it.
const MULTI_HOST: &[(&str, &str)] = &[
    ("lumen.toml", "[mcp]\nport = 0\n"),
    (
        "src/main.lmn",
        r#"<root>
  <label id="shared-label" width="100%" padding="24 0 8 0"
         bind-text="shared" text="waiting" />
  <label id="seen-label" width="100%" padding="0 0 24 0"
         bind-text="seen_by_lua" text="waiting" />
  <script src="model.cdl" />
  <script src="report.lua" />
</root>
"#,
    ),
    (
        "src/model.cdl",
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set("shared", "candela");
}

fn on_ready() {
    lumen::signal_set("candela_ready", "1");
}

fn main() {}
"#,
    ),
    (
        "src/report.lua",
        r#"-- `shared` is named as a string dep so this program never seeds it; the
-- value comes from model.cdl through the signal bus.

function on_start()
    derive("seen_by_lua", { "shared" }, function(v)
        return tostring(v) .. "+lua"
    end)
end

function on_ready()
    signal("lua_ready", ""):set("1")
end
"#,
    ),
];

/// A one-file Lua app whose `[script] engine` pins the Lua host for its
/// inline script.
const LUA_SMOKE: &[(&str, &str)] = &[
    (
        "lumen.toml",
        "[script]\nengine = \"lua\"\n\n[mcp]\nport = 0\n",
    ),
    (
        "src/main.lmn",
        r#"<root>
  <label id="counter-label" width="100%" height="100%" padding="30 0 24 0"
         bind-text="counter_label"
         text="Lua host - waiting" />
  <script>
    function bump(by)
        local clicks = signal("clicks", 0)
        clicks:set(clicks:get() + by)
    end

    function handle_reset_click(id)
        signal("clicks", 0):set(0)
    end

    function on_start()
        local clicks = signal("clicks", 0)
        derive("counter_label", { clicks }, function(n)
            return "Lua host - clicks: " .. n
        end)
        on("click", "reset", "handle_reset_click")
    end

    function on_click(id)
        bump(1)
    end
  </script>
</root>
"#,
    ),
];

/// The app named `name`: the two apps above are written into a fresh temp
/// directory, anything else is read from the in-repo `fixtures/`.
fn app_dir(name: &str) -> std::path::PathBuf {
    let files = match name {
        "multi-host" => MULTI_HOST,
        "lua-smoke" => LUA_SMOKE,
        _ => {
            return std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures")
                .join(name)
                .canonicalize()
                .unwrap_or_else(|e| panic!("fixtures/{name} must exist: {e}"));
        }
    };
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumen_multi_host_{name}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, body) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a file sits in a directory"))
            .expect("create app dir");
        std::fs::write(&path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    dir
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

    let src = dir.join("src");
    let candela_src = std::fs::read_to_string(src.join("model.cdl")).expect("read model.cdl");
    let lua_src = std::fs::read_to_string(src.join("report.lua")).expect("read report.lua");

    // Regrouping by language is what makes reload work at all: handing one host
    // the other language's source is a compile error, which is what a
    // single-blob reload produced for every mixed app.
    assert!(
        lumen_script::reload_script::<lumen_candela_dev::CandelaHost>(
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
            lumen_script::reload_script::<lumen_candela_dev::CandelaHost>(
                &mut app.world,
                &candela_src,
                "<inline>",
            ),
        ),
        (
            "lua",
            lumen_script::reload_script::<lumen_lua::LuaHost>(&mut app.world, &lua_src, "<inline>"),
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

/// `[script] engine` still collapses an app onto one host. The Lua smoke app
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

/// `set_color_scheme` reaches the [`StyleManager`] from a script. candela
/// carries it in its prelude under the `lumen` namespace, so a host losing the
/// registration fails here rather than in a themed app.
#[test]
fn set_color_scheme_applies_from_a_script() {
    use lumen_core::components::{ColorScheme, StyleManager};

    let dir = std::env::temp_dir().join(format!("lumen_scheme_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(
        src.join("main.lmn"),
        "<root>\n  <label id=\"only\" text=\"scheme\"/>\n  \
         <script src=\"main.cdl\"/>\n</root>",
    )
    .unwrap();
    std::fs::write(
        src.join("main.cdl"),
        "import \"lumen.cdl\";\n\
         fn on_ready() { lumen::set_color_scheme(\"force-dark\"); }\n\
         fn main() {}\n",
    )
    .unwrap();

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..6 {
        app.tick();
    }

    let style = *app.world.resource::<StyleManager>();
    assert_eq!(
        style.scheme,
        ColorScheme::ForceDark,
        "set_color_scheme should reach StyleManager"
    );
    assert!(
        style.effective_dark,
        "forcing dark should light up effective_dark"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A handler bound from native code (the C ABI, the Rust SDK) runs once per
/// event, however many script hosts the app runs. The DOM event dispatch is
/// one pass over the binding registry; the hosts are reached from inside it.
#[test]
fn a_native_handler_fires_once_per_event_with_two_hosts() {
    use lumen_core::components::LumenId;
    use lumen_core::prelude::{ClickEvent, PointerButton};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut app = run_ticks(app_dir("multi-host"), 5);
    let target = {
        let mut q = app.world.query::<(bevy_ecs::prelude::Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, id)| id.0.as_str() == "shared-label")
            .map(|(e, _)| e)
            .expect("the fixture has a shared-label")
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let token = lumen_script::event::register_native_binding(
        lumen_core::node::NodeHandle::new(target).pack(),
        "click".to_string(),
        false,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    );
    app.world.write_message(ClickEvent {
        entity: target,
        position: glam::Vec2::new(4.0, 4.0),
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
    lumen_script::event::unregister_binding(token);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one click reaches a native handler once, not once per script host"
    );
}
