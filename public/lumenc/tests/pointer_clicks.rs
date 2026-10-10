// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! What a real pointer click does to a script and to focus, driven through the
//! raw pointer messages a window backend writes.

use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent, Transform};
use lumen_core::input::{
    PointerButton, PointerMoved, PointerPressed, PointerReleased, PointerState,
};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!(
        "lumenc_pointer_clicks_{}_{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..ticks {
        app.tick();
    }
    app
}

/// The centre of the element with `id`.
fn centre_of(app: &mut App, id: &str) -> glam::Vec2 {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    let t = q
        .iter(&app.world)
        .find(|(lid, _)| lid.0 == id)
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("no element {id}"));
    t.absolute + t.size / 2.0
}

/// Press and release the primary button at `p`, a tick for each.
fn click(app: &mut App, p: glam::Vec2) {
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
    app.world.write_message(PointerReleased {
        position: p,
        button: PointerButton::Primary,
        local: None,
    });
    app.tick();
}

fn text_of(app: &mut App, id: &str) -> Option<String> {
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(lid, _)| lid.0 == id)
        .map(|(_, t)| t.0.clone())
}

const COUNTER: &str = r#"
<root padding="20">
  <button id="bump" text="+1" width="200px" height="40px" />
  <label id="clicks" bind-text="clicks" />
  <label id="doubles" bind-text="doubles" />
  <script>
    import "lumen.cdl";
    fn on_start() {
        lumen::signal_set_int("clicks", 0);
        lumen::signal_set_int("doubles", 0);
    }
    fn on_click(id: string) {
        lumen::signal_set_int("clicks", lumen::signal_get_int("clicks") + 1);
    }
    fn on_double_click(id: string) {
        lumen::signal_set_int("doubles", lumen::signal_get_int("doubles") + 1);
    }
    fn main() {}
  </script>
</root>
"#;

/// A quick pair of clicks is two clicks and one double-click, the way a
/// browser fires `click` twice and `dblclick` once.
#[test]
fn a_quick_pair_of_clicks_reaches_on_click_twice_and_on_double_click_once() {
    let mut app = build_and_tick(COUNTER, 6);
    let p = centre_of(&mut app, "bump");
    click(&mut app, p);
    click(&mut app, p);
    app.tick();
    assert_eq!(text_of(&mut app, "clicks").as_deref(), Some("2"));
    assert_eq!(text_of(&mut app, "doubles").as_deref(), Some("1"));
}

fn entity_of(app: &mut App, id: &str) -> bevy_ecs::entity::Entity {
    let mut q = app.world.query::<(bevy_ecs::entity::Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element {id}"))
}

fn focused(app: &App, id_entity: bevy_ecs::entity::Entity) -> bool {
    app.world
        .get::<lumen_core::input::Focused>(id_entity)
        .is_some()
}

const FORM: &str = r#"
<root padding="20" gap="10">
  <input id="name" width="200px" />
  <button id="save" text="Save" width="200px" height="40px" />
</root>
"#;

/// Clicking a button focuses it, as it does in a browser, and the field that
/// held focus gives it up.
#[test]
fn clicking_a_button_focuses_it() {
    let mut app = build_and_tick(FORM, 6);
    let (name, save) = (entity_of(&mut app, "name"), entity_of(&mut app, "save"));
    let p = centre_of(&mut app, "name");
    click(&mut app, p);
    assert!(focused(&app, name), "clicking the field focuses it");
    let p = centre_of(&mut app, "save");
    click(&mut app, p);
    assert!(focused(&app, save), "clicking the button focuses it");
    assert!(!focused(&app, name), "the field gave focus up");
    assert!(
        app.world
            .get::<lumen_core::input::FocusVisible>(save)
            .is_none(),
        "pointer focus carries no keyboard focus ring"
    );
}
