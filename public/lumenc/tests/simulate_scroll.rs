// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! `lumenc scroll x y dx dy` goes through the `lumen.simulate` queue. A real
//! wheel turns with the pointer over the window, so the simulated one has to
//! move the pointer to (x, y) as well: the scroll routing and the `wheel`
//! handlers both target the element under the pointer.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::LumenId;
use lumen_core::input::ScrollOffset;
use lumen_core::prelude::Viewport;
use lumen_mcp::{SimulateKind, SimulateQueue, SimulateRequest};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_sim_scroll_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        probe.local_addr().unwrap().port()
    };
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[mcp]\nport = {port}\nsimulate = true\n"),
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.resource_mut::<Viewport>().size = Vec2::new(400.0, 300.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn offset_of(app: &mut App, id: &str) -> Vec2 {
    let mut q = app.world.query::<(&LumenId, &ScrollOffset)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, o)| o.0)
        .unwrap_or_else(|| panic!("no scroller with id `{id}`"))
}

/// Side-by-side scrollers: a simulated wheel at the right one's coordinates
/// scrolls the right one, with no earlier click or move to put the pointer
/// there.
#[test]
fn a_simulated_wheel_scrolls_the_container_under_the_point() {
    let mut app = build(
        r#"<root>
  <row width="400" height="200">
    <scroll id="left" width="200" height="200">
      <column height="2000" />
    </scroll>
    <scroll id="right" width="200" height="200">
      <column height="2000" />
    </scroll>
  </row>
</root>"#,
    );
    let queue = app.world.resource::<SimulateQueue>().clone();
    queue.push(SimulateRequest {
        kind: SimulateKind::Scroll {
            x: 300.0,
            y: 100.0,
            dx: 0.0,
            dy: 50.0,
        },
        wait_for: None,
    });
    for _ in 0..30 {
        app.tick();
    }
    let right = offset_of(&mut app, "right");
    let left = offset_of(&mut app, "left");
    assert!(
        right.y > 0.0,
        "the scroller under the point moved: {right:?}"
    );
    assert_eq!(left, Vec2::ZERO, "the other scroller stayed put");
}
