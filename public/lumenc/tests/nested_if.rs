// Drives the real pipeline (parse -> spawn -> reconcile), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! `<if>` blocks inside `<if>` blocks, opened and closed together.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::LumenId;
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r#"<root>
  <button id="close" text="Close both"/>
  <script src="main.cdl"/>
  <if signal="outer">
    <column>
      <label id="status" bind-text="$status"/>
      <if signal="code">
        <column><input id="field" bind-text="$code"/></column>
      </if>
    </column>
  </if>
</root>"#;

/// The state the app starts in comes from `on_start`, so both bodies mount
/// with the tree rather than on a later tick; that is the order the blocks
/// are visited in when they close.
const SCRIPT: &str = r#"import "lumen.cdl";
fn on_start() {
    signal<string>("outer").set("1");
    signal<string>("status").set("waiting");
    signal<string>("code").set("ABCD");
}
fn main() {}
"#;

fn build() -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_nested_if_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("main.lmn"), MARKUP).unwrap();
    std::fs::write(dir.join("src").join("main.cdl"), SCRIPT).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(400.0, 300.0);
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn set(app: &mut App, name: &str, value: &str) {
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str(name, value);
}

fn present(app: &mut App, id: &str) -> bool {
    let mut q = app.world.query::<&LumenId>();
    q.iter(&app.world).any(|i| i.0 == id)
}

/// Closing an `<if>` and an `<if>` inside it on one tick takes both bodies
/// away. The outer block despawns the inner one with its body, so the inner
/// block's own update that tick must not reach for it.
#[test]
fn an_if_closed_with_the_if_around_it_goes_quietly() {
    let mut app = build();
    for _ in 0..4 {
        app.tick();
    }
    assert!(present(&mut app, "field"), "both bodies are open");

    set(&mut app, "code", "");
    set(&mut app, "outer", "");
    set(&mut app, "status", "done");
    for _ in 0..3 {
        app.tick();
    }
    assert!(!present(&mut app, "field"));
    assert!(!present(&mut app, "status"));

    // And the pair opens again.
    set(&mut app, "outer", "1");
    set(&mut app, "code", "EFGH");
    for _ in 0..3 {
        app.tick();
    }
    assert!(present(&mut app, "field"));

    // The inner block opening on the tick the outer one closes has no body
    // left to mount into.
    set(&mut app, "code", "");
    for _ in 0..2 {
        app.tick();
    }
    set(&mut app, "code", "IJKL");
    set(&mut app, "outer", "");
    for _ in 0..3 {
        app.tick();
    }
    assert!(!present(&mut app, "field"));
}
