// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! An in-app drag started on a `<for>` row while the list changes under it.
//!
//! A row whose key stays keeps its entity, so a drag on it carries on as if
//! nothing happened. A row whose key changes is built again, and the drag it
//! started still ends on the release and still drops its payload.
//!
//! Headless: no window, no GPU.

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent, Transform};
use lumen_core::input::{
    PointerButton, PointerMoved, PointerPressed, PointerReleased, PointerState,
};
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_os_dnd::{ActiveDrag, DropAccepted};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r##"<root>
  <row>
    <column width="200">
      <for each="cells" key="key">
        <label class="cell" height="40" bg="#333333" drag-payload="{row.name}" text="{row.name}" />
      </for>
    </column>
    <column id="bin" width="200" height="200" drop-target="true" accept="text/plain" />
  </row>
</root>"##;

fn cells(keys: &[(&str, &str)]) -> Vec<ArrayItem> {
    keys.iter()
        .map(|(key, name)| {
            let mut item = ArrayItem::new();
            item.insert("key".to_string(), key.to_string());
            item.insert("name".to_string(), name.to_string());
            item
        })
        .collect()
}

fn app() -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_for_row_drag_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(MARKUP.to_string());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    let _ = std::fs::remove_dir_all(&dir);
    app.world
        .resource_mut::<ArraySignals>()
        .set("cells", cells(&[("a", "a"), ("b", "b"), ("c", "c")]));
    for _ in 0..8 {
        app.tick();
    }
    app
}

fn cell(app: &mut App, name: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &TextContent)>();
    q.iter(&app.world)
        .find(|(_, t)| t.0 == name)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no cell `{name}`"))
}

fn center(app: &mut App, e: Entity) -> Vec2 {
    let t = *app.world.get::<Transform>(e).expect("laid out");
    t.absolute + t.size * 0.5
}

fn bin(app: &mut App) -> Vec2 {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    let e = q
        .iter(&app.world)
        .find(|(_, id)| id.0 == "bin")
        .map(|(e, _)| e)
        .expect("the bin");
    center(app, e)
}

fn pointer_to(app: &mut App, p: Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
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

/// Release at `p` and return the payloads the drop delivered.
fn release(app: &mut App, p: Vec2) -> Vec<(Option<Entity>, String)> {
    let mut drops = app
        .world
        .resource::<Messages<DropAccepted>>()
        .get_cursor_current();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: p,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
    drops
        .read(app.world.resource::<Messages<DropAccepted>>())
        .map(|d| (d.source, d.payload.text().unwrap_or_default()))
        .collect()
}

/// Press on the cell `name` and pull it past the drag threshold.
fn start_drag(app: &mut App, name: &str) -> Entity {
    let source = cell(app, name);
    let grab = center(app, source);
    pointer_to(app, grab);
    press(app, grab);
    pointer_to(app, grab + Vec2::new(30.0, 0.0));
    // The session opens on the tick after the one the drag started on.
    app.tick();
    assert!(
        app.world.resource::<ActiveDrag>().0.is_some(),
        "precondition: the drag session is open"
    );
    source
}

#[test]
fn a_drag_on_a_row_whose_key_stays_survives_another_row_changing() {
    let mut app = app();
    let source = start_drag(&mut app, "a");

    app.world
        .resource_mut::<ArraySignals>()
        .set("cells", cells(&[("a", "a"), ("b", "b"), ("c2", "c")]));
    app.tick();
    assert_eq!(
        cell(&mut app, "a"),
        source,
        "the dragged row keeps its entity"
    );

    let target = bin(&mut app);
    pointer_to(&mut app, target);
    assert_eq!(release(&mut app, target), [(Some(source), "a".to_string())]);
    assert!(app.world.resource::<ActiveDrag>().0.is_none());
}

#[test]
fn a_drag_on_a_row_that_was_rebuilt_still_drops_on_release() {
    let mut app = app();
    let source = start_drag(&mut app, "b");

    // The dragged row's key changes, so it is built again under the drag.
    app.world
        .resource_mut::<ArraySignals>()
        .set("cells", cells(&[("a", "a"), ("b2", "b"), ("c", "c")]));
    app.tick();
    assert!(
        app.world.get_entity(source).is_err(),
        "precondition: the row was rebuilt"
    );

    let target = bin(&mut app);
    pointer_to(&mut app, target);
    assert_eq!(
        release(&mut app, target),
        [(Some(source), "b".to_string())],
        "the release ends the drag and drops what it carried"
    );
    assert!(
        app.world.resource::<ActiveDrag>().0.is_none(),
        "and the session closes"
    );
}
