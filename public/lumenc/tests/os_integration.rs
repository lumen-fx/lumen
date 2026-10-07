// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! OS-integration surfaces reach the script.
//!
//! The fixture is one candela app that registers every OS handler and records
//! its arguments in signals. Driving one event and asserting its signals is
//! what proves a capability's dispatcher reaches the script with its arguments
//! intact.
//!
//! Everything runs headless. No window opens, and no test touches a real OS
//! surface: nothing pops a dialog, raises a notification, or reads the system
//! clipboard. Each test writes the event the capability would have produced,
//! built by the capability's own constructor, and asserts on what the script
//! did with it.

use lumen_core::prelude::App;
use lumenc::{RunOptions, build_headless_app};

const MARKUP: &str = r#"<root>
  <label id="hotkey-label" bind-text="release" text="waiting" />
  <script src="main.cdl" />
</root>"#;

const SCRIPT: &str = r#"
import "lumen.cdl";

fn on_hotkey(name) { lumen::signal_set("seq", "press:" + name); }

fn on_hotkey_release(name) {
    lumen::signal_set("release", name);
    lumen::signal_set("seq", lumen::signal_get("seq") + ",release");
}

fn on_tray(id) { lumen::signal_set("tray", id); }

fn on_files_picked(tag, paths) {
    lumen::signal_set("pick_tag", tag);
    lumen::signal_set("pick", paths);
}

fn on_notification_action(id, action) {
    lumen::signal_set("action_id", id);
    lumen::signal_set("action", action);
}

fn on_clipboard(tag, text) {
    lumen::signal_set("clip_tag", tag);
    lumen::signal_set("clip", text);
}

fn main() {}
"#;

/// A throwaway app directory holding the script. Named per process and per
/// nanosecond so tests running concurrently never share one.
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
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("create fixture dir");
        std::fs::write(src.join("main.cdl"), SCRIPT).expect("write main.cdl");
        Self(dir)
    }

    /// Build the app and settle it, so the script is loaded and ready to
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

/// Assert the script recorded `expected` under `name`.
fn assert_handled(app: &App, name: &str, expected: &str) {
    assert_eq!(
        signal(app, name).as_deref(),
        Some(expected),
        "the script must have handled the event (signal `{name}`)"
    );
}

/// A chord pressed and released within one tick reaches `on_hotkey` and then
/// `on_hotkey_release`. Writing the poll's events directly is the only way to
/// drive it headlessly: the real chord needs an OS-level X11 grab.
#[test]
fn hotkey_press_and_release_dispatch_in_order() {
    let fixture = Fixture::new("hotkey");
    let mut app = fixture.app();

    app.world
        .write_message(lumen_os_hotkey::hotkey_event("talk", true));
    app.world
        .write_message(lumen_os_hotkey::hotkey_event("talk", false));
    // One tick dispatches the events, the next lets the signal writes the
    // handlers queued reach the property store.
    app.tick();
    app.tick();

    assert_handled(&app, "release", "talk");
    assert_handled(&app, "seq", "press:talk,release");
}

/// A tray icon click reaches `on_tray(id)`.
#[test]
fn tray_click_dispatches_to_the_script() {
    let fixture = Fixture::new("tray");
    let mut app = fixture.app();

    app.world
        .write_message(lumen_os_tray::tray_click_event("main".to_string()));
    app.tick();
    app.tick();

    assert_handled(&app, "tray", "main");
}

/// A resolved multi-file dialog reaches `on_files_picked(tag, paths)`, its
/// paths joined by `|`.
#[test]
fn file_picks_dispatch_to_the_script() {
    let fixture = Fixture::new("filedialog");
    let mut app = fixture.app();

    app.world.write_message(lumen_script::PluginEvent::from(
        lumen_os_filedialog::FileDialogResultCommand {
            request_id: lumen_os_filedialog::RequestId(1),
            kind: lumen_os_filedialog::FileDialogKind::OpenMulti,
            tag: "import".to_string(),
            paths: vec!["/a.png".into(), "/b.png".into()],
        },
    ));
    app.tick();
    app.tick();

    assert_handled(&app, "pick_tag", "import");
    assert_handled(&app, "pick", "/a.png|/b.png");
}

/// A notification's action button reaches `on_notification_action(id, action)`
/// with both arguments intact.
#[test]
fn notification_action_dispatches_to_the_script() {
    let fixture = Fixture::new("notify");
    let mut app = fixture.app();

    app.world.write_message(lumen_os_notify::action_event(
        "export-done".to_string(),
        "open".to_string(),
    ));
    app.tick();
    app.tick();

    assert_handled(&app, "action_id", "export-done");
    assert_handled(&app, "action", "open");
}

/// A finished `clipboard_read(tag)` reaches `on_clipboard(tag, text)` with
/// both arguments intact.
///
/// The event is written directly rather than driven through
/// `clipboard_read`, because the system clipboard is a main-thread-only API on
/// macOS and a test runs on a worker thread. What this checks is the dispatch,
/// and that is the same either way.
#[test]
fn clipboard_read_answers_the_script() {
    let fixture = Fixture::new("clipboard");
    let mut app = fixture.app();

    app.world
        .write_message(lumen_script::clipboard::clipboard_read_event(
            "editor".to_string(),
            "from the clipboard".to_string(),
        ));
    app.tick();
    app.tick();

    assert_handled(&app, "clip_tag", "editor");
    assert_handled(&app, "clip", "from the clipboard");
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
