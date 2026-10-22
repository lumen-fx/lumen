//! The pixels, on every render backend.
//!
//! Everything else about this module can be checked without a renderer;
//! whether a canvas appears cannot. These cases run each backend offscreen,
//! draw through the module the way a script would, and read the framebuffer
//! back. The CPU backend runs everywhere; the GPU backend skips itself on a
//! machine with no usable adapter, the same contract every other GPU pixel
//! suite in the tree has.

use lumen_canvas::{Canvas, CanvasPlugin, Context2d, PixelBuffer};
use lumen_core::app::App;
use lumen_core::components::{
    Color, Length, LumenId, LumenTag, Opacity, Style, Transform, Visible,
};
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};

const W: u32 = 64;
const H: u32 = 64;

/// The canvas store is process-global, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A render backend a case runs on.
#[derive(Clone, Copy, Debug)]
enum Backend {
    Gpu,
    Cpu,
}

/// The backends this machine can run: the CPU always, the GPU when it has
/// an adapter.
fn backends() -> Vec<Backend> {
    match gpu_unavailable_reason() {
        None => vec![Backend::Gpu, Backend::Cpu],
        Some(why) => {
            eprintln!("GPU skipped: {why}");
            vec![Backend::Cpu]
        }
    }
}

/// A 64x64 offscreen app on `backend` with the canvas module installed and
/// a black clear.
fn app(backend: Backend) -> App {
    lumen_canvas::store::reset();
    let mut app = App::new();
    match backend {
        Backend::Gpu => app.add_plugin(WgpuRendererPlugin::new(W, H)),
        Backend::Cpu => app.add_plugin(CpuRendererPlugin::new(W, H)),
    };
    app.add_plugin(CanvasPlugin::default());
    let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
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
    match app.render_world.get_non_send::<WgpuRenderer>() {
        Some(gpu) => gpu
            .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
            .expect("readback"),
        None => app
            .render_world
            .get_non_send::<CpuRenderer>()
            .expect("a renderer")
            .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window")),
    }
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
    for backend in backends() {
        let mut app = app(backend);
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
}

#[test]
fn drawing_past_the_edge_is_clipped_to_the_element() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
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
}

#[test]
fn a_box_larger_than_the_drawing_space_scales_the_drawing_onto_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
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
}

#[test]
fn an_ancestor_opacity_reaches_the_pixels() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
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
}

#[test]
fn the_painter_recognizes_the_engines_draw_target() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        // The downcast in the painter is a TypeId match, so it holds only while
        // this module and the renderer mean the same lumen-paint. Nothing here would
        // fail to compile if they drifted apart; the pixels would simply stop
        // arriving, which is what this asserts against.
        let mut app = app(backend);
        spawn_canvas(&mut app, "chart", (0.0, 0.0), (64.0, 64.0), (64.0, 64.0));
        fill_green("chart", 0.0, 0.0, 64.0, 64.0);
        let pixels = draw(&mut app);

        let (_, g, _) = pixel(&pixels, 32, 32);
        assert!(
            g > 200,
            "the canvas painted nothing, which is what a renderer / module \
             lumen-paint mismatch looks like: check that std/canvas takes \
             lumen-paint through lumen-module's paint feature"
        );
    }
}

#[test]
fn a_canvas_that_drew_nothing_this_tick_keeps_its_revision() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
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
}

/// The canvas's revision, which is what tells the renderer its pixels moved.
fn revision(app: &mut App) -> u64 {
    let mut q = app.world.query::<&Canvas>();
    q.iter(&app.world).map(|c| c.revision).next().unwrap_or(0)
}

#[test]
fn a_fade_over_a_canvas_that_is_not_drawing_still_reaches_the_pixels() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        // The seam compares native leaves by their stamp, and treats an equal
        // stamp at equal geometry as equal pixels. A canvas that drew once and
        // then only fades changes nothing else about its leaf, so an opacity left
        // out of the stamp is an alpha that never updates again.
        let mut app = app(backend);
        spawn_canvas(&mut app, "chart", (16.0, 16.0), (32.0, 32.0), (32.0, 32.0));
        fill_green("chart", 0.0, 0.0, 32.0, 32.0);
        let pixels = draw(&mut app);
        let (_, opaque, _) = pixel(&pixels, 32, 32);
        assert!(opaque > 200, "the canvas drew: {opaque}");

        // Fade it, and draw nothing at all.
        let entity = {
            let mut q = app.world.query::<(bevy_ecs::prelude::Entity, &LumenTag)>();
            q.iter(&app.world)
                .map(|(e, _)| e)
                .next()
                .expect("the canvas element")
        };
        app.world.entity_mut(entity).insert(Opacity(0.5));
        app.tick();
        app.tick();

        let (_, faded, _) = pixel(&read_back(&app), 32, 32);
        assert!(
            faded < opaque - 40,
            "the fade reached the pixels: {faded} against {opaque}"
        );
    }
}

