// Drives the real pipeline (parse -> spawn -> layout -> input), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! Popovers end to end: a `popover` opened from its `popovertarget` button
//! shows under the button over the whole window, whatever scrolls or clips
//! the button, takes clicks everywhere it is drawn, flips and shrinks near
//! the window's edges, closes on Escape and on a press outside it, and
//! tells a script it opened and closed. A `<menu>` panel is one too, and a
//! `<dialog>`'s body is there before the dialog first opens.

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Style, Transform};
use lumen_core::input::{
    Hovered, Key, KeyCode, KeyPressed, MenuClicked, Modifiers, NamedKey, PointerButton,
    PointerMoved, PointerPressed, PointerReleased, PointerState, ScrollOffset,
};
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, script: Option<&str>) -> App {
    build_styled(markup, script, "")
}

fn build_styled(markup: &str, script: Option<&str>, css: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_popover_{}_{}",
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
    let mut opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_css(css.to_string());
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

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, l)| l.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn rect(app: &mut App, id: &str) -> Transform {
    let e = entity(app, id);
    *app.world.get::<Transform>(e).expect("laid out")
}

fn centre(t: Transform) -> Vec2 {
    t.absolute + t.size / 2.0
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

fn is_open(app: &App, name: &str) -> bool {
    app.world.resource::<PropertyStore>().get_global_bool(name) == Some(true)
}

/// Press and release the primary button at `p`, then let the tick settle.
fn click(app: &mut App, p: Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    ticks(app, 3);
}

fn press_escape(app: &mut App) {
    app.world.write_message(KeyPressed {
        key: Key::Named(NamedKey::Escape),
        code: KeyCode::Escape,
        modifiers: Modifiers::default(),
        repeat: false,
        time_stamp: lumen_core::time::Instant::now(),
    });
    ticks(app, 2);
}

/// A side panel that scrolls and clips, with the picker's button inside it.
const PICKER: &str = r##"<root>
  <scroll id="side" width="200px" height="120px" overflow="hidden">
    <column height="600px">
      <tile height="60px" bg="#222222"/>
      <button id="open" text="Add" width="120px" height="30px" popovertarget="picker"/>
    </column>
  </scroll>
  <column id="picker" popover="auto" width="240px" bg="#333333">
    <button id="first" text="First" height="40px"/>
    <button id="second" text="Second" height="40px"/>
    <button id="third" text="Third" height="40px"/>
  </column>
</root>"##;

#[test]
fn a_popovertarget_button_opens_its_popover_under_it_over_the_whole_window() {
    let mut app = build(PICKER, None);
    assert!(!is_open(&app, "__popover_open:picker"));
    let button = rect(&mut app, "open");
    click(&mut app, centre(button));
    assert!(is_open(&app, "__popover_open:picker"));
    let picker = rect(&mut app, "picker");
    // Under the button, start edges together, the default 4 px gap.
    assert_eq!(picker.absolute.x, button.absolute.x);
    assert_eq!(picker.absolute.y, button.absolute.y + button.size.y + 4.0);
    // Laid out against the window, not squeezed into the 200 px panel.
    assert_eq!(picker.size.x, 240.0);
    // The third row hangs below the 120 px side panel, and the pointer
    // still finds it there.
    let third = rect(&mut app, "third");
    assert!(
        third.absolute.y > 120.0,
        "third row at {:?}",
        third.absolute
    );
    let p = centre(third);
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    ticks(&mut app, 2);
    let third_e = entity(&mut app, "third");
    assert!(app.world.get::<Hovered>(third_e).is_some(), "hit");
    click(&mut app, p);
    assert!(is_open(&app, "__popover_open:picker"), "a press inside");
}

#[test]
fn the_popover_follows_its_button_when_the_side_panel_scrolls() {
    let mut app = build(PICKER, None);
    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    let before = rect(&mut app, "picker").absolute;
    let side = entity(&mut app, "side");
    app.world.get_mut::<ScrollOffset>(side).unwrap().0 = Vec2::new(0.0, 20.0);
    ticks(&mut app, 3);
    let after = rect(&mut app, "picker").absolute;
    assert_eq!(after.y, before.y - 20.0, "{before:?} -> {after:?}");
}

#[test]
fn a_press_outside_and_escape_close_an_auto_popover() {
    let mut app = build(PICKER, None);
    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    assert!(is_open(&app, "__popover_open:picker"));
    click(&mut app, Vec2::new(590.0, 390.0));
    assert!(!is_open(&app, "__popover_open:picker"), "outside press");

    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    assert!(is_open(&app, "__popover_open:picker"));
    press_escape(&mut app);
    assert!(!is_open(&app, "__popover_open:picker"), "Escape");

    // The button toggles it closed again.
    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    assert!(!is_open(&app, "__popover_open:picker"), "second click");
}

#[test]
fn near_the_bottom_a_popover_flips_above_and_shrinks_to_the_room() {
    let mut app = build(
        r##"<root>
  <column height="300px"/>
  <button id="open" text="Pick" width="100px" height="30px" popovertarget="list"/>
  <column id="list" popover="auto" width="160px" overflow="scroll">
    <tile height="500px" bg="#336699"/>
  </column>
</root>"##,
        None,
    );
    let button = rect(&mut app, "open");
    click(&mut app, centre(button));
    ticks(&mut app, 3);
    let list = rect(&mut app, "list");
    // 400 - 330 - 4 = 66 px below, 296 above: it goes above, 296 tall.
    assert_eq!(list.size.y, button.absolute.y - 4.0, "{list:?}");
    assert_eq!(list.absolute.y, 0.0);
    let list_e = entity(&mut app, "list");
    let style = app.world.get::<Style>(list_e).unwrap();
    assert_eq!(
        style.max_height,
        lumen_core::components::Length::Px(button.absolute.y - 4.0)
    );
}

#[test]
fn position_area_puts_a_popover_beside_its_named_anchor() {
    let mut app = build(
        r##"<root padding="40">
  <button id="target" text="Here" width="100px" height="30px"/>
  <column id="tip" popover="auto" anchor="target" class="tip" width="80px" height="20px"/>
</root>"##,
        None,
    );
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool("__popover_open:tip", true);
    ticks(&mut app, 4);
    let target = rect(&mut app, "target");
    let tip = rect(&mut app, "tip");
    assert_eq!(tip.absolute.y, target.absolute.y + target.size.y + 4.0);

    let mut app = build_styled(
        r##"<root padding="40">
  <button id="target" text="Here" width="100px" height="30px"/>
  <column id="tip" popover="auto" anchor="target" class="tip" width="80px" height="20px"/>
</root>"##,
        None,
        ".tip { position-area: right span-bottom; popup-gap: 8; }",
    );
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool("__popover_open:tip", true);
    ticks(&mut app, 4);
    let target = rect(&mut app, "target");
    let tip = rect(&mut app, "tip");
    assert_eq!(
        tip.absolute,
        Vec2::new(target.absolute.x + target.size.x + 8.0, target.absolute.y)
    );
}

const SCRIPTED: &str = r##"<root>
  <button id="go" text="Show" width="100px" height="30px"/>
  <column id="panel" popover="manual" open="panel_open" width="100px" height="40px"/>
  <script src="main.cdl"/>
</root>"##;

const SCRIPT: &str = r#"import "lumen.cdl";
fn on_toggle(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    let seen = lumen::signal_get("seen") as string;
    lumen::signal_set("seen", seen + lumen::event_type(ev) + ":" + (detail["new_state"] as string) + " ");
}
fn on_go(ev: int) { lumen::toggle_popover("panel"); }
fn on_ready() {
    lumen::signal_set("seen", "");
    let panel = lumen::node_get_by_id("panel");
    lumen::event_on(panel, "beforetoggle", "on_toggle");
    lumen::event_on(panel, "toggle", "on_toggle");
    lumen::event_on(lumen::node_get_by_id("go"), "click", "on_go");
}
fn main() {}
"#;

#[test]
fn a_script_toggles_a_popover_and_hears_it_open_and_close() {
    let mut app = build(SCRIPTED, Some(SCRIPT));
    let p = centre(rect(&mut app, "go"));
    click(&mut app, p);
    assert!(is_open(&app, "panel_open"), "toggle_popover opened it");
    let p = centre(rect(&mut app, "go"));
    click(&mut app, p);
    assert!(!is_open(&app, "panel_open"));
    assert_eq!(
        signal(&app, "seen").as_deref(),
        Some("beforetoggle:open toggle:open beforetoggle:closed toggle:closed ")
    );
    // A manual popover ignores a press outside it and Escape.
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool("panel_open", true);
    ticks(&mut app, 2);
    click(&mut app, Vec2::new(590.0, 390.0));
    press_escape(&mut app);
    assert!(is_open(&app, "panel_open"));
}

#[test]
fn a_menu_inside_a_clipping_row_takes_clicks_below_the_row() {
    let mut app = build(
        r#"<root>
  <column width="200px" height="40px" overflow="hidden">
    <menu id="m">
      <menuitem id="a" label="A"/>
      <menuitem id="b" label="B"/>
      <menuitem id="c" label="C"/>
      <menuitem id="d" label="D"/>
    </menu>
  </column>
</root>"#,
        None,
    );
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool("__menu_open:m", true);
    ticks(&mut app, 4);
    let d = rect(&mut app, "d");
    assert!(d.absolute.y > 40.0, "item d at {:?}", d.absolute);
    // Read the pick on the tick after the release, before the message
    // buffers cycle it away.
    let p = centre(d);
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.world.resource_mut::<Messages<MenuClicked>>().clear();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    let picked: Vec<String> = app
        .world
        .resource_mut::<Messages<MenuClicked>>()
        .drain()
        .map(|m| m.id)
        .collect();
    assert_eq!(picked, vec!["d".to_string()]);
}

#[test]
fn a_dialog_body_is_there_before_the_dialog_first_opens() {
    let script = r#"import "lumen.cdl";
fn on_open(ev: int) {
    lumen::signal_set("dlg", "true");
    let body = lumen::node_get_by_id("body");
    lumen::node_set_style(body, "width", "321px");
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("go"), "click", "on_open");
}
fn main() {}
"#;
    let mut app = build(
        r##"<root>
  <button id="go" text="Open" width="100px" height="30px"/>
  <dialog open="dlg">
    <column id="body" height="50px" bg="#444444"/>
  </dialog>
  <script src="main.cdl"/>
</root>"##,
        Some(script),
    );
    // Mounted from the first frame, hidden.
    let _ = entity(&mut app, "body");
    let p = centre(rect(&mut app, "go"));
    click(&mut app, p);
    assert_eq!(rect(&mut app, "body").size.x, 321.0);
}

#[test]
fn an_opening_popover_focuses_its_autofocus_element() {
    let mut app = build(
        r##"<root>
  <button id="open" text="Add" width="120px" height="30px" popovertarget="picker"/>
  <column id="picker" popover="auto" width="240px">
    <input id="search" autofocus="true" bind-text="search"/>
  </column>
</root>"##,
        None,
    );
    let p = centre(rect(&mut app, "open"));
    click(&mut app, p);
    let search = entity(&mut app, "search");
    assert_eq!(
        app.world.resource::<lumen_core::input::FocusTracker>().0,
        Some(search)
    );
}
