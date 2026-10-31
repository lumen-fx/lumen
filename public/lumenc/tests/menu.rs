// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! `<menu>` end to end: a menu and its items are elements from the page's
//! first frame, a script changes them with the DOM calls it uses for any
//! element, a `<for>` builds them from a list, and an item shows its
//! shortcut.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent, TextStyle, Transform};
use lumen_core::input::{ClickEvent, MenuClicked, PointerButton};
use lumen_core::property_store::PropertyStore;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_primitives::{MenuAccel, MenuItem, MenuItemButton};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, script: Option<&str>) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_menu_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    if let Some(script) = script {
        std::fs::write(src.join("main.cdl"), script).unwrap();
    }
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(600.0, 400.0);
    ticks(&mut app, 4);
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn ticks(app: &mut App, n: usize) {
    for _ in 0..n {
        app.tick();
    }
}

fn open(app: &mut App, menu: &str) {
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool(&format!("__menu_open:{menu}"), true);
    ticks(app, 3);
}

/// The menu items, top to bottom, as `(id, text)`.
fn items(app: &mut App) -> Vec<(String, String)> {
    let mut q = app
        .world
        .query_filtered::<(Entity, Option<&LumenId>), bevy_ecs::query::With<MenuItem>>();
    let mut found: Vec<(Entity, String)> = q
        .iter(&app.world)
        .map(|(e, id)| (e, id.map(|i| i.0.clone()).unwrap_or_default()))
        .collect();
    found.sort_by(|a, b| {
        let y = |e: Entity| app.world.get::<Transform>(e).map_or(0.0, |t| t.absolute.y);
        y(a.0).total_cmp(&y(b.0))
    });
    found
        .into_iter()
        .map(|(e, id)| (id, text(app, e)))
        .collect()
}

fn item(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId, &MenuItem)>();
    q.iter(&app.world)
        .find(|(_, name, _)| name.0 == id)
        .map(|(e, ..)| e)
        .unwrap_or_else(|| panic!("no menu item `{id}`"))
}

