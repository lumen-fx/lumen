//! CSS group opacity, rasterized on the CPU: a faded element and its subtree
//! composite as one picture and fade once, so an opaque child hides its
//! parent's background exactly as it would at full opacity.

use bevy_ecs::hierarchy::ChildOf;
use lumen_core::app::App;
use lumen_core::components::{Color, Fill, Opacity, Transform, Visuals};
use lumen_core::render_world::Viewport;
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};

const W: u32 = 120;
const H: u32 = 120;

fn at(x: f32, y: f32, w: f32, h: f32) -> Transform {
    Transform {
        absolute: glam::Vec2::new(x, y),
        size: glam::Vec2::new(w, h),
        baseline_y: None,
    }
}

fn solid(r: f32, g: f32, b: f32) -> Visuals {
    Visuals {
        fill: Some(Fill::Solid(Color::rgb(r, g, b))),
        ..Default::default()
    }
}

/// White page, a blue 80 x 80 tile at `opacity` with a black 40 x 40 child,
/// the scene of issue #482. Returns the frame's pixels.
fn render(opacity: f32) -> Vec<u8> {
    let mut app = App::new();
    app.add_plugin(CpuRendererPlugin::new(W, H));
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(1.0, 1.0, 1.0);
    }
    let tile = app
        .world
        .spawn((
            at(20.0, 20.0, 80.0, 80.0),
            solid(0.0, 0.0, 1.0),
            Opacity(opacity),
        ))
        .id();
    app.world.spawn((
        at(20.0, 20.0, 40.0, 40.0),
        solid(0.0, 0.0, 0.0),
        ChildOf(tile),
    ));
    app.tick();
    app.render_world.non_send::<CpuRenderer>().read_rgba8()
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (pixels[i], pixels[i + 1], pixels[i + 2])
}

fn near(got: (u8, u8, u8), want: (u8, u8, u8)) -> bool {
    let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
    close(got.0, want.0) && close(got.1, want.1) && close(got.2, want.2)
}

/// Black at 30 % over white is (178, 178, 178), with the blue under the
/// child hidden. Fading each layer on its own lets the blue show through and
/// gives (124, 124, 178) instead.
#[test]
fn an_opaque_child_hides_its_faded_parent() {
    let pixels = render(0.3);
    let child = pixel(&pixels, 30, 30);
    assert!(near(child, (178, 178, 178)), "under the child: {child:?}");
    let parent = pixel(&pixels, 90, 90);
    assert!(near(parent, (178, 178, 255)), "the tile alone: {parent:?}");
    let page = pixel(&pixels, 5, 5);
    assert_eq!(page, (255, 255, 255), "the page around it: {page:?}");
}

/// A fully transparent group paints nothing at all, child included.
#[test]
fn a_transparent_group_paints_nothing() {
    let pixels = render(0.0);
    assert_eq!(pixel(&pixels, 30, 30), (255, 255, 255));
    assert_eq!(pixel(&pixels, 90, 90), (255, 255, 255));
}
