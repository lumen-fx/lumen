//! The compiled-in shape: [`CanvasPlugin`] installed like any other plugin on
//! a headless app, driving the whole surface in process.
//!
//! The tree is hand-built rather than parsed, so these cases say nothing
//! about the markup front-end and everything about what the plugin does with
//! a `<canvas>` element once one exists: it adopts it, gives it a box, keeps
//! its drawing, and answers the script.

use lumen_canvas::{Canvas, CanvasPlugin, UA_SIZE};
use lumen_core::app::App as EcsApp;
use lumen_core::components::{ImageComponent, Transform};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR, LengthSpec};
use lumen_runtime::{RunOptions, build_headless_app};

/// The app directory, the DOM snapshot, and the canvas store are all
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh app directory with an id of its own.
fn app_dir(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumen-canvas-plugin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-canvas-plugin-{name}\"\n"),
    )
    .expect("lumen.toml");
    dir
}

/// One `<canvas>` element, optionally with a declared drawing space.
fn canvas_element(id: &str, size: Option<(f32, f32)>) -> Element {
    let mut attrs = Attributes {
        id: Some(id.to_string()),
        ..Default::default()
    };
    if let Some((w, h)) = size {
        attrs.width = Some(LengthSpec::Px(w));
        attrs.height = Some(LengthSpec::Px(h));
    }
    Element {
        tag: lumen_canvas::TAG.to_string(),
        attrs,
        ..Default::default()
    }
}

