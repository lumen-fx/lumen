//! CSS group opacity, rasterized on the CPU: a faded element and its subtree
//! composite as one picture and fade once, so an opaque child hides its
//! parent's background exactly as it would at full opacity.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use lumen_core::app::App;
use lumen_core::components::{
    Border, Color, Edges, Fill, Opacity, ShadowSpec, TextContent, TextStyle, Transform, Visuals,
};
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumen_text::ShaperService;
use lumen_text_cosmic::CosmicShaper;

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
    {
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
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
    app.render_world
        .non_send::<CpuRenderer>()
        .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
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

/// A row of faded cards, each with a shadow, a border, and two children, and
/// an unfaded ticker in a corner that a frame can change to repaint while the
/// cards stay as they were.
struct Cards {
    app: App,
    cards: Vec<Entity>,
    bodies: Vec<Entity>,
    ticker: Entity,
}

fn cards(width: u32, height: u32) -> Cards {
    let mut app = App::new();
    app.add_plugin(CpuRendererPlugin::new(width, height));
    {
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = glam::Vec2::new(width as f32, height as f32);
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
    Cards {
        app,
        cards,
        bodies,
        ticker,
    }
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
        self.renderer().read_rgba8(
            lumen_core::window::primary_entity(&self.app.world).expect("a primary window"),
        )
    }

    fn renderer(&self) -> &CpuRenderer {
        self.app.render_world.non_send::<CpuRenderer>()
    }

    fn stats(&self) -> lumen_paint::LayerStats {
        self.renderer()
            .layer_stats(
                lumen_core::window::primary_entity(&self.app.world).expect("a primary window"),
            )
            .expect("attached")
    }
}

/// The largest difference of any channel of any pixel outside the ticker.
fn max_diff(a: &[u8], b: &[u8], width: u32) -> u8 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| (*i as u32 % width) >= 4 || (*i as u32 / width) >= 4)
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

/// A group that paints the same is painted directly the first frame, into a
/// layer the second, and composited from that layer from then on, and every
/// one of those frames shows the same pixels.
#[test]
fn a_kept_layer_composites_the_pixels_the_group_painted() {
    let mut scene = cards(W, 80);
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
    assert_eq!((stats.renders, stats.hits), (3, 3));
    assert_eq!(stats.layers, 3);
    assert!(
        stats.bytes > 0 && stats.bytes < 3 * 60 * 60 * 4,
        "{stats:?}"
    );
    assert!(max_diff(&direct, &rendered, W) <= 1);
    assert!(max_diff(&direct, &composited, W) <= 1);
}

/// A fade changes only the alpha the layer composites at: no card repaints,
/// and every frame matches a fresh render of the same alpha.
#[test]
fn a_fade_reuses_the_layer() {
    let mut scene = cards(W, 80);
    scene.frame(0);
    scene.frame(1);
    for (n, alpha) in [0.4, 0.3, 0.2].into_iter().enumerate() {
        for &card in &scene.cards {
            scene.app.world.get_mut::<Opacity>(card).expect("faded").0 = alpha;
        }
        let faded = scene.frame(2 + n);
        assert_eq!(scene.stats().renders, 3, "the fade repainted a card");

        let mut fresh = cards(W, 80);
        for &card in &fresh.cards {
            fresh.app.world.get_mut::<Opacity>(card).expect("faded").0 = alpha;
        }
        let expected = fresh.frame(0);
        assert!(max_diff(&faded, &expected, W) <= 1, "at alpha {alpha}");
    }
}

/// A change inside one group shows the frame it happens, repaints that group
/// alone, and the group is kept again once it holds still.
#[test]
fn a_change_inside_a_group_repaints_that_group() {
    let mut scene = cards(W, 80);
    scene.frame(0);
    scene.frame(1);
    scene.frame(2);
    scene
        .app
        .world
        .get_mut::<Visuals>(scene.bodies[1])
        .expect("a body")
        .fill = Some(Fill::Solid(Color::rgb(1.0, 1.0, 1.0)));
    let changed = scene.frame(3);
    // Half-faded white over the card's blue sits over white: near white.
    let i = ((40 * W + 10 + 36 + 15) * 4) as usize;
    assert!(
        changed[i] > 230 && changed[i + 1] > 230,
        "{:?}",
        &changed[i..i + 3]
    );
    assert_eq!(scene.stats().layers, 2, "the changed card lost its layer");
    scene.frame(4);
    assert_eq!(scene.stats().renders, 4, "kept again once it held still");
    assert_eq!(scene.stats().layers, 3);
}

/// A group that stops fading, or stops painting, lets its layer go.
#[test]
fn a_group_that_stops_fading_lets_its_layer_go() {
    let mut scene = cards(W, 80);
    scene.frame(0);
    scene.frame(1);
    assert_eq!(scene.stats().layers, 3);
    scene
        .app
        .world
        .get_mut::<Opacity>(scene.cards[0])
        .expect("faded")
        .0 = 1.0;
    scene.app.world.despawn(scene.cards[1]);
    scene.frame(2);
    assert_eq!(scene.stats().layers, 1);
}

/// A resize forgets every layer; the new size paints right.
#[test]
fn a_resize_forgets_the_layers() {
    let mut scene = cards(W, 80);
    scene.frame(0);
    scene.frame(1);
    assert_eq!(scene.stats().layers, 3);
    {
        lumen_core::window::primary_viewport_mut(&mut scene.app.world).size =
            glam::Vec2::new(W as f32, 70.0);
    }
    let resized = scene.frame(2);
    assert_eq!(scene.stats().layers, 0);
    assert_eq!(resized.len(), (W * 70 * 4) as usize);
}

/// A label 80 px down the frame, deeper than the layer of the faded group
/// around it is tall, so a glyph placed or culled in the group's own coordinates rather
/// than the frame's misses: inside a group at `opacity`, or with the alpha
/// on its colour instead when `faded` is false. Frames 0 to 2 paint the
/// group directly, into a kept layer, and from it; the stats are the last
/// frame's.
fn label(opacity: f32, faded: bool) -> (Vec<Vec<u8>>, lumen_paint::LayerStats) {
    let mut app = App::new();
    app.add_plugin(CpuRendererPlugin::new(W, H));
    app.render_world
        .insert_non_send(ShaperService::new(CosmicShaper::new()));
    {
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = glam::Vec2::new(W as f32, H as f32);
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
            app.render_world.non_send::<CpuRenderer>().read_rgba8(
                lumen_core::window::primary_entity(&app.world).expect("a primary window"),
            )
        })
        .collect();
    let stats = app
        .render_world
        .non_send::<CpuRenderer>()
        .layer_stats(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
        .expect("attached");
    (frames, stats)
}

/// Text in a faded group fills the same flattened glyphs at the same place
/// as the label drawn unfaded at the group's alpha, painted directly, into
/// the kept layer, and composited from it.
#[test]
fn text_in_a_faded_group_matches_the_label_at_its_alpha() {
    let (expected, _) = label(0.5, false);
    let ink = expected[0].chunks_exact(4).filter(|p| p[0] < 200).count();
    assert!(ink > 100, "the label painted no text: {ink} pixels");
    let (faded, stats) = label(0.5, true);
    assert_eq!((stats.renders, stats.hits), (1, 1), "{stats:?}");
    for (n, frame) in faded.iter().enumerate() {
        let d = max_diff(frame, &expected[n], W);
        assert!(d <= 2, "frame {n} strays {d} from the unfaded label");
    }
}
