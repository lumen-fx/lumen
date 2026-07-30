//! Text-shaping benchmarks for [`lumen_text_cosmic`].
//!
//! Three concerns, three groups:
//!
//! * `shape_cache/warm_hit` - LRU lookup + clone of an already-shaped
//!   run (target <1 us). This is the steady-state cost every re-layout
//!   of unchanged text pays.
//! * `shape_cache/cold_miss_*` - decode + shape + LRU insert on a fresh
//!   key. Two realistic shapes: a short label and a wrapped paragraph
//!   (multi-line, the expensive case). Throughput is reported in bytes
//!   so a regression in per-byte shaping cost is visible.
//! * `font_system_new` - [`CosmicShaper::new`], i.e. `FontSystem::new`'s
//!   scan of every system font directory. This is the single largest
//!   Lumen cold-start phase (see tools/startup-bench); tracking it here
//!   turns a startup regression into a criterion signal, not a one-off
//!   measurement. Sample size is lowered because each call is ~10-20 ms.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use lumen_text::{ShapeOptions, TextShaper, WrapMode};
use lumen_text_cosmic::CosmicShaper;

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

fn bench_shape(c: &mut Criterion) {
    let mut shaper = CosmicShaper::new();
    // Clone options once - the per-iteration clone in the old bench
    // added allocator noise to the sub-us warm-hit measurement.
    let opts = wrapped_opts();
    // Prime the warm entries once so the LRU has them.
    let _ = shaper.shape(LABEL, 16.0, opts.clone());
    let _ = shaper.shape(PARAGRAPH, 15.0, opts.clone());

    let mut group = c.benchmark_group("shape_cache");

    group.bench_function("warm_hit", |b| {
        b.iter(|| std::hint::black_box(shaper.shape(LABEL, 16.0, opts.clone())));
    });

    // Cold miss, short label. Unique keys are pre-generated so the
    // timed region measures shaping, not `format!` allocation. A pool
    // larger than the 512-entry LRU guarantees every shape is a miss.
    let pool: Vec<String> = (0..1024).map(|n| format!("Lumen label {n}")).collect();
    group.throughput(Throughput::Bytes(LABEL.len() as u64));
    group.bench_function("cold_miss_label", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let s = &pool[i % pool.len()];
            i += 1;
            std::hint::black_box(shaper.shape(s, 16.0, opts.clone()));
        });
    });

    // Cold miss, wrapped paragraph - the expensive multi-line path.
    let para_pool: Vec<String> = (0..512)
        .map(|n| format!("{PARAGRAPH} (variant {n})"))
        .collect();
    group.throughput(Throughput::Bytes(PARAGRAPH.len() as u64));
    group.bench_function("cold_miss_paragraph", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let s = &para_pool[i % para_pool.len()];
            i += 1;
            std::hint::black_box(shaper.shape(s, 15.0, opts.clone()));
        });
    });

    group.finish();
}

/// `CosmicShaper::new` = `FontSystem::new` = a full system-font-directory
/// scan. The dominant Lumen cold-start phase; guard it against regression.
fn bench_font_system_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("font_system_new");
    // ~10-20 ms/call - keep the wall time sane.
    group.sample_size(20);
    group.bench_function("cosmic_shaper_new", |b| {
        b.iter(|| std::hint::black_box(CosmicShaper::new()));
    });
    group.finish();
}

criterion_group!(benches, bench_shape, bench_font_system_new);
criterion_main!(benches);
