//! CSS group opacity on the GPU: a faded element and its subtree composite as
//! one picture, and a group that paints the same from frame to frame is
//! composited from the layer kept for it, with the same pixels.
//!
//! Skips itself when no wgpu adapter is available.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use lumen_core::app::App;
use lumen_core::components::{
    Border, Color, Edges, Fill, Opacity, ShadowSpec, TextContent, TextStyle, Transform, Visuals,
};
use lumen_core::render_world::Viewport;
use lumen_paint::LayerStats;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumen_text_cosmic::CosmicShaper;

const W: u32 = 120;
const H: u32 = 80;

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

/// A row of faded cards, each with a shadow, a border, and two children, and
/// an unfaded ticker in a corner that a frame can change to repaint while the
/// cards stay as they were.
struct Cards {
    app: App,
    cards: Vec<Entity>,
    bodies: Vec<Entity>,
    ticker: Entity,
}

fn cards() -> Option<Cards> {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return None;
    }
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(1.0, 1.0, 1.0);
    }
    let (mut cards, mut bodies) = (Vec::new(), Vec::new());
    for i in 0..3 {
        let x = 10.0 + i as f32 * 36.0;
        let card = app
            .world
            .spawn((
                at(x, 20.0, 30.0, 40.0),
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(0.2, 0.4, 0.9))),
                    radius: 4.0,
                    corner_radii: None,
                    shadows: vec![ShadowSpec {
                        offset_x: 0.0,
                        offset_y: 2.0,
                        blur: 3.0,
                        spread: 0.0,
                        color: Color::rgba(0.0, 0.0, 0.0, 0.5),
                        inner: false,
                    }],
                    border: Some(Border::uniform(Edges::all(1.0), Color::rgb(0.0, 0.0, 0.0))),
                },
                Opacity(0.5),
            ))
            .id();
        app.world
            .spawn((at(x, 20.0, 30.0, 8.0), solid(0.9, 0.1, 0.1), ChildOf(card)));
        let body = app
            .world
            .spawn((
                at(x + 4.0, 32.0, 22.0, 22.0),
                solid(0.0, 0.0, 0.0),
                ChildOf(card),
            ))
            .id();
        cards.push(card);
        bodies.push(body);
    }
    let ticker = app
        .world
        .spawn((at(0.0, 0.0, 4.0, 4.0), solid(0.0, 1.0, 0.0)))
        .id();
    Some(Cards {
        app,
        cards,
        bodies,
        ticker,
    })
}

impl Cards {
    /// Paint one frame, changing only the ticker, and return its pixels.
    fn frame(&mut self, n: usize) -> Vec<u8> {
        let shade = (n % 7) as f32 / 7.0;
        self.app
            .world
            .get_mut::<Visuals>(self.ticker)
            .expect("the ticker")
            .fill = Some(Fill::Solid(Color::rgb(shade, 1.0, 0.0)));
        self.app.tick();
        self.renderer().read_rgba8().expect("readback")
    }

    fn renderer(&self) -> &WgpuRenderer {
        self.app.render_world.non_send::<WgpuRenderer>()
    }

    fn stats(&self) -> LayerStats {
        self.renderer().layer_stats().expect("attached")
    }
}

