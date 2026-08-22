//! Regression tests for shaping at a weight the font family does not ship.
//!
//! cosmic-text matches a family through a face whose weight equals the
//! request exactly, or a variable face whose `wght` axis spans it. An
//! authored `font-weight: 650` against a family shipping 400 and 700 hits
//! neither, which used to drop the family out of matching entirely: every
//! glyph came back from a different last-resort face, at that face's
//! advances, and a string like "1.21.1" painted as scattered glyphs with
//! wide gaps between them. Neighbouring weights looked fine, so the
//! symptom read as an isolated bad number rather than a matching failure.
//!
//! `CosmicShaper` now snaps an authored weight to one the resolved family
//! provides before it shapes, so these check the run a snapped weight
//! produces against the runs on either side of it.

use lumen_text::{ShapeOptions, ShapedRun, TextShaper};
use lumen_text_cosmic::CosmicShaper;

/// The string from the report: digits and periods, whose advances differ
/// enough between faces to make a wrong face obvious.
const VERSION: &str = "1.21.1";

fn at_weight(shaper: &mut CosmicShaper, weight: u16) -> ShapedRun {
    let opts = ShapeOptions {
        weight,
        ..ShapeOptions::default()
    };
    shaper
        .shape(VERSION, 16.0, opts)
        .expect("a version string shapes at any weight")
}

/// Widest advance in the run. A run that fell through to the last-resort
/// face list picks up an emoji or CJK face, whose advances are multiples
/// of a Latin digit's.
fn widest_advance(run: &ShapedRun) -> f32 {
    run.glyphs.iter().map(|g| g.advance).fold(0.0, f32::max)
}

/// An authored weight between two the family ships shapes like its
/// neighbours: one face for the whole run, and advances in the same range
/// rather than several times wider.
#[test]
fn an_intermediate_weight_shapes_like_its_neighbours() {
    let mut shaper = CosmicShaper::new();
    let at_600 = at_weight(&mut shaper, 600);
    let at_650 = at_weight(&mut shaper, 650);
    let at_700 = at_weight(&mut shaper, 700);

    assert_eq!(
        at_650.glyphs.len(),
        at_700.glyphs.len(),
        "650 shapes the same glyph count as 700"
    );
    assert_eq!(
        at_650.segments.len(),
        1,
        "650 resolves to one face, not one per glyph: {} segments",
        at_650.segments.len()
    );

    // Between the two neighbours, with room for the metrics of a heavier
    // face on either side. The bug produced roughly twice the width.
    let low = at_600.width.min(at_700.width) * 0.75;
    let high = at_600.width.max(at_700.width) * 1.25;
    assert!(
        (low..=high).contains(&at_650.width),
        "650 measures near 600 ({}) and 700 ({}), got {}",
        at_600.width,
        at_700.width,
        at_650.width
    );

    let widest = widest_advance(&at_650);
    let neighbour = widest_advance(&at_600).max(widest_advance(&at_700));
    assert!(
        widest <= neighbour * 1.25,
        "no glyph at 650 is wider than its neighbours' widest ({widest} vs {neighbour})"
    );
}

/// Advances stay monotone along the run: every glyph starts where the
/// previous one ended. A per-glyph fallback leaves the x positions
/// stepping by one face's advance and the advances reporting another's,
/// which is what showed up as gaps between the glyphs.
#[test]
fn an_intermediate_weight_leaves_no_gaps_between_glyphs() {
    let mut shaper = CosmicShaper::new();
    let run = at_weight(&mut shaper, 650);
    for pair in run.glyphs.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        let gap = right.x - (left.x + left.advance);
        assert!(
            gap.abs() <= 0.5,
            "glyph at {} starts {gap} px from where the previous one ended",
            right.x
        );
    }
}

/// Every weight in the CSS range shapes through one face. The reported
/// bug was filed against 650, but any weight the family does not ship
/// took the same path - 800 included, on a machine whose sans-serif stops
/// at 700.
#[test]
fn every_authored_weight_resolves_to_one_face() {
    let mut shaper = CosmicShaper::new();
    let mut widths = Vec::new();
    for weight in [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000] {
        let run = at_weight(&mut shaper, weight);
        assert_eq!(
            run.segments.len(),
            1,
            "weight {weight} resolves to one face, got {} segments",
            run.segments.len()
        );
        widths.push(run.width);
    }
    // No weight measures wildly apart from the rest of the ladder: a
    // fallback to an emoji or CJK face doubles the run width.
    let narrowest = widths.iter().copied().fold(f32::INFINITY, f32::min);
    let widest = widths.iter().copied().fold(0.0, f32::max);
    assert!(
        widest <= narrowest * 1.5,
        "the weight ladder measures within a face's range: {widths:?}"
    );
}
