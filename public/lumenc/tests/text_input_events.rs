// Needs `lumenc::spawn` / `RunOptions` / `build_headless_app`, which only
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
use lumenc::run::build_headless_app;

fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_text_events_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
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
    import "lumen.cdl";
    fn on_start() { lumen::signal_set("mirror", "EMPTY"); }
    fn on_text_input(id: string, text: string) { lumen::signal_set("mirror", "GOT:" + text); }
    fn main() {}
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
        .write(PointerMoved {
            position: p,
            local: None,
            pointer: Default::default(),
        });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    for ch in chars.chars() {
        app.world
            .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Character(ch.to_string()),
                code: lumen_core::input::KeyCode::Unidentified,
                modifiers: Modifiers::default(),
                repeat: false,
                time_stamp: lumen_core::time::Instant::now(),
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

/// What the field shows on screen.
fn text_of(app: &mut App, id: &str) -> String {
    let e = find(app, id);
    app.world
        .get::<lumen_core::components::TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

fn write_signal(app: &mut App, name: &str, value: &str) {
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str(name, value);
    for _ in 0..3 {
        app.tick();
    }
}

/// A dialog that focuses its first field as it opens shows what the app
/// wrote to the field's signal, and keeps showing later writes while the
/// field has focus and nothing has been typed. Focus alone used to count as
/// an edit in flight, so the field stayed empty. Once the user types, the
/// typing wins over a write until the edit ends.
#[test]
fn a_freshly_focused_field_shows_writes_to_its_signal() {
    let mut app = build_and_tick(
        r##"<root>
  <button id="open" text="Open"/>
  <dialog open="lp_open">
    <input id="name" bind-text="lp_name" text="" width="200" height="30"/>
  </dialog>
</root>"##,
        4,
    );
    write_signal(&mut app, "lp_name", "Layer 1");
    write_signal(&mut app, "lp_open", "1");
    let name = find(&mut app, "name");
    assert!(
        app.world.get::<lumen_core::input::Focused>(name).is_some(),
        "the dialog focuses its first field"
    );
    assert_eq!(text_of(&mut app, "name"), "Layer 1");

    write_signal(&mut app, "lp_name", "Layer 2");
    assert_eq!(
        text_of(&mut app, "name"),
        "Layer 2",
        "a later write shows too"
    );

    app.world
        .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Character("x".to_string()),
            code: lumen_core::input::KeyCode::Unidentified,
            modifiers: Modifiers::default(),
            repeat: false,
            time_stamp: lumen_core::time::Instant::now(),
        });
    for _ in 0..3 {
        app.tick();
    }
    let typed = text_of(&mut app, "name");
    assert!(typed.ends_with('x'), "the key reached the field: {typed:?}");
    write_signal(&mut app, "lp_name", "Clobbered");
    assert_eq!(
        text_of(&mut app, "name"),
        typed,
        "an edit in flight holds a write back"
    );
}
