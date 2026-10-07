// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Which script host an app gets, and what happens when none is there.
//!
//! Each script host is a runtime module, and an app run from source loads the
//! one that runs each of its script languages as an implied dependency. This
//! file proves the three outcomes:
//!
//! * a candela app installs the compiler host from `lumen-candela-dev`, the
//!   module the loader records, and no other host;
//! * `[script] engine = "lua"` installs the Lua host when its module is
//!   compiled in, which this file arranges by linking `lumen-lua`;
//! * a script in a language whose module is not compiled in (rhai here, which
//!   this file deliberately does not link) runs nowhere, and the app still
//!   builds and records why.

use lumen_core::prelude::App;
use lumen_lua as _;
use lumenc::{RunOptions, build_headless_app};

/// Write `files` (path relative to the app root, contents) into a fresh temp
/// app directory named after `tag`.
fn write_app(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumen_host_selection_{tag}_{}_{}",
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

/// Build the app in `dir` headlessly and tick it a few times, so `on_start`
/// fires, its commands drain, and bound labels mirror the signals.
fn run(dir: impl Into<std::path::PathBuf>) -> App {
    let (mut app, _window) = build_headless_app(RunOptions::new(dir)).expect("build_headless_app");
    for _ in 0..5 {
        app.tick();
    }
    app
}

fn texts(app: &mut App) -> Vec<String> {
    let mut q = app.world.query::<&lumen_core::components::TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

fn loaded_modules(app: &App) -> Vec<String> {
    app.world
        .resource::<lumen_runtime::modules::LoadedModules>()
        .loaded
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

/// A candela app run from source gets the compiler host, from the module the
/// candela language descriptor names for source, and no other host.
#[test]
fn a_candela_app_installs_the_candela_compiler_host_alone() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/candela-smoke")
        .canonicalize()
        .expect("fixtures/candela-smoke must exist");
    let mut app = run(dir);

    assert!(
        app.world
            .get_resource::<lumen_candela_dev::CandelaHost>()
            .is_some(),
        "a candela app run from source did not install CandelaHost"
    );
    assert!(
        app.world
            .get_resource::<lumen_candela::CandelaVmHost>()
            .is_none(),
        "a run from source also installed the bytecode host"
    );
    assert!(
        app.world.get_resource::<lumen_lua::LuaHost>().is_none(),
        "a candela app also installed the Lua host"
    );
    assert!(
        app.world
            .get_resource::<lumen_script::ScriptLoadFailure>()
            .is_none(),
        "the candela script failed to load"
    );

    let modules = loaded_modules(&app);
    assert!(
        modules.iter().any(|m| m == "lumen-candela-dev"),
        "the module that runs the script is recorded as loaded: {modules:?}"
    );
    assert!(
        !modules.iter().any(|m| m == "lumen-lua"),
        "a module no script needs was loaded: {modules:?}"
    );

    // The script ran end to end: `on_start` seeded the `greeting` signal and
    // the bind-text reader mirrored it into the label.
    let texts = texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "candela host - ready"),
        "on_start's signal_set did not reach the bound label; TextContents = {texts:?}"
    );
}

/// `[script] engine = "lua"` runs an inline script on the Lua host, when the
/// module that provides it is compiled in.
#[test]
fn engine_lua_installs_the_lua_host_and_runs_the_script() {
    let dir = write_app(
        "lua",
        &[
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
        ],
    );
    let mut app = run(&dir);

    assert!(
        app.world.get_resource::<lumen_lua::LuaHost>().is_some(),
        "engine = \"lua\" did not install LuaHost"
    );
    assert!(
        app.world
            .get_resource::<lumen_candela_dev::CandelaHost>()
            .is_none(),
        "engine = \"lua\" also installed the candela host"
    );

    // `on_start` seeded `clicks = 0` and `derive` computed `counter_label`;
    // the bind-text reader mirrored it into the label.
    let texts = texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "Lua host - clicks: 0"),
        "derived counter_label did not reach the bound label; TextContents = {texts:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A script in a language whose module is not compiled in runs nowhere. The
/// app still builds, and the failure is recorded naming the language, so the
/// author learns why nothing happened.
#[test]
fn a_language_with_no_module_runs_no_script_and_says_so() {
    let dir = write_app(
        "rhai",
        &[
            ("lumen.toml", "[mcp]\nport = 0\n"),
            (
                "src/main.lmn",
                r#"<root>
  <label id="only" text="markup only" />
  <script src="main.rhai" />
</root>
"#,
            ),
            (
                "src/main.rhai",
                "fn on_start() { signal(\"ran\", \"\").set(\"yes\"); }\n",
            ),
        ],
    );
    let mut app = run(&dir);

    let failure = app
        .world
        .get_resource::<lumen_script::ScriptLoadFailure>()
        .expect("a script with no host records a load failure")
        .0
        .clone();
    assert!(
        failure.contains("rhai"),
        "the failure names the language with no host: {failure}"
    );
    assert!(
        app.world
            .get_resource::<lumen_candela_dev::CandelaHost>()
            .is_none()
            && app.world.get_resource::<lumen_lua::LuaHost>().is_none(),
        "another language's host took the rhai script"
    );
    let ran = app
        .world
        .resource::<lumen_core::property_store::PropertyStore>()
        .get_global_str("ran")
        .map(|v| v.to_string());
    assert_ne!(ran.as_deref(), Some("yes"), "the rhai script ran");
    let texts = texts(&mut app);
    assert!(
        texts.iter().any(|t| t == "markup only"),
        "the app built its tree without the script; TextContents = {texts:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
