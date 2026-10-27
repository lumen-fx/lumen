// Drives the linked runtime with the dock module loaded from `lumen.toml`,
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! The dock module end to end, headless: markup read into docks, panels
//! shown, hidden, moved, popped out into windows of their own and docked
//! back, the events a handler hears, the layout saved and restored, and the
//! keys.

use std::path::PathBuf;

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use lumen_core::app::App;
use lumen_core::components::{LumenAttributes, LumenId};
use lumen_core::property_store::PropertyStore;
use lumen_core::window::{
    InputBlocked, PrimaryWindow, Window, WindowKind, WindowOf, WindowPartitions,
};
use lumen_dock::{DockPanel, DockWindow};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// The app directory and the app data directory are process-wide, so the
/// apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MARKUP: &str = r#"<root>
  <dock id="editor" label="Editor">
    <dock-split axis="row">
      <dock-stack basis="200">
        <dock-panel id="files" title="Files"><input id="name"/></dock-panel>
      </dock-stack>
      <dock-stack center="true">
        <dock-panel id="scene" title="Scene"><button id="scene-btn" text="scene"/></dock-panel>
        <dock-panel id="game" title="Game"><button id="game-btn" text="game"/></dock-panel>
      </dock-stack>
      <dock-stack basis="240">
        <dock-panel id="inspector" title="Inspector" hidden="true"><label text="inspect"/></dock-panel>
        <dock-panel id="log" title="Log"><label text="log"/></dock-panel>
      </dock-stack>
    </dock-split>
    <dock-panel id="profiler" title="Profiler" place="right"><label text="prof"/></dock-panel>
    <dock-layout name="Focus">
      <dock-stack center="true"><dock-ref panel="scene"/></dock-stack>
    </dock-layout>
  </dock>
  <dock-panel id="prefs" title="Preferences" place="window" dockable="false"
              window-kind="dialog" modal="parent" window-size="400 300" resizable="false">
    <button id="ok" text="OK"/>
  </dock-panel>
  <script src="main.cdl"/>
</root>"#;

const CANDELA: &str = r#"import "lumen.cdl";
fn on_move(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    lumen::signal_set("moved", detail["to"] as string);
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("editor"), "panelmove", "on_move");
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

fn build(name: &str, markup: &str, script: &str, config: &str) -> TestApp {
    let dir = std::env::temp_dir().join(format!("lumenc_dock_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    build_in(&dir, name, markup, script, config)
}

/// Build the app in `dir`, which may already hold extra sources.
fn build_in(
    dir: &std::path::Path,
    name: &str,
    markup: &str,
    script: &str,
    config: &str,
) -> TestApp {
    build_with(dir, name, markup, script, config, "")
}

/// Build the app in `dir` with `app` added to its `[app]` table.
fn build_with(
    dir: &std::path::Path,
    name: &str,
    markup: &str,
    script: &str,
    config: &str,
    app: &str,
) -> TestApp {
    let dir = dir.to_path_buf();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.cdl"), script).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[app]\nid = \"lumenc-dock-{name}-{}\"\n{app}\n[dependencies]\nlumen-dock = {{ bundled = true{config} }}\n\n[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
            std::process::id()
        ),
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    TestApp { app, dir }
}

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn parent(app: &App, e: Entity) -> Option<Entity> {
    app.world.get::<ChildOf>(e).map(ChildOf::parent)
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

fn ticks(app: &mut App, n: usize) {
    for _ in 0..n {
        app.tick();
    }
}

fn call(app: &mut App, f: impl FnOnce(&lumen_dock::Dock<'_>)) {
    let mut state = bevy_ecs::system::SystemState::<lumen_dock::Dock>::new(&mut app.world);
    let d = state.get(&app.world).expect("the dock is installed");
    f(&d);
}

/// The `<tabs>` a panel shows in, and the name it shows.
fn stack_of(app: &App, panel: Entity) -> (Entity, Option<String>) {
    let tab = parent(app, panel).expect("the panel sits in a tab");
    let tabs = parent(app, tab).expect("the tab sits in a stack");
    let shown = app
        .world
        .get::<LumenAttributes>(tabs)
        .and_then(|a| a.get("value"))
        .map(str::to_string);
    (tabs, shown)
}

#[test]
fn markup_becomes_a_dock_of_tab_stacks() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("markup", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let scene = entity(app, "scene");
    let game = entity(app, "game");
    let files = entity(app, "files");
    let inspector = entity(app, "inspector");
    assert!(
        app.world.get::<DockPanel>(scene).is_some(),
        "the panel was adopted"
    );
    let (center, shown) = stack_of(app, scene);
    assert_eq!(shown.as_deref(), Some("scene"));
    assert_eq!(
        stack_of(app, game).0,
        center,
        "scene and game share a stack"
    );
    assert_ne!(stack_of(app, files).0, center);
    // A panel written hidden waits in the store, as does the catalogue.
    call(app, |d| {
        assert!(!d.is_shown("inspector"));
        assert!(!d.is_shown("profiler"));
        assert!(d.is_shown("log"));
        assert!(!d.is_shown("prefs"));
        assert_eq!(d.panels().len(), 7);
    });
    let _ = inspector;
}

/// The window entity the dock opened for model window `id`, if any.
fn window_entity(app: &mut App) -> Vec<(Entity, String)> {
    let mut q = app.world.query::<(Entity, &DockWindow)>();
    q.iter(&app.world).map(|(e, w)| (e, w.id.clone())).collect()
}

fn window_named(app: &mut App, id: &str) -> Option<Entity> {
    let e = entity(app, id);
    window_of(app, e)
}

fn window_of(app: &App, e: Entity) -> Option<Entity> {
    app.world.resource::<WindowPartitions>().window_of(e)
}

fn primary(app: &mut App) -> Entity {
    let mut q = app
        .world
        .query_filtered::<Entity, bevy_ecs::query::With<PrimaryWindow>>();
    q.iter(&app.world).next().expect("a primary window")
}

#[test]
fn a_panel_pops_out_into_a_window_and_docks_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("detach", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let game = entity(app, "game");
    let button = entity(app, "game-btn");
    let editor = entity(app, "editor");
    let main = primary(app);
    assert_eq!(window_of(app, button), Some(main));

    call(app, |d| d.detach("game").expect("detach"));
    ticks(app, 3);
    let windows = window_entity(app);
    assert_eq!(windows.len(), 1, "one window opened");
    let (w, id) = windows[0].clone();
    assert_eq!(
        window_of(app, button),
        Some(w),
        "the panel's content shows in it"
    );
    assert_eq!(app.world.get::<Window>(w).unwrap().title, "Game");
    // The window's root sits under the dock, so styles and events reach it.
    let mut q = app.world.query::<(Entity, &WindowOf)>();
    let (root, _) = q
        .iter(&app.world)
        .find(|(_, of)| of.window() == w)
        .expect("a surface root");
    assert_eq!(parent(app, root), Some(editor));
    // The element moved; it is the same element.
    assert_eq!(entity(app, "game"), game);
    assert_eq!(
        signal(app, "moved").as_deref(),
        Some("window"),
        "the handler heard panelmove"
    );
    call(app, |d| {
        assert_eq!(d.window_of("game"), lumen_dock::api::window_number(&id));
        assert_eq!(d.window_of("scene"), 0);
        assert_eq!(d.window_of("inspector"), -1);
    });

    call(app, |d| d.attach("game").expect("attach"));
    ticks(app, 3);
    assert!(window_entity(app).is_empty(), "the window closed");
    assert!(app.world.get_entity(w).is_err());
    assert_eq!(window_of(app, button), Some(main));
    let (_, shown) = stack_of(app, game);
    assert_eq!(
        shown.as_deref(),
        Some("game"),
        "it came back as the showing tab"
    );
    assert_eq!(signal(app, "moved").as_deref(), Some("dock"));
}

#[test]
fn closing_a_window_docks_its_panels_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("close", MARKUP, CANDELA, "");
    let app = &mut t.app;
    call(app, |d| d.detach_group("scene").expect("detach group"));
    ticks(app, 3);
    let (w, _) = window_entity(app)[0].clone();
    assert_eq!(window_named(app, "game-btn"), Some(w));
    let mut requests = lumen_core::window_backend::CloseRequests::default();
    let outcome = lumen_core::window_backend::run_close_request(
        app,
        &mut requests,
        Some(w),
        lumen_core::input::CloseReason::WindowClose,
    );
    assert_eq!(outcome, lumen_core::window_backend::CloseOutcome::Closed);
    ticks(app, 2);
    let main = primary(app);
    assert_eq!(window_named(app, "game-btn"), Some(main));
    assert_eq!(window_named(app, "scene-btn"), Some(main));
    call(app, |d| {
        assert!(d.is_shown("scene") && d.is_shown("game"));
        assert_eq!(d.window_of("scene"), 0);
    });
}

#[test]
fn a_free_panel_is_a_modal_dialog_window() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("dialog", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let main = primary(app);
    assert!(window_entity(app).is_empty(), "a free panel starts hidden");
    call(app, |d| d.show("prefs").expect("show"));
    ticks(app, 3);
    let (w, _) = window_entity(app)[0].clone();
    let window = app.world.get::<Window>(w).unwrap().clone();
    assert_eq!(window.kind, WindowKind::Dialog);
    assert_eq!(window.owner, Some(main));
    assert_eq!(window.size, glam::Vec2::new(400.0, 300.0));
    assert!(!window.resizable);
    assert_eq!(
        app.world.get::<InputBlocked>(main).map(|b| b.by),
        Some(w),
        "the dialog blocks the main window"
    );
    assert_eq!(window_named(app, "ok"), Some(w));
    // It does not dock.
    call(app, |d| assert!(d.attach("prefs").is_err()));
    call(app, |d| d.hide("prefs").expect("hide"));
    ticks(app, 3);
    assert!(window_entity(app).is_empty());
    assert!(
        app.world.get::<InputBlocked>(main).is_none(),
        "the block lifts"
    );
}

const SCRIPTED: &str = r#"import "lumen.cdl";
fn on_ready() {
    dock::detach("files");
    lumen::signal_set("files_window", string(dock::window_of("files")));
    lumen::signal_set("shown_before", string(dock::is_shown("inspector")));
    dock::show("inspector");
    dock::move("log", "scene", "tab");
    dock::collapse("files", true);
    lumen::signal_set("count", string(dock::panels().len()));
}
fn main() {}
"#;

#[test]
fn candela_drives_the_dock_through_its_namespace() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("candela", MARKUP, SCRIPTED, "");
    let app = &mut t.app;
    ticks(app, 2);
    assert_eq!(signal(app, "files_window").as_deref(), Some("1"));
    assert_eq!(signal(app, "shown_before").as_deref(), Some("false"));
    assert_eq!(signal(app, "count").as_deref(), Some("7"));
    let windows = window_entity(app);
    assert_eq!(windows.len(), 1);
    assert_eq!(window_named(app, "name"), Some(windows[0].0));
    call(app, |d| {
        assert!(d.is_shown("inspector"));
        assert_eq!(d.window_of("log"), 0);
    });
    // log moved into the center stack, beside scene.
    let scene = entity(app, "scene");
    let log = entity(app, "log");
    assert_eq!(stack_of(app, log).0, stack_of(app, scene).0);
    assert_eq!(signal(app, "dock.shown.inspector").as_deref(), Some("true"));
    assert_eq!(signal(app, "dock.shown.profiler").as_deref(), Some("false"));
}

const RHAI: &str = r#"fn on_ready() {
    dock::hide("log");
    signal("rhai_shown", "").set(`${dock::is_shown("log")}`);
}
"#;

