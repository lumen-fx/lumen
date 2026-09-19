//! Regression tests for [`Transition<f32>`] sampling across easing
//! variants.
//!
//! These used to be criterion benchmarks (never run under `cargo test`
//! or CI). Converted to `#[test]` functions that assert what the
//! timings assumed without checking: sampling a transition at its
//! start, midpoint, and end gives the values the easing curve promises,
//! and the three curves actually diverge from each other at the same
//! elapsed time (that divergence is the entire reason a caller picks
//! one curve over another).
//!
//! No timing assertion here. Unlike the shape cache, sampling has no
//! hit/miss dichotomy and no cached state to silently stop
//! discriminating: `Transition::sample` is a fixed sequence of
//! arithmetic (and, for the cubic-bezier path, a bounded Newton-Raphson
//! iteration with a bisection fallback) that runs the same steps every
//! call regardless of input. There is no plausible regression that
//! keeps the output correct while making this path slower by a
//! meaningful, reliably reproducible margin, so a ratio or ceiling here
//! would just be noise dressed up as a check.

use lumen_primitives::{Easing, Transition};
use std::time::Duration;

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn linear_sample_matches_progress_at_start_mid_and_end() {
    let t = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::Linear);
    assert!(approx(t.sample(t.start), 0.0), "start must read `from`");
    assert!(
        approx(t.sample(t.start + Duration::from_millis(100)), 0.5),
        "linear midpoint must be exactly halfway"
    );
    let end = t.start + Duration::from_millis(200);
    assert!(approx(t.sample(end), 1.0), "end must read `to`");
    assert!(
        t.done(end),
        "transition must report done at its own duration"
    );
}

#[test]
fn ease_out_sample_matches_progress_at_start_mid_and_end() {
    let t = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::EaseOut);
    assert!(approx(t.sample(t.start), 0.0), "start must read `from`");
    // 1 - (1 - 0.5)^3 = 0.875 - well above the linear midpoint, matching
    // ease-out's "fast start, slow settle" shape.
    let mid = t.sample(t.start + Duration::from_millis(100));
    assert!(
        mid > 0.8,
        "ease-out midpoint should be well past the linear halfway point, got {mid}"
    );
    let end = t.start + Duration::from_millis(200);
    assert!(approx(t.sample(end), 1.0), "end must read `to`");
}

#[test]
fn cubic_bezier_sample_matches_progress_at_start_mid_and_end() {
    // The classic CSS `ease` curve.
    let t = Transition::<f32>::new(
        0.0,
        1.0,
        Duration::from_millis(200),
        Easing::CubicBezier(0.25, 0.1, 0.25, 1.0),
    );
    assert!(approx(t.sample(t.start), 0.0), "start must read `from`");
    let mid = t.sample(t.start + Duration::from_millis(100));
    assert!(
        mid > 0.5 && mid < 1.0,
        "CSS ease midpoint should sit strictly between the endpoints, above the linear half, got {mid}"
    );
    let end = t.start + Duration::from_millis(200);
    assert!(approx(t.sample(end), 1.0), "end must read `to`");
}

#[test]
fn easings_diverge_at_the_same_elapsed_time() {
    // The whole point of carrying three curves is that they disagree at
    // the same wall-clock instant. If a regression made every curve
    // collapse to linear, this is the assertion that would catch it.
    let linear = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::Linear);
    let ease_out = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::EaseOut);
    let bezier = Transition::<f32>::new(
        0.0,
        1.0,
        Duration::from_millis(200),
        Easing::CubicBezier(0.25, 0.1, 0.25, 1.0),
    );
    let now_mid = linear.start + Duration::from_millis(100);

    let linear_mid = linear.sample(now_mid);
    let ease_out_mid = ease_out.sample(now_mid);
    let bezier_mid = bezier.sample(now_mid);

    assert!(
        ease_out_mid > linear_mid,
        "ease-out must be ahead of linear at the same elapsed time"
    );
    assert!(
        bezier_mid > linear_mid,
        "the CSS ease curve must be ahead of linear at the same elapsed time"
    );
    assert_ne!(
        ease_out_mid, bezier_mid,
        "ease-out and the CSS ease curve must be genuinely different curves"
    );
}
