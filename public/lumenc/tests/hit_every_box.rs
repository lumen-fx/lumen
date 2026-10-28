// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! Every element with a box is a hit target, as in a browser: a plain `<for>`
//! row with no background, cursor or tab stop takes the hover and the click
//! at its own rect, and a press on the label inside a button holds the
//! button `:active` and reaches the button's click handler.
//!
//! Headless: no window, no GPU.

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Opacity, TextContent, Transform};
use lumen_core::input::{
    ClickEvent, HoverWithin, Hovered, PointerButton, PointerMoved, PointerPressed, PointerReleased,
    PointerState, PressWithin, Pressed,
};
use lumen_core::property_store::PropertyStore;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// Build an app from `src/main.lmn` (`markup`) and a stylesheet.
fn app(tag: &str, markup: &str, css: &str) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_hit_every_box_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.lmn"), markup).unwrap();
    std::fs::write(dir.join("src/main.css"), css).unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    let _ = std::fs::remove_dir_all(&dir);
    for _ in 0..4 {
        app.tick();
    }
    app
}

fn by_id(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element {id}"))
}

fn by_text(app: &mut App, text: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &TextContent)>();
    q.iter(&app.world)
        .find(|(_, t)| t.0 == text)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element showing `{text}`"))
}

fn rect(app: &App, e: Entity) -> Transform {
    *app.world.get::<Transform>(e).expect("laid out")
}

fn parent(app: &App, e: Entity) -> Entity {
    app.world
        .get::<bevy_ecs::hierarchy::ChildOf>(e)
        .expect("a parent")
        .parent()
}

fn pointer_to(app: &mut App, p: Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
}

fn press(app: &mut App, p: Vec2) {
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
}

/// Release at `p` and return the entities this tick's clicks named.
fn release(app: &mut App, p: Vec2) -> Vec<Entity> {
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    let clicked = app
        .world
        .resource::<Messages<ClickEvent>>()
        .iter_current_update_messages()
        .map(|c| c.entity)
        .collect();
    app.tick();
    clicked
}

fn opacity(app: &App, e: Entity) -> Option<f32> {
    app.world.get::<Opacity>(e).map(|o| o.0)
}

/// Keystone's Add Component list: rows of a `<for>` that paint nothing of
/// their own. The pointer beside a row's label hovers the row, its `:hover`
/// style applies, and a click there names the row rather than the list.
#[test]
fn a_plain_for_row_takes_the_hover_and_the_click() {
    let markup = r##"<root>
  <scroll id="list" width="300" height="200" bg="#202020">
    <for each="items" key="key">
      <row class="item" width="100%" height="30"><label text="{row.name}" /></row>
    </for>
  </scroll>
</root>"##;
    let mut app = app("for_row", markup, ".item:hover { opacity: 0.5; }");
    let items: Vec<ArrayItem> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|name| {
            let mut item = ArrayItem::new();
            item.insert("key".to_string(), name.to_string());
            item.insert("name".to_string(), name.to_string());
            item
        })
        .collect();
    app.world.resource_mut::<ArraySignals>().set("items", items);
    for _ in 0..6 {
        app.tick();
    }
    let label = by_text(&mut app, "beta");
    let row = parent(&app, label);
    let list = by_id(&mut app, "list");
    let r = rect(&app, row);
    let l = rect(&app, label);
    // Past the end of the label, inside the row.
    let beside = Vec2::new(
        r.absolute.x + r.size.x - 10.0,
        r.absolute.y + r.size.y / 2.0,
    );
    assert!(
        beside.x > l.absolute.x + l.size.x,
        "the point must miss the label"
    );

    pointer_to(&mut app, beside);
    assert!(
        app.world.get::<Hovered>(row).is_some(),
        "the unpainted row is the hit target"
    );
    assert!(app.world.get::<Hovered>(list).is_none());
    assert_eq!(opacity(&app, row), Some(0.5), "the row's :hover applies");

    press(&mut app, beside);
    assert_eq!(
        release(&mut app, beside),
        vec![row],
        "the click names the row"
    );

    // On the label, the label is the target and the row is still hovered.
    let on_label = l.absolute + l.size / 2.0;
    pointer_to(&mut app, on_label);
    assert!(app.world.get::<Hovered>(label).is_some());
    assert!(app.world.get::<HoverWithin>(row).is_some());
    assert_eq!(opacity(&app, row), Some(0.5));
}

