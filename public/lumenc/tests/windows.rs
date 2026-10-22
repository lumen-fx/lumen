// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! An element popped out into a second window, end to end on the CPU
//! renderer: it lays out against that window, paints into that window's
//! frame and not the primary one, matches media queries against that
//! window, and its events still bubble to ancestors in the primary window.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Transform};
use lumen_core::input::{
    PointerButton, PointerMoved, PointerPressed, PointerReleased, PointerState,
};
use lumen_core::node::NodeHandle;
use lumen_core::prelude::{Color, ColorScheme, StyleManager};
use lumen_core::window::{Window, WindowOf, WindowViewport, primary_entity, primary_viewport_mut};
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumenc::{RunOptions, build_headless_app};

const W: u32 = 300;
const H: u32 = 200;
const POP: Vec2 = Vec2::new(160.0, 120.0);

const MARKUP: &str = r#"<root>
  <column id="dock">
    <tile id="main"/>
    <column id="panel"><tile id="inner"/></column>
  </column>
</root>"#;

const CSS: &str = r#"
root { padding: 0; align: start; }
#dock { width: 300px; height: 200px; padding: 0; gap: 0; align: start; }
#main { width: 40px; height: 40px; bg: #ffff00; }
#panel { width: 100%; height: 100%; padding: 0; align: start; bg: #00ff00; }
#inner { width: 40px; height: 40px; bg: #ff0000; }
@media (max-width: 200px) {
  #inner { bg: #0000ff; }
}
"#;

