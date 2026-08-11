// Needs `lumenc::spawn` / `RunOptions` / `build_app`, which only exist
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! An unresolved `var(--x)` inside a stylesheet declaration is a soft
//! warning (`lumen_ir::css::apply_to_element` drops the one declaration and
//! keeps applying the rest - see `css_var_unknown_without_fallback_warns_and_skips`
//! in `crates/lumenc/tests/parse.rs`). The inline-markup-attribute path
//! (`<tile bg="var(--x)">`) used to differ on both counts: it only ever saw
//! the app's own `main.css` `:root` block, never the active skin's, and an
//! unresolved call there aborted the whole load. These tests exercise the
//! fix through the real load pipeline (`build_app` ->
//! `lumen_runtime::run::loading::load_ir`).

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{Fill, LumenId, TextContent, Visuals};
use lumenc::RunOptions;
use lumenc::run::build_app;

fn build(markup: &str, css: &str, lumen_toml: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_inline_var_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), lumen_toml).unwrap();
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

/// A `lumen.toml [skin] name` skin's `--lumen-window-bg` token is only
/// visible to inline-attribute `var()` resolution because it is merged in
/// from `skin_override`, not from the app's own `main.css` (which is empty
/// here) - see `lumen_runtime::run::loading::load_ir`'s comment on the
/// `root_vars` merge order. `linux.css`'s light `--lumen-window-bg` is
/// `#fafafb`.
#[test]
fn inline_attribute_resolves_a_skin_defined_token() {
    let mut app = build(
        r##"<root><tile id="t" bg="var(--lumen-window-bg)"/></root>"##,
        "",
        "[mcp]\nport = 0\n[skin]\nname = \"linux\"\n",
    );
    let t = find(&mut app, "t");
    let fill = fill_of(&app, t).expect("the tile has a background");
    let want = [
        0xfa as f32 / 255.0,
        0xfa as f32 / 255.0,
        0xfb as f32 / 255.0,
    ];
    assert!(
        (fill[0] - want[0]).abs() < 0.01
            && (fill[1] - want[1]).abs() < 0.01
            && (fill[2] - want[2]).abs() < 0.01,
        "inline `var(--lumen-window-bg)` did not resolve to the active \
         skin's token (expected #fafafb, got {fill:?}) - the skin exists, \
         so this must not be the missing-var case"
    );
}

/// An inline attribute referencing a var with no definition anywhere and no
/// fallback used to abort the whole load (`RunError::ParseHtml`). It now
/// degrades the same way a stylesheet declaration does: the load succeeds,
/// with the unresolved call dropped rather than left as literal `var(...)`
/// text (which would just fail typed attribute parsing for its own,
/// unrelated reason). `text=` is untyped (any string is a valid value), so
/// the drop-to-empty is directly observable instead of colliding with that
/// separate strictness.
#[test]
fn inline_attribute_unknown_var_degrades_instead_of_aborting_the_load() {
    let mut app = build(
        r##"<root><label id="t" text="var(--totally-unknown-token)"/></root>"##,
        "",
        "[mcp]\nport = 0\n",
    );
    let t = find(&mut app, "t");
    let text = app.world.get::<TextContent>(t).map(|t| t.0.clone());
    assert_eq!(
        text.as_deref(),
        Some(""),
        "unresolved var() must drop to empty text, not abort the load or \
         leave the literal `var(...)` call in place"
    );
}