const LUA: &str = r#"function on_ready()
    dock.show("profiler")
    signal("lua_shown", ""):set(tostring(dock.is_shown("profiler")))
end
"#;

#[test]
fn rhai_and_lua_reach_the_same_functions() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let markup = MARKUP.replace(
        r#"<script src="main.cdl"/>"#,
        r#"<script src="main.cdl"/><script src="side.rhai"/><script src="side.lua"/>"#,
    );
    let dir_name = "hosts";
    let dir = std::env::temp_dir().join(format!("lumenc_dock_{dir_name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/side.rhai"), RHAI).unwrap();
    std::fs::write(dir.join("src/side.lua"), LUA).unwrap();
    let mut t = build_in(&dir, dir_name, &markup, CANDELA, "");
    let app = &mut t.app;
    ticks(app, 2);
    assert_eq!(signal(app, "rhai_shown").as_deref(), Some("false"));
    assert_eq!(signal(app, "lua_shown").as_deref(), Some("true"));
    call(app, |d| {
        assert!(!d.is_shown("log"));
        assert!(d.is_shown("profiler"));
    });
}

fn press(app: &mut App, key: &str, ctrl: bool, shift: bool) {
    let key = lumen_core::input::Key::from(key);
    app.world.write_message(lumen_core::input::KeyPressed {
        code: key.us_code(),
        key,
        modifiers: lumen_core::input::Modifiers {
            ctrl,
            shift,
            ..Default::default()
        },
        repeat: false,
        time_stamp: std::time::Instant::now(),
    });
    ticks(app, 2);
}

fn click(app: &mut App, e: Entity, button: lumen_core::input::PointerButton) {
    app.world.write_message(lumen_core::input::ClickEvent {
        entity: e,
        position: glam::Vec2::ZERO,
        button,
        local: None,
    });
    ticks(app, 3);
}

fn focused(app: &App) -> Option<Entity> {
    app.world.resource::<lumen_core::input::FocusTracker>().0
}

/// The strip button that shows `panel`.
fn tab_button(app: &mut App, panel: &str) -> Entity {
    let mut q = app
        .world
        .query::<(Entity, &lumen_primitives::TabStripButton)>();
    let pairs: Vec<(Entity, Entity)> = q.iter(&app.world).map(|(e, b)| (e, b.tab)).collect();
    pairs
        .into_iter()
        .find(|(_, tab)| {
            app.world
                .get::<lumen_dock::DockTab>(*tab)
                .is_some_and(|t| t.panel == panel)
        })
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no tab for {panel}"))
}

#[test]
fn the_user_picks_closes_and_middle_clicks_tabs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("tabs", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let game_tab = tab_button(app, "game");
    click(app, game_tab, lumen_core::input::PointerButton::Primary);
    let game = entity(app, "game");
    let (_, shown) = stack_of(app, game);
    assert_eq!(shown.as_deref(), Some("game"));
    let saved: lumen_dock::Layout = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    assert_eq!(
        saved
            .stack(&saved.find("game").unwrap().stack)
            .unwrap()
            .active
            .as_deref(),
        Some("game"),
        "the pick reached the model"
    );
    // The close button on log's tab hides it.
    let log_tab = tab_button(app, "log");
    let close = {
        let mut q = app
            .world
            .query::<(Entity, &ChildOf, &lumen_dock::DockCloseButton)>();
        q.iter(&app.world)
            .find(|(_, p, _)| p.parent() == log_tab)
            .map(|(e, ..)| e)
            .expect("the tab has a close button")
    };
    click(app, close, lumen_core::input::PointerButton::Primary);
    call(app, |d| {
        assert!(!d.is_shown("log"), "the close button hid it")
    });
    // A middle click on a tab closes it too.
    let scene_tab = tab_button(app, "scene");
    click(app, scene_tab, lumen_core::input::PointerButton::Middle);
    call(app, |d| assert!(!d.is_shown("scene")));
}

const VETO: &str = r#"import "lumen.cdl";
fn keep(ev: int) {
    lumen::event_prevent_default(ev);
    lumen::signal_set("asked", lumen::event_type(ev));
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("files"), "panelclose", "keep");
}
fn main() {}
"#;

#[test]
fn a_handler_keeps_a_panel_open() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("veto", MARKUP, VETO, "");
    let app = &mut t.app;
    let files_tab = tab_button(app, "files");
    click(app, files_tab, lumen_core::input::PointerButton::Middle);
    assert_eq!(signal(app, "asked").as_deref(), Some("panelclose"));
    call(app, |d| assert!(d.is_shown("files"), "the handler kept it"));
}

#[test]
fn keys_move_focus_between_stacks_tabs_and_windows() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("keys", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let name = entity(app, "name");
    // F6 from nowhere lands in the first stack: the files panel's input.
    press(app, "F6", false, false);
    assert_eq!(focused(app), Some(name));
    press(app, "F6", false, false);
    assert_eq!(focused(app), Some(entity(app, "scene-btn")));
    press(app, "F6", false, true);
    assert_eq!(focused(app), Some(name), "Shift+F6 goes back");
    // Ctrl+PageDown in the center stack shows its next tab.
    press(app, "F6", false, false);
    press(app, "PageDown", true, false);
    call(app, |d| {
        let l = d.save();
        assert_eq!(
            l.stack(&l.find("game").unwrap().stack)
                .unwrap()
                .active
                .as_deref(),
            Some("game")
        );
    });
    // Ctrl+F6 goes to a window of panels and back.
    call(app, |d| d.detach("log").unwrap());
    ticks(app, 2);
    let w = window_entity(app)[0].0;
    let main = primary(app);
    lumen_core_focus(app, name);
    press(app, "F6", true, false);
    assert_eq!(focused(app).and_then(|f| window_of(app, f)), Some(w));
    press(app, "F6", true, false);
    assert_eq!(focused(app).and_then(|f| window_of(app, f)), Some(main));
    // Shift+Space on a focused tab maximizes its panel; Escape restores.
    let scene_tab = tab_button(app, "scene");
    lumen_core_focus(app, scene_tab);
    press(app, "Space", false, true);
    call(app, |d| {
        assert_eq!(d.save().docks["editor"].maximized.as_deref(), Some("scene"))
    });
    press(app, "Escape", false, false);
    call(app, |d| {
        assert_eq!(d.save().docks["editor"].maximized, None)
    });
    // Delete on a focused tab asks to close it.
    lumen_core_focus(app, scene_tab);
    press(app, "Delete", false, false);
    ticks(app, 2);
    call(app, |d| assert!(!d.is_shown("scene")));
}

fn lumen_core_focus(app: &mut App, e: Entity) {
    if let Some(prev) = focused(app) {
        app.world
            .entity_mut(prev)
            .remove::<lumen_core::input::Focused>();
    }
    app.world.entity_mut(e).insert(lumen_core::input::Focused);
    app.world
        .resource_mut::<lumen_core::input::FocusTracker>()
        .0 = Some(e);
}

