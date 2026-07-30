//! Benchmarks [`Transition<f32>`] sampling across easing variants.
//!
//! - Linear, ease-out, and cubic-bezier sampling at a fixed midpoint.
//! - Cubic-bezier path runs Newton-Raphson with bisection fallback.
//! - Sub-microsecond per-sample target keeps hundreds of concurrent transitions within frame budget.

use criterion::{Criterion, criterion_group, criterion_main};
use lumen_primitives::{Easing, Transition};
use std::time::{Duration, Instant};

fn bench_sample(c: &mut Criterion) {
    let mut group = c.benchmark_group("transition_sample");
    let linear = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::Linear);
    let ease_out = Transition::<f32>::new(0.0, 1.0, Duration::from_millis(200), Easing::EaseOut);
    let bezier = Transition::<f32>::new(
        0.0,
        1.0,
        Duration::from_millis(200),
        Easing::CubicBezier(0.25, 0.1, 0.25, 1.0),
    );
    let now_mid = linear.start + Duration::from_millis(100);

    group.bench_function("linear", |b| {
        b.iter(|| std::hint::black_box(linear.sample(now_mid)));
    });
    group.bench_function("ease_out", |b| {
        b.iter(|| std::hint::black_box(ease_out.sample(now_mid)));
    });
    group.bench_function("cubic_bezier", |b| {
        b.iter(|| std::hint::black_box(bezier.sample(now_mid)));
    });
    group.bench_function("sample_at_now", |b| {
        b.iter(|| {
            let now = Instant::now();
            std::hint::black_box(linear.sample(now));
        });
    });
    group.finish();
}

criterion_group!(benches, bench_sample);
criterion_main!(benches);
