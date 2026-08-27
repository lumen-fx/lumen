//! The portable app belongs to no thread.
//!
//! Non-send data is bound to the thread that inserted it. A renderer that
//! builds one app per request builds it on a worker, so a single non-send
//! resource anywhere in the assembly is a thread-affine value reached and
//! dropped somewhere it does not belong.
//!
//! bevy's own guard against that panics on the access that gets it wrong:
//! useful, but late, and it names only the one resource that happened to be
//! touched first. The check below looks at the storage directly, so a
//! portable app that picks up a non-send resource fails here, at the point
//! it was assembled, naming every offender at once instead of one panic per
//! test run.

use std::thread;

use bevy_ecs::component::ComponentId;
use bevy_ecs::world::World;
use lumen_core::prelude::App;
use lumen_portable::portable_app;

/// The image the build script compiled, a program that publishes a signal and
/// registers a derivation, so the host is carrying state of its own when the
/// app is dropped.
#[cfg(feature = "host-candela")]
const SMOKE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke.cdlb"));

/// Every non-send slot in `world` that something was put into.
///
/// A slot on its own means nothing: a system taking `Option<NonSendMut<T>>`
/// reserves one for a `T` that may never arrive, and an empty slot binds
/// nothing to any thread. Only a populated one does.
fn thread_bound(world: &World) -> Vec<ComponentId> {
    world
        .storages()
        .non_sends
        .iter()
        .filter(|(_, data)| data.is_present())
        .map(|(id, _)| id)
        .collect()
}

/// Build the app a request would, script and all.
fn booted() -> App {
    let mut app = portable_app();
    #[cfg(feature = "host-candela")]
    lumen_portable::hosts::install(&mut app, "candela", SMOKE, "smoke.cdlb")
        .expect("this build carries the candela host");
    app
}

#[test]
fn the_assembly_installs_no_thread_bound_resource() {
    let mut app = booted();
    app.tick();

    let main = thread_bound(&app.world);
    assert!(
        main.is_empty(),
        "a portable app is built and dropped wherever its caller runs, and \
         {} of its resources are bound to the thread that inserted them \
         ({main:?}); whatever was added last calls `insert_non_send`",
        main.len()
    );
    let render = thread_bound(&app.render_world);
    assert!(render.is_empty(), "in the render world: {render:?}");
}

/// The shape a renderer uses: hand a built app to a worker, run it there, let
/// it go there. This says the assembly survives the move at all; what it does
/// not say is that nothing in it is thread-affine, which is the test above.
#[test]
fn an_app_handed_to_another_thread_ticks_and_drops_there() {
    let mut app = booted();
    // Four ticks here, four there: the derivation the script registers is
    // computed on the tick after registration, so one tick would move an app
    // that had not yet run everything the assembly installs.
    for _ in 0..4 {
        app.tick();
    }
    thread::spawn(move || {
        for _ in 0..4 {
            app.tick();
        }
        drop(app);
    })
    .join()
    .expect("the app runs where it is put");
}
