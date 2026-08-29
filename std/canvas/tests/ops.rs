//! The drawing state machine, on its own.
//!
//! Everything here runs without a world, a GPU, or a script host, because
//! `Gfx::apply` is deliberately the half of the replay that has no vello in
//! it. What it decides - which brush a fill uses, where the transform puts
//! it, what `restore` restores - is what a canvas author observes,
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
fn every_way_of_adding_to_a_path_moves_the_pen() {
    // Each segment kind leaves the pen where it ended, which is what the next
    // segment starts from and what `close_path` closes back to.
    for (name, op, at) in [
        ("line_to", Op::LineTo(4.0, 5.0), (4.0, 5.0)),
        ("quad_to", Op::QuadTo(1.0, 1.0, 4.0, 5.0), (4.0, 5.0)),
        (
            "bezier_to",
            Op::BezierTo(1.0, 1.0, 2.0, 2.0, 4.0, 5.0),
            (4.0, 5.0),
        ),
    ] {
        let gfx = play(&[Op::MoveTo(0.0, 0.0), op]);
        assert_eq!(gfx.pen, Some(kurbo_point(at.0, at.1)), "{name}");
        assert!(gfx.path.elements().len() >= 2, "{name}");
    }
}

#[test]
fn a_rect_is_a_closed_subpath_of_its_own() {
    let gfx = play(&[Op::Rect(1.0, 2.0, 10.0, 20.0)]);
    // Move, three lines, close.
    assert_eq!(gfx.path.elements().len(), 5, "{:?}", gfx.path);
    assert_eq!(gfx.pen, Some(kurbo_point(1.0, 2.0)));
    assert_eq!(gfx.subpath_start, Some(kurbo_point(1.0, 2.0)));
}

#[test]
fn closing_returns_the_pen_to_where_the_subpath_started() {
    let gfx = play(&[Op::MoveTo(2.0, 2.0), Op::LineTo(9.0, 9.0), Op::ClosePath]);
    assert_eq!(gfx.pen, Some(kurbo_point(2.0, 2.0)));

    // Closing a path that was never started is not an error, and adds
    // nothing: a script that calls it first has asked for nothing.
    let gfx = play(&[Op::ClosePath]);
    assert!(gfx.path.elements().is_empty());
}

#[test]
fn an_arc_after_a_subpath_joins_it_rather_than_starting_a_new_one() {
    // The HTML canvas draws a line to the arc's first point when the path is
    // already open, which is what makes a rounded corner one shape.
    let arc = || Op::Arc {
        x: 10.0,
        y: 0.0,
        radius: 5.0,
        start: 0.0,
        end: std::f64::consts::FRAC_PI_2,
    };
    let joined = play(&[Op::MoveTo(0.0, 0.0), arc()]);
    let alone = play(&[arc()]);
    assert!(
        joined.path.elements().len() > alone.path.elements().len(),
        "the join adds a segment the standalone arc does not have"
    );
}

#[test]
fn an_arc_with_no_radius_still_leaves_the_pen_somewhere() {
    let gfx = play(&[Op::Arc {
        x: 3.0,
        y: 4.0,
        radius: -2.0,
        start: 0.0,
        end: 1.0,
    }]);
    assert_eq!(
        gfx.pen,
        Some(kurbo_point(3.0, 4.0)),
        "a negative radius floors at none"
    );
}

#[test]
fn the_stroke_settings_are_carried_on_the_state() {
    let blue = parse_css("blue").expect("blue");
    let gfx = play(&[
        Op::SetStroke(blue),
        Op::SetLineWidth(-4.0),
        Op::SetLineCap(LineCap::Square),
        Op::SetLineJoin(LineJoin::Round),
    ]);
    assert_eq!(gfx.state.stroke, blue);
    assert_eq!(gfx.state.line_width, 0.0, "a negative width floors at none");
    assert_eq!(gfx.state.line_cap, LineCap::Square);
    assert_eq!(gfx.state.line_join, LineJoin::Round);
}

#[test]
fn rotating_and_scaling_place_a_point_where_they_say() {
    let gfx = play(&[Op::Rotate(std::f64::consts::FRAC_PI_2)]);
    let placed = gfx.state.transform * kurbo_point(1.0, 0.0);
    assert!(
        placed.x.abs() < 1e-9 && (placed.y - 1.0).abs() < 1e-9,
        "{placed:?}"
    );

    let gfx = play(&[Op::Scale(3.0, 4.0)]);
    let placed = gfx.state.transform * kurbo_point(1.0, 1.0);
    assert!((placed.x - 3.0).abs() < 1e-9 && (placed.y - 4.0).abs() < 1e-9);
}

#[test]
fn the_font_is_carried_on_the_state_and_survives_a_save() {
    let bold = FontSpec::parse("bold 20px Inter").expect("shorthand");
    let gfx = play(&[
        Op::SetFont(bold.clone()),
        Op::Save,
        Op::SetFont(FontSpec::parse("normal 8px").expect("shorthand")),
        Op::Restore,
    ]);
    assert_eq!(gfx.state.font, bold);
}

#[test]
fn the_alpha_is_clamped_where_it_is_set() {
    assert_eq!(play(&[Op::SetGlobalAlpha(4.0)]).state.global_alpha, 1.0);
    assert_eq!(play(&[Op::SetGlobalAlpha(-4.0)]).state.global_alpha, 0.0);
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
    assert_eq!(FontSpec::parse("normal 12px").map(|s| s.weight), Some(400));
    // A weight outside the CSS range is a family name, not a weight.
    let odd = FontSpec::parse("2000 12px").expect("shorthand");
    assert_eq!((odd.weight, odd.family.as_str()), (400, "2000"));
    // Without a size there is nothing to shape.
    assert!(FontSpec::parse("bold Inter").is_none());
}

/// A point, spelled through the same kurbo the module uses.
fn kurbo_point(x: f64, y: f64) -> lumen_canvas::kurbo::Point {
    lumen_canvas::kurbo::Point::new(x, y)
}
