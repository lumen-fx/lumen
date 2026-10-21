// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! A script's `query(":hover")` and `closest(":hover")` follow the pointer, as
//! in a browser: the element under it and every ancestor of it, nothing else.
//! The other state pseudo-classes follow their state the same way. Its own
//! test binary, because the DOM snapshot a query reads is process-global.

use bevy_ecs::entity::Entity;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Transform};
use lumen_core::input::{
    PointerButton, PointerMoved, PointerPressed, PointerReleased, PointerState,
};
use lumen_core::node::NodeHandle;
use lumen_script::node_query;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_hover_query_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn handle_of(app: &mut App, id: &str) -> u64 {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    let entity = q
        .iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element {id}"));
    NodeHandle::new(entity).pack()
}

fn centre(app: &mut App, id: &str) -> glam::Vec2 {
    let e = NodeHandle::unpack(handle_of(app, id)).unwrap().entity;
    let t = *app.world.get::<Transform>(e).expect("laid out");
    t.absolute + t.size / 2.0
}

fn move_to(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
}

fn query(sel: &str) -> Vec<u64> {
    node_query::run_query(sel)
        .expect("selector parses")
        .collect()
}

const MARKUP: &str = r##"<root id="app" padding="20">
  <row id="bar" gap="10">
    <button id="tool" width="80px" height="80px" padding="20px">
      <tile id="icon" width="40px" height="40px" bg="#336699" />
    </button>
    <button id="other" width="80px" height="80px" />
  </row>
  <tile id="away" width="80px" height="80px" bg="#999999" />
</root>"##;

#[test]
fn state_pseudo_classes_in_a_query_follow_the_live_state() {
    let mut app = build(MARKUP);
    let [root, bar, tool, icon, other] =
        ["app", "bar", "tool", "icon", "other"].map(|id| handle_of(&mut app, id));

    let away_at = centre(&mut app, "away");
    move_to(&mut app, away_at);
    let away = handle_of(&mut app, "away");
    assert_eq!(query(":hover"), [root, away]);
    assert!(query(":active").is_empty());

    let at = centre(&mut app, "icon");
    move_to(&mut app, at);
    assert_eq!(
        query(":hover"),
        [root, bar, tool, icon],
        "the hovered element and its ancestors, in document order"
    );
    assert_eq!(query("button:hover"), [tool]);
    assert_eq!(query("button:not(:hover)"), [other]);
    assert_eq!(
        node_query::node_closest(other, ":hover").unwrap(),
        Some(bar)
    );

    // A press makes the button `:active` while the pointer stays on it.
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: at,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
    let active = query(":active");
    assert!(
        active.contains(&tool) || active.contains(&icon),
        "the pressed element is :active: {active:?}"
    );
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: at,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
    assert!(query(":active").is_empty(), "the release ends :active");
}
