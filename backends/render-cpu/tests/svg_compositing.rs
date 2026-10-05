//! SVG group opacity and masks, rasterized on the CPU: the pixels that
//! prove a group fades as one picture and a mask hides what it should.

use lumen_paint::kurbo::Affine;
use lumen_paint::peniko::Color;
use lumen_paint::svg::paint_svg;
use lumen_render_cpu::CpuPainter;
use lumen_render_cpu::vello_cpu::Pixmap;

const SIZE: u16 = 64;

/// Paint `body` (the inside of a 64 x 64 `<svg>`) over black and read the
/// red channel of each `(x, y)` in `at`.
fn red_at(body: &str, at: &[(u16, u16)]) -> Vec<u8> {
    let svg =
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">{body}</svg>"##);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("valid svg");
    let mut painter = CpuPainter::new(SIZE, SIZE);
    painter.begin_frame(SIZE, SIZE, Color::new([0.0, 0.0, 0.0, 1.0]));
    paint_svg(&mut painter, &tree, Affine::IDENTITY);
    let mut target = Pixmap::new(SIZE, SIZE);
    painter.render_into(&mut target);
    at.iter().map(|&(x, y)| target.sample(x, y).r).collect()
}

fn near(got: u8, want: u8) -> bool {
    got.abs_diff(want) <= 3
}

/// Half opacity on a group halves what it paints, and where two of its
/// children overlap the result is still half: the group fades as one.
#[test]
fn a_faded_group_fades_as_one_picture() {
    let red = red_at(
        r##"<g opacity="0.5">
              <rect width="40" height="64" fill="#ff0000"/>
              <rect x="20" width="40" height="64" fill="#ff0000"/>
            </g>"##,
        &[(10, 32), (30, 32), (62, 32)],
    );
    assert!(near(red[0], 128), "one child: {red:?}");
    assert!(near(red[1], 128), "the overlap: {red:?}");
    assert_eq!(red[2], 0, "outside the group: {red:?}");
}

/// A luminance mask shows its content under white and hides it under
/// black.
#[test]
fn a_luminance_mask_shows_under_white_and_hides_under_black() {
    let red = red_at(
        r##"<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#ffffff"/>
              <rect x="32" width="32" height="64" fill="#000000"/>
            </mask>
            <g mask="url(#m)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "under white: {red:?}");
    assert_eq!(red[1], 0, "under black: {red:?}");
}

/// An alpha mask reads only coverage: opaque black shows the content,
/// where luminance would hide it, and bare mask hides it.
#[test]
fn an_alpha_mask_shows_wherever_the_mask_is_opaque() {
    let red = red_at(
        r##"<mask id="m" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#000000"/>
            </mask>
            <g mask="url(#m)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "under the opaque mask: {red:?}");
    assert_eq!(red[1], 0, "outside it: {red:?}");
}

/// A mask's own mask applies too: the outer mask shows everything, the
/// inner one only the left half.
#[test]
fn a_mask_on_a_mask_applies_both() {
    let red = red_at(
        r##"<mask id="inner" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#000000"/>
            </mask>
            <mask id="outer" mask="url(#inner)" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="64" height="64" fill="#ffffff"/>
            </mask>
            <g mask="url(#outer)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "inside both masks: {red:?}");
    assert_eq!(red[1], 0, "outside the inner mask: {red:?}");
}
