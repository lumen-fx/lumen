// Needs `lumenc::spawn` / `RunOptions` / `build_app`, which only exist
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! An author background must survive the pointer resting on the field.
//!
//! The built-in skin is a user-agent stylesheet, so every declaration in it
//! loses to an author declaration for the same property. That holds for
//! plain `bg`, but the skin also ships `textarea:hover { bg: ... }`, and a
//! state fill is kept in its own attribute slot. An author who sets `bg`
//! and no `hover-bg` therefore leaves the user-agent hover fill unopposed,
//! and it repaints the field as soon as the pointer is over it - which,
//! while typing, is always.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{Fill, LumenId, Transform, Visuals};
use lumen_core::input::{PointerMoved, PointerState};
use lumenc::RunOptions;
use lumenc::run::build_app;

fn build(markup: &str, css: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_skin_bg_{}_{}", std::process::id(), {
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
    for _ in 0..ticks {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

const MARKUP: &str = r##"<root class="app" skin="default">
  <textarea id="ed" class="editor" text="hello" width="400" height="200" />
</root>"##;

/// A light theme with the field color spelled out literally.
const CSS: &str = r##"
.app { bg: #f4f1ea; }
.editor { bg: #fffdf8; text-color: #2a2620; radius: 10; padding: 16; }
"##;

/// The same theme written the way the notes example writes it: the tokens
/// live in a scope class on `<root>` and the field reads them through
/// `var()`, relying on custom-property inheritance down the tree.
const MARKUP_VAR: &str = r##"<root class="app theme-light" skin="default">
  <textarea id="ed" class="editor" text="hello" width="400" height="200" />
</root>"##;

const CSS_VAR: &str = r##"
.theme-light {
  --bg:      #f4f1ea;
  --surface: #fffdf8;
  --text:    #2a2620;
}
.app { bg: var(--bg); text-color: var(--text); }
.editor { bg: var(--surface); text-color: var(--text); radius: 10; padding: 16; }
"##;

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

/// `#fffdf8` is bright: every channel near 1.0. The skin's navy fills
/// (`#0a3358` resting, `#114570` hover) are dark and blue-dominant, so a
/// simple brightness test separates them without pinning exact floats.
fn is_light(c: [f32; 4]) -> bool {
    c[0] > 0.8 && c[1] > 0.8 && c[2] > 0.8
}

#[test]
fn the_author_background_survives_a_hover() {
    let mut app = build(MARKUP, CSS, 4);
    let ed = find(&mut app, "ed");
    let resting = fill_of(&app, ed).expect("the field has a background");
    assert!(
        is_light(resting),
        "author bg lost at rest: {resting:?} is not the authored near-white"
    );

    // Park the pointer over the field, as it is while typing.
    let t = *app.world.get::<Transform>(ed).unwrap();
    let p = t.absolute + t.size * 0.5;
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.tick();
    app.tick();

    assert!(
        app.world.get::<lumen_core::input::Hovered>(ed).is_some(),
        "the pointer is not actually hovering the field; the test would \
         pass vacuously"
    );
    // Assert on the tint rather than the painted fill: the skin gives the
    // field `transition: bg 130ms`, so the fill is still mid-tween on the
    // tick after the pointer arrives and would read as the author color
    // whether or not a hover fill was installed.
    let tint = app
        .world
        .get::<lumen_primitives::Interaction>(ed)
        .and_then(|i| i.hover_tint)
        .map(|c| [c.r, c.g, c.b, c.a]);
    assert!(
        tint.is_none_or(is_light),
        "the user-agent skin installed a hover fill ({tint:?}) over an \
         author-styled field. In a light theme that repaints the field \
         navy under the author's dark text as soon as the pointer arrives."
    );
}

/// The notes example declares its palette in a theme scope class on
/// `<root>` and reads it from descendants with `var()`. If those custom
/// properties do not reach the field, its `bg` declaration drops out and
/// the user-agent skin's navy fill is what remains.
#[test]
fn an_inherited_custom_property_reaches_the_field() {
    let mut app = build(MARKUP_VAR, CSS_VAR, 4);
    let ed = find(&mut app, "ed");
    let resting = fill_of(&app, ed).expect("the field has a background");
    assert!(
        is_light(resting),
        "the field resolved to {resting:?} instead of the light `--surface` \
         the root scope declares; a var() that does not reach the element \
         drops the author bg and leaves the skin's navy"
    );
}
