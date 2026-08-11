//! Reactive wake: schedule a follow-up tick whenever the current tick
//! wrote typed properties that downstream consumers haven't observed
//! yet.
//!
//! ## Why this exists - "the dialog is slow"
//!
//! The app only ticks on wakes (input events, or the window backend's
//! `work_pending` re-arm). Several state chains need **one more tick**
//! after the tick that writes a [`PropertyStore`] cell before their
//! effect is visible:
//!
//! * a click handler writes `dialog_open` -> `reconcile_if_blocks`
//!   mounts / despawns the `<dialog>` body on the *next* tick,
//! * `push_slider_to_signal` mirrors a drag into its signal -> bound
//!   labels pull it on the next tick,
//! * a derivation writes a derived cell that another consumer reads.
//!
//! Without this system, that "next tick" never gets scheduled: the
//! window backend re-arms only when [`AnimationsActive`] or
//! `FrameDirty` says so, and a bare property write raises neither. The
//! transition then sat parked until an unrelated wake - measured live
//! on widget-garden: **~550 ms to open the dialog, ~4 s to close it**
//! (whenever the next incidental event happened to arrive).
//!
//! The fix: at the end of every tick (`A11ySync`, *before*
//! `clear_property_store_dirty` wipes the per-tick queue), raise
//! [`AnimationsActive`] when the dirty queue is non-empty. The window
//! backend's `work_pending` check then re-arms a redraw, the follow-up
//! tick runs within a frame, and the consumers converge. Quiescence is
//! preserved: semantically-equal writes never enter the dirty queue,
//! so a settled UI stops requesting ticks the moment no cell actually
//! changes.

use bevy_ecs::prelude::*;
use lumen_core::property_store::PropertyStore;
use lumen_core::render_world::AnimationsActive;

/// Raise [`AnimationsActive`] when this tick ends with undrained
/// property writes, so the write's downstream consumers (if/for
/// reconcilers, binding pulls, derivations-of-derivations) get their
/// follow-up tick within one frame instead of waiting for the next
/// unrelated input event.
///
/// Registered by [`crate::ControlsPlugin`] in `TickStage::A11ySync`,
/// ordered before `lumen_core::property_store::clear_property_store_dirty`
/// (which empties the queue at the very end of the tick).
pub fn request_tick_on_property_writes(
    store: Option<Res<PropertyStore>>,
    anim: Option<Res<AnimationsActive>>,
) {
    let (Some(store), Some(anim)) = (store, anim) else {
        return;
    };
    if !store.dirty_peek().is_empty() {
        anim.request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    fn world_with(store_dirty: bool) -> World {
        let mut world = World::new();
        let mut store = PropertyStore::default();
        if store_dirty {
            store.set_global_str("dialog_open", "1");
        }
        world.insert_resource(store);
        world.insert_resource(AnimationsActive::default());
        world
    }

    #[test]
    fn dirty_store_requests_a_follow_up_tick() {
        let mut world = world_with(true);
        world
            .run_system_once(request_tick_on_property_writes)
            .unwrap();
        assert!(
            world.resource::<AnimationsActive>().get(),
            "an undrained property write must schedule another tick"
        );
    }

    #[test]
    fn clean_store_stays_quiescent() {
        let mut world = world_with(false);
        world
            .run_system_once(request_tick_on_property_writes)
            .unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "no writes -> no wake -> the scheduler may park"
        );
    }

    #[test]
    fn equal_value_rewrite_does_not_spin() {
        // Semantically-equal writes skip the dirty push entirely, so a
        // steady-state system re-writing the same value every tick
        // cannot keep the app awake.
        let mut world = world_with(true);
        world.resource_mut::<PropertyStore>().clear_dirty();
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("dialog_open", "1"); // same value as before
        world
            .run_system_once(request_tick_on_property_writes)
            .unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "equal-value rewrite must not request a tick"
        );
    }
}
