// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! The pointer over an element hovers it and every ancestor of it, as in a
//! browser: a button keeps its `:hover` style and its `<tooltip>` while the
//! pointer is over the icon inside it. A click still names the innermost
//! element, like `event.target`.

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Opacity, Transform};
use lumen_core::input::{
    ClickEvent, HoverWithin, Hovered, PointerButton, PointerMoved, PointerPressed, PointerReleased,
    PointerState,
};
use lumen_primitives::TooltipPopup;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_hover_chain_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string())
        .with_css(css.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn entity_of(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element {id}"))
}

fn rect_of(app: &App, e: Entity) -> Transform {
    *app.world.get::<Transform>(e).expect("laid out")
}

fn move_to(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world.write_message(PointerMoved {
        position: p,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.tick();
}

/// The tooltip popups on screen, by the trigger each belongs to.
fn tooltips(app: &mut App) -> Vec<Entity> {
    let mut q = app.world.query::<&TooltipPopup>();
    q.iter(&app.world).map(|p| p.trigger).collect()
}

fn opacity(app: &App, e: Entity) -> Option<f32> {
    app.world.get::<Opacity>(e).map(|o| o.0)
}

const TOOLBAR: &str = r##"<root padding="20">
  <tooltip text="Pencil" delay="0">
    <button id="tool" class="tool" width="80px" height="80px" padding="20px">
      <tile id="icon" class="tool-icon" width="40px" height="40px" bg="#336699" />
    </button>
  </tooltip>
  <tile id="away" width="80px" height="80px" bg="#999999" />
</root>"##;

const TOOLBAR_CSS: &str = ".tool:hover { opacity: 0.5; }";

/// The pixl tool bar case: the pointer resting on the icon in the middle of
/// a button shows the button's tooltip and its `:hover` style; leaving the
/// button takes both away; and a click on the icon names the icon.
#[test]
fn a_button_stays_hovered_while_the_pointer_is_over_its_icon() {
    let mut app = build(TOOLBAR, TOOLBAR_CSS);
    let tool = entity_of(&mut app, "tool");
    let icon = entity_of(&mut app, "icon");
    let away = entity_of(&mut app, "away");
    let t = rect_of(&app, icon);
    let over_icon = t.absolute + t.size / 2.0;

    move_to(&mut app, over_icon);
    app.tick();
    assert!(
        app.world.get::<Hovered>(icon).is_some(),
        "the icon is the hit target; the test would pass vacuously otherwise"
    );
    assert!(app.world.get::<Hovered>(tool).is_none());
    assert!(app.world.get::<HoverWithin>(tool).is_some());
    assert_eq!(
        opacity(&app, tool),
        Some(0.5),
        "the button's :hover applies"
    );
    assert_eq!(tooltips(&mut app), vec![tool], "the button's tooltip shows");

    let a = rect_of(&app, away);
    move_to(&mut app, a.absolute + a.size / 2.0);
    app.tick();
    assert!(app.world.get::<HoverWithin>(tool).is_none());
    assert_eq!(opacity(&app, tool), None, "the :hover style lets go");
    assert!(tooltips(&mut app).is_empty(), "the tooltip goes");

    // A click on the icon names the icon, not the button.
    move_to(&mut app, over_icon);
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerPressed {
        position: over_icon,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world.write_message(PointerReleased {
        position: over_icon,
        button: PointerButton::Primary,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
    let clicked: Vec<Entity> = app
        .world
        .resource::<Messages<ClickEvent>>()
        .iter_current_update_messages()
        .map(|c| c.entity)
        .collect();
    assert_eq!(clicked, vec![icon], "the click names the innermost element");
}

const NESTED: &str = r##"<root padding="20">
  <tooltip text="Panel" delay="0">
    <column id="panel" width="200px" height="120px" padding="20px" bg="#222222">
      <tooltip text="Save" delay="0">
        <button id="save" width="80px" height="40px">
          <tile id="glyph" width="20px" height="20px" bg="#336699" />
        </button>
      </tooltip>
    </column>
  </tooltip>
</root>"##;

/// With tooltip triggers nested inside each other, only the innermost
/// hovered one shows, as nested `title` attributes do in a browser.
#[test]
fn nested_tooltips_show_only_the_innermost() {
    let mut app = build(NESTED, "");
    let panel = entity_of(&mut app, "panel");
    let save = entity_of(&mut app, "save");
    let glyph = entity_of(&mut app, "glyph");

    // Over the panel's padding, at its bottom-right corner so the popup
    // (below-right of the pointer) stays clear of the button: the panel's
    // tooltip.
    let p = rect_of(&app, panel);
    move_to(&mut app, p.absolute + p.size - glam::Vec2::new(5.0, 5.0));
    app.tick();
    assert_eq!(tooltips(&mut app), vec![panel]);

    // Over the glyph inside the button inside the panel: both triggers are
    // hovered, the button's tooltip alone shows.
    let g = rect_of(&app, glyph);
    move_to(&mut app, g.absolute + g.size / 2.0);
    app.tick();
    assert!(app.world.get::<Hovered>(glyph).is_some());
    assert!(app.world.get::<HoverWithin>(panel).is_some());
    assert!(app.world.get::<HoverWithin>(save).is_some());
    assert_eq!(tooltips(&mut app), vec![save]);

    // Back onto the panel's padding: the button's tooltip goes and the
    // panel's comes back.
    move_to(&mut app, p.absolute + p.size - glam::Vec2::new(5.0, 5.0));
    app.tick();
    assert_eq!(tooltips(&mut app), vec![panel]);
}
