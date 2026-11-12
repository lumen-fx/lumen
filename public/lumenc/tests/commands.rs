// Drives the linked runtime with the commands module loaded from
// `lumen.toml`, which needs `RunOptions` / `build_headless_app`; lumenc only
// exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! The commands module end to end, headless: commands read from markup and
//! made by a script, run by their chords, by menu items, by the palette and
//! by a call; disabled ones staying put; the text-field rule; the user's
//! bindings recorded, rebound, reset and kept between runs; and the
//! `command` event heard by a script and by a Rust system.

use std::path::PathBuf;

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use lumen_commands::{CommandRun, Commands, Palette, PaletteInput};
use lumen_core::app::App;
use lumen_core::components::{Disabled, LumenAttributes, LumenId, TextContent};
use lumen_core::input::{FocusTracker, Focused, Key, KeyPressed, MenuClicked, Modifiers};
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// The app directory and the app data directory are process-wide, so the
/// apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MARKUP: &str = r#"<root>
  <commands>
    <command id="file.save" label="Save" keys="Ctrl+S"/>
    <command id="file.open" label="Open" keys="Ctrl+O" enabled="false"/>
    <command id="edit.undo" label="Undo" keys="Ctrl+Z"/>
    <command id="view.zoom" label="Zoom in" keys="Ctrl+=, F9"/>
  </commands>
  <menu id="file-menu">
    <menuitem command="file.save"/>
    <menuitem id="open-item" command="file.open"/>
    <menuitem id="plain" label="Plain"/>
  </menu>
  <input id="name"/>
  <command-palette/>
  <script src="main.cdl"/>
</root>"#;

const CANDELA: &str = r#"import "lumen.cdl";
fn on_command(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    lumen::signal_set("ran", detail["id"] as string);
}
fn on_ready() {
    lumen::event_on(lumen::node_document(), "command", "on_command");
    commands::register("tool.extra", "Extra tool", "F7");
}
fn main() {}
"#;

struct TestApp {
    app: App,
    dir: PathBuf,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn build(name: &str, config: &str) -> TestApp {
    let dir = std::env::temp_dir().join(format!("lumenc_commands_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    build_in(dir, name, config)
}

fn build_in(dir: PathBuf, name: &str, config: &str) -> TestApp {
    build_from(dir, name, config, MARKUP, "")
}

fn build_from(dir: PathBuf, name: &str, config: &str, markup: &str, toml: &str) -> TestApp {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.cdl"), CANDELA).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[app]\nid = \"lumenc-commands-{name}-{}\"\n\n{toml}[dependencies]\nlumen-commands = {{ bundled = true{config} }}\n\n[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
            std::process::id()
        ),
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    ticks(&mut app, 4);
    TestApp { app, dir }
}

fn ticks(app: &mut App, n: usize) {
    for _ in 0..n {
        app.tick();
    }
}

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

/// The command the script heard last, and forget it.
fn ran(app: &mut App) -> Option<String> {
    let mut store = app.world.resource_mut::<PropertyStore>();
    let v = store.get_global_str("ran").map(|v| v.to_string());
    store.set_global_str("ran", "");
    v.filter(|v| !v.is_empty())
}

fn press_with(app: &mut App, key: &str, modifiers: Modifiers) {
    let key = Key::from(key);
    app.world.write_message(KeyPressed {
        code: key.us_code(),
        key,
        modifiers,
        repeat: false,
        time_stamp: std::time::Instant::now(),
    });
    ticks(app, 3);
}

fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }
}

fn press(app: &mut App, key: &str) {
    press_with(app, key, ctrl());
}

