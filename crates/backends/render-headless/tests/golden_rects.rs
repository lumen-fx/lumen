//! Golden-image test for the headless rasterizer on the render-world model.
//!
//! Drives a full tick: layout in main world -> extract into render world ->
//! headless render system rasterizes ExtractedRect entities -> framebuffer.

use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};
use lumen_render_headless::{HeadlessRenderer, HeadlessRendererPlugin};

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/two_rects.png");

fn render_two_rects() -> (u32, u32, Vec<u8>) {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    app.add_plugin(HeadlessRendererPlugin {
        width: 200,
        height: 100,
    });

    {
        let mut layout = app.world.get_non_send_mut::<LayoutResource>().unwrap();
        layout.set_viewport(200.0, 100.0);
    }
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(200.0, 100.0);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }

    let root = app
        .world
        .spawn((
            Style {
                width: Length::Px(200.0),
                height: Length::Px(100.0),
                flex_direction: FlexDirection::Row,
                ..Default::default()
            },
            DirtyLayout,
        ))
        .id();

    app.world.spawn((
        Style {
            width: Length::Px(100.0),
            height: Length::Px(100.0),
            ..Default::default()
        },
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
            ..Default::default()
        },
        ChildOf(root),
        DirtyLayout,
    ));
    app.world.spawn((
        Style {
            width: Length::Px(100.0),
            height: Length::Px(100.0),
            ..Default::default()
        },
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(0.0, 0.5, 1.0))),
            ..Default::default()
        },
        ChildOf(root),
        DirtyLayout,
    ));

    app.tick();

    let renderer = app
        .render_world
        .get_non_send::<HeadlessRenderer>()
        .expect("renderer installed by plugin");
    let (w, h) = renderer.size();
    (w, h, renderer.framebuffer().to_vec())
}

#[test]
fn two_rects_match_golden() {
    let (w, h, actual) = render_two_rects();
    assert_eq!(actual.len(), (w * h * 4) as usize, "framebuffer length");

    let update = std::env::var("UPDATE_GOLDENS").ok().as_deref() == Some("1");
    if update {
        image::save_buffer(GOLDEN, &actual, w, h, image::ColorType::Rgba8)
            .expect("write golden PNG");
        eprintln!("UPDATE_GOLDENS=1: wrote {GOLDEN}");
        return;
    }

    let golden = match image::open(GOLDEN) {
        Ok(img) => img.into_rgba8(),
        Err(_) => {
            panic!("missing golden image at {GOLDEN} - rerun with UPDATE_GOLDENS=1 to create it")
        }
    };
    assert_eq!(golden.width(), w);
    assert_eq!(golden.height(), h);
    let expected = golden.into_raw();
    if actual != expected {
        let diff_count = actual.iter().zip(&expected).filter(|(a, b)| a != b).count();
        panic!(
            "framebuffer differs from golden in {} bytes - rerun with UPDATE_GOLDENS=1 if intentional",
            diff_count
        );
    }
}
