//! Regression tests for the standalone-`bevy_ecs` removal-event lifecycle.
//!
//! Lumen drives `bevy_ecs` as a library (no `bevy_app`), so nothing rotates
//! the double-buffered `RemovedComponentEvents` unless [`App::tick`] calls
//! `World::clear_trackers` itself. Two invariants are policed here:
//!
//! 1. The main world's removal buffers stay bounded across a long session of
//!    spawn/despawn churn (the leak fix), while same-tick `RemovedComponents`
//!    readers still observe this tick's removals (clear-trackers placement is
//!    correct: it runs AFTER the whole `Tick` schedule).
//! 2. A bulk `ChildOf` removal in one tick is fully drained by
//!    `roll_up_frame_dirty`, so `FrameDirty` does not stay raised on the idle
//!    ticks that follow (the `.count()` vs `.next()` drain fix).

use lumen_core::prelude::*;

#[derive(Component)]
struct Marker;

/// A `RemovedComponents<Marker>` reader installed into the `Tick` schedule,
/// recording how many `Marker` removals it observed across all ticks.
#[derive(Resource, Default)]
struct SeenRemovals(usize);

fn observe_removals(mut removed: RemovedComponents<Marker>, mut seen: ResMut<SeenRemovals>) {
    seen.0 += removed.read().count();
}

/// A producer that despawns a target entity exactly once, in an EARLIER stage
/// than the observer, to prove a same-tick cross-stage removal survives the
/// end-of-tick `clear_trackers`.
#[derive(Resource)]
struct Fire {
    target: Entity,
    armed: bool,
}

fn maybe_despawn(mut commands: Commands, mut fire: ResMut<Fire>) {
    if fire.armed {
        commands.entity(fire.target).despawn();
        fire.armed = false;
    }
}

/// Invariant 1a: hundreds of spawn+despawn cycles must NOT grow the main
/// world's `Marker` removal buffer. Without `clear_trackers`, every removal
/// accumulates forever and `world.removed::<Marker>()` would report ~N.
#[test]
fn removal_buffer_stays_bounded() {
    let mut app = App::new();
    const CYCLES: usize = 500;
    for _ in 0..CYCLES {
        let e = app.world.spawn(Marker).id();
        app.world.despawn(e);
        app.tick();
    }
    // bevy retains at most the current + previous frame of removals, and each
    // tick despawns exactly one `Marker`, so the live buffer holds a tiny,
    // constant number of entries - never the full CYCLES history.
    let live = app.world.removed::<Marker>().count();
    assert!(
        live <= 4,
        "removal buffer leaked: {live} live Marker removals after {CYCLES} tick cycles \
         (clear_trackers not rotating the buffer)"
    );
}

/// Invariant 1b: the end-of-tick `clear_trackers` runs AFTER the whole `Tick`
/// schedule, so a removal produced in an early stage (`Systems`) is still
/// visible to a `RemovedComponents` reader in a later stage (`A11ySync`) the
/// SAME tick. If clear-trackers were placed mid-schedule this would drop to 0.
#[test]
fn same_tick_removal_still_observed() {
    let mut app = App::new();
    app.world.insert_resource(SeenRemovals::default());
    let target = app.world.spawn(Marker).id();
    app.world.insert_resource(Fire {
        target,
        armed: false,
    });
    // Producer in Systems, observer in A11ySync (a strictly later stage).
    app.add_systems(TickStage::Systems, maybe_despawn);
    app.add_systems(TickStage::A11ySync, observe_removals);

    // Warm-up tick: nothing removed yet.
    app.tick();
    assert_eq!(
        app.world.resource::<SeenRemovals>().0,
        0,
        "observed a phantom removal before anything was despawned"
    );

    // Arm the producer; the despawn now happens mid-schedule.
    app.world.resource_mut::<Fire>().armed = true;
    app.tick();
    assert_eq!(
        app.world.resource::<SeenRemovals>().0,
        1,
        "same-tick cross-stage removal was lost - clear_trackers ran before the reader"
    );

    // A subsequent idle tick must not re-surface the already-drained removal.
    app.tick();
    assert_eq!(
        app.world.resource::<SeenRemovals>().0,
        1,
        "a drained removal re-appeared on a later tick (buffer not rotated)"
    );
}

/// Invariant 2: removing many `ChildOf` in a single tick must be fully drained
/// by `roll_up_frame_dirty`. With the buggy `.next()` drain, K-1 stale entries
/// linger and re-raise `FrameDirty` on later idle ticks; with `.count()` the
/// app parks immediately.
#[test]
fn bulk_childof_removal_lets_app_idle() {
    let mut app = App::new();
    const K: usize = 8;

    let parent = app.world.spawn(()).id();
    let children: Vec<Entity> = (0..K)
        .map(|_| app.world.spawn(ChildOf(parent)).id())
        .collect();

    // Settle: run a tick so the spawn churn is folded in.
    app.tick();

    // Bulk-remove every ChildOf in ONE tick by despawning all children.
    for c in &children {
        app.world.despawn(*c);
    }
    app.tick();
    // That tick legitimately repainted (children vanished).
    assert!(
        app.world.resource::<FrameDirty>().dirty,
        "the bulk-removal tick itself should be dirty"
    );

    // Now prove the app parks: on each idle tick we clear FrameDirty and assert
    // roll_up_frame_dirty does not re-raise it from leftover ChildOf entries.
    for i in 0..K + 2 {
        app.world.resource_mut::<FrameDirty>().dirty = false;
        app.tick();
        assert!(
            !app.world.resource::<FrameDirty>().dirty,
            "FrameDirty re-raised on idle tick {i} after a bulk ChildOf removal - \
             the removal reader is not fully drained each tick"
        );
    }
}
