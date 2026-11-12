// Drives the linked runtime, which needs `RunOptions` / `build_headless_app`;
// lumenc only exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! Typed in-app drags, end to end on the real pipeline: a `drag-type` names
//! the payload's kind, an `accept` list decides which targets light
//! `:drag-over` and take the drop, the DOM drag events reach candela, Rhai,
//! Lua and native handlers in the web's order with the payload readable by
//! kind, a `dragstart` handler adds to the payload, and every event's
//! `local` position is measured from the box as it is painted inside a
//! scrolled container. A file the OS drops reaches the same `drop` event.
//!
//! Headless: no window, no GPU.

use std::sync::{Arc, Mutex};

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{DropHovered, LumenId, Transform};
use lumen_core::input::{
    DragEndEvent, DragStartEvent, FileDropped, PointerButton, PointerMoved, PointerPressed,
    PointerReleased, PointerState, ScrollOffset,
};
use lumen_core::node::NodeHandle;
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r##"<root>
  <row>
    <column width="200px">
      <tile id="asset" height="40px" bg="#333333" drag-payload="asset-7" drag-type="application/x-asset"/>
      <tile id="plain" height="40px" bg="#444444" drag-payload="hello"/>
    </column>
    <scroll id="sc" width="300px" height="200px">
      <column id="board" width="300px" height="1000px">
        <tile id="pad" height="300px" bg="#111111"/>
        <tile id="images" height="50px" bg="#222222" drop-target="true" accept="image/*"/>
        <tile id="assets" height="50px" bg="#555555" drop-target="true" accept="image/*, application/x-asset"/>
        <tile id="any" height="50px" bg="#666666" drop-target="true"/>
      </column>
    </scroll>
  </row>
  <script src="main.cdl"/>
  <script src="side.rhai"/>
  <script src="side.lua"/>
</root>"##;

const CANDELA: &str = r#"import "lumen.cdl";
fn log(entry: string) {
    lumen::signal_set("log", lumen::signal_get("log") + entry + ";");
}
fn on_start_drag(ev: int) {
    log("dragstart@" + lumen::node_id(lumen::event_target(ev)));
    lumen::event_set_data(ev, "text/plain", "extra");
}
fn on_enter(ev: int) { log("dragenter"); }
fn on_over(ev: int) { log("dragover"); }
fn on_leave(ev: int) { log("dragleave@" + lumen::node_id(lumen::event_target(ev))); }
fn on_target_drop(ev: int) {
    log("drop");
    let kinds = "";
    for t in lumen::event_types(ev) { kinds = kinds + t + ","; }
    lumen::signal_set("kinds", kinds);
    lumen::signal_set("asset", lumen::event_get_data(ev, "application/x-asset"));
    lumen::signal_set("text", lumen::event_get_data(ev, "text"));
    lumen::signal_set("local", string(lumen::event_x(ev)) + "," + string(lumen::event_y(ev)));
    lumen::signal_set("late_write", string(lumen::event_set_data(ev, "text/plain", "no")));
}
fn on_board_drop(ev: int) { log("drop@board"); }
fn on_end(ev: int) { log("dragend"); }
fn on_tap(ev: int) {
    lumen::signal_set("click_local", string(lumen::event_x(ev)) + "," + string(lumen::event_y(ev)));
}
fn on_drop(target: string, text: string) {
    lumen::signal_set("global_drop", target + "=" + text);
}
fn on_drag_start(source: string, text: string) {
    lumen::signal_set("global_start", source + "=" + text);
}
fn on_file_dropped(id: string, path: string) {
    lumen::signal_set("file_dropped", id + "=" + path);
}
fn on_ready() {
    let asset = lumen::node_get_by_id("asset");
    let assets = lumen::node_get_by_id("assets");
    lumen::event_on(asset, "dragstart", "on_start_drag");
    lumen::event_on(asset, "dragend", "on_end");
    lumen::event_on(assets, "dragenter", "on_enter");
    lumen::event_on(assets, "dragover", "on_over");
    lumen::event_on(assets, "dragleave", "on_leave");
    lumen::event_on(assets, "drop", "on_target_drop");
    lumen::event_on(lumen::node_get_by_id("board"), "drop", "on_board_drop");
    lumen::event_on(lumen::node_get_by_id("any"), "click", "on_tap");
}
fn main() {}
"#;

