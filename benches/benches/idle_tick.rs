//! Idle-tick regression guard.
//!
//! Builds a representative headless app world - a shallow tree of styled
//! containers plus signal-bound text / toggle / slider leaves - settles it,
//! then criterion-measures a single `App::tick()` with **nothing dirty**.
//!
//! With no signal writes, no layout mutations, and no `Changed<T>` in flight,
//! a tick should do almost nothing: the per-tick binding readers, the typed
//! mirror, the derivation pass, and the layout sync all early-return on an
//! empty dirty set (efficiency-audit quick wins). This bench is the guard
//! that keeps those early-returns in place - a regression that reintroduces
//! a per-tick full-store rescan, whole-map rebuild, or O(N) layout scan
//! shows up here as a step change in the idle tick cost.
//!
//! Render is deliberately not exercised: with `FrameDirty` unset, `App::tick`
//! returns before extract/encode, so an idle tick never touches the GPU path
//! (there is none in this headless build anyway).

use bevy_ecs::hierarchy::ChildOf;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use lumen_core::prelude::*;
use lumen_layout_taffy::TaffyLayoutPlugin;

/// Number of container rows; each row holds a handful of leaves.
const ROWS: usize = 40;

/// Build the app world and register the per-tick systems the audit touched.
fn build_app() -> App {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);

    // Reactive binding readers - the systems whose idle cost this bench
    // guards. Registered the same way `lumenc::run` wires them.
    app.add_systems(TickStage::Systems, lumen_core::signals::apply_text_bindings);
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_checked_bindings,
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_value_bindings,
    );
    // Typed FFI mirror, ordered before the dirty-queue clear so it observes
    // this tick's writes (and, post-fix, early-returns when there are none).
    app.add_systems(
        TickStage::A11ySync,
        lumen_core::property_store::mirror_property_store_to_typed_cache
            .before(lumen_core::property_store::clear_property_store_dirty),
    );

    // A definite viewport so `sync_viewport` settles after the first tick
    // instead of re-dirtying every frame.
    if let Some(mut vp) = app.world.get_resource_mut::<Viewport>() {
        vp.size = glam::Vec2::new(1280.0, 800.0);
    }

    // Representative tree: a classed root, `ROWS` flex containers, and a
    // mix of plain + signal-bound leaves under each.
    let root = app
        .world
        .spawn((Style::default(), LumenClasses(vec!["app".into()])))
        .id();

    for row in 0..ROWS {
        let container = app.world.spawn((Style::default(), ChildOf(root))).id();

        // Plain text label.
        app.world.spawn((
            Style::default(),
            TextContent(format!("row {row}")),
            TextStyle::default(),
            ChildOf(container),
        ));
        // Signal-bound text leaf.
        let sig = format!("label_{row}");
        app.world.spawn((
            Style::default(),
            TextContent(String::new()),
            TextStyle::default(),
            BindText::from(sig.as_str()),
            ChildOf(container),
        ));
        // Signal-bound toggle + slider leaves.
        app.world.spawn((
            Style::default(),
            Toggleable::default(),
            BindChecked(format!("flag_{row}")),
            ChildOf(container),
        ));
        app.world.spawn((
            Style::default(),
            SliderValue {
                value: 0.0,
                min: 0.0,
                max: 100.0,
                step: None,
            },
            BindValue(format!("level_{row}")),
            ChildOf(container),
        ));
    }

    // Seed every bound signal once so the first settle-tick pulls it and the
    // steady state has the bindings converged (no divergence to re-pull).
    if let Some(mut store) = app.world.get_resource_mut::<PropertyStore>() {
        for row in 0..ROWS {
            store.set(
                PropertyKey::global(format!("label_{row}")),
                PropertyValue::Str(format!("value {row}").into()),
            );
            store.set(
                PropertyKey::global(format!("flag_{row}")),
                PropertyValue::Bool(row % 2 == 0),
            );
            store.set(
                PropertyKey::global(format!("level_{row}")),
                PropertyValue::F64(row as f64),
            );
        }
    }

    // Settle: a handful of ticks drains every `Added`/`Changed` flag, applies
    // the initial bindings, lays out the tree, and clears the dirty queue so
    // subsequent ticks are genuinely idle.
    for _ in 0..8 {
        app.tick();
    }
    app
}

/// Leaves spawned per row in [`build_app`] (text, bound text, toggle,
/// slider). Used to report a per-element throughput so the idle-tick
/// cost is comparable across tree-size changes.
const LEAVES_PER_ROW: usize = 4;

fn bench_idle_tick(c: &mut Criterion) {
    let mut app = build_app();
    let mut group = c.benchmark_group("idle_tick");
    group.throughput(Throughput::Elements((ROWS * LEAVES_PER_ROW) as u64));

    // Genuinely-idle tick: nothing dirty, every early-return armed.
    group.bench_function("settled", |b| {
        b.iter(|| {
            app.tick();
            std::hint::black_box(&app);
        });
    });

    // One-signal-dirty tick: complements the idle guard by pricing the
    // reactive write path - a single bound signal changes each tick, so
    // the binding readers, derivation, and downstream layout can't all
    // early-return. Guards against an O(N) fan-out creeping into the
    // "one thing changed" case that dominates real interactive frames.
    group.bench_function("one_signal_dirty", |b| {
        let mut n: i64 = 0;
        b.iter(|| {
            n = n.wrapping_add(1);
            if let Some(mut store) = app.world.get_resource_mut::<PropertyStore>() {
                store.set(
                    PropertyKey::global("label_0"),
                    PropertyValue::Str(format!("value {n}").into()),
                );
            }
            app.tick();
            std::hint::black_box(&app);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_idle_tick);
criterion_main!(benches);