const LISTEN: &str = r#"import "lumen.cdl";
fn on_error(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    lumen::signal_set("error", detail["reason"] as string);
}
fn on_restore(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    lumen::signal_set("restore", (detail["panel"] as string) + ":" + (detail["kind"] as string));
}
fn on_change(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    lumen::signal_set("change", detail["reason"] as string);
}
fn on_ready() {
    let dock = lumen::node_get_by_id("editor");
    lumen::event_on(dock, "layouterror", "on_error");
    lumen::event_on(dock, "panelrestore", "on_restore");
    lumen::event_on(dock, "layoutchange", "on_change");
}
fn main() {}
"#;

#[test]
fn a_layout_saves_restores_and_names() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build(
        "layouts",
        MARKUP,
        LISTEN,
        ", config = { layout-version = 3 }",
    );
    let app = &mut t.app;
    let mut saved = String::new();
    call(app, |d| {
        d.detach("game").unwrap();
        d.hide("files").unwrap();
    });
    ticks(app, 3);
    assert_eq!(signal(app, "change").as_deref(), Some("api"));
    call(app, |d| saved = d.save().to_json());
    assert!(saved.contains("\"layout-version\": 3"));
    call(app, |d| d.reset());
    ticks(app, 3);
    call(app, |d| {
        assert!(d.is_shown("files"));
        assert_eq!(d.window_of("game"), 0);
    });
    assert_eq!(signal(app, "change").as_deref(), Some("layout"));
    // Restoring brings the window back.
    let restored = Some(lumen_dock::Layout::from_json(&saved, 3).unwrap());
    call(app, |d| d.restore(restored.as_ref().unwrap()));
    ticks(app, 3);
    assert_eq!(window_entity(app).len(), 1);
    call(app, |d| assert!(!d.is_shown("files")));

    // A layout of another version is refused with an event.
    let older = saved.replace("\"layout-version\": 3", "\"layout-version\": 2");
    call(app, |d| assert!(d.restore_json(&older).is_err()));
    ticks(app, 3);
    assert_eq!(signal(app, "error").as_deref(), Some("version"));

    // Named layouts: the markup's and the user's.
    call(app, |d| {
        assert_eq!(d.layouts(), vec!["Focus".to_string()]);
        d.save_layout("Mine");
        assert_eq!(d.layouts(), vec!["Focus".to_string(), "Mine".to_string()]);
        d.apply_layout("Focus").unwrap();
    });
    ticks(app, 3);
    call(app, |d| {
        let l = d.save();
        // Panels the layout does not name join its center.
        assert_eq!(
            l.find("files").unwrap().stack,
            l.find("scene").unwrap().stack
        );
        assert!(d.apply_layout("Nope").is_err());
        d.apply_layout("Mine").unwrap();
        assert!(d.delete_layout("Mine"));
        assert!(!d.delete_layout("Mine"));
    });
    ticks(app, 2);
    assert_eq!(signal(app, "dock.layout").as_deref(), Some("Mine"));
}

#[test]
fn auto_persist_keeps_the_layout_between_runs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let config = ", config = { persist = \"auto\" }";
    let file;
    {
        let mut t = build("persist", MARKUP, LISTEN, config);
        let app = &mut t.app;
        file = lumen_core::app_paths::data_dir().join(lumen_dock::FILE);
        let _ = std::fs::remove_file(&file);
        call(app, |d| {
            d.detach("game").unwrap();
            d.hide("files").unwrap();
            d.save_layout("Mine");
        });
        ticks(app, 3);
        let text = std::fs::read_to_string(&file).expect("the layout was kept");
        assert!(text.contains("\"windows\""));
        assert!(text.contains("\"Mine\""));
    }
    let mut t = build("persist", MARKUP, LISTEN, config);
    let app = &mut t.app;
    ticks(app, 3);
    call(app, |d| {
        assert!(!d.is_shown("files"), "the kept layout was restored");
        assert!(d.window_of("game") > 0);
        assert!(d.layouts().contains(&"Mine".to_string()));
    });
    assert_eq!(window_entity(app).len(), 1);
    // A file of another format is set aside with a layouterror.
    drop(t);
    std::fs::write(&file, "{\"lumen-dock\": 99}").unwrap();
    let mut t = build("persist", MARKUP, LISTEN, config);
    let app = &mut t.app;
    ticks(app, 3);
    assert_eq!(signal(app, "error").as_deref(), Some("format"));
    call(app, |d| assert!(d.is_shown("files")));
    let _ = std::fs::remove_file(&file);
}

#[test]
fn maximize_collapse_and_resize_shape_the_elements() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("shape", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let files = entity(app, "files");
    let scene = entity(app, "scene");
    let (files_stack, _) = stack_of(app, files);
    let (center, _) = stack_of(app, scene);
    let display = |app: &App, e: Entity| {
        app.world
            .get::<lumen_core::components::Style>(e)
            .map(|s| s.display)
    };
    call(app, |d| d.maximize("scene").unwrap());
    ticks(app, 2);
    assert_eq!(
        display(app, files_stack),
        Some(lumen_core::components::Display::None)
    );
    assert_ne!(
        display(app, center),
        Some(lumen_core::components::Display::None)
    );
    let has = |app: &App, e: Entity, c: &str| {
        app.world
            .get::<lumen_core::components::LumenClasses>(e)
            .is_some_and(|cl| cl.has(c))
    };
    assert!(has(app, center, "maximized"));
    call(app, |d| assert!(d.restore_maximized("editor")));
    ticks(app, 2);
    assert_ne!(
        display(app, files_stack),
        Some(lumen_core::components::Display::None)
    );
    assert!(!has(app, center, "maximized"));

    call(app, |d| d.collapse("files", true).unwrap());
    ticks(app, 2);
    assert!(has(app, files_stack, "collapsed"));
    call(app, |d| d.collapse("files", false).unwrap());
    ticks(app, 2);
    assert!(!has(app, files_stack, "collapsed"));

    // A gutter drag the user ends lands in the model.
    let split = parent(app, files_stack).expect("the stack sits in a split");
    app.world.write_message(lumen_primitives::SplitResized {
        split,
        sizes: vec![150.0, 500.0, 300.0],
    });
    ticks(app, 2);
    call(app, |d| {
        let l = d.save();
        let lumen_dock::model::Node::Split(root) = &l.docks["editor"].root else {
            panic!("a split root")
        };
        assert_eq!(root.children[0].basis, Some(150.0));
    });
}

const DYNAMIC: &str = r#"import "lumen.cdl";
fn on_ready() {
    let p = lumen::node_spawn("dock-panel");
    lumen::node_set_attr(p, "id", "doc-1");
    lumen::node_set_attr(p, "title", "Doc 1");
    lumen::node_set_attr(p, "kind", "doc");
    lumen::node_set_attr(p, "state", "{\"n\":1}");
    lumen::node_append(lumen::node_get_by_id("editor"), p);
}
fn main() {}
"#;

#[test]
fn panels_come_and_go_at_run_time() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("dynamic", MARKUP, DYNAMIC, "");
    let app = &mut t.app;
    ticks(app, 3);
    call(app, |d| {
        assert!(d.is_shown("doc-1"), "an appended panel takes its place")
    });
    let doc = entity(app, "doc-1");
    let scene = entity(app, "scene");
    assert_eq!(
        stack_of(app, doc).0,
        stack_of(app, scene).0,
        "in the center"
    );
    // The DOM sees the panel under the dock, not the structure around it.
    let editor = entity(app, "editor");
    let idx = lumen_core::node::dom_index_snapshot();
    assert_eq!(idx.parent(doc), Some(editor));
    assert!(idx.children(editor).contains(&doc));
    let saved = {
        let mut out = String::new();
        call(app, |d| out = d.save().to_json());
        out
    };
    assert!(saved.contains("\"kind\": \"doc\""));
    // Removing the element leaves its place as a placeholder; a restore asks
    // the app for it.
    app.world.entity_mut(doc).despawn();
    ticks(app, 2);
    call(app, |d| {
        assert!(d.save().find("doc-1").is_some(), "the place is kept");
        d.restore_json(&saved).unwrap();
    });
    ticks(app, 2);
    call(app, |d| assert!(d.forget("doc-1")));
}

#[test]
fn closing_a_destroy_on_close_panel_removes_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let markup = MARKUP.replace(
        r#"<dock-panel id="log" title="Log">"#,
        r#"<dock-panel id="log" title="Log" destroy-on-close="true">"#,
    );
    let mut t = build("destroy", &markup, CANDELA, "");
    let app = &mut t.app;
    let log = entity(app, "log");
    call(app, |d| d.close("log").unwrap());
    ticks(app, 2);
    assert!(app.world.get_entity(log).is_err(), "the element is gone");
    call(app, |d| {
        assert!(!d.panels().contains(&"log".to_string()));
        assert!(d.show("log").is_err());
        assert!(d.toggle("files").is_ok());
        assert!(!d.is_shown("files"));
        assert!(d.move_to("game", "nope", "tab").is_err());
        assert!(d.move_to("game", "files", "middle").is_err());
        d.move_to("game", "editor", "left").unwrap();
        d.focus("game").unwrap();
    });
    ticks(app, 2);
    let game_btn = entity(app, "game-btn");
    assert_eq!(focused(app), Some(game_btn), "focus went into the panel");
}