const RHAI: &str = r#"fn on_ready() {
    get_by_id("any").on("drop", |e| {
        signal("rhai_drop", "").set(e.types().len().to_string() + ":" + e.get_data("url"));
    });
}
"#;

const LUA: &str = r#"function on_ready()
    get_by_id("any"):on("drop", function(e)
        signal("lua_drop", ""):set(#e:types() .. ":" .. e:get_data("text/uri-list"))
    end)
end
"#;

/// The event binding registry and the current event are process-wide, so
/// the tests here take turns.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_script::event::clear_all_bindings();
    guard
}

fn build() -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_drag_events_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, text) in [
        ("main.lmn", MARKUP),
        ("main.cdl", CANDELA),
        ("side.rhai", RHAI),
        ("side.lua", LUA),
    ] {
        std::fs::write(src.join(name), text).unwrap();
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
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn signal(app: &App, name: &str) -> String {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// Scroll `sc` so `pad` is mostly out of view, and answer where `id`'s box
/// is painted: its laid-out origin less the scroll.
fn scroll_and_place(app: &mut App, id: &str) -> Vec2 {
    let sc = entity(app, "sc");
    app.world.get_mut::<ScrollOffset>(sc).unwrap().0 = Vec2::new(0.0, 280.0);
    for _ in 0..3 {
        app.tick();
    }
    let e = entity(app, id);
    app.world.get::<Transform>(e).unwrap().absolute - Vec2::new(0.0, 280.0)
}

fn point_at(app: &mut App, at: Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
}

fn start_drag(app: &mut App, source: &str) {
    let e = entity(app, source);
    let at = app.world.get::<Transform>(e).unwrap().absolute + Vec2::new(5.0, 5.0);
    app.world
        .resource_mut::<Messages<DragStartEvent>>()
        .write(DragStartEvent {
            entity: e,
            start: at,
            position: at + Vec2::new(8.0, 0.0),
        });
    app.tick();
}

fn end_drag(app: &mut App, source: &str, at: Vec2) {
    let e = entity(app, source);
    app.world
        .resource_mut::<Messages<DragEndEvent>>()
        .write(DragEndEvent {
            entity: e,
            position: at,
        });
    for _ in 0..3 {
        app.tick();
    }
}

fn hovered(app: &mut App, id: &str) -> bool {
    let e = entity(app, id);
    app.world.get::<DropHovered>(e).is_some()
}

/// A click on an element inside a scrolled container reports `local` from
/// the box the user sees, not from where it sits before the scroll.
#[test]
fn a_click_inside_a_scrolled_container_is_local_to_the_painted_box() {
    let _serial = serial();
    let mut app = build();
    let origin = scroll_and_place(&mut app, "any");
    let at = origin + Vec2::new(5.0, 10.0);
    point_at(&mut app, at);
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: at,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: at,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
    assert_eq!(signal(&app, "click_local"), "5.0,10.0");
}

/// An `accept` list lights `:drag-over` only on the targets that take the
/// drag's kind: `image/*` refuses an asset, the list naming it and the
/// target with no list take it, and a plain-text drag lights only the
/// target with no list.
#[test]
fn accept_lists_decide_which_targets_light() {
    let _serial = serial();
    let mut app = build();
    let images = scroll_and_place(&mut app, "images");
    let assets = scroll_and_place(&mut app, "assets");
    let any = scroll_and_place(&mut app, "any");

    start_drag(&mut app, "asset");
    point_at(&mut app, images + Vec2::new(10.0, 10.0));
    assert!(!hovered(&mut app, "images"), "image/* refuses an asset");
    point_at(&mut app, assets + Vec2::new(10.0, 10.0));
    assert!(
        hovered(&mut app, "assets"),
        "the list naming the kind takes it"
    );
    point_at(&mut app, any + Vec2::new(10.0, 10.0));
    assert!(hovered(&mut app, "any"), "no list takes anything");
    assert!(!hovered(&mut app, "assets"));
    end_drag(&mut app, "asset", Vec2::new(590.0, 390.0));

    start_drag(&mut app, "plain");
    point_at(&mut app, assets + Vec2::new(10.0, 10.0));
    assert!(!hovered(&mut app, "assets"), "text is not on the list");
    end_drag(&mut app, "plain", assets + Vec2::new(10.0, 10.0));
    assert_eq!(
        signal(&app, "global_drop"),
        "",
        "a refused drop lands nowhere"
    );
}

/// The DOM drag events fire in the web's order on the web's targets, carry
/// the payload by kind, take a `dragstart` handler's addition, bubble, and
/// measure `local` from the painted box; a native handler hears the same.
#[test]
fn drag_events_fire_in_order_with_the_payload() {
    let _serial = serial();
    let mut app = build();
    let assets = scroll_and_place(&mut app, "assets");
    let any = scroll_and_place(&mut app, "any");
    let assets_entity = entity(&mut app, "assets");

    let native = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = native.clone();
    lumen_script::event::register_native_binding(
        NodeHandle::new(assets_entity).pack(),
        "drop".into(),
        false,
        Arc::new(move || {
            let ev = lumen_script::event::current_event();
            seen.lock().unwrap().push(format!(
                "{}|{}|{}|{:?}",
                lumen_script::event::event_types().join(","),
                lumen_script::event::event_get_data("application/x-asset"),
                lumen_script::event::event_get_data("text/plain"),
                ev.client,
            ));
        }),
    );

    start_drag(&mut app, "asset");
    point_at(&mut app, assets + Vec2::new(10.0, 10.0));
    point_at(&mut app, assets + Vec2::new(12.0, 10.0));
    point_at(&mut app, any + Vec2::new(10.0, 10.0));
    point_at(&mut app, assets + Vec2::new(14.0, 10.0));
    let drop_at = assets + Vec2::new(7.0, 13.0);
    end_drag(&mut app, "asset", drop_at);

    assert_eq!(
        signal(&app, "log"),
        "dragstart@asset;dragenter;dragover;dragover;dragleave@assets;dragenter;dragover;\
         drop;drop@board;dragend;"
    );
    assert_eq!(
        signal(&app, "kinds"),
        "application/x-asset,text/plain,",
        "the dragstart handler's kind follows the markup's"
    );
    assert_eq!(signal(&app, "asset"), "asset-7");
    assert_eq!(signal(&app, "text"), "extra");
    assert_eq!(
        signal(&app, "local"),
        "7.0,13.0",
        "local to the scrolled box"
    );
    assert_eq!(signal(&app, "late_write"), "false", "only dragstart writes");
    assert_eq!(signal(&app, "global_drop"), "assets=extra");
    assert_eq!(signal(&app, "global_start"), "asset=asset-7");
    let native = native.lock().unwrap();
    assert_eq!(
        native.as_slice(),
        [format!(
            "application/x-asset,text/plain|asset-7|extra|({:?}, {:?})",
            f64::from(drop_at.x),
            f64::from(drop_at.y)
        )]
    );
    lumen_script::event::clear_all_bindings();
}

/// A `dragstart` handler's `prevent_default` cancels the drag: nothing
/// lights and nothing drops.
#[test]
fn a_cancelled_dragstart_drops_nothing() {
    let _serial = serial();
    let mut app = build();
    let any = scroll_and_place(&mut app, "any");
    let plain = entity(&mut app, "plain");
    lumen_script::event::register_native_binding(
        NodeHandle::new(plain).pack(),
        "dragstart".into(),
        false,
        Arc::new(lumen_script::event::event_prevent_default),
    );
    start_drag(&mut app, "plain");
    point_at(&mut app, any + Vec2::new(10.0, 10.0));
    assert!(!hovered(&mut app, "any"));
    end_drag(&mut app, "plain", any + Vec2::new(10.0, 10.0));
    assert_eq!(signal(&app, "global_drop"), "");
    lumen_script::event::clear_all_bindings();
}

/// A file the OS drops reaches the DOM `drop` event as `text/uri-list` in
/// every host, and `on_drop` gets the uri; `on_file_dropped` still fires.
#[test]
fn a_file_drop_reaches_the_dom_drop_event() {
    let _serial = serial();
    let mut app = build();
    let any = scroll_and_place(&mut app, "any");
    let target = entity(&mut app, "any");
    app.world.write_message(FileDropped {
        entity: target,
        path: "/tmp/shot.png".into(),
        position: any + Vec2::new(3.0, 4.0),
    });
    for _ in 0..3 {
        app.tick();
    }
    assert_eq!(signal(&app, "rhai_drop"), "1:file:///tmp/shot.png");
    assert_eq!(signal(&app, "lua_drop"), "1:file:///tmp/shot.png");
    assert_eq!(signal(&app, "global_drop"), "any=file:///tmp/shot.png");
    assert_eq!(signal(&app, "file_dropped"), "any=/tmp/shot.png");
}
