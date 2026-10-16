// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! `<split>` end to end: the parser's gutters, the user-agent sheet's
//! sizing, layout's measure, and a drag and keys through the simulate queue.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenAttributes, LumenId, Transform};
use lumen_core::input::FocusTracker;
use lumen_core::prelude::Viewport;
use lumen_mcp::{SimulateKind, SimulateQueue, SimulateRequest};
use lumen_primitives::SplitGutter;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_split_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.css"), css).unwrap();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        probe.local_addr().unwrap().port()
    };
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[mcp]\nport = {port}\nsimulate = true\n\n[render]\nbackend = \"cpu\"\n"),
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.resource_mut::<Viewport>().size = Vec2::new(600.0, 400.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn rect(app: &mut App, id: &str) -> Transform {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn gutter(app: &mut App) -> (bevy_ecs::entity::Entity, Transform) {
    let mut q = app
        .world
        .query_filtered::<(bevy_ecs::entity::Entity, &Transform), bevy_ecs::query::With<SplitGutter>>();
    q.iter(&app.world)
        .map(|(e, t)| (e, *t))
        .next()
        .expect("the split has a gutter")
}

fn run(app: &mut App, kinds: Vec<SimulateKind>) {
    let queue = app.world.resource::<SimulateQueue>().clone();
    for kind in kinds {
        queue.push(SimulateRequest {
            kind,
            wait_for: None,
        });
    }
    for _ in 0..30 {
        app.tick();
    }
}

fn key(name: &str) -> SimulateKind {
    serde_json::from_value(serde_json::json!({ "kind": "key", "key": name })).expect("a key")
}

const ROW: &str = r#"<root>
  <split id="s" width="400" height="200">
    <column id="left" grow="1" min-width="100"/>
    <column id="right" grow="1"/>
  </split>
</root>"#;

/// The gutter sits between the panes and takes its share of the axis; a
/// drag moves the boundary with the pointer and leaves the split's size
/// alone; the focused gutter's arrow keys step it and Home takes the pane
/// before it to its `min-width`. A split laid out as a column stacks its
/// panes and drags its gutter up and down. One test, so the two apps never
/// run at once in this process.
#[test]
fn gutters_resize_the_panes() {
    row_split();
    column_split();
}

fn row_split() {
    let mut app = build(ROW, "");
    let (gutter_e, g) = gutter(&mut app);
    let left = rect(&mut app, "left");
    let right = rect(&mut app, "right");
    assert_eq!(g.size.x, 6.0, "the gutter's share of the axis");
    assert_eq!(g.size.y, 200.0, "the gutter stretches across");
    assert_eq!(left.size.x, 197.0);
    assert_eq!(right.size.x, 197.0);
    let attrs = app
        .world
        .get::<LumenAttributes>(gutter_e)
        .expect("separator values");
    assert_eq!(attrs.get("aria-valuenow"), Some("50"));
    assert_eq!(attrs.get("aria-orientation"), Some("vertical"));

    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x + 50.0,
                y: at.y + 30.0,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x + 50.0,
                y: at.y + 30.0,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    assert_eq!(rect(&mut app, "left").size.x, 247.0);
    assert_eq!(rect(&mut app, "right").size.x, 147.0);
    assert_eq!(rect(&mut app, "s").size.x, 400.0);
    let attrs = app.world.get::<LumenAttributes>(gutter_e).unwrap();
    assert_eq!(attrs.get("aria-valuenow"), Some("63"));

    assert_eq!(
        app.world.resource::<FocusTracker>().0,
        Some(gutter_e),
        "a press focuses the gutter"
    );
    run(&mut app, vec![key("ArrowLeft"), key("ArrowLeft")]);
    assert_eq!(rect(&mut app, "left").size.x, 227.0);
    assert_eq!(rect(&mut app, "right").size.x, 167.0);
    run(&mut app, vec![key("Home")]);
    assert_eq!(rect(&mut app, "left").size.x, 100.0);
    assert_eq!(rect(&mut app, "right").size.x, 294.0);
}

fn column_split() {
    let mut app = build(
        r#"<root>
  <split id="s" width="300" height="306">
    <column id="top" grow="1"/>
    <column id="bottom" grow="1"/>
  </split>
</root>"#,
        "#s { flex-direction: column; }",
    );
    let (_, g) = gutter(&mut app);
    assert_eq!(g.size.y, 6.0);
    assert_eq!(g.size.x, 300.0);
    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x,
                y: at.y - 40.0,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x,
                y: at.y - 40.0,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    assert_eq!(rect(&mut app, "top").size.y, 110.0);
    assert_eq!(rect(&mut app, "bottom").size.y, 190.0);
}