#[test]
fn a_window_remembers_its_place_on_the_desktop() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("place", MARKUP, CANDELA, "");
    let app = &mut t.app;
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    {
        let mut win = app.world.get_mut::<Window>(w).unwrap();
        win.position = Some(glam::IVec2::new(300, 200));
        win.size = glam::Vec2::new(500.0, 400.0);
    }
    ticks(app, 3);
    let saved = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    let place = &saved.windows[0].place;
    let rect = place.rect.expect("a rect");
    assert_eq!((rect.w, rect.h), (500.0, 400.0));
    assert!(place.monitor.is_some(), "the monitor is named");
    // Restored, the window opens there again.
    call(app, |d| d.attach("game").unwrap());
    ticks(app, 2);
    call(app, |d| d.restore(&saved));
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let win = app.world.get::<Window>(w).unwrap();
    assert_eq!(win.size, glam::Vec2::new(500.0, 400.0));
    assert_eq!(
        win.position.map(|p| (p.x, p.y)),
        Some(((rect.x) as i32, (rect.y) as i32))
    );
}

#[test]
fn check_refuses_a_dock_inside_a_dock() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let nested = MARKUP.replace(
        r#"<label text="log"/>"#,
        r#"<dock id="inner"><dock-stack><dock-panel id="deep" title="Deep"/></dock-stack></dock>"#,
    );
    let t = build("nested", MARKUP, CANDELA, "");
    std::fs::write(t.dir.join("src/main.lmn"), &nested).unwrap();
    let err = lumenc::check_app(&t.dir).expect_err("a nested dock is refused");
    assert!(
        err.to_string()
            .contains("<dock> cannot be written inside <dock>"),
        "{err}"
    );
    std::fs::write(t.dir.join("src/main.lmn"), MARKUP).unwrap();
    lumenc::check_app(&t.dir).expect("the plain dock checks");
}

const WINDOWED: &str = r#"import "lumen.cdl";
fn on_focus(ev: int) {
    lumen::signal_set("focus", lumen::event_type(ev));
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("game"), "windowfocus", "on_focus");
    lumen::event_on(lumen::node_get_by_id("game"), "windowblur", "on_focus");
    dock::detach("game");
    lumen::signal_set("windows", string(dock::windows().len()));
}
fn main() {}
"#;

#[test]
fn a_window_of_panels_tells_them_about_its_focus() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("winfocus", MARKUP, WINDOWED, "");
    let app = &mut t.app;
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    app.world.write_message(lumen_core::input::WindowFocused {
        window: w,
        focused: true,
    });
    ticks(app, 3);
    assert_eq!(signal(app, "focus").as_deref(), Some("windowfocus"));
    app.world.write_message(lumen_core::input::WindowFocused {
        window: w,
        focused: false,
    });
    ticks(app, 3);
    assert_eq!(signal(app, "focus").as_deref(), Some("windowblur"));
}

#[test]
fn window_close_hide_and_keys_off_follow_the_config() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let config = ", config = { window-close = \"hide\", keys = { next-stack = \"Alt+F7\", close-tab = \"\" } }";
    let mut t = build("config", MARKUP, CANDELA, config);
    let app = &mut t.app;
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let mut requests = lumen_core::window_backend::CloseRequests::default();
    lumen_core::window_backend::run_close_request(
        app,
        &mut requests,
        Some(w),
        lumen_core::input::CloseReason::WindowClose,
    );
    ticks(app, 2);
    call(app, |d| {
        assert!(!d.is_shown("game"), "closing the window hid it")
    });
    // F6 is rebound to Alt+F7.
    press(app, "F6", false, false);
    assert_eq!(focused(app), None);
    app.world.write_message(lumen_core::input::KeyPressed {
        key: lumen_core::input::Key::from("F7"),
        code: lumen_core::input::Key::from("F7").us_code(),
        modifiers: lumen_core::input::Modifiers {
            alt: true,
            ..Default::default()
        },
        repeat: false,
        time_stamp: std::time::Instant::now(),
    });
    ticks(app, 2);
    assert_eq!(focused(app), Some(entity(app, "name")));
    // Delete is off.
    let files_tab = tab_button(app, "files");
    lumen_core_focus(app, files_tab);
    press(app, "Delete", false, false);
    call(app, |d| assert!(d.is_shown("files")));
}

const SCRIPT_LAYOUTS: &str = r#"import "lumen.cdl";
fn on_ready() {
    let saved = dock::save();
    dock::hide("files");
    lumen::signal_set("bad", string(dock::restore("not a layout")));
    lumen::signal_set("good", string(dock::restore(saved)));
    lumen::signal_set("named", string(dock::save_layout("Mine")) + string(dock::apply_layout("Focus")));
    lumen::signal_set("deleted", string(dock::delete_layout("Mine")));
    dock::reset();
    lumen::signal_set("focus", string(dock::focus("game")));
    lumen::signal_set("maxed", string(dock::maximize("game")) + string(dock::restore_maximized("editor")));
    lumen::signal_set("toggled", string(dock::toggle("log")));
    lumen::signal_set("closed", string(dock::close("log")));
    lumen::signal_set("forgot", string(dock::forget("profiler")));
    lumen::signal_set("group", string(dock::detach_group("scene")) + string(dock::attach("scene")));
    lumen::signal_set("missing", string(dock::show("nope")));
}
fn main() {}
"#;

#[test]
fn every_script_function_answers() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("allfns", MARKUP, SCRIPT_LAYOUTS, "");
    let app = &mut t.app;
    ticks(app, 3);
    for (name, want) in [
        ("bad", "false"),
        ("good", "true"),
        ("named", "truetrue"),
        ("deleted", "true"),
        ("focus", "true"),
        ("maxed", "truetrue"),
        ("toggled", "false"),
        ("closed", "true"),
        ("forgot", "false"),
        ("group", "truetrue"),
        ("missing", "false"),
    ] {
        assert_eq!(signal(app, name).as_deref(), Some(want), "{name}");
    }
}

/// A saved layout's window that was maximized or fullscreen comes back that
/// way, and keeps the rectangle it goes back to when the user restores it.
#[test]
fn a_window_comes_back_maximized_or_fullscreen() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("maxwin", MARKUP, CANDELA, "");
    let app = &mut t.app;
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    {
        let mut win = app.world.get_mut::<Window>(w).unwrap();
        win.position = Some(glam::IVec2::new(300, 200));
        win.size = glam::Vec2::new(500.0, 400.0);
    }
    ticks(app, 2);
    // The user maximizes it; the platform hands it the whole screen.
    {
        let mut win = app.world.get_mut::<Window>(w).unwrap();
        win.maximized = true;
        win.size = glam::Vec2::new(1920.0, 1080.0);
    }
    ticks(app, 3);
    let saved = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    let place = &saved.windows[0].place;
    assert!(place.maximized && !place.fullscreen);
    let rect = place.rect.expect("a rect");
    assert_eq!(
        (rect.w, rect.h),
        (500.0, 400.0),
        "the rectangle it restores to"
    );
    let text = {
        let mut out = String::new();
        call(app, |d| out = d.save().to_json());
        out
    };
    assert!(text.contains("\"maximized\": true"), "{text}");

    // Docked back and restored: it opens maximized over its old rectangle.
    call(app, |d| d.attach("game").unwrap());
    ticks(app, 2);
    assert!(window_entity(app).is_empty());
    call(app, |d| d.restore(&saved));
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let win = app.world.get::<Window>(w).unwrap();
    assert!(win.maximized && !win.fullscreen);
    assert_eq!(win.size, glam::Vec2::new(500.0, 400.0));
    assert!(
        app.world
            .get::<lumen_core::window::WindowRuntime>(w)
            .unwrap()
            .maximized,
        "the window reports it"
    );

    // A restore into the window it already has applies the state too.
    let mut full = saved.clone();
    full.windows[0].place.maximized = false;
    full.windows[0].place.fullscreen = true;
    call(app, |d| d.restore(&full));
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let win = app.world.get::<Window>(w).unwrap();
    assert!(!win.maximized && win.fullscreen);
}

/// On a desktop that places windows itself and does not say where (the
/// headless stand-in's Wayland mode), a window opens at its saved size
/// where the desktop puts it, and the saved position is kept, so the same
/// layout restores in full on a desktop that does position windows.
#[test]
fn wayland_places_windows_and_the_saved_position_survives() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("wayland", MARKUP, CANDELA, "");
    let app = &mut t.app;
    app.world
        .insert_resource(lumen_core::window_backend::HeadlessDesktop {
            positioning: false,
            toplevel_drag: false,
        });
    ticks(app, 1);
    // A pop-out with no saved place opens where the desktop puts it.
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let runtime = app
        .world
        .get::<lumen_core::window::WindowRuntime>(w)
        .unwrap()
        .clone();
    assert_eq!(runtime.position, None, "the desktop does not say where");
    assert!(runtime.monitor.is_some(), "it does say which monitor");
    // A layout saved elsewhere: its position is not applied, its size is.
    let mut layout = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    layout.windows[0].place.rect = Some(lumen_dock::model::Rect {
        x: 300.0,
        y: 200.0,
        w: 500.0,
        h: 400.0,
    });
    layout.windows[0].place.monitor = Some(lumen_dock::model::MonitorKey {
        name: "virtual-1".into(),
        origin: [0, 0],
        scale: 1.0,
    });
    call(app, |d| d.attach("game").unwrap());
    ticks(app, 2);
    assert!(window_entity(app).is_empty());
    call(app, |d| {
        d.restore_json(&layout.to_json())
            .expect("a layout this app can use")
    });
    ticks(app, 3);
    let (w, _) = window_entity(app)[0].clone();
    assert_eq!(
        app.world.get::<Window>(w).unwrap().size,
        glam::Vec2::new(500.0, 400.0),
        "the size restores"
    );
    assert_eq!(
        app.world
            .get::<lumen_core::window::WindowRuntime>(w)
            .unwrap()
            .position,
        None
    );
    // Saved again, it keeps where it was on the other desktop.
    let saved = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    let place = &saved.windows[0].place;
    let rect = place.rect.expect("a rect");
    assert_eq!(
        (rect.x, rect.y, rect.w, rect.h),
        (300.0, 200.0, 500.0, 400.0)
    );
    assert_eq!(
        place.monitor.as_ref().map(|m| m.name.as_str()),
        Some("virtual-1")
    );
}