fn call<T>(app: &mut App, f: impl FnOnce(&Commands<'_>) -> T) -> T {
    let mut state = bevy_ecs::system::SystemState::<Commands>::new(&mut app.world);
    let c = state.get(&app.world).expect("the module is installed");
    f(&c)
}

fn focus(app: &mut App, e: Option<Entity>) {
    if let Some(old) = app.world.resource::<FocusTracker>().0 {
        app.world.entity_mut(old).remove::<Focused>();
    }
    if let Some(e) = e {
        app.world.entity_mut(e).insert(Focused);
    }
    app.world.resource_mut::<FocusTracker>().0 = e;
}

#[test]
fn chords_run_commands_and_disabled_ones_stay_put() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("chords", ", config = { persist = \"none\" }");
    let app = &mut t.app;
    assert_eq!(
        call(app, |c| c.ids()),
        vec![
            "commands.palette",
            "file.save",
            "file.open",
            "edit.undo",
            "view.zoom",
            "tool.extra"
        ]
    );
    press(app, "s");
    assert_eq!(ran(app).as_deref(), Some("file.save"));
    press(app, "o");
    assert_eq!(ran(app), None, "a disabled command does not run");
    press_with(app, "F9", Modifiers::default());
    assert_eq!(ran(app).as_deref(), Some("view.zoom"), "its second chord");
    press_with(app, "F7", Modifiers::default());
    assert_eq!(
        ran(app).as_deref(),
        Some("tool.extra"),
        "a command a script made"
    );
    // A held key's repeats do not run it again.
    app.world.write_message(KeyPressed {
        key: Key::from("s"),
        code: Key::from("s").us_code(),
        modifiers: ctrl(),
        repeat: true,
        time_stamp: std::time::Instant::now(),
    });
    ticks(app, 3);
    assert_eq!(ran(app), None);
    // A Rust system hears it too.
    assert!(call(app, |c| c.run("edit.undo")));
    app.tick();
    let heard: Vec<String> = app
        .world
        .resource::<Messages<CommandRun>>()
        .iter_current_update_messages()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(heard, vec!["edit.undo"]);
    ticks(app, 2);
    assert_eq!(ran(app).as_deref(), Some("edit.undo"));
    assert!(!call(app, |c| c.run("file.open")), "disabled");
    // Gone, its chord does nothing.
    assert!(call(app, |c| c.unregister("tool.extra")));
    press_with(app, "F7", Modifiers::default());
    assert_eq!(ran(app), None);
}

#[test]
fn a_text_field_keeps_its_editing_chords_and_lets_the_others_through() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("field", ", config = { persist = \"none\" }");
    let app = &mut t.app;
    let name = entity(app, "name");
    focus(app, Some(name));
    press(app, "z");
    assert_eq!(ran(app), None, "Ctrl+Z undoes the field's edit");
    press_with(app, "s", Modifiers::default());
    assert_eq!(ran(app), None, "a plain key types");
    press(app, "s");
    assert_eq!(
        ran(app).as_deref(),
        Some("file.save"),
        "Ctrl+S saves from the field"
    );
    focus(app, None);
    press(app, "z");
    assert_eq!(ran(app).as_deref(), Some("edit.undo"));
}

#[test]
fn menu_items_follow_their_command_and_run_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("menus", ", config = { persist = \"none\" }");
    let app = &mut t.app;
    // The `<command>` has the id too; the menu item is the one to read.
    let save = {
        let mut q = app
            .world
            .query_filtered::<(Entity, &LumenId), bevy_ecs::query::With<lumen_primitives::MenuItem>>();
        q.iter(&app.world)
            .find(|(_, id)| id.0 == "file.save")
            .map(|(e, _)| e)
            .expect("the save item")
    };
    let open = entity(app, "open-item");
    let attrs = |app: &App, e: Entity, name: &str| {
        app.world
            .get::<LumenAttributes>(e)
            .and_then(|a| a.get(name))
            .map(str::to_string)
    };
    assert_eq!(attrs(app, save, "label").as_deref(), Some("Save"));
    assert_eq!(attrs(app, save, "accel").as_deref(), Some("Ctrl+S"));
    assert_eq!(
        app.world.get::<TextContent>(save).map(|t| t.0.as_str()),
        Some("Save")
    );
    assert!(
        app.world.get::<Disabled>(open).is_some(),
        "file.open is off"
    );
    assert!(app.world.get::<Disabled>(save).is_none());

    app.world.write_message(MenuClicked {
        id: "file.save".into(),
    });
    ticks(app, 2);
    assert_eq!(ran(app).as_deref(), Some("file.save"));
    app.world.write_message(MenuClicked { id: "plain".into() });
    ticks(app, 2);
    assert_eq!(ran(app), None, "an item with no command runs none");

    // Turning the command on, and rebinding it, reach the item.
    call(app, |c| {
        c.set_enabled("file.open", true).unwrap();
        c.bind(
            "file.save",
            lumen_commands::Chord::parse_list("F2").unwrap(),
        )
        .unwrap();
    });
    ticks(app, 3);
    assert!(app.world.get::<Disabled>(open).is_none());
    assert_eq!(attrs(app, save, "accel").as_deref(), Some("F2"));
    app.world.write_message(MenuClicked {
        id: "open-item".into(),
    });
    ticks(app, 2);
    assert_eq!(ran(app).as_deref(), Some("file.open"));
}

