// Needs `lumenc::spawn` / `RunOptions` / `build_app`, which only exist
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! When a script-visible text event reaches the app while the user types.
//!
//! The notes example drives its live markdown preview from
//! `on_text_input(id, text)` and documents that as "event-driven live
//! preview - there is no tick watcher". That only works if the event fires
//! per edit; if it fires only when the edit is committed, the preview sits
//! stale until the field is submitted.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::LumenId;
use lumen_core::input::{
    Key, KeyPressed, Modifiers, PointerButton, PointerMoved, PointerPressed, PointerState,
};
use lumenc::RunOptions;
use lumenc::run::build_app;

fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!(
        "lumenc_text_events_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _winit) = build_app(opts).expect("build_app");
    app.add_plugin(lumen_window_winit::WinitPlugin);
    for _ in 0..ticks {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// A textarea whose `on_text_input` handler mirrors the text into a label,
/// exactly like the notes app rebuilds its preview.
const MARKUP: &str = r##"<root>
  <textarea id="ed" text="" width="400" height="200" bg="#223344" font-size="16" />
  <label id="mirror" bind-text="mirror" />
  <script>
    fn on_start() { signal("mirror", "").set("EMPTY"); }
    fn on_text_input(id, text) { signal("mirror", "").set("GOT:" + text); }
  </script>
</root>"##;

fn find(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, l)| l.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn mirror_text(app: &mut App) -> String {
    let e = find(app, "mirror");
    app.world
        .get::<lumen_core::components::TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

fn focus_and_type(app: &mut App, target: Entity, chars: &str) {
    let t = *app
        .world
        .get::<lumen_core::components::Transform>(target)
        .unwrap();
    let p = t.absolute + t.size * 0.5;
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
        });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    for ch in chars.chars() {
        app.world
            .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Character(ch.to_string()),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        app.tick();
    }
    app.tick();
}

/// Typing must reach the script. A live preview cannot be built on an
/// event that only arrives when the field is submitted.
#[test]
fn typing_reaches_the_script_text_handler() {
    let mut app = build_and_tick(MARKUP, 4);
    let ed = find(&mut app, "ed");
    focus_and_type(&mut app, ed, "hi");
    let buf = app
        .world
        .get::<lumen_core::text_model::TextBuffer>(ed)
        .expect("buffer")
        .to_string();
    assert_eq!(buf, "hi", "the keystrokes reached the buffer");
    assert_eq!(
        mirror_text(&mut app),
        "GOT:hi",
        "on_text_input never fired for a plain edit, so a live preview \
         stays stale until the field is committed"
    );
}
