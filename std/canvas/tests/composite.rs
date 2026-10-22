//! Composite operations, clipping, `clear_rect` and reading a canvas back,
//! checked in pixels on every render backend.
//!
//! The page under the canvas is cleared red in every case, so a drawing that
//! erased the page instead of its own pixels would show as black, not red.
//! The CPU backend runs everywhere; the GPU backend skips itself on a machine
//! with no usable adapter.

use bevy_ecs::prelude::*;
use lumen_canvas::{CanvasPlugin, CanvasReader, CompositeOperation, Context2d, PixelBuffer};
use lumen_core::app::App;
use lumen_core::components::{Color, Length, LumenId, LumenTag, Style, Transform, Visible};
use lumen_core::prelude::TickStage;
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};

const W: u32 = 64;
const H: u32 = 64;

/// The canvas store is process-global, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug)]
enum Backend {
    Gpu,
    Cpu,
}

fn backends() -> Vec<Backend> {
    match gpu_unavailable_reason() {
        None => vec![Backend::Gpu, Backend::Cpu],
        Some(why) => {
            eprintln!("GPU skipped: {why}");
            vec![Backend::Cpu]
        }
    }
}

/// A 64x64 app on `backend`, cleared red, with one 64x64 canvas `c` covering
/// the window one canvas unit to the pixel.
fn app(backend: Backend) -> App {
    lumen_canvas::store::reset();
    let mut app = App::new();
    match backend {
        Backend::Gpu => app.add_plugin(WgpuRendererPlugin::new(W, H)),
        Backend::Cpu => app.add_plugin(CpuRendererPlugin::new(W, H)),
    };
    app.add_plugin(CanvasPlugin::default());
    {
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(1.0, 0.0, 0.0);
    }
    app.world.spawn((
        LumenTag(lumen_canvas::TAG.into()),
        LumenId("c".to_string()),
        Style {
            width: Length::Px(W as f32),
            height: Length::Px(H as f32),
            ..Default::default()
        },
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::new(W as f32, H as f32),
            baseline_y: None,
        },
        Visible(true),
    ));
    app
}

fn frame(app: &App) -> Vec<u8> {
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

/// Tick until what was recorded is on screen, and read the frame.
fn draw(app: &mut App) -> Vec<u8> {
    app.tick();
    app.tick();
    frame(app)
}

fn rgb(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// Equal within a couple of levels, which is how far the two renderers'
/// rounding apart can be.
fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 3)
}

#[track_caller]
fn assert_rgb(pixels: &[u8], x: u32, y: u32, want: [u8; 3], what: &str) {
    let got = rgb(pixels, x, y);
    assert!(
        near(got, want),
        "{what}: ({x}, {y}) is {got:?}, want {want:?}"
    );
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

#[test]
fn destination_out_erases_the_canvas_and_not_the_page_under_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_global_composite_operation(CompositeOperation::DestinationOut);
        ctx.fill_rect(0.0, 0.0, 32.0, 64.0);
        let px = draw(&mut app);
        assert_rgb(&px, 8, 32, RED, &format!("{backend:?} erased half"));
        assert_rgb(&px, 48, 32, GREEN, &format!("{backend:?} kept half"));
    }
}

#[test]
fn copy_replaces_the_whole_canvas_with_the_draw() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_global_composite_operation(CompositeOperation::Copy);
        ctx.set_fill_style("#0000ff").unwrap();
        ctx.fill_rect(16.0, 16.0, 16.0, 16.0);
        let px = draw(&mut app);
        assert_rgb(&px, 24, 24, BLUE, &format!("{backend:?} the draw"));
        assert_rgb(&px, 48, 48, RED, &format!("{backend:?} cleared around it"));
    }
}

#[test]
fn the_blend_modes_mix_the_colors() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#ffff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_global_composite_operation(CompositeOperation::Multiply);
        ctx.set_fill_style("#00ffff").unwrap();
        ctx.fill_rect(0.0, 0.0, 32.0, 64.0);
        ctx.set_global_composite_operation(CompositeOperation::Difference);
        ctx.set_fill_style("#ffffff").unwrap();
        ctx.fill_rect(32.0, 0.0, 32.0, 64.0);
        let px = draw(&mut app);
        // Yellow times cyan is green; white minus yellow is blue.
        assert_rgb(&px, 8, 32, GREEN, &format!("{backend:?} multiply"));
        assert_rgb(&px, 48, 32, BLUE, &format!("{backend:?} difference"));
    }
}