/// Redraw a buffer the way an editor does after a stroke: empty the canvas
/// and draw the buffer again, zoomed `zoom` times with smoothing as asked.
fn redraw(ctx: &Context2d, buffer: &PixelBuffer, zoom: f64, smoothing: bool) {
    ctx.clear();
    ctx.set_image_smoothing(smoothing);
    let (w, h) = buffer.size().expect("the buffer exists");
    ctx.draw_buffer_scaled(buffer, 0.0, 0.0, f64::from(w) * zoom, f64::from(h) * zoom);
}

#[test]
fn a_rust_app_writes_a_buffer_region_and_sees_each_write_on_screen() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        spawn_canvas(&mut app, "art", (0.0, 0.0), (64.0, 64.0), (64.0, 64.0));
        // Two tiles across and down, so a write lands in one tile of four.
        let buffer = PixelBuffer::new(128, 128).expect("a buffer");
        buffer.fill_rect(0, 0, 128, 128, 0x0000ffff).expect("fill");
        let ctx = Context2d::new("art");
        redraw(&ctx, &buffer, 2.0, false);
        let first = draw(&mut app);
        assert_eq!(pixel(&first, 21, 21), (0, 0, 255), "{backend:?}: the fill");

        // One call writes a red and a green pixel side by side.
        buffer
            .write_rgba8(10, 10, 2, 1, &[255, 0, 0, 255, 0, 255, 0, 255])
            .expect("write");
        redraw(&ctx, &buffer, 2.0, false);
        app.tick();
        let second = read_back(&app);
        // Zoomed twice without smoothing, buffer pixel 10 covers screen
        // pixels 20 and 21 exactly, and 11 covers 22 and 23.
        assert_eq!(
            pixel(&second, 21, 21),
            (255, 0, 0),
            "{backend:?}: the red pixel"
        );
        assert_eq!(
            pixel(&second, 22, 21),
            (0, 255, 0),
            "{backend:?}: the green pixel"
        );
        assert_eq!(
            pixel(&second, 19, 21),
            (0, 0, 255),
            "{backend:?}: around it"
        );

        // A write in another tile after the first upload: the earlier write
        // stays, the new one shows.
        // At its own size, shifted 8 left, buffer columns 8..72 are on
        // screen: the first tile's write and the second tile's both.
        buffer.set_pixel(70, 4, 0xffffffff).expect("write");
        ctx.clear();
        ctx.draw_buffer(&buffer, -8.0, 0.0);
        app.tick();
        let third = read_back(&app);
        assert_eq!(
            pixel(&third, 62, 4),
            (255, 255, 255),
            "{backend:?}: the new write"
        );
        assert_eq!(
            pixel(&third, 2, 10),
            (255, 0, 0),
            "{backend:?}: the old write kept"
        );
    }
}

/// A draw shows the buffer as it was at the call, as `drawImage` does on the
/// web, even when the buffer is written again before the tick encodes: the
/// screen shows the write only once the buffer is drawn again.
#[test]
fn a_write_after_a_draw_in_the_same_tick_waits_for_the_next_draw() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        spawn_canvas(&mut app, "art", (0.0, 0.0), (64.0, 64.0), (64.0, 64.0));
        let buffer = PixelBuffer::new(8, 8).expect("a buffer");
        buffer.fill_rect(0, 0, 8, 8, 0x0000ffff).expect("fill");
        let ctx = Context2d::new("art");
        redraw(&ctx, &buffer, 8.0, false);
        // Written after the draw call and before the tick that encodes it.
        buffer.fill_rect(0, 0, 8, 8, 0xff0000ff).expect("fill");
        let first = draw(&mut app);
        assert_eq!(
            pixel(&first, 20, 20),
            (0, 0, 255),
            "{backend:?}: the pixels of the call, not of the encode"
        );

        redraw(&ctx, &buffer, 8.0, false);
        app.tick();
        let second = read_back(&app);
        assert_eq!(
            pixel(&second, 20, 20),
            (255, 0, 0),
            "{backend:?}: drawn again, the write shows"
        );
    }
}

#[test]
fn image_smoothing_off_keeps_zoomed_pixels_sharp() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let zoomed = |smoothing: bool| {
            let mut app = app(backend);
            spawn_canvas(&mut app, "art", (0.0, 0.0), (64.0, 64.0), (64.0, 64.0));
            let buffer = PixelBuffer::new(4, 4).expect("a buffer");
            buffer.fill_rect(0, 0, 2, 4, 0xff0000ff).expect("fill");
            buffer.fill_rect(2, 0, 2, 4, 0x0000ffff).expect("fill");
            redraw(&Context2d::new("art"), &buffer, 16.0, smoothing);
            draw(&mut app)
        };
        // The red half ends at 32: its last screen pixel is still red when
        // the zoom keeps pixels sharp, and mixes towards blue when smoothed.
        let sharp = zoomed(false);
        assert_eq!(pixel(&sharp, 31, 8), (255, 0, 0), "{backend:?}: sharp");
        assert_eq!(pixel(&sharp, 32, 8), (0, 0, 255), "{backend:?}: sharp");
        let smooth = zoomed(true);
        let (r, _, b) = pixel(&smooth, 31, 8);
        assert!(
            r < 250 && b > 5,
            "{backend:?}: smoothing mixes the edge: {r},{b}"
        );
    }
}