const BUTTON: &str = r##"<root padding="20">
  <button id="go" class="go" width="160" height="60">
    <label text="Go" />
  </button>
  <tile id="away" width="80" height="80" bg="#999999" />
  <script><![CDATA[
    import "lumen.cdl";
    fn on_click(id: string) { signal<string>("clicked").set(id); }
    fn main() {}
  ]]></script>
</root>"##;

/// A press on the label inside a button holds the button `:active`, the way
/// `:active` covers the ancestors of the pressed element in a browser. The
/// release on the label clicks it, and the click reaches the button's
/// handler. Sliding off and back un-presses and re-presses the button.
#[test]
fn a_press_on_a_buttons_label_holds_the_button_active() {
    let mut app = app("button_label", BUTTON, ".go:active { opacity: 0.5; }");
    let button = by_id(&mut app, "go");
    let label = by_text(&mut app, "Go");
    let l = rect(&app, label);
    let on_label = l.absolute + l.size / 2.0;

    pointer_to(&mut app, on_label);
    assert!(
        app.world.get::<Hovered>(label).is_some(),
        "the label is the hit target; the test would pass vacuously otherwise"
    );
    press(&mut app, on_label);
    app.tick();
    assert!(
        app.world.get::<Pressed>(label).is_some(),
        "the press target"
    );
    assert!(
        app.world.get::<PressWithin>(button).is_some(),
        "the button is in the press chain"
    );
    assert_eq!(opacity(&app, button), Some(0.5), "the button is :active");

    // Dragged off: the press is kept, the button shows un-pressed.
    let away_e = by_id(&mut app, "away");
    let a = rect(&app, away_e);
    pointer_to(&mut app, a.absolute + a.size / 2.0);
    assert_eq!(opacity(&app, button), None, "dragged off un-presses");
    // Back onto the button's padding, beside the label: pressed again.
    let b = rect(&app, button);
    let padding = Vec2::new(b.absolute.x + b.size.x - 4.0, b.absolute.y + 4.0);
    pointer_to(&mut app, padding);
    assert!(app.world.get::<Hovered>(button).is_some());
    assert_eq!(opacity(&app, button), Some(0.5), "back over the button");

    pointer_to(&mut app, on_label);
    assert_eq!(release(&mut app, on_label), vec![label], "event.target");
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("clicked")
            .as_deref(),
        Some("go"),
        "the button's on_click handler hears the click"
    );
    assert_eq!(opacity(&app, button), None, "released");
    assert!(app.world.get::<PressWithin>(button).is_none());
}

/// A press on the label released over the button's padding clicks the
/// button, the nearest element that holds both ends of the gesture.
#[test]
fn a_press_on_the_label_released_on_the_padding_clicks_the_button() {
    let mut app = app("button_padding", BUTTON, "");
    let button = by_id(&mut app, "go");
    let label = by_text(&mut app, "Go");
    let l = rect(&app, label);
    let on_label = l.absolute + l.size / 2.0;
    let b = rect(&app, button);
    let padding = Vec2::new(b.absolute.x + b.size.x - 4.0, b.absolute.y + 4.0);

    pointer_to(&mut app, on_label);
    press(&mut app, on_label);
    pointer_to(&mut app, padding);
    assert_eq!(release(&mut app, padding), vec![button]);

    // Released over an unrelated element: no click.
    let away_e = by_id(&mut app, "away");
    let a = rect(&app, away_e);
    let away = a.absolute + a.size / 2.0;
    pointer_to(&mut app, on_label);
    press(&mut app, on_label);
    pointer_to(&mut app, away);
    assert!(release(&mut app, away).is_empty());
}
