//! A caught panic in one tick must not poison the next.
//!
//! bevy's `schedule_scope` takes the running schedule out of the world and
//! only puts it back on a clean return, so a system panic used to leak the
//! `Tick` schedule out of the world: the panic itself was survivable (an
//! embedder's `catch_unwind`, a module's tick), but every later tick then
//! failed on "schedule not found". `App::tick` now restores the schedule on
//! unwind; these tests are the contract.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Resource, Default)]
struct Ticks(u32);

fn count(mut ticks: ResMut<Ticks>) {
    ticks.0 += 1;
}

fn explode_on_second(ticks: Res<Ticks>) {
    if ticks.0 == 2 {
        panic!("scripted panic at tick 2");
    }
}

#[test]
fn a_system_panic_leaves_later_ticks_running() {
    let mut app = App::new();
    app.world.init_resource::<Ticks>();
    app.add_systems(TickStage::Systems, (count, explode_on_second).chain());

    assert!(catch_unwind(AssertUnwindSafe(|| app.tick())).is_ok());

    let unwound = catch_unwind(AssertUnwindSafe(|| app.tick()));
    let payload = unwound.expect_err("the second tick's system panics");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(message.contains("scripted panic at tick 2"), "{message}");

    // The tick after the caught panic runs its systems again: the schedule
    // survived the unwind, and so did the world's state.
    assert!(catch_unwind(AssertUnwindSafe(|| app.tick())).is_ok());
    assert_eq!(app.world.resource::<Ticks>().0, 3);
}

#[derive(Resource, Default)]
struct Ran([u32; 4]);

fn ran<const N: usize>(mut ran: ResMut<Ran>) {
    ran.0[N] += 1;
}

#[derive(Resource, Default)]
struct Fired(bool);

fn explode_once(mut fired: ResMut<Fired>) {
    if !fired.0 {
        fired.0 = true;
        panic!("scripted panic while siblings are in flight");
    }
}

#[test]
fn recovery_installs_a_full_replacement_executor() {
    // `Schedule` exposes no getter for its executor, so the executor that
    // recovery installs cannot be inspected directly; what it is documented to
    // be is the same platform default a fresh schedule starts with. This
    // asserts the observable half of that contract: after a caught panic the
    // replacement executor is re-initialized from the whole schedule and runs
    // every system - including the unchained, parallel-eligible siblings of
    // the one that panicked - rather than replaying the dead run's progress
    // as a partial tick or a no-op.
    let mut app = App::new();
    app.world.init_resource::<Ran>();
    app.world.init_resource::<Fired>();
    app.add_systems(
        TickStage::Systems,
        (ran::<0>, ran::<1>, ran::<2>, ran::<3>, explode_once),
    );

    // The first tick panics partway through: the siblings the executor had
    // already reached ran once, the rest not at all.
    assert!(catch_unwind(AssertUnwindSafe(|| app.tick())).is_err());
    let after_panic = app.world.resource::<Ran>().0;

    // The next tick runs on the replacement executor. Every sibling advances
    // by exactly one, whatever the dead run had reached.
    assert!(catch_unwind(AssertUnwindSafe(|| app.tick())).is_ok());
    let after_recovery = app.world.resource::<Ran>().0;
    for (i, (before, after)) in after_panic.iter().zip(after_recovery.iter()).enumerate() {
        assert_eq!(
            *after,
            before + 1,
            "system {i} runs exactly once on the tick after recovery"
        );
    }
}

#[test]
fn a_missing_schedule_still_fails_fast() {
    // Parity with `World::run_schedule`: an app whose Tick schedule was never
    // registered is a wiring bug, and quietly ticking nothing would hide it.
    let mut app = App::new();
    app.world
        .resource_mut::<bevy_ecs::schedule::Schedules>()
        .remove(lumen_core::app::Tick);
    let outcome = catch_unwind(AssertUnwindSafe(|| app.tick()));
    assert!(outcome.is_err(), "a label without a schedule panics");
}
