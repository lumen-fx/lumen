// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Proof that an app reaches the candela standard library the toolchain ships.
//!
//! `fixtures/candela-std` imports `std/time` for the wall clock and calls an
//! array method the compiler loads from `std/list` on its own. Both read the
//! library tree beside the running executable, so the two bound labels stay at
//! their markup text if the tree is missing and the script never compiled.

use lumenc::{RunOptions, build_headless_app};

/// The epoch second the clock has to be past: 2020-01-01.
const AFTER_2020: i64 = 1_577_836_800;

fn std_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/candela-std")
        .canonicalize()
        .expect("fixtures/candela-std must exist")
}

fn label_text(app: &mut lumen_core::prelude::App, id: &str) -> Option<String> {
    use lumen_core::components::{LumenId, TextContent};
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0.as_str() == id)
        .map(|(_, t)| t.0.clone())
}

#[test]
fn an_app_reads_the_clock_and_the_array_methods() {
    let opts = RunOptions::new(std_dir());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");

    // A few ticks so on_start's writes drain and the bound labels mirror them.
    for _ in 0..5 {
        app.tick();
    }

    let clock = label_text(&mut app, "clock-label").expect("clock-label present");
    let seconds: i64 = clock
        .parse()
        .unwrap_or_else(|_| panic!("std/time now() must reach the label, which reads {clock}"));
    assert!(
        seconds > AFTER_2020,
        "the label must carry a wall-clock second, got {seconds}"
    );

    let total = label_text(&mut app, "total-label").expect("total-label present");
    assert_eq!(
        total, "6",
        "sum() over [1, 2, 3] must reach the label from std/list"
    );
}
