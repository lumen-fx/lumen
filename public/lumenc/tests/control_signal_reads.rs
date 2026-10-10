// Needs `lumenc::spawn` / `RunOptions` / `build_headless_app`, which only
// exist under `dev-run`.
#![cfg(feature = "dev-run")]

//! What a script handler reads from a signal a control is bound to.
//!
//! A control's own callback (`on_toggle`, `on_slider`, `on_text_input`) and a
//! handler on another element that runs right after an edit (a Save button
//! clicked just after typing) both read the bound signal. They must see the
//! value the control now shows, not the one from before the edit; a form
//! whose Save handler reads its fields would otherwise save stale values.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent, Transform};
use lumen_core::input::{
    ClickEvent, Key, KeyPressed, Modifiers, PointerButton, PointerMoved, PointerPressed,
    PointerState,
};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// The DOM event bindings a script makes with `node.on(...)` live in
/// process-wide state that another app's script host clears as it loads, so
/// the apps in this file run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_control_reads_{}_{}", std::process::id(), {
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

fn find(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, l)| l.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn text_of(app: &mut App, id: &str) -> String {
    let e = find(app, id);
    app.world
        .get::<TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

fn click(app: &mut App, id: &str, at: glam::Vec2) {
    let entity = find(app, id);
    app.world.write_message(ClickEvent {
        entity,
        position: at,
        button: PointerButton::Primary,
        local: None,
    });
}

fn focus(app: &mut App, id: &str) {
    let target = find(app, id);
    let t = *app.world.get::<Transform>(target).unwrap();
    let p = t.absolute + t.size * 0.5;
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
    });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: p,
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.tick();
}

fn key(app: &mut App, ch: char) {
    app.world.write_message(KeyPressed {
        key: Key::Character(ch.to_string()),
        modifiers: Modifiers::default(),
        repeat: false,
    });
}

const CONTROLS: &str = r##"<root padding="20" gap="10">
  <toggle id="t" width="48px" height="28px" bind-checked="on" />
  <slider id="s" width="300px" height="20px" min="0" max="10" step="1" value="0" bind-value="v" />
  <input id="name" bind-text="name" width="300px" />
  <label id="seen" width="300px" height="30px" bind-text="seen" text="-" />
  <script>
    import "lumen.cdl";
    fn on_start() { lumen::signal_set("name", ""); }
    fn on_toggle(id: string, checked: bool) {
        lumen::signal_set("seen", "arg=" + str(checked) + " signal=" + str(lumen::signal_get("on")));
    }
    fn on_slider(id: string, value: float) {
        lumen::signal_set("seen", "slider=" + str(lumen::signal_get("v")));
    }
    fn on_text_input(id: string, text: string) {
        lumen::signal_set("seen", "arg=" + text + " signal=" + str(lumen::signal_get("name")));
    }
    fn main() {}
  </script>
</root>"##;

/// `on_toggle` reads the bound signal after the flip has been written back.
#[test]
fn toggle_callback_reads_the_new_value() {
    let _serial = serial();
    let mut app = build_and_tick(CONTROLS, 4);
    for expect in ["true", "false", "true", "false"] {
        click(&mut app, "t", glam::Vec2::ZERO);
        app.tick();
        app.tick();
        assert_eq!(
            text_of(&mut app, "seen"),
            format!("arg={expect} signal={expect}"),
            "on_toggle read the bound signal from before the flip"
        );
    }
}

/// `on_slider` reads the bound signal after the move has been written back.
#[test]
fn slider_callback_reads_the_new_value() {
    let _serial = serial();
    let mut app = build_and_tick(CONTROLS, 4);
    let s = find(&mut app, "s");
    let t = *app.world.get::<Transform>(s).unwrap();
    for frac in [0.95_f32, 0.05, 0.95, 0.05] {
        let at = t.absolute + glam::Vec2::new(t.size.x * frac, t.size.y * 0.5);
        click(&mut app, "s", at);
        app.tick();
        app.tick();
        let want = if frac > 0.5 { "slider=10" } else { "slider=0" };
        assert_eq!(
            text_of(&mut app, "seen"),
            want,
            "on_slider read the bound signal from before the move"
        );
    }
}

