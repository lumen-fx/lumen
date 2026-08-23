//! The software renderer paints no plugin extensions.
//!
//! It rasterises the extracted rects straight into its framebuffer and never walks the node tree,
//! so a native leaf cannot reach a painter here. This pins that down as the backend's documented
//! behaviour rather than something a later change could alter unnoticed.

use lumen_core::prelude::*;
use lumen_render_headless::{HeadlessRenderer, HeadlessRendererPlugin};
use std::sync::Arc;

const W: u32 = 32;
const H: u32 = 32;

#[derive(Component)]
struct Painted;

struct Loud;

impl NativePainter for Loud {
    fn paint(&self, _ctx: &mut NativePaintCtx<'_>) {
        panic!("the software renderer must never call a native painter");
    }
}

fn extract_painted(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Painted)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, _)| {
            let placed = place.place(e, transform, None)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: "test.loud".into(),
                    payload: Arc::new(()),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: next_revision(),
                    clip_to_bounds: true,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, "test.loud", leaves);
}

fn framebuffer(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<HeadlessRenderer>()
        .expect("renderer")
        .framebuffer()
        .to_vec()
}

fn app_with_background() -> App {
    let mut app = App::new();
    app.add_plugin(HeadlessRendererPlugin {
        width: W,
        height: H,
    });
    let mut vp = app.render_world.resource_mut::<Viewport>();
    vp.size = glam::Vec2::new(W as f32, H as f32);
    vp.clear = Color::rgb(0.0, 0.0, 0.0);
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(4.0, 4.0),
            size: glam::Vec2::new(8.0, 8.0),
            baseline_y: None,
        },
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
            ..Default::default()
        },
    ));
    app
}

#[test]
fn a_native_leaf_leaves_the_framebuffer_byte_identical() {
    let mut plain = app_with_background();
    plain.tick();

    let mut with_leaf = app_with_background();
    with_leaf.add_extract_fn(extract_painted);
    with_leaf.register_native_painter("test.loud", Loud);
    with_leaf.world.spawn((
        Transform {
            absolute: glam::Vec2::new(16.0, 16.0),
            size: glam::Vec2::new(12.0, 12.0),
            baseline_y: None,
        },
        Painted,
    ));
    with_leaf.tick();

    assert_eq!(
        framebuffer(&plain),
        framebuffer(&with_leaf),
        "a native leaf must not change what this backend rasterises",
    );
}