fn text(app: &App, e: Entity) -> String {
    app.world
        .get::<TextContent>(e)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

/// Click `id` and return what the menu reported.
fn click(app: &mut App, id: &str) -> Vec<String> {
    let entity = item(app, id);
    app.world.write_message(ClickEvent {
        entity,
        position: Vec2::ZERO,
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
    let ids = app
        .world
        .resource::<Messages<MenuClicked>>()
        .iter_current_update_messages()
        .map(|m| m.id.clone())
        .collect();
    ticks(app, 2);
    ids
}

/// A popup menu item shows its `accel` in a part of its own, written the way
/// this platform's menus write a shortcut, at the far end of the row and in
/// the skin's dimmer text color. The item keeps its label as its own text.
#[test]
fn a_popup_menu_item_shows_its_shortcut() {
    let mut app = build(
        r#"<root>
  <menu id="edit">
    <menuitem id="undo" label="Undo" accel="CommandOrControl+Z"/>
    <menuitem id="prefs" label="Preferences"/>
  </menu>
</root>"#,
        None,
    );
    open(&mut app, "edit");
    let rows = items(&mut app);
    assert_eq!(
        rows,
        [
            ("undo".to_string(), "Undo".to_string()),
            ("prefs".to_string(), "Preferences".to_string())
        ]
    );
    let undo = item(&mut app, "undo");
    let mut parts = app.world.query::<(Entity, &MenuAccel, &ChildOf)>();
    let parts: Vec<(Entity, Entity)> = parts
        .iter(&app.world)
        .map(|(e, _, p)| (e, p.parent()))
        .collect();
    assert_eq!(parts.len(), 1, "only the item with an accel has a part");
    let (part, parent) = parts[0];
    assert_eq!(parent, undo, "the part sits inside its item");
    let want = if cfg!(target_os = "macos") {
        "\u{2318}Z"
    } else {
        "Ctrl+Z"
    };
    assert_eq!(text(&app, part), want);

    let row = *app.world.get::<Transform>(undo).unwrap();
    let at = *app.world.get::<Transform>(part).unwrap();
    assert!(
        (at.absolute.x + at.size.x - (row.absolute.x + row.size.x)).abs() <= 16.5,
        "the part runs to the row's far end: part {at:?}, row {row:?}"
    );
    let ink = |e| app.world.get::<TextStyle>(e).map(|t| t.color);
    assert!(ink(part).is_some());
    assert_ne!(
        ink(part),
        ink(undo),
        "the shortcut is dimmer than the label"
    );
}

/// A menu's items are elements before it first opens, and a script changes
/// them with the DOM calls it uses for any element: it finds one by id, adds
/// one with `create` and `append`, relabels one with `set_attr("label")`,
/// gives one a shortcut with `set_attr("accel")`, and takes one out with
/// `remove`. An item it adds is clicked like one the markup wrote.
#[test]
fn a_script_changes_a_menu_s_items() {
    let mut app = build(
        r#"<root>
  <menu id="file">
    <menuitem id="open" label="Open"/>
    <menuitem id="close" label="Close"/>
    <separator/>
    <menuitem id="quit" label="Quit"/>
  </menu>
  <script src="main.cdl"/>
</root>"#,
        Some(
            r#"import "lumen.cdl";
fn on_ready() {
    let menu = get_by_id("file");
    let recent = create("menuitem");
    recent.set_id("recent");
    recent.set_attr("label", "Open Recent");
    menu.insert_before(recent, get_by_id("close"));
    get_by_id("open").set_attr("accel", "CommandOrControl+O");
    get_by_id("quit").set_attr("label", "Exit");
    get_by_id("close").remove();
    lumen::signal_set("found", string(menu.valid()));
}
fn main() {}
"#,
        ),
    );
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("found")
            .as_deref(),
        Some("true"),
        "on_ready found the menu before it ever opened"
    );
    open(&mut app, "file");
    assert_eq!(
        items(&mut app),
        [
            ("open".to_string(), "Open".to_string()),
            ("recent".to_string(), "Open Recent".to_string()),
            ("quit".to_string(), "Exit".to_string()),
        ]
    );
    let open_item = item(&mut app, "open");
    let mut parts = app.world.query::<(&MenuAccel, &ChildOf)>();
    assert!(
        parts.iter(&app.world).any(|(_, p)| p.parent() == open_item),
        "the shortcut a script gave shows"
    );
    assert_eq!(click(&mut app, "recent"), ["recent"]);
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_bool("__menu_open:file"),
        Some(false),
        "picking an added item closes the menu"
    );
}

/// A `<for>` in a menu builds a row per record of a list signal, and the
/// rows follow the list: a record added is an item, one taken away is gone.
/// A row is clicked under the id its record gave it.
#[test]
fn a_menu_lists_rows_from_a_signal() {
    let mut app = build(
        r#"<root>
  <menu id="window">
    <menuitem id="reset" label="Reset layout"/>
    <separator/>
    <for each="layouts" key="id">
      <menuitem id="layout-{id}" label="{name}"/>
    </for>
  </menu>
</root>"#,
        None,
    );
    let set = |app: &mut App, rows: &[(&str, &str)]| {
        let rows: Vec<ArrayItem> = rows
            .iter()
            .map(|(id, name)| {
                let mut m = ArrayItem::new();
                m.insert("id".into(), (*id).to_string());
                m.insert("name".into(), (*name).to_string());
                m
            })
            .collect();
        app.world
            .resource_mut::<ArraySignals>()
            .set("layouts", rows);
        ticks(app, 3);
    };
    set(&mut app, &[("a", "Editing"), ("b", "Review")]);
    open(&mut app, "window");
    assert_eq!(
        items(&mut app),
        [
            ("reset".to_string(), "Reset layout".to_string()),
            ("layout-a".to_string(), "Editing".to_string()),
            ("layout-b".to_string(), "Review".to_string()),
        ]
    );
    assert_eq!(click(&mut app, "layout-b"), ["layout-b"]);
    set(&mut app, &[("b", "Review"), ("c", "Paint")]);
    let ids: Vec<String> = items(&mut app).into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, ["reset", "layout-b", "layout-c"]);
    let row = item(&mut app, "layout-c");
    let wired = app
        .world
        .get::<MenuItemButton>(row)
        .map(|b| b.open_signal.clone());
    assert_eq!(wired.as_deref(), Some("__menu_open:window"));
}
