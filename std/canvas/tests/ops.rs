//! The drawing state machine, on its own.
//!
//! Everything here runs without a world, a GPU, or a script host, because
//! `Gfx::apply` is deliberately the half of the replay that has no vello in
//! it. What it decides - which brush a fill uses, where the transform puts
//! it, what `restore` restores - is what a canvas author actually observes,
//! so it is worth testing where the failures are readable.

use lumen_canvas::color::{Rgba, parse_css};
use lumen_canvas::ops::{FontSpec, Gfx, LineCap, LineJoin, Op};

/// Play a list of ops through a fresh state machine.
fn play(ops: &[Op]) -> Gfx {
    let mut gfx = Gfx::default();
    for op in ops {
        gfx.apply(op);
    }
    gfx
}

#[test]
fn a_canvas_starts_opaque_black_with_a_one_unit_pen() {
    let gfx = Gfx::default();
    assert_eq!(gfx.state.fill, Rgba::BLACK);
    assert_eq!(gfx.state.stroke, Rgba::BLACK);
    assert_eq!(gfx.state.line_width, 1.0);
    assert_eq!(gfx.state.line_cap, LineCap::Butt);
    assert_eq!(gfx.state.line_join, LineJoin::Miter);
    assert_eq!(gfx.state.global_alpha, 1.0);
}

#[test]
fn save_and_restore_nest() {
    let red = parse_css("red").expect("red");
    let blue = parse_css("blue").expect("blue");
    let gfx = play(&[
        Op::SetFill(red),
        Op::Save,
        Op::SetFill(blue),
        Op::Save,
        Op::SetLineWidth(8.0),
        Op::Restore,
        Op::Restore,
    ]);
    assert_eq!(gfx.state.fill, red, "two restores undo two saves");
    assert_eq!(gfx.state.line_width, 1.0);
    assert!(gfx.stack.is_empty());
}

#[test]
fn restoring_past_the_bottom_leaves_the_state_alone() {
    // A script that pops too far is mid-refactor. Resetting its brush over it
    // would leave a blank canvas and no clue why.
    let red = parse_css("red").expect("red");
    let gfx = play(&[Op::SetFill(red), Op::Restore, Op::Restore]);
    assert_eq!(gfx.state.fill, red);
}

#[test]
fn transforms_compose_in_the_order_they_were_applied() {
    let gfx = play(&[Op::Translate(10.0, 20.0), Op::Scale(2.0, 3.0)]);
    let placed = gfx.state.transform * kurbo_point(1.0, 1.0);
    assert!((placed.x - 12.0).abs() < 1e-9, "{placed:?}");
    assert!((placed.y - 23.0).abs() < 1e-9, "{placed:?}");

    // The reverse order puts the same point somewhere else, which is the
    // whole reason the order is observable.
    let gfx = play(&[Op::Scale(2.0, 3.0), Op::Translate(10.0, 20.0)]);
    let placed = gfx.state.transform * kurbo_point(1.0, 1.0);
    assert!((placed.x - 22.0).abs() < 1e-9, "{placed:?}");
}

#[test]
fn a_reset_and_an_explicit_transform_replace_rather_than_compose() {
    let gfx = play(&[Op::Translate(100.0, 100.0), Op::ResetTransform]);
    let placed = gfx.state.transform * kurbo_point(1.0, 1.0);
    assert!((placed.x - 1.0).abs() < 1e-9);

    let gfx = play(&[
        Op::Translate(100.0, 100.0),
        Op::SetTransform([1.0, 0.0, 0.0, 1.0, 5.0, 5.0]),
    ]);
    let placed = gfx.state.transform * kurbo_point(0.0, 0.0);
    assert!((placed.x - 5.0).abs() < 1e-9, "{placed:?}");
}

#[test]
fn a_save_carries_the_transform_too() {
    let gfx = play(&[
        Op::Save,
        Op::Translate(50.0, 0.0),
        Op::Restore,
        Op::Translate(1.0, 0.0),
    ]);
    let placed = gfx.state.transform * kurbo_point(0.0, 0.0);
    assert!((placed.x - 1.0).abs() < 1e-9, "{placed:?}");
}

#[test]
fn the_global_alpha_folds_into_both_brushes() {
    let gfx = play(&[
        Op::SetFill(Rgba::new(1.0, 0.0, 0.0, 0.8)),
        Op::SetStroke(Rgba::new(0.0, 0.0, 1.0, 1.0)),
        Op::SetGlobalAlpha(0.5),
    ]);
    assert!((gfx.fill_brush().a - 0.4).abs() < 1e-6);
    assert!((gfx.stroke_brush().a - 0.5).abs() < 1e-6);
    // The color itself is untouched; only the brush the draw uses is scaled.
    assert!((gfx.state.fill.a - 0.8).abs() < 1e-6);
}

#[test]
fn a_new_path_drops_what_was_there() {
    let gfx = play(&[
        Op::MoveTo(0.0, 0.0),
        Op::LineTo(10.0, 10.0),
        Op::BeginPath,
        Op::MoveTo(5.0, 5.0),
    ]);
    assert_eq!(gfx.path.elements().len(), 1, "{:?}", gfx.path);
}

#[test]
fn a_curve_with_no_move_to_still_starts_somewhere() {
    // A path that opened with `line_to` is a mistake worth surviving: the
    // segment starts where it was told to rather than being dropped.
    let gfx = play(&[Op::LineTo(4.0, 4.0)]);
    assert!(!gfx.path.elements().is_empty());
    assert_eq!(gfx.pen, Some(kurbo_point(4.0, 4.0)));
}

#[test]
fn an_arc_leaves_the_pen_at_its_end() {
    let gfx = play(&[Op::Arc {
        x: 0.0,
        y: 0.0,
        radius: 10.0,
        start: 0.0,
        end: std::f64::consts::FRAC_PI_2,
    }]);
    let pen = gfx.pen.expect("the arc moved the pen");
    assert!(pen.x.abs() < 1e-6 && (pen.y - 10.0).abs() < 1e-6, "{pen:?}");
}

#[test]
fn the_line_style_names_are_the_css_ones() {
    assert_eq!(LineCap::parse("Round"), Some(LineCap::Round));
    assert_eq!(LineJoin::parse(" bevel "), Some(LineJoin::Bevel));
    assert_eq!(LineCap::parse("flat"), None);
    assert_eq!(LineJoin::parse(""), None);
}

#[test]
fn the_font_shorthand_takes_a_weight_a_size_and_a_family() {
    let spec = FontSpec::parse("bold 16px Inter").expect("shorthand");
    assert_eq!(spec.weight, 700);
    assert_eq!(spec.size, 16.0);
    assert_eq!(spec.family, "Inter");

    let bare = FontSpec::parse("12px").expect("size alone");
    assert_eq!((bare.weight, bare.size), (400, 12.0));
    assert!(bare.family.is_empty());

    assert_eq!(
        FontSpec::parse("500 24px Fira Sans").map(|s| s.weight),
        Some(500)
    );
    // Without a size there is nothing to shape.
    assert!(FontSpec::parse("bold Inter").is_none());
}

/// A point, spelled through the same kurbo the module uses.
fn kurbo_point(x: f64, y: f64) -> lumen_canvas::kurbo::Point {
    lumen_canvas::kurbo::Point::new(x, y)
}
