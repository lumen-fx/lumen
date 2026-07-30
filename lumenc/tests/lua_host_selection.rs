// Boots an app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Proof that `[script] engine = "lua"` actually switches the runtime
//! script host from Rhai to `lumen-script-lua`'s `LuaHost`.
//!
//! Without a real switch, `build_app` would install `ScriptRhaiPlugin` and
//! the app's `<script>` (written in Lua syntax: `function ... end`) would fail
//! to compile as Rhai - no `LuaHost` resource would exist and the derived
//! `counter_label` signal would never populate. Both assertions below would
//! then fail, so this test is a live guard on the match in `build_app`.

use lumenc::{RunOptions, build_headless_app};

/// The in-repo Lua fixture: `apps/lua-smoke` (lumen.toml pins
/// `engine = "lua"`; the inline `<script>` is Lua).
fn lua_smoke_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apps/lua-smoke")
        .canonicalize()
        .expect("apps/lua-smoke fixture must exist")
}

#[test]
fn engine_lua_installs_lua_host_and_runs_script() {
    let opts = RunOptions::new(lua_smoke_dir());
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    // A few ticks so `on_start` fires, `derive("counter_label", ...)`
    // registers, and the derivation + bind-text readers run.
    for _ in 0..5 {
        app.tick();
    }

    // 1. The Lua host is installed - the Rhai arm would have inserted a
    //    `RhaiHost` (and failed to compile the Lua `<script>`) instead.
    assert!(
        app.world
            .get_resource::<lumen_script_lua::LuaHost>()
            .is_some(),
        "engine = \"lua\" did not install LuaHost; the match did not switch hosts"
    );
    assert!(
        app.world
            .get_resource::<lumen_script_rhai::RhaiHost>()
            .is_none(),
        "engine = \"lua\" also installed the Rhai host"
    );

    // 2. The Lua script actually ran end to end: `on_start` seeded
    //    `clicks = 0` and `derive` computed `counter_label`; the bind-text
    //    reader mirrored it into the label's TextContent.
    let mut q = app.world.query::<&lumen_core::components::TextContent>();
    let texts: Vec<String> = q.iter(&app.world).map(|t| t.0.clone()).collect();
    assert!(
        texts.iter().any(|t| t == "Lua host - clicks: 0"),
        "derived counter_label did not reach the bound label; TextContents = {texts:?}"
    );
}
