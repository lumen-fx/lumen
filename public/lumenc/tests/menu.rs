// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! `<menu>` end to end: what an open popup menu shows.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenClasses, TextContent, TextStyle, Transform};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_menu_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(600.0, 400.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn with_class(app: &mut App, class: &str) -> Vec<Entity> {
    let mut q = app.world.query::<(Entity, &LumenClasses)>();
    let mut found: Vec<Entity> = q
        .iter(&app.world)
        .filter(|(_, c)| c.0.iter().any(|c| &**c == class))
        .map(|(e, _)| e)
        .collect();
    found.sort_by_key(|e| app.world.get::<Transform>(*e).map(|t| t.absolute.y as i32));
    found
}

fn text(app: &App, e: Entity) -> String {
    app.world
        .get::<TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

/// A popup menu item shows its `accel` in a part of its own, written the way
/// this platform's menus write a shortcut, at the far end of the row and in
/// the skin's dimmer text color. The item keeps its label as its own text.
#[test]
fn a_popup_menu_item_shows_its_shortcut() {
    let mut app = build(
        r#"<root>
  <menu id="edit">
    <menuitem id="undo" label="Undo" accel="CommandOrControl+Z"/>
    <menuitem id="prefs" label="Preferences"/>
  </menu>
</root>"#,
    );
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_bool("__menu_open:edit", true);
    for _ in 0..4 {
        app.tick();
    }
    let items = with_class(&mut app, "menu-item");
    assert_eq!(items.len(), 2, "both items mounted once the menu opened");
    assert_eq!(text(&app, items[0]), "Undo");
    let parts = with_class(&mut app, "menu-accel");
    assert_eq!(parts.len(), 1, "only the item with an accel has a part");
    let part = parts[0];
    assert_eq!(
        app.world.get::<ChildOf>(part).map(ChildOf::parent),
        Some(items[0]),
        "the part sits inside its item"
    );
    let want = if cfg!(target_os = "macos") {
        "\u{2318}Z"
    } else {
        "Ctrl+Z"
    };
    assert_eq!(text(&app, part), want);

    let row = *app.world.get::<Transform>(items[0]).unwrap();
    let at = *app.world.get::<Transform>(part).unwrap();
    assert!(
        (at.absolute.x + at.size.x - (row.absolute.x + row.size.x)).abs() <= 12.5,
        "the part runs to the row's far end: part {at:?}, row {row:?}"
    );
    let ink = |e| app.world.get::<TextStyle>(e).map(|t| t.color);
    assert!(ink(part).is_some());
    assert_ne!(
        ink(part),
        ink(items[0]),
        "the shortcut is dimmer than the label"
    );
}
