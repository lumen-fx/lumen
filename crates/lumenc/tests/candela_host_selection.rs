// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Proof that `[script] engine = "candela"` actually switches the runtime script
//! host from Rhai to `lumen-script-candela`'s `CandelaHost`, and that an external
//! `.cdl` script runs end to end.
//!
//! Without a real switch, `build_app` would install `ScriptRhaiPlugin` and the
//! app's candela `<script src="main.cdl">` (with a `host "lumen" { ... }` block and
//! `fn on_start()` syntax) would fail to compile as Rhai - no `CandelaHost`
//! resource would exist and the `greeting` signal would never populate the
//! bound label. The assertions below are a live guard on the candela arm of the
//! match in `build_app`.

use lumenc::{RunOptions, build_headless_app};

/// The in-repo candela fixture: `fixtures/candela-smoke` (lumen.toml pins
/// `engine = "candela"`; the script lives in an external `main.cdl`).
fn candela_smoke_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/candela-smoke")
        .canonicalize()
        .expect("fixtures/candela-smoke must exist")
}

#[test]
fn engine_candela_installs_candela_host_and_runs_script() {
    let opts = RunOptions::new(candela_smoke_dir());
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    // A few ticks so `on_start` fires, its `signal_set("greeting", ...)` command
    // drains, and the `bind-text` reader mirrors it into the label's
    // TextContent.
    for _ in 0..5 {
        app.tick();
    }

    // 1. The candela host is installed - the Rhai arm would have inserted a
    //    `RhaiHost` (and failed to compile the candela script) instead.
    assert!(
        app.world
            .get_resource::<lumen_script_candela::CandelaHost>()
            .is_some(),
        "engine = \"candela\" did not install CandelaHost; the match did not switch hosts"
    );
    assert!(
        app.world
            .get_resource::<lumen_script_rhai::RhaiHost>()
            .is_none(),
        "engine = \"candela\" also installed the Rhai host"
    );

    // 2. The candela script actually ran end to end: `on_start` seeded the
    //    `greeting` signal and the bind-text reader mirrored it into the
    //    label's TextContent.
    let mut q = app.world.query::<&lumen_core::components::TextContent>();
    let texts: Vec<String> = q.iter(&app.world).map(|t| t.0.clone()).collect();
    assert!(
        texts.iter().any(|t| t == "candela host - ready"),
        "on_start's signal_set did not reach the bound label; TextContents = {texts:?}"
    );
}