/// With `[app] quit = "explicit"`, closing the main window hides it and the
/// windows of panels with it; the app runs on, and showing the main window
/// again shows them again.
#[test]
fn quit_explicit_hides_every_window_and_brings_them_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("lumenc_dock_explicit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut t = build_with(
        &dir,
        "explicit",
        MARKUP,
        CANDELA,
        "",
        "quit = \"explicit\"\n",
    );
    let app = &mut t.app;
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    let main = primary(app);
    let mut requests = lumen_core::window_backend::CloseRequests::default();
    let outcome = lumen_core::window_backend::run_close_request(
        app,
        &mut requests,
        Some(main),
        lumen_core::input::CloseReason::WindowClose,
    );
    assert_eq!(outcome, lumen_core::window_backend::CloseOutcome::Hidden);
    ticks(app, 2);
    assert!(!app.world.get::<Window>(main).unwrap().visible);
    assert!(!app.world.get::<Window>(w).unwrap().visible);
    call(app, |d| {
        assert!(d.window_of("game") > 0, "the panel stays in its window")
    });
    app.world.get_mut::<Window>(main).unwrap().visible = true;
    ticks(app, 2);
    assert!(app.world.get::<Window>(w).unwrap().visible);
}

/// A page's dock and the windows its panels popped out into close when the
/// app navigates away, without asking anyone, and come back where they were
/// when it returns.
#[test]
fn page_navigation_closes_a_pages_windows_and_brings_them_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("lumenc_dock_pages_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/other.lmn"),
        r#"<root><label id="other" text="OTHER"/></root>"#,
    )
    .unwrap();
    let mut t = build_with(
        &dir,
        "pages",
        MARKUP,
        CANDELA,
        "",
        "\n[pages]\nentry = \"main\"\n",
    );
    let app = &mut t.app;
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    assert_eq!(window_entity(app).len(), 1);
    lumen_core::nav::navigate("other");
    ticks(app, 5);
    assert!(window_entity(app).is_empty(), "the page's window closed");
    call(app, |d| {
        assert_eq!(d.window_of("game"), 1, "its place is remembered");
    });
    lumen_core::nav::navigate("main");
    ticks(app, 6);
    let windows = window_entity(app);
    assert_eq!(windows.len(), 1, "the window is back");
    let button = entity(app, "game-btn");
    assert_eq!(window_of(app, button), Some(windows[0].0));
}

/// The items of the open tab menu, in order.
fn menu_items(app: &mut App) -> Vec<(Entity, lumen_dock::MenuAction)> {
    let mut q = app
        .world
        .query::<(Entity, &lumen_dock::DockMenuItem, &ChildOf)>();
    let mut items: Vec<(Entity, lumen_dock::MenuAction, Entity)> = q
        .iter(&app.world)
        .map(|(e, a, p)| (e, a.0, p.parent()))
        .collect();
    // Only the latest menu: the one whose popup is open.
    let open = signal(app, &format!("__menu_open:{}", lumen_dock::MENU_ID));
    if open.as_deref() != Some("true") {
        return Vec::new();
    }
    let panel = items.last().map(|i| i.2);
    items.retain(|i| Some(i.2) == panel);
    let order = |e: Entity| {
        app.world
            .get::<ChildOf>(e)
            .and_then(|p| app.world.get::<bevy_ecs::hierarchy::Children>(p.parent()))
            .and_then(|kids| kids.iter().position(|k| *k == e))
            .unwrap_or(0)
    };
    items.sort_by_key(|i| order(i.0));
    items.into_iter().map(|(e, a, _)| (e, a)).collect()
}

fn context_menu(app: &mut App, on: Entity) {
    app.world
        .write_message(lumen_core::input::ContextMenuEvent {
            entity: on,
            position: glam::Vec2::new(40.0, 30.0),
            button: lumen_core::input::PointerButton::Secondary,
            local: None,
        });
    ticks(app, 2);
}

/// What the dock announced, kept by [`hear`] before the accessibility
/// tree takes the queue.
#[derive(bevy_ecs::resource::Resource, Default)]
struct Heard(Vec<String>);

/// Keep every announcement the dock queues, for [`announced`].
fn hear(app: &mut App) {
    use bevy_ecs::schedule::IntoScheduleConfigs;
    app.world.init_resource::<Heard>();
    app.add_systems(
        lumen_core::tick::TickStage::Systems,
        (|queue: bevy_ecs::system::Res<lumen_core::components::A11yAnnouncementQueue>,
          mut heard: bevy_ecs::system::ResMut<Heard>| {
            heard.0.extend(queue.pending.iter().map(|(m, _)| m.clone()));
        })
        .after(lumen_dock::DockSet::Report),
    );
}

fn announced(app: &App) -> Vec<String> {
    app.world.resource::<Heard>().0.clone()
}

/// A tab's context menu offers what fits where the panel is, does it, and
/// says so to a screen reader.
#[test]
fn the_tab_menu_moves_panels_out_and_back() {
    use lumen_dock::MenuAction as A;
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("tabmenu", MARKUP, CANDELA, "");
    let app = &mut t.app;
    hear(app);
    let game_tab = tab_button(app, "game");
    context_menu(app, game_tab);
    let items = menu_items(app);
    let actions: Vec<A> = items.iter().map(|i| i.1).collect();
    assert_eq!(
        actions,
        [A::Detach, A::DetachGroup, A::Maximize, A::Close],
        "a docked panel sharing its stack"
    );
    let text = |app: &App, e: Entity| {
        app.world
            .get::<lumen_core::components::TextContent>(e)
            .map(|t| t.0.clone())
    };
    assert_eq!(text(app, items[0].0).as_deref(), Some("Move to new window"));
    click(app, items[0].0, lumen_core::input::PointerButton::Primary);
    let windows = window_entity(app);
    assert_eq!(windows.len(), 1, "Move to new window opened one");
    let w = windows[0].0;
    assert_eq!(window_named(app, "game-btn"), Some(w));
    assert!(
        announced(app).contains(&"Game moved to a new window".to_string()),
        "{:?}",
        announced(app)
    );

    // In its own window, alone: dock it back or close it.
    let game_tab = tab_button(app, "game");
    context_menu(app, game_tab);
    let items = menu_items(app);
    assert_eq!(
        items.iter().map(|i| i.1).collect::<Vec<_>>(),
        [A::Attach, A::Close]
    );
    assert_eq!(
        window_of(app, items[0].0),
        Some(w),
        "the menu opens in the window the tab is in"
    );
    click(app, items[0].0, lumen_core::input::PointerButton::Primary);
    assert!(window_entity(app).is_empty(), "Dock back closed the window");
    assert!(announced(app).contains(&"Game docked back".to_string()));

    // Maximize and back, then close through the menu.
    let scene_tab = tab_button(app, "scene");
    context_menu(app, scene_tab);
    let maximize = menu_items(app)[2].0;
    click(app, maximize, lumen_core::input::PointerButton::Primary);
    call(app, |d| {
        assert_eq!(d.save().docks["editor"].maximized.as_deref(), Some("scene"))
    });
    let scene_tab = tab_button(app, "scene");
    context_menu(app, scene_tab);
    let items = menu_items(app);
    assert_eq!(
        items.iter().map(|i| i.1).collect::<Vec<_>>(),
        [A::Detach, A::DetachGroup, A::Restore, A::Close]
    );
    click(app, items[2].0, lumen_core::input::PointerButton::Primary);
    call(app, |d| {
        assert_eq!(d.save().docks["editor"].maximized, None)
    });
    let log_tab = tab_button(app, "log");
    context_menu(app, log_tab);
    let items = menu_items(app);
    assert_eq!(items.last().map(|i| i.1), Some(A::Close));
    click(
        app,
        items.last().unwrap().0,
        lumen_core::input::PointerButton::Primary,
    );
    call(app, |d| assert!(!d.is_shown("log"), "Close hid it"));

    // A move inside the dock names its new neighbour.
    call(app, |d| d.move_to("files", "scene", "tab").unwrap());
    ticks(app, 2);
    assert!(
        announced(app).contains(&"Files moved beside Scene".to_string()),
        "{:?}",
        announced(app)
    );
}

/// The Menu key on a focused tab opens the same menu, and the arrow keys
/// and Enter pick an item.
#[test]
fn the_tab_menu_answers_the_keyboard() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("tabmenukeys", MARKUP, CANDELA, "");
    let app = &mut t.app;
    let files_tab = tab_button(app, "files");
    lumen_core_focus(app, files_tab);
    press(app, "ContextMenu", false, false);
    let items = menu_items(app);
    assert_eq!(
        items.first().map(|i| i.1),
        Some(lumen_dock::MenuAction::Detach)
    );
    press(app, "ArrowDown", false, false);
    assert_eq!(
        focused(app),
        Some(items[0].0),
        "the first item is highlighted"
    );
    press(app, "Enter", false, false);
    ticks(app, 2);
    call(app, |d| {
        assert!(d.window_of("files") > 0, "Enter moved it out")
    });
}