/// `on_text_input` reads the bound signal with the keystroke in it.
#[test]
fn text_input_callback_reads_the_new_value() {
    let _serial = serial();
    let mut app = build_and_tick(CONTROLS, 4);
    focus(&mut app, "name");
    let mut typed = String::new();
    for ch in "abcd".chars() {
        typed.push(ch);
        key(&mut app, ch);
        app.tick();
        app.tick();
        assert_eq!(
            text_of(&mut app, "seen"),
            format!("arg={typed} signal={typed}"),
            "on_text_input read the bound signal from before the keystroke"
        );
    }
}

const SAVE_FORM: &str = r##"<root padding="20" gap="10">
  <input id="amt" bind-text="amt" width="300px" />
  <button id="go" text="Go" width="200px" />
  <button id="save" text="Save" width="200px" />
  <label id="log" bind-text="log" />
  <script>
    import "lumen.cdl";
    fn on_start() { lumen::signal_set("amt", ""); lumen::signal_set("log", ""); }
    fn on_click(id: string) {
        if id != "amt" {
            lumen::signal_set("log", str(lumen::signal_get("log")) + "[" + str(lumen::signal_get("amt")) + "]");
        }
    }
    fn main() {}
  </script>
</root>"##;

/// A click on another element on the tick right after a keystroke reads the
/// bound signal with that keystroke in it. The rounds alternate between two
/// buttons so back-to-back clicks are not taken for a double click.
#[test]
fn click_right_after_typing_reads_the_typed_value() {
    let _serial = serial();
    let mut app = build_and_tick(SAVE_FORM, 4);
    focus(&mut app, "amt");
    let mut want = String::new();
    let mut typed = String::new();
    for (ch, button) in "1234".chars().zip(["go", "save", "go", "save"]) {
        typed.push(ch);
        key(&mut app, ch);
        app.tick();
        click(&mut app, button, glam::Vec2::ZERO);
        app.tick();
        app.tick();
        want.push_str(&format!("[{typed}]"));
        assert_eq!(
            text_of(&mut app, "log"),
            want,
            "the click read the input's signal from before the keystroke"
        );
    }
}

const SUBMIT_AND_CLEAR: &str = r##"<root padding="20" gap="10">
  <input id="name" bind-text="name" width="300px" />
  <script>
    import "lumen.cdl";
    fn on_ready() {
        lumen::signal_set("name", "");
        get_by_id("name").on("submit", "on_submit");
    }
    fn on_submit(ev: int) { lumen::signal_set("name", "cleared"); }
    fn main() {}
  </script>
</root>"##;

/// Enter ends the edit in a single-line field, so the write its submit
/// handler makes reaches the field while it still has focus, and the next
/// keystroke edits the new text.
#[test]
fn enter_lets_a_submit_handler_clear_the_field_it_came_from() {
    let _serial = serial();
    let mut app = build_and_tick(SUBMIT_AND_CLEAR, 4);
    focus(&mut app, "name");
    for ch in "hello".chars() {
        key(&mut app, ch);
        app.tick();
    }
    assert_eq!(text_of(&mut app, "name"), "hello");
    app.world.write_message(KeyPressed {
        key: Key::Named(lumen_core::input::NamedKey::Enter),
        modifiers: Modifiers::default(),
        repeat: false,
    });
    let e = find(&mut app, "name");
    for _ in 0..4 {
        app.tick();
    }
    assert!(
        app.world.get::<lumen_core::input::Focused>(e).is_some(),
        "the field keeps focus"
    );
    assert_eq!(
        text_of(&mut app, "name"),
        "cleared",
        "the field took the write"
    );
    key(&mut app, '!');
    app.tick();
    app.tick();
    assert_eq!(
        text_of(&mut app, "name"),
        "cleared!",
        "typing resumes on the new text"
    );
}