/// Build a headless app whose tree is `children`, running one script.
fn build_app(
    dir: &std::path::Path,
    engine: &str,
    source: &str,
    children: Vec<Element>,
    plugin: Option<CanvasPlugin>,
) -> EcsApp {
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                children,
                ..Default::default()
            },
            ..Default::default()
        },
        script_source: source.to_string(),
        scripts: vec![CompiledScript {
            engine: engine.to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(dir).with_artifact_bytes(bytes);
    if let Some(plugin) = plugin {
        opts = opts.with_plugin(plugin);
    }
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app
}

/// One signal, as the string a bound label would read.
fn signal(app: &EcsApp, name: &str) -> Option<String> {
    use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

/// The adopted canvas whose id matches, if there is one.
fn canvas_of(app: &mut EcsApp, id: &str) -> Option<(u64, (f32, f32))> {
    let mut q = app.world.query::<&Canvas>();
    q.iter(&app.world)
        .find(|c| c.id == id)
        .map(|c| (c.revision, c.logical))
}

#[test]
fn the_element_is_adopted_and_sized_from_its_declared_drawing_space() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("adopt");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_start() {}\n",
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );

    let (_, logical) = canvas_of(&mut app, "chart").expect("the canvas was adopted");
    assert_eq!(logical, (200.0, 120.0));

    // The box the layout engine gave it comes from the same numbers, because
    // the drawing space is the element's natural size.
    let mut q = app.world.query::<(&Canvas, &ImageComponent, &Transform)>();
    let (_, image, transform) = q.iter(&app.world).next().expect("a laid-out canvas");
    assert_eq!(image.natural_size, Some(glam::Vec2::new(200.0, 120.0)));
    assert_eq!(
        (transform.size.x, transform.size.y),
        (200.0, 120.0),
        "taffy sized the box from the natural size"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_canvas_with_no_declared_size_takes_the_long_standing_default() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("default-size");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_start() {}\n",
        vec![canvas_element("plain", None)],
        Some(CanvasPlugin::default()),
    );

    let (_, logical) = canvas_of(&mut app, "plain").expect("adopted");
    assert_eq!(logical, UA_SIZE);
    assert_eq!(UA_SIZE, (300.0, 150.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rhai_draws_and_the_canvas_says_it_changed() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("rhai-draw");
    let mut app = build_app(
        &dir,
        "rhai",
        r##"
fn on_start() {
    canvas::set_fill_style("chart", "#3b82f6");
    canvas::fill_rect("chart", 10.0, 10.0, 40.0, 20.0);
}
"##,
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    let (revision, _) = canvas_of(&mut app, "chart").expect("adopted");
    assert!(revision > 0, "the drawing bumped the canvas's revision");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lua_reaches_the_same_surface_through_its_table() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("lua");
    let mut app = build_app(
        &dir,
        "lua",
        r#"
function on_start()
  canvas.resize("chart", 64, 32)
  canvas.fill_rect("chart", 0, 0, 8, 8)
end
"#,
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    let (_, logical) = canvas_of(&mut app, "chart").expect("adopted");
    assert_eq!(logical, (64.0, 32.0), "the script resized the canvas");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn candela_reaches_the_module_surface_through_its_folded_block() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("candela");
    let mut app = build_app(
        &dir,
        "candela",
        r#"
import "lumen.cdl";

fn on_start() {
    canvas::set_fill_rgba("chart", 1.0, 0.0, 0.0, 1.0);
    canvas::fill_rect("chart", 0.0, 0.0, 10.0, 10.0);
}

fn main() {}
"#,
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    let (revision, _) = canvas_of(&mut app, "chart").expect("adopted");
    assert!(revision > 0, "candela's calls reached the same surface");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_resize_empties_the_canvas_and_its_drawing_state() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("resize-clears");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    canvas::set_global_alpha("chart", 0.25);
    canvas::set_line_width("chart", 9.0);
    canvas::fill_rect("chart", 0.0, 0.0, 10.0, 10.0);
    canvas::resize("chart", 64.0, 32.0);
}
"#,
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    // The resize is the last call in the handler, and it takes the drawing
    // and the state that made it, the way writing `width` on an HTML canvas
    // does.
    let mut store = lumen_canvas::store::store();
    let surface = store.surface("chart");
    assert_eq!(surface.logical, (64.0, 32.0));
    assert_eq!(surface.gfx.state.global_alpha, 1.0);
    assert_eq!(surface.gfx.state.line_width, 1.0);
    assert!(surface.pending.is_empty());
    drop(store);

    let (_, logical) = canvas_of(&mut app, "chart").expect("adopted");
    assert_eq!(logical, (64.0, 32.0), "the element followed the resize");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_canvas_reports_the_size_a_pending_resize_asked_for() {
    // `resize` is recorded, not applied on the spot, so the very next line of
    // the same handler has to see the new size or every loop bounded by
    // `width()` draws the wrong picture.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("size-readback");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    canvas::resize("chart", 64.0, 32.0);
    signal("w", "").set(canvas::width("chart"));
    signal("h", "").set(canvas::height("chart"));
}
"#,
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    assert_eq!(signal(&app, "w").as_deref(), Some("64"));
    assert_eq!(signal(&app, "h").as_deref(), Some("32"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drawing_with_no_element_is_reported_once() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("orphan");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { canvas::fill_rect("typo", 0.0, 0.0, 4.0, 4.0); }
"#,
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        Some(CanvasPlugin::default()),
    );
    for _ in 0..5 {
        app.tick();
    }
    // Five ticks with a drawing nothing answers for, and one report: the
    // second call through `report_once` is the one that has to stay quiet, so
    // asking it again here is what proves the emission was a one-off rather
    // than something that merely happened to be printed once.
    assert!(lumen_canvas::store::was_reported("typo"));
    assert!(!lumen_canvas::store::was_reported("chart"));
    assert!(
        !lumen_canvas::store::store().report_once("typo", "again"),
        "the id was already reported, so nothing else prints for it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_canvas_whose_element_goes_away_is_forgotten() {
    // A `<for>` block cycling rows spawns and despawns canvases with distinct
    // ids. Without retirement every one it ever showed keeps its recorded
    // calls and its encoded scene for the life of the app.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("retire");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_ready() { canvas::fill_rect("chart", 0.0, 0.0, 10.0, 10.0); }
"#,
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();
    app.tick();
    assert!(canvas_of(&mut app, "chart").is_some());
    assert!(lumen_canvas::store::store().surfaces.contains_key("chart"));

    let entity = {
        let mut q = app.world.query::<(bevy_ecs::prelude::Entity, &Canvas)>();
        q.iter(&app.world)
            .find(|(_, c)| c.id == "chart")
            .map(|(e, _)| e)
            .expect("the canvas element")
    };
    app.world.entity_mut(entity).despawn();
    app.tick();

    assert!(
        !lumen_canvas::store::store().surfaces.contains_key("chart"),
        "the surface goes with the element"
    );
    assert!(!lumen_canvas::store::store().answered.contains("chart"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drawing_call_wakes_a_parked_loop() {
    // An app with nothing else going on parks between events. A script that
    // draws has to be able to ask for the tick that shows it.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("waker");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_ready() { canvas::fill_rect(\"chart\", 0.0, 0.0, 4.0, 4.0); }\n",
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        Some(CanvasPlugin::default()),
    );
    let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&woken);
    app.world
        .insert_resource(lumen_core::app::EventLoopWaker(std::sync::Arc::new(
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        )));
    app.tick();
    app.tick();

    lumen_canvas::store::store().rearm_wake();
    lumen_canvas::store::store().record("chart", lumen_canvas::ops::Op::Clear);
    assert_eq!(
        woken.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the module took the app's waker when it adopted the element"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_resize_moves_the_drawing_space_and_leaves_the_box_where_css_put_it() {
    // The element declared a size, so that is the box layout keeps. A script
    // resizing the canvas changes how many units it draws in, and the painter
    // scales those units onto the box it already has - which is the whole
    // point of the two being separate.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("natural-size");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_ready() { canvas::resize(\"chart\", 64.0, 32.0); }\n",
        vec![canvas_element("chart", Some((200.0, 120.0)))],
        Some(CanvasPlugin::default()),
    );
    for _ in 0..3 {
        app.tick();
    }

    let (_, logical) = canvas_of(&mut app, "chart").expect("adopted");
    assert_eq!(
        logical,
        (64.0, 32.0),
        "the drawing space followed the script"
    );

    let mut q = app.world.query::<(&Canvas, &ImageComponent, &Transform)>();
    let (_, image, transform) = q.iter(&app.world).next().expect("a laid-out canvas");
    assert_eq!(
        image.natural_size,
        Some(glam::Vec2::new(64.0, 32.0)),
        "and so did the size the element would take without a declaration"
    );
    assert_eq!(
        (transform.size.x, transform.size.y),
        (200.0, 120.0),
        "the declared box is unmoved; the drawing is scaled onto it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_canvas_drawn_at_forever_is_bounded() {
    // A draw loop against an id nothing answers for is a typo, and the module
    // says so once. What it must not do is keep every call the typo made.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("bounded");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_ready() {}\n",
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();

    {
        let mut store = lumen_canvas::store::store();
        for _ in 0..(lumen_canvas::store::UNANSWERED_JOURNAL_CAP + 500) {
            store.record("typo", lumen_canvas::ops::Op::Fill);
        }
    }
    app.tick();

    assert_eq!(
        lumen_canvas::store::store().surface("typo").pending.len(),
        lumen_canvas::store::UNANSWERED_JOURNAL_CAP,
        "the oldest calls fall off; the canvas may still be mounted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_canvas_whose_surface_went_is_left_alone() {
    // `reset()` is a test-only door, and it is the shape of any surface that
    // is retired while its element is still there. The encode must skip it
    // rather than resurrect one.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("no-surface");
    let mut app = build_app(
        &dir,
        "rhai",
        "fn on_ready() { canvas::fill_rect(\"chart\", 0.0, 0.0, 4.0, 4.0); }\n",
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        Some(CanvasPlugin::default()),
    );
    app.tick();
    app.tick();
    let (revision, _) = canvas_of(&mut app, "chart").expect("adopted");

    lumen_canvas::store::store().surfaces.remove("chart");
    app.tick();

    assert_eq!(
        canvas_of(&mut app, "chart").expect("still adopted").0,
        revision,
        "the canvas keeps what it had rather than being emptied"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_canvas_that_was_never_given_a_box_still_encodes() {
    // Adoption is what hands an element its box, and it is the only thing
    // that does. A canvas the module did not adopt - one built by hand, or
    // one whose element the app assembled itself - still draws; it simply has
    // no size for layout to read.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();

    let mut app = lumen_core::app::App::new();
    app.add_plugin(CanvasPlugin::default());
    app.world.spawn(Canvas {
        id: "bare".to_string(),
        logical: (32.0, 32.0),
        scene: std::sync::Arc::new(lumen_render_wgpu::vello::Scene::new()),
        revision: 0,
    });
    lumen_canvas::store::store()
        .record("bare", lumen_canvas::ops::Op::FillRect(0.0, 0.0, 8.0, 8.0));
    app.tick();

    let mut q = app.world.query::<&Canvas>();
    let canvas = q
        .iter(&app.world)
        .next()
        .expect("the canvas is still there");
    assert!(canvas.revision > 0, "the drawing was encoded");
    assert!(
        app.world
            .query::<&ImageComponent>()
            .iter(&app.world)
            .next()
            .is_none(),
        "and nothing invented a box for it"
    );
}

#[test]
fn without_the_plugin_there_is_no_canvas_and_the_app_still_runs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_canvas::store::reset();
    let dir = app_dir("absent");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { canvas::fill_rect("chart", 0.0, 0.0, 4.0, 4.0); }
"#,
        // The element is still in the tree; nothing adopts it.
        vec![canvas_element("chart", Some((40.0, 40.0)))],
        None,
    );
    for _ in 0..5 {
        app.tick();
    }
    assert!(
        canvas_of(&mut app, "chart").is_none(),
        "no plugin, no canvas component"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