fn boot(name: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_windows_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    let mut opts = RunOptions::new(&dir).with_markup(MARKUP).with_css(CSS);
    opts.hot_reload = false;
    opts.size = (W, H);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.add_plugin(CpuRendererPlugin::new(W, H));
    {
        let vp = &mut *primary_viewport_mut(&mut app.world);
        vp.size = Vec2::new(W as f32, H as f32);
        vp.scale_factor = 1.0;
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    app.world
        .resource_mut::<StyleManager>()
        .set_scheme(ColorScheme::ForceDark);
    settle(&mut app);
    app
}

fn settle(app: &mut App) {
    for _ in 0..12 {
        app.tick();
    }
}

fn entity_of(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element {id}"))
}

fn rect_of(app: &mut App, id: &str) -> (Vec2, Vec2) {
    let e = entity_of(app, id);
    let t = app.world.get::<Transform>(e).expect("laid out");
    (t.absolute, t.size)
}

/// Pop `#panel` out into a new window of size [`POP`].
fn pop_out(app: &mut App) -> Entity {
    let window = app.world.spawn(Window::new("Panel", POP)).id();
    let panel = entity_of(app, "panel");
    app.world.entity_mut(panel).insert(WindowOf(window));
    settle(app);
    window
}

/// The RGB of the pixel at `at` in `window`'s frame.
fn pixel(app: &App, window: Entity, at: Vec2) -> [u8; 3] {
    let cpu = app
        .render_world
        .get_non_send::<CpuRenderer>()
        .expect("the CPU renderer");
    let (w, _) = cpu.size(window).expect("the window has a target");
    let pixels = cpu.read_rgba8(window);
    let i = (at.y as usize * w as usize + at.x as usize) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

#[test]
fn a_popped_out_element_lays_out_and_paints_in_its_window() {
    let mut app = boot("paint");
    let primary = primary_entity(&app.world).expect("a primary window");
    // Docked, the panel sits under `#main` and the primary media applies.
    let (origin, _) = rect_of(&mut app, "inner");
    assert_eq!(pixel(&app, primary, origin + Vec2::splat(5.0)), [255, 0, 0]);

    let window = pop_out(&mut app);
    assert_eq!(
        app.world.get::<WindowViewport>(window).unwrap().size,
        POP,
        "the headless desktop gave the window its size"
    );
    let (origin, size) = rect_of(&mut app, "panel");
    assert_eq!(
        origin,
        Vec2::ZERO,
        "a surface root sits at its window's origin"
    );
    assert_eq!(size, POP, "and fills the window it shows in");
    let (inner, _) = rect_of(&mut app, "inner");
    assert_eq!(inner, Vec2::ZERO);

    // The pop-out's frame: the inner tile, blue by the pop-out's narrow
    // media, over the panel's green.
    assert_eq!(pixel(&app, window, Vec2::splat(5.0)), [0, 0, 255]);
    assert_eq!(pixel(&app, window, Vec2::new(100.0, 100.0)), [0, 255, 0]);
    // The primary frame no longer paints the panel: below `#main` is the
    // dock's empty space.
    assert_eq!(pixel(&app, primary, Vec2::splat(5.0)), [255, 255, 0]);
    assert_eq!(pixel(&app, primary, Vec2::new(5.0, 60.0)), [0, 0, 0]);

    // Taking it back docks it again.
    let panel = entity_of(&mut app, "panel");
    app.world.entity_mut(panel).remove::<WindowOf>();
    app.world.despawn(window);
    settle(&mut app);
    let (inner, _) = rect_of(&mut app, "inner");
    let (_, main) = rect_of(&mut app, "main");
    assert_eq!(inner, Vec2::new(0.0, main.y), "back under `#main`");
    assert_eq!(pixel(&app, primary, inner + Vec2::splat(5.0)), [255, 0, 0]);
    let cpu = app.render_world.get_non_send::<CpuRenderer>().unwrap();
    assert!(
        cpu.size(window).is_none(),
        "the closed window's target is gone"
    );
}

/// A click in the pop-out targets the element there and bubbles up the one
/// tree to a listener on the dock in the primary window.
#[test]
fn a_click_in_the_popout_bubbles_to_the_dock() {
    let mut app = boot("bubble");
    let window = pop_out(&mut app);
    let dock = entity_of(&mut app, "dock");
    let inner = entity_of(&mut app, "inner");

    static HITS: AtomicUsize = AtomicUsize::new(0);
    static TARGET: AtomicU64 = AtomicU64::new(0);
    let token = lumen_script::event::register_native_binding(
        NodeHandle::new(dock).pack(),
        "click".into(),
        false,
        Arc::new(|| {
            HITS.fetch_add(1, Ordering::SeqCst);
            TARGET.store(lumen_script::event::event_target(), Ordering::SeqCst);
        }),
    );

    let at = Vec2::splat(10.0);
    {
        let mut state = app.world.resource_mut::<PointerState>();
        state.position = Some(at);
        state.window = Some(window);
    }
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    app.tick();
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
    lumen_script::event::unregister_binding(token);

    assert_eq!(HITS.load(Ordering::SeqCst), 1, "the dock heard the click");
    assert_eq!(
        TARGET.load(Ordering::SeqCst),
        NodeHandle::new(inner).pack(),
        "the click targeted the element under the pointer in the pop-out"
    );
}

/// A tree outside the document (the devtools panel is one) shown in a
/// window keeps the styles it was given: the document's stylesheet never
/// restyles it for that window's media.
#[test]
fn a_tree_outside_the_document_keeps_its_own_styles_in_a_window() {
    use lumen_core::components::{Length, Style};

    let mut app = boot("outside");
    let window = app.world.spawn(Window::new("Tools", POP)).id();
    let style = Style {
        width: Length::Px(77.0),
        height: Length::Px(33.0),
        ..Style::default()
    };
    let tools = app.world.spawn((style, WindowOf(window))).id();
    settle(&mut app);
    let kept = app.world.get::<Style>(tools).expect("still styled");
    assert_eq!(kept.width, Length::Px(77.0));
    assert_eq!(
        app.world.get::<Transform>(tools).map(|t| t.size),
        Some(Vec2::new(77.0, 33.0))
    );
}