#[test]
fn the_palette_filters_moves_and_runs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("palette", ", config = { persist = \"none\" }");
    let app = &mut t.app;
    let primary = Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        super_: cfg!(target_os = "macos"),
        shift: true,
        ..Modifiers::default()
    };
    press_with(app, "P", primary);
    assert!(app.world.resource::<Palette>().is_open());
    assert_eq!(ran(app).as_deref(), Some("commands.palette"));
    assert_eq!(
        app.world.resource::<Palette>().shown(),
        vec!["file.save", "edit.undo", "view.zoom", "tool.extra"],
        "enabled commands, in order"
    );
    let mut q = app
        .world
        .query_filtered::<Entity, bevy_ecs::query::With<PaletteInput>>();
    let input = q.single(&app.world).expect("one search field");
    assert_eq!(
        app.world.resource::<FocusTracker>().0,
        Some(input),
        "focus is in the search field"
    );
    // The panel opens near the top of the window, centred, and its list
    // scrolls inside its height cap.
    let panel = app
        .world
        .get::<bevy_ecs::hierarchy::ChildOf>(input)
        .map(|c| c.parent())
        .expect("the field sits in the panel");
    let panel_box = *app
        .world
        .get::<lumen_core::components::Transform>(panel)
        .expect("laid out");
    let window = lumen_core::window::primary_viewport(&app.world)
        .expect("a window")
        .size;
    assert!(
        (panel_box.absolute.x + panel_box.size.x / 2.0 - window.x / 2.0).abs() < 1.0,
        "centred: {panel_box:?} in {window:?}"
    );
    assert!(
        panel_box.absolute.y >= 40.0,
        "below the strip: {panel_box:?}"
    );
    let mut lists = app.world.query_filtered::<Entity, (
        bevy_ecs::query::With<lumen_core::input::Scroll>,
        bevy_ecs::query::With<bevy_ecs::hierarchy::ChildOf>,
    )>();
    assert!(
        lists.iter(&app.world).any(|e| app
            .world
            .get::<bevy_ecs::hierarchy::ChildOf>(e)
            .map(|c| c.parent())
            == Some(panel)),
        "the list is a scroll container"
    );
    // Typed into the field, the way a user types.
    for ch in ["Z", "o", "o", "m"] {
        press_with(app, ch, Modifiers::default());
    }
    assert_eq!(
        app.world.get::<TextContent>(input).map(|t| t.0.as_str()),
        Some("Zoom")
    );
    assert_eq!(app.world.resource::<Palette>().shown(), vec!["view.zoom"]);
    // The list shrinks to the one row it holds now.
    let list = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(panel)
        .and_then(|kids| kids.get(1).copied())
        .expect("the list follows the field");
    let row_h = {
        let mut q = app
            .world
            .query_filtered::<&lumen_core::components::Transform, bevy_ecs::query::With<lumen_commands::PaletteRow>>();
        q.single(&app.world).expect("one row").size.y
    };
    let list_h = app
        .world
        .get::<lumen_core::components::Transform>(list)
        .expect("laid out")
        .size
        .y;
    assert!(list_h <= row_h + 1.0, "list {list_h} for one {row_h} row");
    app.world.entity_mut(input).insert(TextContent("o".into()));
    ticks(app, 2);
    assert_eq!(
        app.world.resource::<Palette>().shown(),
        vec!["edit.undo", "view.zoom", "tool.extra"]
    );
    press_with(app, "ArrowDown", Modifiers::default());
    assert_eq!(
        app.world.resource::<Palette>().highlighted(),
        Some("view.zoom")
    );
    press_with(app, "Enter", Modifiers::default());
    assert!(!app.world.resource::<Palette>().is_open());
    assert_eq!(ran(app).as_deref(), Some("view.zoom"));
    assert_ne!(app.world.resource::<FocusTracker>().0, Some(input));

    // Rows that go while the search stays put shrink the list too.
    call(app, |c| c.open_palette());
    ticks(app, 3);
    let list = app.world.resource::<Palette>().shown().len();
    assert!(list > 1, "several commands shown");
    let list_e = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(panel)
        .and_then(|kids| kids.get(1).copied())
        .expect("the list follows the field");
    let full = app
        .world
        .get::<lumen_core::components::Transform>(list_e)
        .expect("laid out")
        .size
        .y;
    for id in ["file.save", "edit.undo", "view.zoom"] {
        call(app, |c| c.set_enabled(id, false)).expect("a command");
    }
    ticks(app, 3);
    let after = app
        .world
        .get::<lumen_core::components::Transform>(list_e)
        .expect("laid out")
        .size
        .y;
    assert!(after < full, "the list shrank: {full} -> {after}");
    for id in ["file.save", "edit.undo", "view.zoom"] {
        call(app, |c| c.set_enabled(id, true)).expect("a command");
    }
    press_with(app, "Escape", Modifiers::default());

    // Escape closes it, and running nothing.
    call(app, |c| c.open_palette());
    ticks(app, 2);
    assert!(app.world.resource::<Palette>().is_open());
    press_with(app, "Escape", Modifiers::default());
    assert!(!app.world.resource::<Palette>().is_open());
    assert_eq!(ran(app), None);
}

