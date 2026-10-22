// Drives the real pipeline with a plugin installed, which needs `RunOptions`
// / `build_headless_app`; lumenc only exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! What a plugin ships as defaults: a user-agent stylesheet, which the
//! built-in sheet, a skin, the app's CSS and markup all override, and Fluent
//! messages, which the app's catalogue overrides message by message.

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::app::{App, Plugin};
use lumen_core::components::{Color, Fill, LumenId, Style, TextContent, Transform, Visuals};
use lumen_core::prelude::Viewport;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// A plugin shipping a stylesheet and messages, as a module would.
struct Shipper;

impl Plugin for Shipper {
    fn build(self, app: &mut App) {
        app.add_ua_stylesheet(
            "shipper",
            ".demo { bg: #ff0000; width: 40; height: 20; padding: 3; }\n\
             .tab-strip { padding: 1; }\n\
             button { min-height: 10; }",
        );
        app.add_ua_stylesheet("broken", ".x { width: ");
        app.add_default_messages("en-US", "demo-close = Close\ndemo-hide = Hide\n");
        app.add_default_messages("de", "demo-hide = Ausblenden\n");
    }
}

const MARKUP: &str = r#"<root skin="default">
  <column>
    <tile id="plain" class="demo"/>
    <tile id="sized" class="demo" width="80"/>
    <tile id="themed" class="demo mine"/>
    <button id="low" text="b"/>
    <tabs id="t"><tab name="a"/></tabs>
    <label id="close" translatable="demo-close" text="?"/>
    <label id="hide" translatable="demo-hide" text="?"/>
  </column>
</root>"#;

fn build(css: &str, catalogue: Option<&str>) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_module_defaults_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), MARKUP).unwrap();
    std::fs::write(src.join("main.css"), css).unwrap();
    if let Some(ftl) = catalogue {
        std::fs::create_dir_all(dir.join("locale")).unwrap();
        std::fs::write(dir.join("locale").join("en-US.ftl"), ftl).unwrap();
    }
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nlocale = \"en-US\"\n\n[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_plugin(Shipper);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.resource_mut::<Viewport>().size = Vec2::new(400.0, 300.0);
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

fn width(app: &mut App, id: &str) -> f32 {
    let e = entity(app, id);
    app.world.get::<Transform>(e).expect("laid out").size.x
}

fn fill(app: &mut App, id: &str) -> Option<Color> {
    let e = entity(app, id);
    app.world
        .get::<Visuals>(e)
        .and_then(|v| v.fill.as_ref())
        .and_then(Fill::as_solid)
}

fn text(app: &mut App, id: &str) -> String {
    let e = entity(app, id);
    app.world
        .get::<TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

/// One app per case, built in turn, so two never run at once.
#[test]
fn a_plugin_ships_styles_and_messages_under_the_app() {
    styles_sit_under_everything_the_app_says();
    messages_sit_under_the_app_catalogue();
}

fn styles_sit_under_everything_the_app_says() {
    let mut app = build(".mine { bg: #0000ff; }", None);
    assert_eq!(width(&mut app, "plain"), 40.0, "the plugin's rule applies");
    assert_eq!(
        fill(&mut app, "plain"),
        Some(Color::rgb(1.0, 0.0, 0.0)),
        "and paints"
    );
    assert_eq!(width(&mut app, "sized"), 80.0, "a markup attribute wins");
    assert_eq!(
        fill(&mut app, "themed"),
        Some(Color::rgb(0.0, 0.0, 1.0)),
        "the app's stylesheet wins"
    );
    let low = entity(&mut app, "low");
    assert_eq!(
        app.world.get::<Style>(low).unwrap().min_height,
        lumen_core::components::Length::Px(36.0),
        "the built-in sheet's rule of the same weight wins too"
    );
    let tabs = entity(&mut app, "t");
    let strip = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(tabs)
        .unwrap()[0];
    assert_eq!(
        app.world.get::<Style>(strip).unwrap().padding.left,
        4.0,
        "and so does the skin's"
    );
}

fn messages_sit_under_the_app_catalogue() {
    let mut app = build("", Some("demo-close = Shut\n"));
    assert_eq!(text(&mut app, "close"), "Shut", "the app's wording wins");
    assert_eq!(
        text(&mut app, "hide"),
        "Hide",
        "the plugin's fills the rest"
    );
}
