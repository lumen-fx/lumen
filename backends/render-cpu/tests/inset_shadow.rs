//! An inset `box-shadow`, rasterized on the CPU: the shadow shades the
//! inner rim of the box and leaves its middle alone.

use lumen_core::components::Color as LumenColor;
use lumen_core::render_world::ExtractedShadow;
use lumen_paint::emit::emit_shadow;
use lumen_paint::peniko::Color;
use lumen_render_cpu::CpuPainter;
use lumen_render_cpu::vello_cpu::Pixmap;

const SIZE: u16 = 100;

/// Paint a 100 x 100 white canvas, then an inset red shadow with the given
/// offset, blur and spread over the whole of it, and read the red and green
/// channels of each `(x, y)` in `at`.
fn inset_at(offset: (f32, f32), blur: f32, spread: f32, at: &[(u16, u16)]) -> Vec<(u8, u8)> {
    let mut painter = CpuPainter::new(SIZE, SIZE);
    painter.begin_frame(SIZE, SIZE, Color::new([1.0, 1.0, 1.0, 1.0]));
    emit_shadow(
        &mut painter,
        &ExtractedShadow {
            origin: glam::Vec2::new(offset.0, offset.1),
            size: glam::Vec2::new(100.0, 100.0),
            radius: 0.0,
            spread,
            blur,
            color: LumenColor::rgba(1.0, 0.0, 0.0, 1.0),
            order: 0,
            inner: true,
            rect_origin: glam::Vec2::ZERO,
        },
    );
    let mut target = Pixmap::new(SIZE, SIZE);
    painter.render_into(&mut target);
    at.iter()
        .map(|&(x, y)| {
            let p = target.sample(x, y);
            (p.r, p.g)
        })
        .collect()
}

/// `inset 10 10 0 red` is a 10 px band along the top and left edges and
/// nothing else. The shadow once filled the whole box.
#[test]
fn an_offset_inset_shadow_shades_only_the_near_edges() {
    let px = inset_at(
        (10.0, 10.0),
        0.0,
        0.0,
        &[(5, 50), (50, 5), (50, 50), (95, 95)],
    );
    assert_eq!(px[0], (255, 0), "left band: {px:?}");
    assert_eq!(px[1], (255, 0), "top band: {px:?}");
    assert_eq!(px[2], (255, 255), "the middle stays white: {px:?}");
    assert_eq!(px[3], (255, 255), "the far corner stays white: {px:?}");
}

/// A blurred inset shadow is darkest at the rim and fades to nothing in
/// the middle; a spread pushes it further in.
#[test]
fn a_blurred_inset_shadow_fades_toward_the_middle() {
    let px = inset_at((0.0, 0.0), 5.0, 10.0, &[(1, 50), (50, 50)]);
    assert!(px[0].1 < 40, "the rim is red: {px:?}");
    assert_eq!(px[1], (255, 255), "the middle stays white: {px:?}");
}
