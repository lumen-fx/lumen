//! The pixels, on a real GPU.
//!
//! Everything else about this module can be checked without one; whether a
//! canvas appears cannot. These cases run the wgpu/vello backend
//! offscreen, draw through the module the way a script would, and read the
//! framebuffer back.
//!
//! They skip themselves on a machine with no usable adapter, which is the
//! same contract every other pixel-level suite in the tree has.

use lumen_canvas::{Canvas, CanvasPlugin};
use lumen_core::app::App;
use lumen_core::components::{
    Color, Length, LumenId, LumenTag, Opacity, Style, Transform, Visible,
};
use lumen_core::prelude::Viewport;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};

const W: u32 = 64;
const H: u32 = 64;

/// The canvas store is process-global, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A 64x64 offscreen app with the canvas module installed and a black clear.
fn app() -> App {
    lumen_canvas::store::reset();
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    app.add_plugin(CanvasPlugin::default());
    let mut vp = app.render_world.resource_mut::<Viewport>();
    vp.size = glam::Vec2::new(W as f32, H as f32);
    vp.clear = Color::rgb(0.0, 0.0, 0.0);
    app
}

/// Spawn a `<canvas>` element the module will adopt: `logical` is the drawing
/// space its markup declares, `size` the box layout gave it.
fn spawn_canvas(app: &mut App, id: &str, at: (f32, f32), size: (f32, f32), logical: (f32, f32)) {
    app.world.spawn((
        LumenTag(lumen_canvas::TAG.into()),
        LumenId(id.to_string()),
        Style {
            width: Length::Px(logical.0),
            height: Length::Px(logical.1),
            ..Default::default()
        },
        Transform {
            absolute: glam::Vec2::new(at.0, at.1),
            size: glam::Vec2::new(size.0, size.1),
            baseline_y: None,
        },
        Visible(true),
    ));
}

/// Record one green rectangle, the way a script's `canvas::fill_rect` does.
fn fill_green(id: &str, x: f64, y: f64, w: f64, h: f64) {
    use lumen_canvas::color::Rgba;
    use lumen_canvas::ops::Op;
    let mut store = lumen_canvas::store::store();
    store.record(id, Op::SetFill(Rgba::new(0.0, 1.0, 0.0, 1.0)));
    store.record(id, Op::FillRect(x, y, w, h));
}

fn read_back(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8()
        .expect("readback")
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (pixels[i], pixels[i + 1], pixels[i + 2])
}

/// Two ticks: one to adopt the element, one to encode and draw what the
/// script recorded.
fn draw(app: &mut App) -> Vec<u8> {
    app.tick();
    app.tick();
    read_back(app)
}

#[test]
fn what_a_script_draws_lands_inside_the_element_and_nowhere_else() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app();
    spawn_canvas(&mut app, "chart", (16.0, 16.0), (32.0, 32.0), (32.0, 32.0));
    fill_green("chart", 0.0, 0.0, 32.0, 32.0);
    let pixels = draw(&mut app);

    let (r, g, b) = pixel(&pixels, 32, 32);
    assert!(g > 200 && r < 40 && b < 40, "canvas centre: {r},{g},{b}");
    let (r, g, b) = pixel(&pixels, 2, 2);
    assert!(
        r < 40 && g < 40 && b < 40,
        "outside the element should stay clear: {r},{g},{b}"
    );
}

#[test]
fn drawing_past_the_edge_is_clipped_to_the_element() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app();
    spawn_canvas(&mut app, "chart", (16.0, 16.0), (16.0, 16.0), (16.0, 16.0));
    // Four times the drawing space, in canvas units.
    fill_green("chart", 0.0, 0.0, 64.0, 64.0);
    let pixels = draw(&mut app);

    let (_, g, _) = pixel(&pixels, 20, 20);
    assert!(g > 200, "inside the element: {g}");
    let (r, g, b) = pixel(&pixels, 40, 40);
    assert!(
        r < 40 && g < 40 && b < 40,
        "a canvas must not spill onto its siblings: {r},{g},{b}"
    );
}

#[test]
fn a_box_larger_than_the_drawing_space_scales_the_drawing_onto_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app();
    // A 16-unit drawing space in a 48-pixel box: one unit is three pixels.
    spawn_canvas(&mut app, "chart", (8.0, 8.0), (48.0, 48.0), (16.0, 16.0));
    // The top-left quarter of the drawing space.
    fill_green("chart", 0.0, 0.0, 8.0, 8.0);
    let pixels = draw(&mut app);

    // Scaled up, that quarter covers the top-left 24 pixels of the box.
    let (_, g, _) = pixel(&pixels, 26, 26);
    assert!(g > 200, "the drawing scaled with the box: {g}");
    let (r, g, b) = pixel(&pixels, 44, 44);
    assert!(
        r < 40 && g < 40 && b < 40,
        "and only the quarter that was drawn: {r},{g},{b}"
    );
}

#[test]
fn an_ancestor_opacity_reaches_the_pixels() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app();
    spawn_canvas(&mut app, "chart", (16.0, 16.0), (32.0, 32.0), (32.0, 32.0));
    fill_green("chart", 0.0, 0.0, 32.0, 32.0);
    let mut q = app.world.query::<(bevy_ecs::prelude::Entity, &LumenTag)>();
    let entity = q
        .iter(&app.world)
        .map(|(e, _)| e)
        .next()
        .expect("the canvas element");
    app.world.entity_mut(entity).insert(Opacity(0.5));
    let pixels = draw(&mut app);

    let (_, g, _) = pixel(&pixels, 32, 32);
    assert!(
        (100..=180).contains(&g),
        "half-opaque green over black: {g}"
    );
}

#[test]
fn the_painter_recognizes_the_engines_draw_target() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    // The downcast in the painter is a TypeId match, so it holds only while
    // this module and the renderer mean the same vello. Nothing here would
    // fail to compile if they drifted apart; the pixels would simply stop
    // arriving, which is what this asserts against.
    let mut app = app();
    spawn_canvas(&mut app, "chart", (0.0, 0.0), (64.0, 64.0), (64.0, 64.0));
    fill_green("chart", 0.0, 0.0, 64.0, 64.0);
    let pixels = draw(&mut app);

    let (_, g, _) = pixel(&pixels, 32, 32);
    assert!(
        g > 200,
        "the canvas painted nothing, which is what a renderer / module vello \
         mismatch looks like: check that std/canvas takes lumen-render-wgpu \
         through lumen-module's paint feature"
    );
}

#[test]
fn a_canvas_that_drew_nothing_this_tick_keeps_its_revision() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }

    let mut app = app();
    spawn_canvas(&mut app, "chart", (16.0, 16.0), (32.0, 32.0), (32.0, 32.0));
    fill_green("chart", 0.0, 0.0, 32.0, 32.0);
    draw(&mut app);
    let after_drawing = revision(&mut app);
    assert!(after_drawing > 0);

    // Two idle ticks: no calls recorded, so nothing is re-encoded and the
    // renderer is told the leaf is the one it already has.
    app.tick();
    app.tick();
    assert_eq!(revision(&mut app), after_drawing);

    // One more call and it moves again.
    fill_green("chart", 0.0, 0.0, 4.0, 4.0);
    app.tick();
    assert!(revision(&mut app) > after_drawing);
}

/// The canvas's revision, which is what tells the renderer its pixels moved.
fn revision(app: &mut App) -> u64 {
    let mut q = app.world.query::<&Canvas>();
    q.iter(&app.world).map(|c| c.revision).next().unwrap_or(0)
}
