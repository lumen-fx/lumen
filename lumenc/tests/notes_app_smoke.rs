// Needs `RunOptions` / `build_app`, only available under `dev-run`.
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
use lumenc::run::build_app;

fn notes_app() -> lumen_core::app::App {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps/notes");
    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _w) = build_app(opts).expect("build_app");
    app.add_plugin(lumen_window_winit::WinitPlugin);
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