const RETITLE: &str = r#"import "lumen.cdl";
fn on_ready() {
    lumen::signal_set("game.file", "level1.map");
}
fn main() {}
"#;

/// A panel's tab follows its title and icon when a script changes them,
/// and its window's title follows the signal it binds.
#[test]
fn a_tab_follows_its_panels_title_icon_and_bound_window_title() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let markup = MARKUP.replace(
        r#"<dock-panel id="game" title="Game">"#,
        r#"<dock-panel id="game" title="Game" icon="icons/game.svg" bind-window-title="game.file">"#,
    );
    let mut t = build("retitle", &markup, RETITLE, "");
    let app = &mut t.app;
    let game_tab = tab_button(app, "game");
    let first_child = |app: &App, e: Entity| {
        app.world
            .get::<bevy_ecs::hierarchy::Children>(e)
            .and_then(|k| k.first().copied())
    };
    let icon = first_child(app, game_tab).expect("the tab has children");
    assert!(
        app.world.get::<lumen_dock::DockTabIcon>(icon).is_some(),
        "the icon comes first"
    );
    // A new title reaches the tab, its close button and the stack's name.
    let game = entity(app, "game");
    app.world
        .get_mut::<LumenAttributes>(game)
        .unwrap()
        .set("title", "Play");
    ticks(app, 3);
    let game_tab = tab_button(app, "game");
    let caption = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(game_tab)
        .and_then(|kids| {
            kids.iter()
                .copied()
                .find(|&k| app.world.get::<lumen_dock::DockTabCaption>(k).is_some())
        })
        .expect("the tab has a caption");
    assert_eq!(
        app.world
            .get::<lumen_core::components::TextContent>(caption)
            .map(|t| t.0.as_str()),
        Some("Play")
    );
    assert_eq!(
        app.world
            .get::<lumen_core::components::A11yLabel>(game_tab)
            .map(|l| l.0.as_str()),
        Some("Play"),
        "the tab's accessible name follows"
    );
    let mut q = app.world.query::<(
        &lumen_dock::DockCloseButton,
        &lumen_core::components::A11yLabel,
    )>();
    let label = q
        .iter(&app.world)
        .find(|(b, _)| b.panel == "game")
        .map(|(_, l)| l.0.clone());
    assert_eq!(label.as_deref(), Some("Close Play"));
    // A new icon replaces the old one; no icon takes it away.
    app.world
        .get_mut::<LumenAttributes>(game)
        .unwrap()
        .set("icon", "icons/play.svg");
    ticks(app, 3);
    let icons = {
        let mut q = app.world.query::<(Entity, &lumen_dock::DockTabIcon)>();
        q.iter(&app.world).count()
    };
    assert_eq!(icons, 1);
    app.world
        .get_mut::<LumenAttributes>(game)
        .unwrap()
        .remove("icon");
    ticks(app, 3);
    let icons = {
        let mut q = app.world.query::<(Entity, &lumen_dock::DockTabIcon)>();
        q.iter(&app.world).count()
    };
    assert_eq!(icons, 0);

    // Popped out, its window takes the bound signal's value as its title.
    call(app, |d| d.detach("game").unwrap());
    ticks(app, 2);
    let (w, _) = window_entity(app)[0].clone();
    assert_eq!(app.world.get::<Window>(w).unwrap().title, "level1.map");
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str("game.file", "level2.map");
    ticks(app, 3);
    assert_eq!(app.world.get::<Window>(w).unwrap().title, "level2.map");
    // Unbound, the window goes back to the panel's title.
    app.world
        .get_mut::<LumenAttributes>(game)
        .unwrap()
        .remove("bind-window-title");
    ticks(app, 3);
    assert_eq!(app.world.get::<Window>(w).unwrap().title, "Play");
}

/// The live-check demo's markup: an icon on the first panel, a center
/// stack of two, an inspector on the right.
const DEMO: &str = r#"<root>
  <dock id="editor" label="Editor">
    <dock-split axis="row">
      <dock-stack basis="240">
        <dock-panel id="files" title="Files"><column padding="12" gap="8"><label text="Files panel"/><input id="name" text="kept text"/></column></dock-panel>
      </dock-stack>
      <dock-stack center="true">
        <dock-panel id="scene" title="Scene"><column padding="12" grow="1"><label text="Scene view"/></column></dock-panel>
        <dock-panel id="game" title="Game"><column padding="12" grow="1"><label text="Game view"/></column></dock-panel>
      </dock-stack>
      <dock-stack basis="260">
        <dock-panel id="inspector" title="Inspector"><column padding="12" grow="1"><label text="Inspector panel"/></column></dock-panel>
      </dock-stack>
    </dock-split>
  </dock>
  <script src="main.cdl"/>
</root>"#;

/// The laid-out rectangle of `e`: origin and size.
fn rect(app: &App, e: Entity) -> (glam::Vec2, glam::Vec2) {
    let t = app
        .world
        .get::<lumen_core::components::Transform>(e)
        .expect("laid out");
    (t.absolute, t.size)
}

