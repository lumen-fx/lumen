//! Cross-thread event bus for portable runtime plugins.
//!
//! A portable plugin (see `lumen-plugin`) pushes events at the engine from
//! whichever thread it likes: a worker delivering what it watched for, or a
//! call body handing work past its own return. The event crosses the plugin
//! boundary as encoded bytes, and this bus is where those bytes wait for the
//! tick that drains them.
//!
//! The core stores bytes rather than decoded events on purpose: the event
//! shape is built from script-surface types this crate does not know, and the
//! script layer that does know them registers the per-tick drain
//! (`lumen-script`'s `collect_plugin_events`). What lives here is the bus
//! itself, mirroring the external typed-property bus in
//! [`crate::property_store`], so [`crate::tick::work_pending`] can count
//! undrained events as pending work and a parked app wakes to take them.

use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender, unbounded};

static PLUGIN_EVENT_TX: OnceLock<Sender<Vec<u8>>> = OnceLock::new();
static PLUGIN_EVENT_RX: OnceLock<Mutex<Receiver<Vec<u8>>>> = OnceLock::new();

fn init_plugin_event_channel() -> &'static Sender<Vec<u8>> {
    PLUGIN_EVENT_TX.get_or_init(|| {
        let (tx, rx) = unbounded();
        let _ = PLUGIN_EVENT_RX.set(Mutex::new(rx));
        tx
    })
}

/// Idempotently initialises the plugin-event channel. Safe to call multiple
/// times.
pub fn init_plugin_events() {
    let _ = init_plugin_event_channel();
}

/// Queue one encoded plugin event from any thread. Picked up on the next
/// tick by the script layer's drain.
///
/// Returns `false` when the channel has disconnected.
pub fn push_plugin_event(bytes: Vec<u8>) -> bool {
    init_plugin_event_channel().send(bytes).is_ok()
}

/// Take every queued event, in arrival order. Returns the empty vector when
/// the channel was never initialised, is empty, or its lock is poisoned.
pub fn drain_plugin_events() -> Vec<Vec<u8>> {
    let Some(rx_lock) = PLUGIN_EVENT_RX.get() else {
        return Vec::new();
    };
    let Ok(rx) = rx_lock.lock() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    while let Ok(bytes) = rx.try_recv() {
        events.push(bytes);
    }
    events
}

/// Whether the bus currently holds undrained events.
///
/// Non-destructive; one of [`crate::tick::work_pending`]'s sources, so a
/// driver that only wakes on events schedules another tick while an event a
/// worker thread pushed is still sitting here.
pub fn plugin_events_pending() -> bool {
    PLUGIN_EVENT_RX
        .get()
        .and_then(|rx_lock| rx_lock.lock().ok().map(|rx| !rx.is_empty()))
        .unwrap_or(false)
}

/// Empties the bus, throwing away whatever it holds.
///
/// One channel per process, so a caller that runs several apps in sequence
/// calls this between them, beside
/// [`crate::property_store::discard_external_properties`].
pub fn discard_plugin_events() {
    let Some(rx_lock) = PLUGIN_EVENT_RX.get() else {
        return;
    };
    let Ok(rx) = rx_lock.lock() else {
        return;
    };
    while rx.try_recv().is_ok() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bus is one process-global channel, so its tests hold this to keep
    /// from draining each other's pushes.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn events_queue_report_pending_and_drain_in_order() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        assert!(!plugin_events_pending());
        assert!(push_plugin_event(vec![1]));
        assert!(push_plugin_event(vec![2, 3]));
        assert!(plugin_events_pending());
        assert_eq!(drain_plugin_events(), vec![vec![1], vec![2, 3]]);
        assert!(!plugin_events_pending());
        assert!(drain_plugin_events().is_empty());
    }

    #[test]
    fn discard_empties_the_bus() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        assert!(push_plugin_event(vec![9]));
        discard_plugin_events();
        assert!(!plugin_events_pending());
        assert!(drain_plugin_events().is_empty());
    }

    #[test]
    fn an_undrained_event_counts_as_pending_work() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        // A bare world carries no animation or frame-dirty resources, so with
        // an event queued the bus is what `work_pending` reports. Only the
        // monotonic direction is asserted against `work_pending` itself: the
        // sibling external-property bus is process-global too, and a parallel
        // test may hold a write in it while this one runs.
        let world = bevy_ecs::world::World::new();
        assert!(push_plugin_event(vec![7]));
        assert!(
            crate::tick::work_pending(&world),
            "a queued plugin event must report pending so a parked driver schedules a tick"
        );
        discard_plugin_events();
        assert!(!plugin_events_pending());
    }
}
