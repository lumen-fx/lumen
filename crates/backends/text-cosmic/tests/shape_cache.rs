//! Regression tests for `lumen-text-cosmic`'s shape-result LRU cache.
//!
//! These used to be criterion benchmarks. `cargo bench` is never run in
//! CI (only `--all-targets` builds them), so none of this was ever
//! exercised; the timings they reported measured nothing. Converted to
//! `#[test]` functions that assert the invariants those timings
//! implicitly depended on: a repeat shape is a genuine cache hit, a
//! novel shape is a genuine miss, and an input that belongs in the
//! cache key actually changes the output. One test also asserts a hit
//! is dramatically cheaper than a miss - a cache key that stops
//! discriminating turns every hit into a silent reshape, results stay
//! correct, and the only symptom is that things get slower, so timing
//! is the only signal that catches it.

use lumen_text::{GlyphPosition, ShapeOptions, ShapedRun, TextShaper, WrapMode};
use lumen_text_cosmic::CosmicShaper;
use std::time::{Duration, Instant};

const LABEL: &str = "Lumen - fast UI";
// A realistic wrapped paragraph: the common expensive shape (multi-line
// word wrap + fallback resolution), not a one-line label.
const PARAGRAPH: &str = "Lumen is a cross-platform Rust UI framework: \
    ECS-driven layout, GPU-accelerated vector rendering, and \
    system-font text shaping - the quick brown fox jumps over the \
    lazy dog 0123456789 while packing my box with five dozen liquor jugs.";

fn wrapped_opts() -> ShapeOptions {
    ShapeOptions {
        wrap: WrapMode::Word,
        max_lines: None,
        width: Some(280.0),
        ..ShapeOptions::default()
    }
}

/// One glyph reduced to comparable integers: id, x, y and advance as raw
/// bits (floats have no total order), and the source byte it starts at.
type GlyphPrint = (u32, u32, u32, u32, u32);

/// Cheap structural fingerprint of a shaped run: its width plus one entry
/// per glyph. `ShapedRun` has no `PartialEq` (deriving it would make every
/// clone-on-cache-hit compare glyph vectors it never needs to), so tests
/// compare this instead.
fn fingerprint(run: &ShapedRun) -> (u32, Vec<GlyphPrint>) {
    let glyphs = run
        .glyphs
        .iter()
        .map(|g: &GlyphPosition| {
            (
                g.id,
                g.x.to_bits(),
                g.y.to_bits(),
                g.advance.to_bits(),
                g.byte_start,
            )
        })
        .collect();
    (run.width.to_bits(), glyphs)
}

#[test]
fn repeat_shape_is_a_genuine_cache_hit() {
    let mut shaper = CosmicShaper::new();
    let opts = wrapped_opts();
    let first = shaper
        .shape(LABEL, 16.0, opts.clone())
        .expect("label should shape");
    let after_first = shaper.cache_len();
    assert!(after_first >= 1, "shaping a new key must insert an entry");

    let second = shaper
        .shape(LABEL, 16.0, opts.clone())
        .expect("repeat label should shape");
    assert_eq!(
        shaper.cache_len(),
        after_first,
        "an identical second call must be a cache hit, not a second insert"
    );
    assert_eq!(
        fingerprint(&first),
        fingerprint(&second),
        "a cache hit must return the same shape as the original call"
    );
}

#[test]
fn different_text_produces_different_output_and_a_second_entry() {
    let mut shaper = CosmicShaper::new();
    let opts = wrapped_opts();
    let label_run = shaper.shape(LABEL, 16.0, opts.clone()).expect("label");
    let after_label = shaper.cache_len();
    let paragraph_run = shaper
        .shape(PARAGRAPH, 15.0, opts.clone())
        .expect("paragraph");
    assert!(
        shaper.cache_len() > after_label,
        "distinct text must miss the cache and insert a new entry"
    );
    assert_ne!(
        fingerprint(&label_run),
        fingerprint(&paragraph_run),
        "distinct text must produce distinct shaped output"
    );
}

#[test]
fn width_change_belongs_in_the_cache_key() {
    // Same text, same size, two widths far apart enough to land in
    // different width buckets and force a different wrap point.
    let mut shaper = CosmicShaper::new();
    let narrow = ShapeOptions {
        wrap: WrapMode::Word,
        width: Some(80.0),
        ..ShapeOptions::default()
    };
    let wide = ShapeOptions {
        wrap: WrapMode::Word,
        width: Some(280.0),
        ..ShapeOptions::default()
    };
    let at_narrow = shaper
        .shape(PARAGRAPH, 15.0, narrow)
        .expect("paragraph at narrow width");
    let before_wide = shaper.cache_len();
    let at_wide = shaper
        .shape(PARAGRAPH, 15.0, wide)
        .expect("paragraph at wide width");
    assert!(
        shaper.cache_len() > before_wide,
        "a different wrap width must miss the cache, not silently reuse \
         the narrow-width shape"
    );
    assert_ne!(
        fingerprint(&at_narrow),
        fingerprint(&at_wide),
        "a narrower wrap width must reflow the paragraph differently"
    );
}

