//! Regression tests for `App::tick()`'s idle-tick fast path.
//!
//! Builds a representative headless app world - a shallow tree of styled
//! containers plus signal-bound text / toggle / slider leaves - settles
//! it, then asserts what "idle" was supposed to guarantee: with nothing
//! dirty, `App::tick()` never marks the frame dirty and never rewrites a
//! bound component whose signal didn't change. A complementary test
//! asserts the fast path isn't just permanently short-circuiting: a
//! single bound-signal write still reaches its component and still
//! marks the frame dirty.
//!
//! These used to be criterion benchmarks (never run under `cargo test`
//! or CI). No timing assertion here, unlike `shape_cache.rs`. The
//! regression class the original bench's doc comment called out - a
//! per-tick full-store rescan, a whole-map rebuild, an O(N) layout scan
//! creeping back in - is a real timing-only failure mode (deleting an
//! early-return still produces the same final component values, just
//! redundantly), but the work it would redundantly repeat here is a
//! handful of query iterations over ~40-160 entities: not the kind of
//! structurally-guaranteed, order-of-magnitude gap that font shaping vs.
//! an LRU lookup has in `shape_cache.rs`. A ratio or ceiling loose
//! enough to survive a busy shared runner would also be loose enough to
//! miss the regression it's meant to catch, so this file sticks to the
//! part that's directly, reliably observable: the `FrameDirty` state the
//! early-returns are gated on, and the component values they protect.

use bevy_ecs::hierarchy::ChildOf;
use lumen_core::prelude::*;
use lumen_layout_taffy::TaffyLayoutPlugin;

/// Number of container rows; each row holds a handful of leaves.
const ROWS: usize = 40;

/// Build the app world and register the per-tick systems the audit
/// touched. Returns the settled app plus the row-0 bound-text leaf's
/// entity id, so tests can inspect its `TextContent` directly.
fn build_app() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);

    // Reactive binding readers - the systems whose idle-tick behaviour
    // this file guards.
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
    // this tick's writes (and, when nothing changed, early-returns).
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

    let mut row0_text_entity = None;

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
        let text_leaf = app
            .world
            .spawn((
                Style::default(),
                TextContent(String::new()),
                TextStyle::default(),
                BindText::from(sig.as_str()),
                ChildOf(container),
            ))
            .id();
        if row == 0 {
            row0_text_entity = Some(text_leaf);
        }
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

    // Seed every bound signal once so the first settle-tick pulls it and
    // the steady state has the bindings converged (no divergence left to
    // re-pull on the ticks under test).
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

    // Settle: a handful of ticks drains every `Added`/`Changed` flag,
    // applies the initial bindings, lays out the tree, and clears the
    // dirty queue so subsequent ticks are genuinely idle.
    for _ in 0..8 {
        app.tick();
    }
    (
        app,
        row0_text_entity.expect("row 0 spawns a bound-text leaf"),
    )
}

#[test]
fn settling_reaches_genuine_quiescence() {
    // The other two tests only mean anything if "settled" really is
    // idle. `PropertyStore::dirty_peek` isn't useful here: the store's
    // own `clear_property_store_dirty` system empties it unconditionally
    // at the end of every tick, settled or not, so checking it
    // post-tick would pass even if settling never converged. `FrameDirty`
    // is the real signal - it is only raised by `Changed<T>` /
    // property-notify observations, never cleared by the tick loop
    // itself (that's the window backend's job after it submits a frame,
    // which this headless test never does).
    let (mut app, _row0) = build_app();

    // `FrameDirty` starts true so the first frame paints, and only the
    // window backend clears it once it has submitted one. A headless test
    // never submits, so clear it here to stand in for a consumed frame.
    // Without this the resource is unconditionally true and asserting on
    // it proves nothing.
    app.world
        .get_resource_mut::<FrameDirty>()
        .expect("FrameDirty resource")
        .dirty = false;

    // Several ticks, because convergence is the claim: one quiet tick
    // could be luck, a settled tree stays quiet.
    for _ in 0..8 {
        app.tick();
    }

    let frame_dirty = app
        .world
        .get_resource::<FrameDirty>()
        .expect("FrameDirty resource");
    assert!(
        !frame_dirty.dirty,
        "a settled app must stop requesting redraws once a frame is consumed"
    );
}

#[test]
fn idle_tick_does_no_dirty_work() {
    let (mut app, row0) = build_app();
    let text_before = app.world.get::<TextContent>(row0).unwrap().0.clone();

    // Stand in for the window backend consuming the frame; see
    // `settling_reaches_genuine_quiescence` for why this is needed.
    app.world
        .get_resource_mut::<FrameDirty>()
        .expect("FrameDirty resource")
        .dirty = false;

    app.tick();

    let frame_dirty = app.world.get_resource::<FrameDirty>().unwrap();
    assert!(
        !frame_dirty.dirty,
        "an idle tick must not mark the frame dirty - there is nothing to redraw"
    );
    let text_after = app.world.get::<TextContent>(row0).unwrap().0.clone();
    assert_eq!(
        text_before, text_after,
        "an idle tick must not rewrite a bound TextContent whose signal didn't change"
    );
}

#[test]
fn one_signal_write_produces_real_dirty_work() {
    // Complements the idle guard: prove the fast path isn't just
    // permanently short-circuiting. A single bound-signal change must
    // reach its `TextContent` and mark the frame dirty.
    let (mut app, row0) = build_app();

    if let Some(mut store) = app.world.get_resource_mut::<PropertyStore>() {
        store.set(
            PropertyKey::global("label_0"),
            PropertyValue::Str("updated".into()),
        );
    }
    app.tick();

    let text_after = app.world.get::<TextContent>(row0).unwrap().0.clone();
    assert_eq!(
        text_after, "updated",
        "a signal write must reach its bound TextContent"
    );
    let frame_dirty = app.world.get_resource::<FrameDirty>().unwrap();
    assert!(
        frame_dirty.dirty,
        "a tick that changed a bound property must mark the frame dirty"
    );
}
