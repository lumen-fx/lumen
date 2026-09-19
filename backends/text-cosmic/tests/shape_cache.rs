//! Regression tests for `lumen-text-cosmic`'s shape-result LRU cache.
//!
//! These assert the cache's invariants directly: a repeat shape is a
//! cache hit that runs no shaping work, a novel shape is a miss, and an
//! input that belongs in the cache key changes the output. They are
//! `#[test]` functions rather than criterion benchmarks (which they once
//! were) because `cargo bench` is never run in CI, so nothing here would
//! ever have been exercised.
//!
//! The failure mode they exist for is a cache key that stops
//! discriminating: every "hit" becomes a silent reshape, results stay
//! correct, and the only symptom is that things get slower.
//! `CosmicShaper::shape_misses` turns that into a counter, so the tests
//! assert on shaping work done instead of on wall-clock time.
//! `warm_versus_cold_shape_timing` still reports the timings, on demand.

use lumen_text::{GlyphPosition, ShapeOptions, ShapedRun, TextShaper, WrapMode};
use lumen_text_cosmic::CosmicShaper;
use std::time::Instant;

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
    // `CosmicShaper::new()` loads the font database, through the
    // persistent font-metadata cache when that cache is warm. The
    // invariant worth protecting directly is that the load leaves the
    // shaper usable afterwards.
    let mut shaper = CosmicShaper::new();
    assert!(
        shaper.shape(LABEL, 16.0, ShapeOptions::default()).is_some(),
        "a freshly constructed shaper must be able to shape text"
    );
}

/// A repeat of an already-shaped input must not reach the cosmic-text
/// pipeline at all. This is the assertion that catches a cache key
/// which stopped discriminating: the shaped output would still be
/// correct, so every other test here passes, and the only trace is the
/// reshape counted on each call.
///
/// Asserts on the delta, never on an absolute miss count, so a future
/// internal shape during construction does not break it.
#[test]
fn a_warm_hit_runs_no_shaping_work() {
    let mut shaper = CosmicShaper::new();
    let opts = wrapped_opts();
    let _ = shaper.shape(LABEL, 16.0, opts.clone());
    let after_first = shaper.shape_misses();

    for _ in 0..64 {
        let _ = shaper.shape(LABEL, 16.0, opts.clone());
    }

    assert_eq!(
        shaper.shape_misses(),
        after_first,
        "64 repeats of an already-shaped input must all be cache hits; a \
         rising miss count means the key stopped discriminating and every \
         call is reshaping from scratch"
    );
}

/// The other direction, which is what proves the miss counter tracks
/// real work rather than sitting stuck: novel inputs each shape once,
/// and only once.
#[test]
fn each_novel_input_shapes_exactly_once() {
    // Far under `SHAPE_CACHE_CAP` (512 entries), so no eviction turns a
    // hit on the second pass back into a miss.
    const NOVEL: u64 = 32;

    let mut shaper = CosmicShaper::new();
    let opts = wrapped_opts();
    let before = shaper.shape_misses();
    for n in 0..NOVEL {
        let _ = shaper.shape(&format!("cold miss corpus item {n}"), 16.0, opts.clone());
    }
    assert_eq!(
        shaper.shape_misses(),
        before + NOVEL,
        "each distinct input must miss the cache exactly once"
    );

    let after_novel = shaper.shape_misses();
    for n in 0..NOVEL {
        let _ = shaper.shape(&format!("cold miss corpus item {n}"), 16.0, opts.clone());
    }
    assert_eq!(
        shaper.shape_misses(),
        after_novel,
        "a second pass over the same corpus must be all hits"
    );
}

/// Manual timing harness: `cargo test -p lumen-text-cosmic --release
/// --test shape_cache -- --ignored --nocapture`. Prints the minimum and
/// median of 200 warm hits against 200 cold misses.
///
/// It asserts nothing. The ratio is a measurement of the machine as much
/// as of the cache, and the invariant it used to stand in for is now
/// `a_warm_hit_runs_no_shaping_work`.
#[test]
#[ignore = "manual timing harness"]
fn warm_versus_cold_shape_timing() {
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

    warm.sort_unstable();
    cold.sort_unstable();
    println!(
        "warm hit: min {:?}, median {:?}\ncold miss: min {:?}, median {:?}",
        warm[0],
        warm[SAMPLES / 2],
        cold[0],
        cold[SAMPLES / 2]
    );
}