/// The strip buttons of the stack `panel` shows in, in strip order.
fn strip_buttons(app: &mut App, panel: &str) -> Vec<Entity> {
    let p = entity(app, panel);
    let (stack, _) = stack_of(app, p);
    let strip = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(stack)
        .and_then(|k| {
            k.iter()
                .copied()
                .find(|&c| app.world.get::<lumen_primitives::TabStrip>(c).is_some())
        })
        .expect("the stack has a strip");
    app.world
        .get::<bevy_ecs::hierarchy::Children>(strip)
        .map(|k| {
            k.iter()
                .copied()
                .filter(|&c| {
                    app.world
                        .get::<lumen_primitives::TabStripButton>(c)
                        .is_some()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every panel of the demo shows in its stack, inside the window, with a
/// tab sized to its title.
fn assert_demo_laid_out(app: &mut App, width: f32) {
    call(app, |d| {
        for p in ["files", "scene", "game", "inspector"] {
            assert!(d.is_shown(p), "`{p}` shows at {width} wide");
        }
    });
    let files = entity(app, "files");
    let scene = entity(app, "scene");
    let game = entity(app, "game");
    let inspector = entity(app, "inspector");
    let (files_stack, _) = stack_of(app, files);
    let (center, shown) = stack_of(app, scene);
    assert_eq!(shown.as_deref(), Some("scene"), "the first panel shows");
    assert_eq!(stack_of(app, game).0, center);
    let (right, _) = stack_of(app, inspector);
    let (fo, fs) = rect(app, files_stack);
    let (co, cs) = rect(app, center);
    let (ro, rs) = rect(app, right);
    assert!(
        fs.x > 1.0 && cs.x > 1.0 && rs.x > 1.0,
        "every stack has room at {width}: {fs} {cs} {rs}"
    );
    assert!(fo.x < co.x && co.x < ro.x, "the stacks keep their order");
    assert!(
        ro.x + rs.x <= width + 0.5,
        "the right stack ends inside the window at {width}: {ro} {rs}"
    );
    for panel in ["files", "scene", "inspector"] {
        let buttons = strip_buttons(app, panel);
        for b in &buttons {
            let (_, size) = rect(app, *b);
            assert!(
                size.x > 20.0 && size.x < 140.0,
                "`{panel}`'s tab is sized to its title, not to its strip: {size}"
            );
        }
    }
    assert_eq!(
        strip_buttons(app, "scene").len(),
        2,
        "both center tabs show"
    );
}

/// The demo laid out at a small window, after the window manager resizes it
/// right after it opens, and after a resize through a tiny size.
#[test]
fn a_small_or_resized_window_keeps_every_panel_in_place() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("lumenc_dock_small_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut t = build_with(
        &dir,
        "small",
        DEMO,
        "import \"lumen.cdl\";\nfn main() {}\n",
        "",
        "\n[window]\nsize = [1000, 640]\n",
    );
    let app = &mut t.app;
    ticks(app, 2);
    assert_demo_laid_out(app, 1000.0);
    for (w, h) in [(670.0, 1040.0), (1.0, 1.0), (861.0, 1356.0), (520.0, 400.0)] {
        let primary = primary(app);
        app.world.get_mut::<Window>(primary).unwrap().size = glam::Vec2::new(w, h);
        ticks(app, 3);
        if w > 100.0 {
            assert_demo_laid_out(app, w);
        }
    }
}

fn has_class(app: &App, e: Entity, class: &str) -> bool {
    app.world
        .get::<lumen_core::components::LumenClasses>(e)
        .is_some_and(|c| c.has(class))
}

/// With no skin, the dock still draws its stacks and marks its shown tabs
/// in its own colors, and the shown tab's button carries `active`.
#[test]
fn the_default_look_needs_no_skin() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("look", DEMO, "import \"lumen.cdl\";\nfn main() {}\n", "");
    let app = &mut t.app;
    let scene = entity(app, "scene");
    let (center, _) = stack_of(app, scene);
    let fill = app
        .world
        .get::<lumen_core::components::Visuals>(center)
        .and_then(|v| v.fill.clone());
    assert!(fill.is_some(), "a stack has a background without a skin");
    let buttons = strip_buttons(app, "scene");
    let style = |app: &App, b: Entity| {
        *app.world
            .get::<lumen_primitives::TabButtonStyle>(b)
            .expect("a strip button")
    };
    assert_ne!(
        style(app, buttons[0]).selected_bg,
        lumen_primitives::TAB_SELECTED_BG,
        "the shown tab takes the dock's color, not the view switcher's"
    );
    assert!(has_class(app, buttons[0], "active"));
    assert!(!has_class(app, buttons[1], "active"));
    call(app, |d| d.activate("game").unwrap());
    ticks(app, 3);
    let buttons = strip_buttons(app, "scene");
    assert!(!has_class(app, buttons[0], "active"));
    assert!(
        has_class(app, buttons[1], "active"),
        "`active` follows the shown tab"
    );
    // A tab the user picks moves it too.
    click(app, buttons[0], lumen_core::input::PointerButton::Primary);
    ticks(app, 3);
    let buttons = strip_buttons(app, "scene");
    assert!(has_class(app, buttons[0], "active"));
    assert!(!has_class(app, buttons[1], "active"));
}

/// What the pointer does in [`pointer`].
#[derive(Clone, Copy, PartialEq)]
enum Ptr {
    Down,
    Move,
    Up,
}

/// Drive the mouse as a window backend reports it: `at` in the logical
/// pixels of `window` (the primary window when `None`), then one tick.
fn pointer(app: &mut App, window: Option<Entity>, at: glam::Vec2, what: Ptr) {
    use lumen_core::input::{
        PointerButton, PointerData, PointerMoved, PointerPressed, PointerReleased, PointerState,
    };
    {
        let mut state = app.world.resource_mut::<PointerState>();
        state.window = window;
        state.position = Some(at);
        match what {
            Ptr::Down => state.primary_down = true,
            Ptr::Up => state.primary_down = false,
            Ptr::Move => {}
        }
    }
    let held = if what == Ptr::Up { 0 } else { 1 };
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: PointerData::mouse(held).stamped(),
    });
    match what {
        Ptr::Down => {
            app.world.write_message(PointerPressed {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: PointerData::mouse(1).stamped(),
            });
        }
        Ptr::Up => {
            app.world.write_message(PointerReleased {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: PointerData::mouse(0).stamped(),
            });
        }
        Ptr::Move => {}
    }
    ticks(app, 1);
}

/// The middle of `e` on screen.
fn middle(app: &App, e: Entity) -> glam::Vec2 {
    let t = app
        .world
        .get::<lumen_core::components::Transform>(e)
        .expect("laid out");
    t.absolute + t.size / 2.0
}

/// The rectangle of `e`: its corner and size.
fn rect_of(app: &App, e: Entity) -> (glam::Vec2, glam::Vec2) {
    let t = app
        .world
        .get::<lumen_core::components::Transform>(e)
        .expect("laid out");
    (t.absolute, t.size)
}

/// Press `from` and drag through `path` in `window`, without releasing.
fn drag_through(app: &mut App, window: Option<Entity>, from: glam::Vec2, path: &[glam::Vec2]) {
    pointer(app, window, from, Ptr::Down);
    // Past the threshold, then each stop.
    pointer(app, window, from + glam::Vec2::new(8.0, 0.0), Ptr::Move);
    for &p in path {
        pointer(app, window, p, Ptr::Move);
    }
}

/// The panels of the stack holding `panel`, in tab order.
fn stack_panels(app: &mut App, panel: &str) -> Vec<String> {
    let mut out = Vec::new();
    call(app, |d| {
        let ws = d.save();
        let loc = ws.find(panel).expect("shown");
        out = ws.stack(&loc.stack).unwrap().panels.clone();
    });
    out
}

/// The drop preview, if one shows: its zone class and its rectangle.
fn preview(app: &mut App) -> Option<(String, (glam::Vec2, glam::Vec2))> {
    let mut q = app.world.query::<(
        Entity,
        &lumen_core::components::LumenClasses,
        &lumen_core::components::Transform,
    )>();
    let found: Vec<(Entity, Vec<String>, lumen_core::components::Transform)> = q
        .iter(&app.world)
        .filter(|(_, c, _)| c.has("dock-drop-preview"))
        .map(|(e, c, t)| (e, c.0.iter().map(|s| s.to_string()).collect(), t.clone()))
        .collect();
    let (e, classes, t) = found.into_iter().next()?;
    let hidden = app
        .world
        .get::<lumen_core::components::Style>(e)
        .is_some_and(|s| s.display == lumen_core::components::Display::None);
    if hidden {
        return None;
    }
    let zone = classes
        .into_iter()
        .find(|c| c.starts_with("dock-drop-") && c != "dock-drop-preview")?;
    Some((zone, (t.absolute, t.size)))
}

/// Dragging tabs within the window: reorder in a strip, onto another stack,
/// beside one on an edge, along the dock's own edge; the ghost and preview
/// show while it runs, Escape puts everything back, and a screen reader
/// hears where a panel went.
#[test]
fn tabs_drag_within_the_window() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("drag_in", MARKUP, CANDELA, "");
    let app = &mut t.app;
    hear(app);
    ticks(app, 2);
    assert_eq!(stack_panels(app, "scene"), vec!["scene", "game"]);
    // Game before scene, in the strip.
    let game = tab_button(app, "game");
    let scene = tab_button(app, "scene");
    let (so, ss) = rect_of(app, scene);
    let target = glam::Vec2::new(so.x + 3.0, so.y + ss.y / 2.0);
    drag_through(app, None, middle(app, game), &[target]);
    assert!(app.world.resource::<lumen_dock::DockDrag>().active());
    let (zone, _) = preview(app).expect("the preview shows");
    assert_eq!(zone, "dock-drop-tab");
    let ghost = {
        let mut q = app
            .world
            .query::<(Entity, &lumen_core::components::LumenClasses)>();
        q.iter(&app.world)
            .find(|(_, c)| c.has("dock-drag-ghost"))
            .map(|(e, _)| e)
    };
    let ghost = ghost.expect("a ghost follows the pointer");
    let (go, _) = rect_of(app, ghost);
    let grab = middle(app, game) - rect_of(app, game).0;
    assert!(
        (go - (target - grab)).abs().max_element() < 2.0,
        "the ghost is held where the tab was grabbed: {go} vs {}",
        target - grab
    );
    let dock = entity(app, "editor");
    assert!(
        app.world
            .get::<lumen_core::components::LumenClasses>(dock)
            .unwrap()
            .has("dock-dragging")
    );
    pointer(app, None, target, Ptr::Up);
    ticks(app, 2);
    assert_eq!(stack_panels(app, "scene"), vec!["game", "scene"]);
    assert!(!app.world.resource::<lumen_dock::DockDrag>().active());
    assert!(preview(app).is_none(), "the overlays go with the drag");

    // Files onto the middle of the center stack: a tab at its end.
    let files = tab_button(app, "files");
    let scene_panel = entity(app, "scene");
    let center = parent(app, parent(app, scene_panel).unwrap()).unwrap();
    let mid = middle(app, center);
    drag_through(app, None, middle(app, files), &[mid]);
    assert_eq!(
        preview(app).map(|p| p.0).as_deref(),
        Some("dock-drop-center")
    );
    pointer(app, None, mid, Ptr::Up);
    ticks(app, 2);
    assert_eq!(stack_panels(app, "scene"), vec!["game", "scene", "files"]);
    ticks(app, 3);
    assert_eq!(signal(app, "moved").as_deref(), Some("dock"));

    // Log onto the left edge of the center stack: a stack of its own.
    let log = tab_button(app, "log");
    let scene_panel = entity(app, "scene");
    let center = parent(app, parent(app, scene_panel).unwrap()).unwrap();
    let (co, cs) = rect_of(app, center);
    let edge = glam::Vec2::new(co.x + 30.0, co.y + cs.y / 2.0);
    drag_through(app, None, middle(app, log), &[edge]);
    let (zone, (po, ps)) = preview(app).expect("the preview shows");
    assert_eq!(zone, "dock-drop-left");
    assert!(
        (ps.x - cs.x / 2.0).abs() < 2.0 && (po.x - co.x).abs() < 2.0,
        "the preview fills the half the panel takes"
    );
    pointer(app, None, edge, Ptr::Up);
    ticks(app, 2);
    assert_eq!(stack_panels(app, "log"), vec!["log"]);
    let heard = announced(app);
    assert!(
        heard.iter().any(|t| t.starts_with("Log moved")),
        "a screen reader hears where it went: {heard:?}"
    );

    // Escape mid-drag puts it all back, and the drag ends.
    let before = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    let game = tab_button(app, "game");
    let dock_rect = rect_of(app, dock);
    let root = glam::Vec2::new(dock_rect.0.x + 4.0, dock_rect.0.y + dock_rect.1.y / 2.0);
    drag_through(app, None, middle(app, game), &[root]);
    assert_eq!(
        preview(app).map(|p| p.0).as_deref(),
        Some("dock-drop-root-left")
    );
    press(app, "Escape", false, false);
    assert!(!app.world.resource::<lumen_dock::DockDrag>().active());
    assert!(announced(app).contains(&"Move cancelled".to_string()));
    pointer(app, None, root, Ptr::Up);
    ticks(app, 2);
    let after = {
        let mut out = None;
        call(app, |d| out = Some(d.save()));
        out.unwrap()
    };
    // Pressing a tab shows it, as in a browser; nothing else changed.
    let shape = |ws: &lumen_dock::Layout| {
        ws.docks["editor"]
            .root
            .stacks()
            .iter()
            .map(|s| (s.id.clone(), s.panels.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&after), shape(&before), "nothing moved");

    // Along the dock's own edge, for real this time.
    let game = tab_button(app, "game");
    drag_through(app, None, middle(app, game), &[root]);
    pointer(app, None, root, Ptr::Up);
    ticks(app, 2);
    call(app, |d| {
        let ws = d.save();
        let root = &ws.docks["editor"].root;
        let lumen_dock::model::Node::Split(split) = root else {
            panic!("a split at the root")
        };
        assert_eq!(split.axis, lumen_dock::model::Axis::Row);
        assert_eq!(
            split.children[0].node.panels(),
            vec!["game"],
            "game docks along the left edge of the whole dock"
        );
    });
    // A press that never passes the threshold is a click, not a drag.
    let scene = tab_button(app, "scene");
    let at = middle(app, scene);
    pointer(app, None, at, Ptr::Down);
    pointer(app, None, at + glam::Vec2::new(1.0, 0.0), Ptr::Move);
    assert!(!app.world.resource::<lumen_dock::DockDrag>().active());
    pointer(app, None, at, Ptr::Up);
}

/// The window `panel` shows in, as an entity.
fn window_holding(app: &mut App, panel: &str) -> Entity {
    let e = entity(app, panel);
    app.world
        .resource::<WindowPartitions>()
        .window_of(e)
        .expect("a window")
}

/// Tearing a tab off: past the window's edge it pops out into a window that
/// follows the pointer and stays where it is released; dragging that
/// window by its tab over the main window shows the preview there, hides
/// the window being dragged, and docks it on release. A tab dragged out of
/// a window of two tears off on its own.
#[test]
fn a_tab_tears_off_and_drags_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("tearoff", MARKUP, CANDELA, "");
    let app = &mut t.app;
    ticks(app, 2);
    let main = primary(app);
    let size = app
        .world
        .get::<lumen_core::window::WindowViewport>(main)
        .unwrap()
        .size;
    let files = tab_button(app, "files");
    let start = middle(app, files);
    let grab = start - rect_of(app, files).0;
    // Out past the right edge of the window: it tears off.
    let out = glam::Vec2::new(size.x + 200.0, 300.0);
    drag_through(
        app,
        None,
        start,
        &[glam::Vec2::new(size.x + 5.0, 300.0), out],
    );
    ticks(app, 1);
    let popped = window_holding(app, "files");
    assert_ne!(popped, main, "the tab tore off into a window of its own");
    // It follows the pointer, held where the tab was grabbed.
    let out2 = out + glam::Vec2::new(40.0, 30.0);
    pointer(app, None, out2, Ptr::Move);
    let at = app
        .world
        .get::<lumen_core::window::WindowRuntime>(popped)
        .unwrap()
        .position
        .expect("the desktop positions windows");
    let want = (out2 - grab).round().as_ivec2();
    assert!(
        (at - want).abs().max_element() <= 1,
        "it trails the pointer: {at} vs {want}"
    );
    pointer(app, None, out2, Ptr::Up);
    ticks(app, 2);
    assert_eq!(window_holding(app, "files"), popped, "released, it stays");
    assert!(app.world.get::<Window>(popped).unwrap().visible);

    // Drag it back by its tab: over the main window's center stack the
    // preview shows there and the dragged window hides.
    let tab = tab_button(app, "files");
    let from = middle(app, tab);
    let scene_panel = entity(app, "scene");
    let center = parent(app, parent(app, scene_panel).unwrap()).unwrap();
    let target_main = middle(app, center);
    drag_through(
        app,
        Some(popped),
        from,
        &[from + glam::Vec2::new(10.0, 0.0)],
    );
    // The window follows the pointer, so the platform reports each move in
    // the window's coordinates where it is now: the main window's point,
    // through the desktop.
    let pos = app
        .world
        .get::<lumen_core::window::WindowRuntime>(popped)
        .unwrap()
        .position
        .unwrap()
        .as_vec2();
    let target = target_main - pos;
    pointer(app, Some(popped), target, Ptr::Move);
    let (zone, _) = preview(app).expect("the main window previews the drop");
    assert_eq!(zone, "dock-drop-center");
    assert!(
        !app.world.get::<Window>(popped).unwrap().visible,
        "the window being dragged gets out of the way"
    );
    let pos = app
        .world
        .get::<lumen_core::window::WindowRuntime>(popped)
        .unwrap()
        .position
        .unwrap()
        .as_vec2();
    pointer(app, Some(popped), target_main - pos, Ptr::Up);
    ticks(app, 3);
    assert_eq!(window_holding(app, "files"), main, "it docked");
    assert_eq!(stack_panels(app, "files"), vec!["scene", "game", "files"]);
    assert!(window_entity(app).is_empty(), "the emptied window went");

    // A window of two: dragging one tab out tears that one off alone.
    call(app, |d| d.detach_group("scene").unwrap());
    ticks(app, 3);
    let group = window_holding(app, "scene");
    assert_ne!(group, main);
    let game = tab_button(app, "game");
    let from = middle(app, game);
    let gsize = app
        .world
        .get::<lumen_core::window::WindowViewport>(group)
        .unwrap()
        .size;
    let off = glam::Vec2::new(gsize.x + 300.0, gsize.y + 300.0);
    drag_through(app, Some(group), from, &[off]);
    ticks(app, 1);
    let alone = window_holding(app, "game");
    assert_ne!(alone, group);
    assert_ne!(alone, main);
    assert_eq!(window_holding(app, "scene"), group, "the rest stays");
    // Escape now puts it back in the window it came from.
    press(app, "Escape", false, false);
    ticks(app, 2);
    assert_eq!(window_holding(app, "game"), group);
    pointer(app, Some(group), off, Ptr::Up);
    ticks(app, 2);
}

