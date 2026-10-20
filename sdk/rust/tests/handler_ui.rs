//! What a handler reaches through `Ui` beyond signals: menus, resources, the
//! world, the element tree, and the records of a list; plus the run
//! conditions a system gates on and the `CompiledUi` an app starts from.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lumenui::prelude::*;

// The element tree a handler queries is process-global; one full app at a
// time.
static APP_ISOLATION: Mutex<()> = Mutex::new(());

fn bare() -> lumenui::App {
    lumenui::App::from_markup("<root/>")
}

fn click(app: &mut EcsApp, entity: Entity) {
    app.world.write_message(ClickEvent {
        entity,
        position: Vec2::ZERO,
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
}

fn store(app: &EcsApp) -> &PropertyStore {
    app.world.resource::<PropertyStore>()
}

fn menu_open(app: &EcsApp, id: &str) -> Option<bool> {
    store(app).get_global_bool(&format!("__menu_open:{id}"))
}

fn menu_at(app: &EcsApp, id: &str) -> Option<String> {
    store(app)
        .get_global_str(&format!("__menu_at:{id}"))
        .map(|s| s.to_string())
}

/// `toggle` flips a bool signal and hands back the new value.
#[test]
fn a_handler_toggles_a_signal() {
    let mut app = bare()
        .on_click("flip", |ui, _| {
            let on = ui.toggle("on");
            ui.set("seen", on);
        })
        .build_bare();
    let flip = app.world.spawn(LumenId("flip".to_string())).id();
    click(&mut app, flip);
    assert_eq!(store(&app).get_global_bool("on"), Some(true));
    assert_eq!(store(&app).get_global_bool("seen"), Some(true));
    click(&mut app, flip);
    assert_eq!(store(&app).get_global_bool("on"), Some(false));
}

/// A handler opens a menu where the pointer is, toggles it, and closes it.
#[test]
fn a_handler_opens_toggles_and_closes_a_menu() {
    let mut app = bare()
        .on_click("open", |ui, _| ui.open_menu("file"))
        .on_click("open-at", |ui, _| ui.open_menu_at("ctx", 12.0, 34.5))
        .on_click("toggle", |ui, _| ui.toggle_menu("file"))
        .on_click("close", |ui, _| ui.close_menu("ctx"))
        .build_bare();
    let [open, open_at, toggle, close] =
        ["open", "open-at", "toggle", "close"].map(|id| app.world.spawn(LumenId(id.into())).id());

    click(&mut app, open);
    assert_eq!(menu_open(&app, "file"), Some(true));
    assert_eq!(menu_at(&app, "file").as_deref(), Some(""));

    click(&mut app, toggle);
    assert_eq!(menu_open(&app, "file"), Some(false));
    click(&mut app, toggle);
    assert_eq!(menu_open(&app, "file"), Some(true));

    click(&mut app, open_at);
    assert_eq!(menu_open(&app, "ctx"), Some(true));
    assert_eq!(menu_at(&app, "ctx").as_deref(), Some("12 34.5"));
    click(&mut app, close);
    assert_eq!(menu_open(&app, "ctx"), Some(false));
}

/// A system toggles a menu through `Signals` the way a handler does through
/// `Ui`, and reads whether it is open.
#[test]
fn a_system_toggles_a_menu() {
    fn toggle(mut signals: Signals) {
        signals.toggle_menu("view");
        let open = signals.is_menu_open("view");
        signals.set("view_open", open);
    }
    let mut app = bare().add_systems(TickStage::Systems, toggle).build_bare();
    app.tick();
    assert_eq!(store(&app).get_global_bool("view_open"), Some(true));
    app.tick();
    assert_eq!(store(&app).get_global_bool("view_open"), Some(false));
}

/// A handler reads resources, borrows one beside the rest of `Ui`, and
/// reaches the world for what `Ui` does not wrap.
#[test]
fn a_handler_reads_and_scopes_resources_and_reaches_the_world() {
    #[derive(Resource)]
    struct Zoom(i64);
    #[derive(Resource)]
    struct Spawned(Entity);

    let mut app = bare()
        .insert_resource(Zoom(2))
        .on_click("zoom", |ui, _| {
            let zoom = ui.resource::<Zoom>().0;
            let missing = ui.get_resource::<Spawned>().is_none();
            ui.set("zoom_seen", zoom);
            ui.set("missing", missing);
            ui.resource_scope(|ui, mut z: Mut<'_, Zoom>| {
                z.0 *= 2;
                ui.set("zoom_scoped", z.0);
            });
            let marker = ui.world().spawn_empty().id();
            ui.world().insert_resource(Spawned(marker));
        })
        .build_bare();
    let zoom = app.world.spawn(LumenId("zoom".to_string())).id();
    click(&mut app, zoom);

    let read = |name| Property::<i64>::new(name).get(store(&app));
    assert_eq!(read("zoom_seen"), Some(2));
    assert_eq!(read("zoom_scoped"), Some(4));
    assert_eq!(store(&app).get_global_bool("missing"), Some(true));
    assert_eq!(app.world.resource::<Zoom>().0, 4);
    let marker = app.world.resource::<Spawned>().0;
    assert!(app.world.get_entity(marker).is_ok());
}

/// A record is built field by field, from a map, or from pairs, and each
/// lands in the list the same way.
#[test]
fn records_build_from_fields_maps_and_pairs() {
    let mut app = bare()
        .on_click("fill", |ui, _| {
            let mut first = Record::new();
            first.set("name", "set");
            let from_map = Record::from(HashMap::from([("name".to_string(), "map".to_string())]));
            let from_pairs: Record = [("name", "pairs"), ("n", "3")].into_iter().collect();
            ui.set_list("rows", [first, from_map, from_pairs]);
        })
        .build_bare();
    let fill = app.world.spawn(LumenId("fill".to_string())).id();
    click(&mut app, fill);

    let arrays = app
        .world
        .resource::<lumenui::lumen_core::signals::ArraySignals>();
    let rows = arrays.get("rows").expect("the list was written");
    let names: Vec<_> = rows
        .iter()
        .map(|r| r.get("name").map(String::as_str))
        .collect();
    assert_eq!(names, [Some("set"), Some("map"), Some("pairs")]);
    assert_eq!(rows[2].get("n").map(String::as_str), Some("3"));
}

/// `on_click` gates a system on a click on the id or inside it, and
/// `on_toggle` on a click that lands inside a toggleable element of that id.
#[test]
fn run_conditions_follow_clicks_into_the_element() {
    #[derive(Resource, Default)]
    struct Count {
        clicks: u32,
        toggles: u32,
    }
    fn count_click(mut c: ResMut<Count>) {
        c.clicks += 1;
    }
    fn count_toggle(mut c: ResMut<Count>) {
        c.toggles += 1;
    }

    let mut app = bare()
        .insert_resource(Count::default())
        .add_systems(TickStage::Systems, count_click.run_if(on_click("go")))
        .add_systems(TickStage::Systems, count_toggle.run_if(on_toggle("dark")))
        .build_bare();

    let go = app.world.spawn(LumenId("go".to_string())).id();
    let icon = app.world.spawn(ChildOf(go)).id();
    let other = app.world.spawn(LumenId("other".to_string())).id();
    let dark = app
        .world
        .spawn((LumenId("dark".to_string()), Toggleable::default()))
        .id();
    let knob = app.world.spawn(ChildOf(dark)).id();
    // A toggleable element with another id stops the walk before `dark`.
    let inner = app
        .world
        .spawn((
            LumenId("inner".to_string()),
            Toggleable::default(),
            ChildOf(dark),
        ))
        .id();

    click(&mut app, icon);
    click(&mut app, go);
    click(&mut app, other);
    click(&mut app, knob);
    click(&mut app, inner);
    // A plain element with the id is not a toggle.
    let plain = app.world.spawn(LumenId("dark".to_string())).id();
    click(&mut app, plain);

    let count = app.world.resource::<Count>();
    assert_eq!(count.clicks, 2);
    assert_eq!(count.toggles, 1);
}

/// What `lumenui::ui!()` expands to watches the crate directory in a debug
/// build, and `hot_reload` overrides it either way.
#[test]
fn compiled_ui_from_a_build_names_its_directory_and_reload() {
    static BYTES: &[u8] = b"";
    let debug = CompiledUi::from_build(BYTES, "/app/crate", true);
    assert_eq!(debug.source_dir(), Some(Path::new("/app/crate")));
    let release = CompiledUi::from_build(BYTES, "/app/crate", false);
    assert_eq!(release.source_dir(), Some(Path::new("/app/crate")));
    // The flag is private; Debug shows it.
    assert!(format!("{debug:?}").contains("watch: true"));
    assert!(format!("{:?}", debug.hot_reload(false)).contains("watch: false"));
    assert!(format!("{:?}", release.hot_reload(true)).contains("watch: true"));
}

fn fixture(tag: &str, markup: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenui-handler-ui-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("main.lmn"), markup).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    dir
}

