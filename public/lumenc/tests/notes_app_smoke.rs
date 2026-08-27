// Needs `RunOptions` / `build_headless_app`, only available under `dev-run`.
#![cfg(feature = "dev-run")]

//! End-to-end check against the real `apps/notes` sources.
//!
//! The example is the reference for text editing on a multiline field, so
//! it is worth asserting the two things the owner reported broken directly
//! against the shipped markup, CSS and script rather than a synthetic
//! stand-in.

use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn notes_app() -> lumen_core::app::App {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("public/lumenc sits two levels below the workspace root")
        .join("apps/notes");
    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..8 {
        app.tick();
    }
    app
}

fn editor_of(app: &mut lumen_core::app::App) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, l)| l.0 == "editor")
        .map(|(e, _)| e)
        .expect("#editor")
}

/// The editor must keep the palette the app authored. The built-in skin
/// ships a navy hover fill for `textarea`; on a light theme that lands
/// dark author text on a dark blue field.
#[test]
fn the_notes_editor_keeps_its_authored_palette() {
    let mut app = notes_app();
    let ed = editor_of(&mut app);
    let tint = app
        .world
        .get::<lumen_primitives::Interaction>(ed)
        .and_then(|i| i.hover_tint);
    assert!(
        tint.is_none(),
        "the skin still installs a hover fill ({tint:?}) over the editor's \
         authored background"
    );
}

/// The editor is a stacked block, so its text starts at the top of the
/// field rather than floating in the middle of the pane.
#[test]
fn the_notes_editor_is_top_aligned() {
    let mut app = notes_app();
    let ed = editor_of(&mut app);
    let origin = app
        .world
        .get::<lumen_core::components::TextBlockOrigin>(ed)
        .expect("the shaping producer published an origin");
    assert_eq!(origin.top, 0.0);
}

/// A selection in the editor must be painted in a colour that is actually
/// distinguishable from the field behind it, in EVERY theme the app ships.
/// The app authors its own field colour, so a skin token tuned for the
/// skin's own surface can land invisibly on an authored one - selection
/// state correct, highlight unseen. The earlier version of this test only
/// ran the default theme and passed while the light theme painted white
/// on near-white.
#[test]
fn the_notes_editor_selection_is_visible_in_every_theme() {
    use lumen_core::components::{Fill, LumenClasses, TextInputPaint, TextStyle};

    for theme in ["theme-dark", "theme-light"] {
        let mut app = notes_app();

        // Swap the root class scope the way `set_root_class` does, then
        // let the restyle pass re-resolve the cascade.
        let root = {
            let mut q = app.world.query::<(Entity, &LumenClasses)>();
            q.iter(&app.world)
                .find(|(_, c)| c.0.iter().any(|c| &**c == "app"))
                .map(|(e, _)| e)
                .expect("the root carries the app class")
        };
        if let Some(mut classes) = app.world.get_mut::<LumenClasses>(root) {
            classes.0 = vec!["app".into(), theme.into()];
        }

        // The skin tweens the field fill (`transition: bg 130ms`), so a
        // fixed tick count samples the animation at an arbitrary point and
        // the assertions below would read a colour that is on screen for
        // one frame. Tick until the fill stops moving.
        let ed = editor_of(&mut app);
        let fill_of = |app: &lumen_core::app::App| {
            app.world
                .get::<lumen_core::components::Visuals>(ed)
                .and_then(|v| v.fill.clone())
        };
        let mut last = fill_of(&app);
        let mut stable = 0;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(4));
            app.tick();
            let now = fill_of(&app);
            stable = if now == last { stable + 1 } else { 0 };
            last = now;
            if stable >= 5 {
                break;
            }
        }
        assert!(
            stable >= 5,
            "{theme}: the field fill never settled, so the sample below is \
             a frame of the transition rather than the resting colour"
        );

        let sel = app
            .world
            .get::<TextStyle>(ed)
            .and_then(|s| s.selection_color)
            .unwrap_or_else(|| panic!("{theme}: the skin gives the field a selection colour"));
        assert!(
            sel.a > 0.05,
            "{theme}: selection highlight is effectively transparent: {sel:?}"
        );

        // The fill has to survive compositing over the field the app
        // authored, not just over the skin's own surface.
        if let Some(Fill::Solid(bg)) = app
            .world
            .get::<lumen_core::components::Visuals>(ed)
            .and_then(|v| v.fill.clone())
        {
            let blended = |c: f32, b: f32| c * sel.a + b * (1.0 - sel.a);
            let dr = (blended(sel.r, bg.r) - bg.r).abs();
            let dg = (blended(sel.g, bg.g) - bg.g).abs();
            let db = (blended(sel.b, bg.b) - bg.b).abs();
            assert!(
                dr + dg + db > 0.15,
                "{theme}: the selection tint is indistinguishable from the \
                 field: tint {sel:?} over {bg:?}"
            );
        }

        // Qt pairs Highlight with HighlightedText and Slint pairs
        // selection-background-color with selection-foreground-color. An
        // opaque fill without the pair swallows the glyphs it covers.
        let fg = app
            .world
            .get::<TextInputPaint>(ed)
            .and_then(|p| p.selection_foreground);
        assert!(
            fg.is_some(),
            "{theme}: the fill is opaque but no selected-glyph colour is set, \
             so selected text is painted over"
        );
    }
}