#[test]
fn every_operation_name_parses_back_to_itself() {
    for op in CompositeOperation::ALL {
        assert_eq!(op.name().parse::<CompositeOperation>(), Ok(op));
    }
    assert!("normal".parse::<CompositeOperation>().is_err());
}

#[test]
fn a_clip_keeps_draws_inside_it_until_restore() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.save();
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 32.0, 64.0);
        ctx.clip();
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 32.0);
        ctx.restore();
        ctx.set_fill_style("#0000ff").unwrap();
        ctx.fill_rect(0.0, 32.0, 64.0, 32.0);
        let px = draw(&mut app);
        assert_rgb(&px, 8, 8, GREEN, &format!("{backend:?} inside the clip"));
        assert_rgb(&px, 48, 8, RED, &format!("{backend:?} outside the clip"));
        assert_rgb(&px, 48, 48, BLUE, &format!("{backend:?} after restore"));
    }
}

#[test]
fn clips_intersect_and_evenodd_leaves_a_hole() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        // A square with a square hole, under even-odd.
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 64.0, 64.0);
        ctx.rect(16.0, 16.0, 32.0, 32.0);
        ctx.clip_evenodd();
        // And then only the top half of that.
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 64.0, 32.0);
        ctx.clip();
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        let px = draw(&mut app);
        assert_rgb(&px, 4, 4, GREEN, &format!("{backend:?} the ring, top"));
        assert_rgb(&px, 32, 24, RED, &format!("{backend:?} the hole"));
        assert_rgb(&px, 4, 60, RED, &format!("{backend:?} the ring, bottom"));
    }
}

#[test]
fn a_composite_under_a_clip_applies_only_inside_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 32.0, 64.0);
        ctx.clip();
        ctx.set_global_composite_operation(CompositeOperation::DestinationOut);
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        let px = draw(&mut app);
        assert_rgb(&px, 8, 32, RED, &format!("{backend:?} erased inside"));
        assert_rgb(&px, 48, 32, GREEN, &format!("{backend:?} kept outside"));
    }
}

#[test]
fn clear_rect_ignores_alpha_and_composite_and_obeys_the_clip() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_global_alpha(0.25);
        ctx.set_global_composite_operation(CompositeOperation::Lighter);
        ctx.clear_rect(0.0, 0.0, 16.0, 64.0);
        ctx.begin_path();
        ctx.rect(32.0, 0.0, 32.0, 32.0);
        ctx.clip();
        ctx.clear_rect(32.0, 0.0, 32.0, 64.0);
        let px = draw(&mut app);
        assert_rgb(&px, 8, 32, RED, &format!("{backend:?} cleared"));
        assert_rgb(&px, 24, 32, GREEN, &format!("{backend:?} untouched"));
        assert_rgb(&px, 48, 8, RED, &format!("{backend:?} cleared in the clip"));
        assert_rgb(&px, 48, 48, GREEN, &format!("{backend:?} outside the clip"));
    }
}

#[test]
fn clearing_the_whole_canvas_drops_the_drawing_and_keeps_the_state() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#0000ff").unwrap();
        for _ in 0..10 {
            ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
            ctx.clear_rect(0.0, 0.0, 64.0, 64.0);
        }
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        let px = draw(&mut app);
        assert_rgb(&px, 4, 4, BLUE, &format!("{backend:?} fill kept"));
        assert_rgb(&px, 32, 32, RED, &format!("{backend:?} cleared"));
        let store = lumen_canvas::store::store();
        assert_eq!(
            store.surfaces["c"].drawing.len(),
            1,
            "only what was drawn after the last full clear is kept"
        );
    }
}

/// Pixels a system read with [`CanvasReader`], kept for the test to look at.
#[derive(Resource, Default)]
struct Read(Option<Result<Vec<u8>, lumen_canvas::CanvasError>>);

fn read_corner(mut reader: CanvasReader, mut read: ResMut<Read>) {
    let mut out = vec![0u8; 4 * 4 * 4];
    let result = reader
        .read_rgba8(&Context2d::new("c"), 30, 30, 4, 4, &mut out)
        .map(|()| out);
    read.0 = Some(result);
}