/// A handler finds, queries, creates and classes elements through `Ui`. The
/// writes apply on the next tick, where a system reads them back.
#[test]
fn a_ready_handler_works_the_element_tree() {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn read_back() {
        let (Some(list), Some(row)) = (dom::get_by_id("list"), dom::get_by_id("row")) else {
            return;
        };
        let mut classes = list.classes();
        classes.sort();
        SEEN.lock().unwrap().push(format!(
            "{} {}",
            classes.join(","),
            row.parent() == Some(list)
        ));
    }

    let _guard = APP_ISOLATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = fixture(
        "dom",
        r#"<root id="app"><column id="list" class="list kept"></column></root>"#,
    );

    App::new(CompiledUi::compile(&dir).expect("the UI compiles"))
        .on_ready(|ui| {
            let list = ui.get_by_id("list").expect("the column is there");
            let found = ui.query(".list").single() == Ok(list);
            SEEN.lock().unwrap().push(format!("query {found}"));
            let row = ui.create("label");
            row.set_id("row");
            list.append(row);
            list.set_class_on("active", true)
                .set_class_on("list", false)
                .set_class_on("kept", true)
                .set_class_on("absent", false);
        })
        .add_systems(TickStage::Systems, read_back)
        .run_headless(4)
        .expect("the app runs");

    let seen = SEEN.lock().unwrap();
    assert_eq!(seen.first().map(String::as_str), Some("query true"));
    assert_eq!(seen.last().map(String::as_str), Some("active,kept true"));
}