/// The largest difference of any channel of any pixel outside the ticker.
fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| (*i as u32 % W) >= 4 || (*i as u32 / W) >= 4)
        .map(|(_, (p, q))| {
            p.iter()
                .zip(q)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

/// How far a kept layer may stray from painting the group directly: the
/// layer's texture holds 8-bit straight alpha, so a faint pixel loses a
/// little colour precision on the round trip.
const TOLERANCE: u8 = 3;

/// An opaque child hides its faded parent, as CSS group opacity does: black
/// at half over white, not black over a half-visible blue.
#[test]
fn an_opaque_child_hides_its_faded_parent() {
    let Some(mut scene) = cards() else { return };
    let pixels = scene.frame(0);
    let i = ((43 * W + 21) * 4) as usize;
    let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
    assert!(
        r.abs_diff(128) <= 3 && g.abs_diff(128) <= 3 && b.abs_diff(128) <= 3,
        "{r} {g} {b}"
    );
}

/// A group that paints the same is painted directly the first frame, into a
/// layer the second, and composited from that layer from then on, and every
/// one of those frames shows the same pixels.
#[test]
fn a_kept_layer_composites_the_pixels_the_group_painted() {
    let Some(mut scene) = cards() else { return };
    let direct = scene.frame(0);
    assert_eq!(scene.stats().layers, 0, "nothing is kept the first frame");
    let rendered = scene.frame(1);
    assert_eq!(
        scene.stats().renders,
        3,
        "each card is kept once it held still"
    );
    let composited = scene.frame(2);
    let stats = scene.stats();
    assert_eq!((stats.renders, stats.hits, stats.layers), (3, 3, 3));
    let (a, b) = (max_diff(&direct, &rendered), max_diff(&direct, &composited));
    assert!(a <= TOLERANCE && b <= TOLERANCE, "{a} {b}");
}

/// A fade changes only the alpha the layer composites at: no card repaints,
/// and the frame matches the group painted directly at that alpha.
#[test]
fn a_fade_reuses_the_layer() {
    let Some(mut scene) = cards() else { return };
    scene.frame(0);
    scene.frame(1);
    for &card in &scene.cards {
        scene.app.world.get_mut::<Opacity>(card).expect("faded").0 = 0.3;
    }
    let faded = scene.frame(2);
    assert_eq!(scene.stats().renders, 3, "the fade repainted a card");

    let Some(mut fresh) = cards() else { return };
    for &card in &fresh.cards {
        fresh.app.world.get_mut::<Opacity>(card).expect("faded").0 = 0.3;
    }
    let expected = fresh.frame(0);
    let d = max_diff(&faded, &expected);
    assert!(d <= TOLERANCE, "{d}");
}

/// A change inside one group shows the frame it happens and repaints that
/// group alone; a group that stops painting lets its layer go.
#[test]
fn a_change_inside_a_group_repaints_only_that_group() {
    let Some(mut scene) = cards() else { return };
    scene.frame(0);
    scene.frame(1);
    scene
        .app
        .world
        .get_mut::<Visuals>(scene.bodies[1])
        .expect("a body")
        .fill = Some(Fill::Solid(Color::rgb(1.0, 1.0, 1.0)));
    let changed = scene.frame(2);
    let i = ((40 * W + 10 + 36 + 15) * 4) as usize;
    assert!(
        changed[i] > 230 && changed[i + 1] > 230,
        "{:?}",
        &changed[i..i + 3]
    );
    assert_eq!(scene.stats().layers, 2, "the changed card lost its layer");
    scene.app.world.despawn(scene.cards[0]);
    scene.frame(3);
    let stats = scene.stats();
    assert_eq!((stats.renders, stats.layers), (4, 2), "{stats:?}");
}

/// The label frame's height: room for a label below a layer's height.
const LABEL_H: u32 = 120;

/// A label 80 px down the frame, deeper than the layer of the faded group
/// around it is tall, so a glyph placed or culled in the group's own coordinates rather
/// than the frame's misses: inside a group at `opacity`, or with the alpha
/// on its colour instead when `faded` is false. Frames 0 to 2 paint the
/// group directly, into a kept layer, and from it; the stats are the last
/// frame's.
fn label(opacity: f32, faded: bool) -> Option<(Vec<Vec<u8>>, LayerStats)> {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return None;
    }
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, LABEL_H).with_text_shaper(CosmicShaper::new()));
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, LABEL_H as f32);
        vp.clear = Color::rgb(1.0, 1.0, 1.0);
    }
    let alpha = if faded { 1.0 } else { opacity };
    let text = (
        at(13.0, 80.0, 100.0, 30.0),
        TextContent("Hamburg".to_string()),
        TextStyle {
            color: Color::rgba(0.0, 0.0, 0.0, alpha),
            size_px: 24.0,
            ..Default::default()
        },
    );
    if faded {
        let group = app
            .world
            .spawn((
                at(10.0, 80.0, 106.0, 30.0),
                solid(1.0, 1.0, 1.0),
                Opacity(opacity),
            ))
            .id();
        app.world.spawn((text, ChildOf(group)));
    } else {
        app.world.spawn(text);
    }
    let ticker = app
        .world
        .spawn((at(0.0, 0.0, 4.0, 4.0), solid(0.0, 1.0, 0.0)))
        .id();
    let frames = (0..3)
        .map(|n| {
            app.world
                .get_mut::<Visuals>(ticker)
                .expect("the ticker")
                .fill = Some(Fill::Solid(Color::rgb(n as f32 / 3.0, 1.0, 0.0)));
            app.tick();
            let renderer = app.render_world.non_send::<WgpuRenderer>();
            renderer.read_rgba8().expect("readback")
        })
        .collect();
    let stats = app
        .render_world
        .non_send::<WgpuRenderer>()
        .layer_stats()
        .expect("attached");
    Some((frames, stats))
}

/// Text in a faded group fills the same flattened glyphs at the same place
/// as the label drawn unfaded at the group's alpha, painted directly, into
/// the kept layer, and composited from it.
#[test]
fn text_in_a_faded_group_matches_the_label_at_its_alpha() {
    let Some((expected, _)) = label(0.5, false) else {
        return;
    };
    let ink = expected[0].chunks_exact(4).filter(|p| p[0] < 200).count();
    assert!(ink > 100, "the label painted no text: {ink} pixels");
    let Some((faded, stats)) = label(0.5, true) else {
        return;
    };
    assert_eq!((stats.renders, stats.hits), (1, 1), "{stats:?}");
    for (n, frame) in faded.iter().enumerate() {
        let d = max_diff(frame, &expected[n]);
        assert!(
            d <= TOLERANCE,
            "frame {n} strays {d} from the unfaded label"
        );
    }
}
