// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! A handler that raises keeps what it did before the error, the way a
//! browser keeps the DOM changes a throwing listener made. Every host behaves
//! the same: the text set and the timer armed before the failing line both
//! apply.

use lumen_core::components::{LumenId, TextContent};
use lumen_core::prelude::App;
use lumenc::{RunOptions, build_headless_app};

const MARKUP: &str = r#"<root>
  <label id="out" text="initial" />
  <label id="timer" text="initial" />
  <script src="main.SUFFIX" />
</root>
"#;

fn app_with(ext: &str, script: &str) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumen_handler_errors_{ext}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(src.join("main.lmn"), MARKUP.replace("SUFFIX", ext)).unwrap();
    std::fs::write(src.join(format!("main.{ext}")), script).unwrap();
    let (mut app, _window) = build_headless_app(RunOptions::new(dir)).expect("build_headless_app");
    for _ in 0..8 {
        app.tick();
    }
    app
}

fn label_text(app: &mut App, id: &str) -> Option<String> {
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0.as_str() == id)
        .map(|(_, t)| t.0.clone())
}

fn assert_kept(ext: &str, script: &str) {
    let mut app = app_with(ext, script);
    assert_eq!(
        label_text(&mut app, "out").as_deref(),
        Some("before"),
        "{ext}: the text set before the error must apply"
    );
    assert_eq!(
        label_text(&mut app, "timer").as_deref(),
        Some("fired"),
        "{ext}: the timer armed before the error must fire"
    );
}

#[test]
fn candela_handler_error_keeps_queued_commands() {
    assert_kept(
        "cdl",
        r#"import "lumen.cdl";
fn on_ready() {
    lumen::set_text("out", "before");
    lumen::set_timeout("t", 0);
    let a = [1];
    let b = a[3];
}
fn on_timer(name: string) { lumen::set_text("timer", "fired"); }
fn main() {}
"#,
    );
}

#[test]
fn rhai_handler_error_keeps_queued_commands() {
    assert_kept(
        "rhai",
        r#"fn on_ready() {
    set_text("out", "before");
    set_timeout("t", 0);
    throw "deliberate failure";
}
fn on_timer(name) { set_text("timer", "fired"); }
"#,
    );
}

#[test]
fn lua_handler_error_keeps_queued_commands() {
    assert_kept(
        "lua",
        r#"function on_ready()
    set_text("out", "before")
    set_timeout("t", 0)
    error("deliberate failure")
end
function on_timer(name) set_text("timer", "fired") end
"#,
    );
}