/// As on Wayland without the toplevel-drag protocol: no window follows the
/// pointer; a tab released over no window opens in a window of its own
/// where the desktop puts it, and one released over a window docks there.
#[test]
fn without_a_following_window_a_tab_opens_where_it_drops() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("drop_to_open", MARKUP, CANDELA, "");
    let app = &mut t.app;
    app.world
        .insert_resource(lumen_core::window_backend::HeadlessDesktop {
            positioning: false,
            toplevel_drag: false,
        });
    ticks(app, 2);
    let main = primary(app);
    let size = app
        .world
        .get::<lumen_core::window::WindowViewport>(main)
        .unwrap()
        .size;
    let files = tab_button(app, "files");
    let out = glam::Vec2::new(size.x + 200.0, 300.0);
    drag_through(app, None, middle(app, files), &[out]);
    ticks(app, 1);
    assert_eq!(
        window_holding(app, "files"),
        main,
        "nothing pops out while the drag runs"
    );
    assert!(window_entity(app).is_empty());
    pointer(app, None, out, Ptr::Up);
    ticks(app, 3);
    let popped = window_holding(app, "files");
    assert_ne!(popped, main, "released over no window, it opens in one");

    // Dragged back over the main window, it docks.
    let tab = tab_button(app, "files");
    let from = middle(app, tab);
    let log = tab_button(app, "log");
    let log_stack = parent(app, parent(app, log).unwrap()).unwrap();
    let target_main = middle(app, log_stack);
    // The stand-in cascades windows from the corner; the pop-out is the
    // newest, so a point it covers is over it. Find the main-window point
    // through the pop-out's place on the stand-in's desktop: the seam does
    // that itself, so a move reported in main-window coordinates works.
    drag_through(app, Some(popped), from, &[]);
    pointer(app, None, target_main, Ptr::Move);
    pointer(app, None, target_main, Ptr::Up);
    ticks(app, 3);
    assert_eq!(window_holding(app, "files"), main);
    assert!(stack_panels(app, "log").contains(&"files".to_string()));
}

/// With no backend carrying drags across windows, a tab still drags within
/// its window, from the plain pointer messages.
#[test]
fn without_the_seam_tabs_drag_within_the_window() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = build("no_seam", MARKUP, CANDELA, "");
    let app = &mut t.app;
    app.world
        .remove_resource::<lumen_core::surface_drag::SurfaceDragSupport>();
    ticks(app, 2);
    let game = tab_button(app, "game");
    let scene = tab_button(app, "scene");
    let (so, ss) = rect_of(app, scene);
    let target = glam::Vec2::new(so.x + 3.0, so.y + ss.y / 2.0);
    drag_through(app, None, middle(app, game), &[target]);
    assert_eq!(preview(app).map(|p| p.0).as_deref(), Some("dock-drop-tab"));
    pointer(app, None, target, Ptr::Up);
    ticks(app, 2);
    assert_eq!(stack_panels(app, "scene"), vec!["game", "scene"]);
    // Off the window there is nowhere to land: nothing moves.
    let files = tab_button(app, "files");
    let out = glam::Vec2::new(-300.0, 20.0);
    drag_through(app, None, middle(app, files), &[out]);
    pointer(app, None, out, Ptr::Up);
    ticks(app, 2);
    assert_eq!(window_holding(app, "files"), primary(app));
    assert!(window_entity(app).is_empty());
}