#[test]
fn bindings_are_recorded_rebound_reset_and_kept() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let data = std::env::temp_dir().join(format!("lumenc_commands_data_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    // SAFETY: the tests in this file run one at a time under SERIAL, and
    // nothing else in this process reads the variable concurrently.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    let dir = std::env::temp_dir().join(format!("lumenc_commands_keep_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let mut t = build_in(dir.clone(), "keep", "");
        let app = &mut t.app;
        assert!(call(app, |c| c.record("file.save").is_ok()));
        press_with(app, "Shift", Modifiers::default());
        press_with(app, "F5", Modifiers::default());
        assert_eq!(
            call(app, |c| c.keys("file.save")),
            lumen_commands::Chord::parse_list("F5").unwrap()
        );
        press_with(app, "F5", Modifiers::default());
        assert_eq!(ran(app).as_deref(), Some("file.save"));
        // Escape cancels a recording and keeps what was bound.
        call(app, |c| c.record("file.save").unwrap());
        press_with(app, "Escape", Modifiers::default());
        assert_eq!(
            call(app, |c| c.keys("file.save")),
            lumen_commands::Chord::parse_list("F5").unwrap()
        );
        // A chord another command has moves.
        call(app, |c| {
            c.bind(
                "view.zoom",
                lumen_commands::Chord::parse_list("Ctrl+Z").unwrap(),
            )
            .unwrap()
        });
        ticks(app, 2);
        assert!(call(app, |c| c.keys("edit.undo")).is_empty());
        press(app, "z");
        assert_eq!(ran(app).as_deref(), Some("view.zoom"));
    }
    let file = lumen_core::app_paths::data_dir_under(
        &data,
        &format!("lumenc-commands-keep-{}", std::process::id()),
    )
    .join(lumen_commands::FILE);
    assert!(file.exists(), "{} was written", file.display());
    {
        let mut t = build_in(dir.clone(), "keep", "");
        let app = &mut t.app;
        assert_eq!(
            call(app, |c| c.keys("file.save")),
            lumen_commands::Chord::parse_list("F5").unwrap(),
            "the binding came back"
        );
        assert!(call(app, |c| c.keys("edit.undo")).is_empty());
        assert!(call(app, |c| c.reset("file.save")));
        ticks(app, 2);
        assert_eq!(
            call(app, |c| c.keys("file.save")),
            lumen_commands::Chord::parse_list("Ctrl+S").unwrap()
        );
    }
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: as above.
    unsafe { std::env::remove_var("XDG_DATA_HOME") };
}

/// An item of the menu bar drawn in the window that has both an `accel` and a
/// `command` fires once on its chord: the command runs and the bar leaves
/// the chord to it.
#[test]
fn a_menu_bar_item_with_a_command_fires_once() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("lumenc_commands_bar_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut t = build_from(
        dir,
        "bar",
        ", config = { persist = \"none\" }",
        r#"<root>
  <menubar>
    <menu label="File">
      <menuitem command="file.save" accel="Ctrl+S"/>
      <menuitem id="file.close" label="Close" accel="Ctrl+W"/>
    </menu>
  </menubar>
  <commands>
    <command id="file.save" label="Save" keys="Ctrl+S"/>
  </commands>
  <script src="main.cdl"/>
</root>"#,
        "[window]\nmenubar = \"window\"\n\n",
    );
    let app = &mut t.app;
    let clicked = |app: &mut App| -> Vec<String> {
        app.world
            .resource::<Messages<MenuClicked>>()
            .iter_current_update_messages()
            .map(|m| m.id.clone())
            .collect()
    };
    let key = Key::from("s");
    app.world.write_message(KeyPressed {
        code: key.us_code(),
        key,
        modifiers: ctrl(),
        repeat: false,
        time_stamp: std::time::Instant::now(),
    });
    app.tick();
    assert!(
        clicked(app).is_empty(),
        "the bar leaves Ctrl+S to the command"
    );
    ticks(app, 2);
    assert_eq!(ran(app).as_deref(), Some("file.save"));
    let key = Key::from("w");
    app.world.write_message(KeyPressed {
        code: key.us_code(),
        key,
        modifiers: ctrl(),
        repeat: false,
        time_stamp: std::time::Instant::now(),
    });
    app.tick();
    assert_eq!(
        clicked(app),
        vec!["file.close"],
        "a plain item keeps its chord"
    );
}
