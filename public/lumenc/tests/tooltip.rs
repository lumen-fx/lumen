// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! `<tooltip>` end to end: the popup a hovered trigger opens is styled by the
//! app's stylesheet like any element, `var()` tokens on the root included.

use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{
    Color, Fill, LumenId, Style, StyleVersion, TextStyle, Transform, Visuals,
};
use lumen_core::input::{PointerMoved, PointerState};
use lumen_primitives::TooltipPopup;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_tooltip_{}_{}",
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
        format!("[mcp]\nport = {port}\n\n[render]\nbackend = \"cpu\"\n"),
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

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn rect(app: &mut App, id: &str) -> Transform {
    let e = entity(app, id);
    *app.world.get::<Transform>(e).unwrap()
}

/// Rest the pointer on `id` and return its tooltip on the tick it opens.
fn hover_until_open(app: &mut App, id: &str) -> Entity {
    let t = rect(app, id);
    let at = t.absolute + t.size / 2.0;
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    for _ in 0..6 {
        app.tick();
        let mut q = app.world.query_filtered::<Entity, With<TooltipPopup>>();
        if let Some(popup) = q.iter(&app.world).next() {
            return popup;
        }
    }
    panic!("the tooltip did not open");
}

fn fill(app: &App, e: Entity) -> Option<Fill> {
    app.world.get::<Visuals>(e).and_then(|v| v.fill.clone())
}

const MARKUP: &str = r#"<root>
  <row id="bar">
    <tooltip text="Save the file" delay="0"><button id="save" text="Save"/></tooltip>
  </row>
</root>"#;

/// An app rule for `.tooltip` restyles the popup over the skin's look from
/// the frame it opens, its `var()` tokens resolve against the root, the
/// popup still opens at the pointer, and it does not count as a child of
/// the root for structural selectors.
#[test]
fn author_css_styles_the_tooltip() {
    let mut app = build(
        MARKUP,
        ":root { --tip-ink: #00ff00; }\n\
         .tooltip { bg: #ff0000; text-color: var(--tip-ink); padding: 20 20; }\n\
         #bar:last-child { bg: #0000ff; }\n",
    );
    let popup = hover_until_open(&mut app, "save");
    assert_eq!(
        fill(&app, popup),
        Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
        "the app's background reaches the popup in the frame it opens"
    );
    assert_eq!(
        app.world.get::<TextStyle>(popup).map(|t| t.color),
        Some(Color::rgb(0.0, 1.0, 0.0)),
        "a root token resolves in the popup"
    );
    assert_eq!(
        app.world.get::<Style>(popup).map(|s| s.padding.left),
        Some(20.0),
        "the app's padding reaches the popup"
    );
    // A whole-document restyle (a theme flip, say) while the tooltip is open.
    StyleVersion::bump(&mut app.world);
    for _ in 0..2 {
        app.tick();
    }
    let bar = entity(&mut app, "bar");
    assert_eq!(
        fill(&app, bar),
        Some(Fill::Solid(Color::rgb(0.0, 0.0, 1.0))),
        "the open tooltip is nobody's sibling: the bar stays the root's last child"
    );
    let save = rect(&mut app, "save");
    let cursor = save.absolute + save.size / 2.0;
    let at = app.world.get::<Transform>(popup).unwrap().absolute;
    assert!(
        (at - cursor).length() < 40.0,
        "the popup opens beside the pointer: {at:?} vs {cursor:?}"
    );
}

/// The pointer passes through an open popup to what lies under it, as with a
/// browser's `title` tooltip. A popup that took the hit unhovered its own
/// trigger, closed, and reopened after the delay, so it flickered whenever
/// the pointer drifted onto it.
#[test]
fn the_pointer_passes_through_an_open_tooltip() {
    let mut app = build(
        r#"<root>
  <tooltip text="Save the file" delay="0"><button id="save" text="Save" width="300" height="200"/></tooltip>
</root>"#,
        "",
    );
    let popup = hover_until_open(&mut app, "save");
    let corner = app.world.get::<Transform>(popup).unwrap().absolute;
    let onto = corner + Vec2::splat(2.0);
    let save = rect(&mut app, "save");
    assert!(
        onto.cmplt(save.absolute + save.size).all(),
        "the test moves onto a part of the popup that covers the trigger"
    );
    app.world.resource_mut::<PointerState>().position = Some(onto);
    app.world.write_message(PointerMoved {
        position: onto,
        local: None,
        pointer: Default::default(),
    });
    for _ in 0..4 {
        app.tick();
        let mut q = app.world.query_filtered::<Entity, With<TooltipPopup>>();
        let open: Vec<Entity> = q.iter(&app.world).collect();
        assert_eq!(open, [popup], "the popup stays open under the pointer");
    }
    let trigger = entity(&mut app, "save");
    assert!(
        app.world
            .get::<lumen_core::input::Hovered>(trigger)
            .is_some(),
        "the trigger keeps the hit"
    );
}
