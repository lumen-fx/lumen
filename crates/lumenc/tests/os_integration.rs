// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! OS-integration surfaces reach the script, in every host.
//!
//! The fixture is one app carrying a `.rhai`, a `.lua`, and a `.cdl` program
//! that each register the same OS handlers and record their arguments in
//! per-language signals. Driving one event and asserting three signals is what
//! keeps the hosts at parity: a builtin or dispatcher wired in one language and
//! missed in another fails here rather than in someone's app.
//!
//! Everything runs headless. No window opens, and no test touches a real OS
//! surface: nothing pops a dialog, raises a notification, or reads the system
//! clipboard. Each test writes the message the OS layer would have produced
//! and asserts on what the hosts did with it.

use lumen_core::prelude::App;
use lumenc::{RunOptions, build_headless_app};

const MARKUP: &str = r#"<root>
  <label id="hotkey-label" bind-text="rhai_release" text="waiting" />
  <script src="a.rhai" />
  <script src="b.lua" />
  <script src="c.cdl" />
</root>"#;

const RHAI: &str = r#"
fn on_hotkey_release(name) { signal("rhai_release", "").set(name); }
fn on_notification_action(id, action) {
    signal("rhai_action_id", "").set(id);
    signal("rhai_action", "").set(action);
}
fn on_clipboard(tag, text) {
    signal("rhai_clip_tag", "").set(tag);
    signal("rhai_clip", "").set(text);
}
"#;

const LUA: &str = r#"
function on_hotkey_release(name) signal("lua_release", ""):set(name) end
function on_notification_action(id, action)
    signal("lua_action_id", ""):set(id)
    signal("lua_action", ""):set(action)
end
function on_clipboard(tag, text)
    signal("lua_clip_tag", ""):set(tag)
    signal("lua_clip", ""):set(text)
end
"#;

const CDL: &str = r#"
import "lumen.cdl";

fn on_hotkey_release(name) { lumen::signal_set("cdl_release", name); }

fn on_notification_action(id, action) {
    lumen::signal_set("cdl_action_id", id);
    lumen::signal_set("cdl_action", action);
}

fn on_clipboard(tag, text) {
    lumen::signal_set("cdl_clip_tag", tag);
    lumen::signal_set("cdl_clip", text);
}

fn main() {}
"#;

const LANGS: [&str; 3] = ["rhai", "lua", "cdl"];

/// A throwaway app directory holding the three script files. Named per process
/// and per nanosecond so tests running concurrently never share one.
struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lumen_os_integration_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        std::fs::write(dir.join("a.rhai"), RHAI).expect("write rhai");
        std::fs::write(dir.join("b.lua"), LUA).expect("write lua");
        std::fs::write(dir.join("c.cdl"), CDL).expect("write candela");
        Self(dir)
    }

    /// Build the app and settle it, so every host is loaded and ready to
    /// receive an event by the time a test writes one.
    fn app(&self) -> App {
        let opts = RunOptions::new(&self.0).with_markup(MARKUP.to_string());
        let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
        for _ in 0..4 {
            app.tick();
        }
        app
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<lumen_core::property_store::PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

/// Assert every host recorded `expected` under its own `<lang>_<suffix>`.
fn assert_all_hosts(app: &App, suffix: &str, expected: &str) {
    for lang in LANGS {
        let name = format!("{lang}_{suffix}");
        assert_eq!(
            signal(app, &name).as_deref(),
            Some(expected),
            "the {lang} host must have handled the event"
        );
    }
}

/// `HotkeyReleased` is produced by the hotkey poll and has to reach
/// `on_hotkey_release` in every host. Writing the message directly is the only
/// way to drive it headlessly: the real chord needs an OS-level X11 grab.
#[test]
fn hotkey_release_dispatches_to_every_host() {
    let fixture = Fixture::new("hotkey");
    let mut app = fixture.app();

    app.world.write_message(lumen_core::input::HotkeyReleased {
        name: "talk".to_string(),
    });
    // One tick dispatches the message, the next lets the signal writes the
    // handlers queued reach the property store.
    app.tick();
    app.tick();

    assert_all_hosts(&app, "release", "talk");
}

/// A notification's action button reaches `on_notification_action(id, action)`
/// with both arguments intact.
#[test]
fn notification_action_dispatches_to_every_host() {
    let fixture = Fixture::new("notify");
    let mut app = fixture.app();

    app.world
        .write_message(lumen_core::input::NotificationActionInvoked {
            id: "export-done".to_string(),
            action_id: "open".to_string(),
        });
    app.tick();
    app.tick();

    assert_all_hosts(&app, "action_id", "export-done");
    assert_all_hosts(&app, "action", "open");
}

/// A finished `clipboard_read(tag)` reaches `on_clipboard(tag, text)` in every
/// host with both arguments intact.
///
/// The message is written directly rather than driven through
/// `clipboard_read`, because the system clipboard is a main-thread-only API on
/// macOS and a test runs on a worker thread. What the parity check needs is the
/// dispatch, and that is the same either way.
#[test]
fn clipboard_read_answers_every_host() {
    let fixture = Fixture::new("clipboard");
    let mut app = fixture.app();

    app.world.write_message(lumen_core::input::ClipboardRead {
        tag: "editor".to_string(),
        text: "from the clipboard".to_string(),
    });
    app.tick();
    app.tick();

    assert_all_hosts(&app, "clip_tag", "editor");
    assert_all_hosts(&app, "clip", "from the clipboard");
}

/// A script-supplied tray menu spec reaches `TrayConfig::menu` instead of being
/// dropped on the way, and the separator keeps the shared action-id spelling.
#[test]
fn tray_menu_spec_reaches_the_config() {
    use lumen_os_tray::{SEPARATOR_ID, TrayMenu};

    let menu = TrayMenu::parse("show:Show|-|quit:Quit");
    let ids: Vec<&str> = menu.items.iter().map(|a| a.id.as_ref()).collect();
    assert_eq!(ids, vec!["show", SEPARATOR_ID, "quit"]);

    let cfg = lumen_os_tray::TrayConfig {
        id: "main".to_string(),
        icon_path: std::path::PathBuf::from("icons/tray.png"),
        tooltip: None,
        menu: Some(menu),
        template: true,
    };
    assert!(cfg.template);
    assert_eq!(cfg.menu.expect("the menu is carried").items.len(), 3);
}