#[test]
fn a_system_reads_the_composed_canvas_back_on_the_spot() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        app.world.init_resource::<Read>();
        app.add_systems(TickStage::Systems, read_corner);
        // No frame yet, so nothing to rasterize with.
        app.tick();
        assert_eq!(
            app.world.resource::<Read>().0,
            Some(Err(lumen_canvas::CanvasError::NoRenderer)),
            "{backend:?}"
        );
        app.tick();
        // Drawn this tick, after the encode ran: the read encodes it first.
        let ctx = Context2d::new("c");
        ctx.set_fill_style("rgba(0, 0, 255, 0.5)").unwrap();
        ctx.fill_rect(0.0, 0.0, 32.0, 32.0);
        app.tick();
        let pixels = app
            .world
            .resource::<Read>()
            .0
            .clone()
            .expect("the system ran")
            .expect("read");
        // (30, 30) is inside the fill, (33, 33) outside; straight alpha.
        let at = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        let inside = at(0, 0);
        assert!(
            inside[2] >= 250 && inside[3].abs_diff(128) <= 2,
            "{backend:?} inside {inside:?}"
        );
        assert_eq!(at(3, 3), &[0, 0, 0, 0], "{backend:?} outside");
    }
}

#[test]
fn a_script_read_fills_its_buffer_when_the_canvas_encodes() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_global_composite_operation(CompositeOperation::DestinationOut);
        ctx.fill_rect(0.0, 0.0, 2.0, 2.0);
        let read: PixelBuffer = ctx.get_image_data(0, 0, 4, 4).expect("buffer");
        // A draw after the read is not in it.
        ctx.set_global_composite_operation(CompositeOperation::SourceOver);
        ctx.set_fill_style("#0000ff").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        assert_eq!(read.pixel(3, 3), 0, "nothing until the canvas encodes");
        // The first tick has no frame to rasterize with yet; the read waits
        // for it.
        app.tick();
        app.tick();
        app.tick();
        assert_eq!(read.pixel(0, 0), 0x0000_0000, "{backend:?} erased");
        assert_eq!(read.pixel(3, 3), 0x00ff_00ff, "{backend:?} green");
    }
}

#[test]
fn a_read_of_a_large_canvas_is_one_rasterize() {
    // Not a benchmark; it holds a 1024x1024 read to finishing and to the
    // right size on each backend, which is what the documentation's cost
    // statement is about.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.resize(1024.0, 1024.0);
        ctx.set_fill_style("#ff00ff").unwrap();
        ctx.fill_rect(0.0, 0.0, 1024.0, 1024.0);
        app.tick();
        app.tick();
        let rasterizer = lumen_canvas::store::store()
            .rasterizer
            .clone()
            .expect("published");
        let drawing = lumen_canvas::store::store().surfaces["c"].drawing.clone();
        let started = std::time::Instant::now();
        let pixels = rasterizer
            .rasterize(&drawing, lumen_canvas::kurbo::Affine::IDENTITY, 1024, 1024)
            .expect("rasterized");
        eprintln!("{backend:?}: 1024x1024 read in {:?}", started.elapsed());
        assert_eq!(pixels.len(), 1024 * 1024 * 4);
        assert_eq!(&pixels[..4], &[255, 0, 255, 255]);
    }
}

#[test]
fn an_operator_that_clears_clears_only_inside_the_clip() {
    // `copy` clears whatever the draw does not cover, but only inside the
    // clip. The clip's edge at 20 is mid-tile on purpose: the GPU renderer
    // composites a blend layer over whole 16-pixel tiles, so it also clears
    // the rest of the tiles the edge crosses (here x 20..32). The CPU
    // renderer clears exactly the clip, as the web does.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 20.0, 64.0);
        ctx.clip();
        ctx.set_global_composite_operation(CompositeOperation::Copy);
        ctx.set_fill_style("#0000ff").unwrap();
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        let px = draw(&mut app);
        let b = format!("{backend:?}");
        assert_rgb(&px, 4, 4, BLUE, &format!("{b} the draw"));
        assert_rgb(&px, 12, 32, RED, &format!("{b} cleared inside the clip"));
        assert_rgb(&px, 40, 32, GREEN, &format!("{b} kept outside the clip"));
        if matches!(backend, Backend::Cpu) {
            assert_rgb(&px, 24, 32, GREEN, &format!("{b} kept just past the edge"));
        }
    }
}

#[test]
fn an_operator_that_keeps_stops_exactly_at_the_clip_edge() {
    // The operators that leave alone what the draw does not cover are exact
    // on both renderers, whatever the clip's edge.
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in backends() {
        let mut app = app(backend);
        let ctx = Context2d::new("c");
        ctx.set_fill_style("#00ff00").unwrap();
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 20.0, 64.0);
        ctx.clip();
        ctx.set_global_composite_operation(CompositeOperation::DestinationOut);
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        let px = draw(&mut app);
        let b = format!("{backend:?}");
        assert_rgb(&px, 18, 32, RED, &format!("{b} erased inside"));
        assert_rgb(&px, 22, 32, GREEN, &format!("{b} kept just past the edge"));
    }
}
