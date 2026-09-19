//! The cosmic shaper driving the layout engine's shaped-text producer.
//!
//! `lumen-layout-taffy` shapes an editable's buffer once per change into
//! the `ShapedText` component that the editing, IME, and paint paths all
//! read. The producer talks to whichever `TextShaper` the app installed,
//! so the behaviour it promises - real glyph geometry, a stable shape
//! version, the masked string under a concealed echo mode - can only be
//! checked against a shaper that resolves real fonts. That is this crate,
//! which is why these live here rather than beside the producer.

use bevy_ecs::entity::Entity;
use bevy_ecs::system::RunSystemOnce;
use bevy_ecs::world::World;
use glam::Vec2;
use lumen_core::components::{
    EchoMode, LineHeightSpec, Style, TextBlockOrigin, TextStyle, Transform,
};
use lumen_core::text_model::TextBuffer;
use lumen_layout_taffy::update_shaped_text;
use lumen_text::{ShapedText, ShaperService, TextViewport};
use lumen_text_cosmic::CosmicShaper;

/// D4c: the producer shapes an editable's buffer into a `ShapedText`
/// (with usable geometry) plus a `TextViewport`, and skips the reshape
/// when nothing changed (stable `shape_version`).
#[test]
fn update_shaped_text_writes_component_and_versions() {
    let mut world = World::new();
    world.insert_non_send(ShaperService::new(CosmicShaper::new()));
    let e = world
        .spawn((
            TextBuffer::single_line("hello"),
            Transform::new(Vec2::new(0.0, 0.0), Vec2::new(200.0, 30.0)),
            TextStyle::default(),
            Style::default(),
        ))
        .id();

    world.run_system_once(update_shaped_text).unwrap();
    let st = world.get::<ShapedText>(e).expect("ShapedText produced");
    let v1 = st.shape_version;
    // Geometry maps bytes: the end caret sits to the right of the start.
    assert!(st.geometry.caret_xy(5).0 > st.geometry.caret_xy(0).0);
    assert!(world.get::<TextViewport>(e).is_some());

    // Re-running with no change keeps the same version (no reshape).
    world.run_system_once(update_shaped_text).unwrap();
    let v2 = world.get::<ShapedText>(e).unwrap().shape_version;
    assert_eq!(v1, v2);
}

/// Spawn one editable in a `size` box and run the producer over it.
fn shaped_world(
    text: &str,
    size: Vec2,
    echo: Option<EchoMode>,
    multiline: bool,
) -> (World, Entity) {
    let buf = if multiline {
        TextBuffer::multi_line(text)
    } else {
        TextBuffer::single_line(text)
    };
    let mut world = World::new();
    world.insert_non_send(ShaperService::new(CosmicShaper::new()));
    let mut ent = world.spawn((
        buf,
        Transform::new(Vec2::ZERO, size),
        TextStyle::default(),
        Style::default(),
    ));
    if let Some(mode) = echo {
        ent.insert(mode);
    }
    let e = ent.id();
    world.run_system_once(update_shaped_text).unwrap();
    (world, e)
}

/// The producer publishes the vertical origin the drawn baseline and
/// the pointer hit test share: a lone line in a single-line field
/// centers in the box, a stacked block starts at its top.
#[test]
fn update_shaped_text_publishes_the_block_origin() {
    let tall = Vec2::new(200.0, 400.0);
    let (world, e) = shaped_world("one line", tall, None, false);
    let top = world.get::<TextBlockOrigin>(e).expect("origin").top;
    assert!(
        top > 100.0,
        "a lone line centers in a 400px box (got {top})"
    );

    let (world, e) = shaped_world("first\nsecond\nthird", tall, None, true);
    let lines = world.get::<ShapedText>(e).unwrap().geometry.line_count();
    assert_eq!(lines, 3);
    assert_eq!(
        world.get::<TextBlockOrigin>(e).expect("origin").top,
        0.0,
        "a stacked block starts at the inner box top"
    );
}

/// A text area stays top-aligned however little it holds, so typing
/// the first newline does not make the content jump.
#[test]
fn a_text_area_stays_top_aligned_while_it_holds_one_line() {
    let (world, e) = shaped_world("one line", Vec2::new(200.0, 400.0), None, true);
    assert_eq!(world.get::<TextBlockOrigin>(e).expect("origin").top, 0.0);
}

/// A concealed field shapes its MASK glyphs, so measuring, hit-testing
/// and drawing all agree on one run.
#[test]
fn update_shaped_text_shapes_the_mask_for_a_concealed_field() {
    let size = Vec2::new(200.0, 30.0);
    let (plain_world, pe) = shaped_world("WWWWW", size, None, false);
    let (masked_world, me) = shaped_world("WWWWW", size, Some(EchoMode::Password), false);
    let plain_w = plain_world.get::<ShapedText>(pe).unwrap().run.width;
    let masked_w = masked_world.get::<ShapedText>(me).unwrap().run.width;
    assert!(
        masked_w < plain_w,
        "mask glyphs are narrower than 'W' ({masked_w} vs {plain_w})"
    );

    // `NoEcho` draws nothing at all.
    let (world, e) = shaped_world("WWWWW", size, Some(EchoMode::NoEcho), false);
    assert_eq!(world.get::<ShapedText>(e).unwrap().run.width, 0.0);
}

/// `TextViewport::line_h` falls back to `size_px * 1.2` absent a CSS
/// `line-height`, and honours an explicit override - the same
/// override/fallback shape as every other CSS-supplied value.
/// Changing only `line_height` (same buffer / size / width) must also
/// bust the shape version so the reshape actually happens.
#[test]
fn update_shaped_text_honours_line_height_override_and_busts_version() {
    let mut world = World::new();
    world.insert_non_send(ShaperService::new(CosmicShaper::new()));
    let e = world
        .spawn((
            TextBuffer::single_line("hello"),
            Transform::new(Vec2::new(0.0, 0.0), Vec2::new(200.0, 30.0)),
            TextStyle::default(),
            Style::default(),
        ))
        .id();

    world.run_system_once(update_shaped_text).unwrap();
    let default_line_h = world.get::<TextViewport>(e).unwrap().line_h;
    assert_eq!(default_line_h, 16.0 * 1.2);
    let v1 = world.get::<ShapedText>(e).unwrap().shape_version;

    // Author a CSS `line-height: 40px` override; nothing else changes.
    world.get_mut::<TextStyle>(e).unwrap().line_height = Some(LineHeightSpec::Px(40.0));
    world.run_system_once(update_shaped_text).unwrap();
    let overridden_line_h = world.get::<TextViewport>(e).unwrap().line_h;
    assert_eq!(overridden_line_h, 40.0);
    let v2 = world.get::<ShapedText>(e).unwrap().shape_version;
    assert_ne!(
        v1, v2,
        "a line-height-only change must bust the shape version"
    );
}
