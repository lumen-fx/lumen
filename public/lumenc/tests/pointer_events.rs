// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! CSS `pointer-events` end to end: the stylesheet, the cascade, a restyle
//! and the hit test.

use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Transform};
use lumen_core::input::{Hovered, PointerEvents, PointerMoved, PointerState};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    // One directory per app: the tests run in parallel, and a shared one
    // would hand one test the stylesheet another just wrote.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("lumenc_pointer_events_{}_{n}", std::process::id()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.css"), css).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(400.0, 300.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn hovered_at(app: &mut App, at: Vec2) -> Vec<String> {
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    for _ in 0..3 {
        app.tick();
    }
    let mut q = app
        .world
        .query_filtered::<Option<&LumenId>, With<Hovered>>();
    q.iter(&app.world)
        .map(|id| id.map(|id| id.0.clone()).unwrap_or_default())
        .collect()
}

fn center(app: &mut App, id: &str) -> Vec2 {
    let e = entity(app, id);
    let t = app.world.get::<Transform>(e).copied().expect("laid out");
    t.absolute + t.size / 2.0
}

/// An overlay under `pointer-events: none` lets the pointer reach the button
/// beneath it, a child that sets `auto` takes the pointer again, and the
/// value survives a restyle of the overlay.
#[test]
fn pointer_events_none_lets_the_pointer_through() {
    let markup = r#"<root>
  <button id="under" width="300" height="200" text="Under"/>
  <overlay id="cover" class="cover">
    <button id="grab" class="grab" width="60" height="40" text="Grab"/>
  </overlay>
</root>"#;
    let css = ".cover { pointer-events: none; bg: #00000020; } .grab { pointer-events: auto; }";
    let mut app = build(markup, css);
    let cover = entity(&mut app, "cover");
    assert_eq!(
        app.world.get::<PointerEvents>(cover),
        Some(&PointerEvents::None)
    );

    let grab = center(&mut app, "grab");
    assert_eq!(hovered_at(&mut app, grab), vec!["grab".to_string()]);
    let beside = Vec2::new(250.0, 150.0);
    assert_eq!(hovered_at(&mut app, beside), vec!["under".to_string()]);

    app.world
        .entity_mut(cover)
        .insert(lumen_core::components::StyleDirty);
    for _ in 0..2 {
        app.tick();
    }
    assert_eq!(
        app.world.get::<PointerEvents>(cover),
        Some(&PointerEvents::None),
        "a restyle keeps the declared value"
    );
    assert_eq!(hovered_at(&mut app, beside), vec!["under".to_string()]);
}

/// An element with a box takes the pointer whether or not it paints, so an
/// unpainted cover over a button hides the button from the pointer, as in a
/// browser. `pointer-events: none` on the cover lets the pointer through.
#[test]
fn an_unpainted_cover_takes_the_pointer_unless_it_lets_it_through() {
    let markup = r#"<root>
  <button id="under" width="300" height="200" text="Under"/>
  <overlay id="cover" class="cover" />
</root>"#;
    let at = Vec2::new(150.0, 100.0);
    let mut app = build(markup, "");
    assert_eq!(hovered_at(&mut app, at), vec!["cover".to_string()]);
    let mut app = build(markup, ".cover { pointer-events: none; }");
    assert_eq!(hovered_at(&mut app, at), vec!["under".to_string()]);
}