#[test]
fn wrapped_paragraph_is_genuinely_multi_line_unlike_the_label() {
    // The paragraph fixture is only the "expensive multi-line" case the
    // module doc claims if it actually wraps to more than one line at a
    // width the label stays on one line at.
    let mut shaper = CosmicShaper::new();
    let (_, label_h) = shaper.measure(LABEL, 16.0, Some(280.0), WrapMode::Word, None);
    let (_, paragraph_h) = shaper.measure(PARAGRAPH, 15.0, Some(280.0), WrapMode::Word, None);
    assert!(
        paragraph_h > label_h * 1.5,
        "paragraph height ({paragraph_h}) should span multiple lines \
         versus the single-line label ({label_h})"
    );
}

#[test]
fn font_system_new_yields_a_shaper_that_can_shape() {
    // `CosmicShaper::new()` does the full system-font-directory scan
    // (the single largest Lumen cold-start phase). Without a timing
    // harness the only invariant worth protecting directly is that the
    // scan actually leaves the shaper usable afterwards.
    let mut shaper = CosmicShaper::new();
    assert!(
        shaper.shape(LABEL, 16.0, ShapeOptions::default()).is_some(),
        "a freshly constructed shaper must be able to shape text"
    );
}

/// A cache key that stops discriminating (e.g. a field dropped from the
/// key struct) turns every "hit" into a silent reshape: the result is
/// still correct, so nothing above catches it, and the only symptom is
/// that things get slower. That failure mode needs a timing signal -
/// but a ratio against a cold miss taken in the same run, not an
/// absolute duration, so the assertion doesn't care how fast the
/// machine is.
///
/// Stability: warms the cache up front so the timed samples don't pay
/// one-time setup cost, then compares the minimum of many samples on
/// each side. Scheduler noise on a loaded runner only ever adds time to
/// a sample, so the minimum is the one statistic it cannot inflate: the
/// fastest warm hit is a true LRU lookup plus an `Arc` clone, the
/// fastest cold miss still resolves fonts and lays text out, and the
/// order-of-magnitude gap between those holds on any machine while a
/// degraded cache (reshaping on every "hit") cannot produce it.
#[test]
fn warm_hit_is_at_least_an_order_of_magnitude_faster_than_a_cold_miss() {
    let mut shaper = CosmicShaper::new();
    let opts = wrapped_opts();
    let _ = shaper.shape(LABEL, 16.0, opts.clone());

    // Warm up: let allocator / branch-predictor state settle before
    // timing so the first sample isn't penalised for one-time cost.
    for _ in 0..50 {
        std::hint::black_box(shaper.shape(LABEL, 16.0, opts.clone()));
    }

    const SAMPLES: usize = 200;
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        std::hint::black_box(shaper.shape(LABEL, 16.0, opts.clone()));
        warm.push(start.elapsed());
    }

    // Distinct keys, well under the shaper's LRU capacity, so every one
    // of these is a genuine miss rather than an eviction-driven miss.
    let pool: Vec<String> = (0..SAMPLES)
        .map(|n| format!("cold miss corpus item {n}"))
        .collect();
    let mut cold = Vec::with_capacity(SAMPLES);
    for s in &pool {
        let start = Instant::now();
        std::hint::black_box(shaper.shape(s, 16.0, opts.clone()));
        cold.push(start.elapsed());
    }

    let warm_min = warm.iter().min().copied().unwrap();
    let cold_min = cold.iter().min().copied().unwrap();

    assert!(
        warm_min.saturating_mul(10) <= cold_min,
        "the fastest warm cache hit ({warm_min:?}) should be at least 10x \
         faster than the fastest cold miss ({cold_min:?}); if this fails \
         the shape cache key may have stopped discriminating and every \
         call is reshaping from scratch"
    );
    // Generous backstop far above the doc's <1us warm-hit target: this
    // only fires on a catastrophic regression, never on a loaded runner.
    assert!(
        warm_min < Duration::from_millis(5),
        "the fastest warm cache hit took {warm_min:?}, expected well under 1ms"
    );
}
