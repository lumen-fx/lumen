// Needs `lumenc::spawn` / `RunOptions` / `build_app`, which only exist
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! The built-in Palette theme (`lumen_core::palette::Palette`) feeds CSS
//! `var()` resolution as the lowest-precedence `:root` layer - see
//! `lumen_ir::css::palette_root_css`. These tests exercise it through the
//! real load pipeline (`build_app` -> `lumen_runtime::run::loading::load_ir`):
//! an inline attribute referencing a Palette-named token resolves with no
//! skin and no app CSS at all, and the app's own `:root` still overrides it
//! when it redeclares the same custom-property name.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{Fill, LumenId, Visuals};
use lumenc::RunOptions;
use lumenc::run::build_app;

fn build(markup: &str, css: &str) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_palette_theme_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string())
        .with_css(css.to_string());
    let (mut app, _winit) = build_app(opts).expect("build_app");
    app.add_plugin(lumen_window_winit::WinitPlugin);
    for _ in 0..2 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn find(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, l)| l.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn fill_of(app: &App, e: Entity) -> Option<[f32; 4]> {
    match app.world.get::<Visuals>(e)?.fill.as_ref()? {
        Fill::Solid(c) => Some([c.r, c.g, c.b, c.a]),
        _ => None,
    }
}

fn approx(c: [f32; 4], hex: (u8, u8, u8)) -> bool {
    let want = [
        hex.0 as f32 / 255.0,
        hex.1 as f32 / 255.0,
        hex.2 as f32 / 255.0,
    ];
    (c[0] - want[0]).abs() < 0.01 && (c[1] - want[1]).abs() < 0.01 && (c[2] - want[2]).abs() < 0.01
}

/// With no skin and no app `:root`, `var(--window-bg-color)` still resolves,
/// to the built-in Palette's `adwaita_light` value (`#fafafb`). Nothing else
/// in an unskinned app defines that name, so reaching it at all proves the
/// Palette layer is merged into `var()` resolution.
#[test]
fn palette_token_resolves_with_no_skin_and_no_app_css() {
    let mut app = build(
        r##"<root><tile id="t" bg="var(--window-bg-color)"/></root>"##,
        "",
    );
    let t = find(&mut app, "t");
    let fill = fill_of(&app, t).expect("the tile has a background");
    assert!(
        approx(fill, (0xfa, 0xfa, 0xfb)),
        "expected the Palette's adwaita-light window_bg_color (#fafafb), got {fill:?}"
    );
}

/// The app's own `:root` redeclares the same custom-property name the
/// Palette theme defines - the app must win, matching the ordinary
/// author-beats-user-agent cascade rule the Palette layer is documented to
/// respect.
#[test]
fn app_root_still_overrides_the_palette_token() {
    let markup = r##"<root><tile id="t" bg="var(--window-bg-color)"/></root>"##;
    let css = ":root { --window-bg-color: #112233; }";
    let mut app = build(markup, css);
    let t = find(&mut app, "t");
    let fill = fill_of(&app, t).expect("the tile has a background");
    assert!(
        approx(fill, (0x11, 0x22, 0x33)),
        "app :root override did not win over the Palette token, got {fill:?}"
    );
}
