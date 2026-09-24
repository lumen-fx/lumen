//! Cross-thread event bus for plugins, modules and add-ons.
//!
//! Something outside a script call pushes events at the engine from whichever
//! thread it likes: a worker delivering what it watched for, a call body
//! handing work past its own return, a promise settling in a page. This bus is
//! where those events wait for the tick that drains them.
//!
//! An event arrives in one of two forms. A portable plugin lives across a
//! serialized boundary, so its event is encoded bytes. Code that shares the
//! engine's address space (a runtime module, a browser add-on) hands over the
//! value itself, and nothing is encoded or decoded on its way through.
//!
//! The core knows neither shape: the event is built from script-surface types
//! this crate does not know, and the script layer that does know them
//! registers the per-tick drain (`lumen-script`'s `collect_plugin_events`).
//! What lives here is the bus itself, mirroring the external typed-property
//! bus in [`crate::property_store`], so [`crate::tick::work_pending`] can count
//! undrained events as pending work and a parked app wakes to take them.

use std::any::Any;
use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender, unbounded};

/// One event waiting on the bus, in the form it was pushed in.
pub enum QueuedEvent {
    /// Encoded by a plugin on the other side of a serialized boundary.
    Bytes(Vec<u8>),
    /// Handed over by code in the engine's own address space. The script
    /// layer that drains the bus knows the type to take it back as.
    Value(Box<dyn Any + Send>),
}

impl QueuedEvent {
    /// The encoded bytes, for an event pushed in that form.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            QueuedEvent::Bytes(bytes) => Some(bytes),
            QueuedEvent::Value(_) => None,
        }
    }
}

impl std::fmt::Debug for QueuedEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueuedEvent::Bytes(bytes) => f.debug_tuple("Bytes").field(bytes).finish(),
            QueuedEvent::Value(_) => f.write_str("Value(..)"),
        }
    }
}

static PLUGIN_EVENT_TX: OnceLock<Sender<QueuedEvent>> = OnceLock::new();
static PLUGIN_EVENT_RX: OnceLock<Mutex<Receiver<QueuedEvent>>> = OnceLock::new();
static PLUGIN_EVENT_WAKER: Mutex<Option<crate::app::EventLoopWaker>> = Mutex::new(None);

fn init_plugin_event_channel() -> &'static Sender<QueuedEvent> {
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

/// Install the waker a push nudges, so an event arriving while the app sits
/// parked in its event loop's `Wait` state triggers the tick that drains it
/// instead of waiting for an unrelated wake. The backends install it beside
/// the [`crate::app::EventLoopWaker`] resource; a later install replaces the
/// earlier one, because a process can run apps in sequence and the bus
/// belongs to whichever loop is live.
pub fn set_plugin_event_waker(waker: crate::app::EventLoopWaker) {
    if let Ok(mut slot) = PLUGIN_EVENT_WAKER.lock() {
        *slot = Some(waker);
    }
}

/// Queue one encoded plugin event from any thread. Picked up on the next
/// tick by the script layer's drain; a parked event loop is woken to run
/// that tick when a waker is installed ([`set_plugin_event_waker`]).
///
/// Returns `false` when the channel has disconnected.
pub fn push_plugin_event(bytes: Vec<u8>) -> bool {
    push(QueuedEvent::Bytes(bytes))
}

/// Queue one event as the value itself, from any thread, for code that shares
/// the engine's address space. Delivered and woken for exactly like
/// [`push_plugin_event`]; the drain takes it back as the type it was pushed
/// as.
///
/// Returns `false` when the channel has disconnected.
pub fn push_plugin_value(value: Box<dyn Any + Send>) -> bool {
    push(QueuedEvent::Value(value))
}

fn push(event: QueuedEvent) -> bool {
    let sent = init_plugin_event_channel().send(event).is_ok();
    if sent
        && let Ok(slot) = PLUGIN_EVENT_WAKER.lock()
        && let Some(waker) = slot.as_ref()
    {
        waker.wake();
    }
    sent
}

/// Take every queued event, in arrival order. Returns the empty vector when
/// the channel was never initialised, is empty, or its lock is poisoned.
pub fn drain_plugin_events() -> Vec<QueuedEvent> {
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

    /// The encoded events of a drain, in order.
    fn bytes_of(events: Vec<QueuedEvent>) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|event| event.bytes().map(<[u8]>::to_vec))
            .collect()
    }

    #[test]
    fn an_event_says_which_form_it_travels_in() {
        let bytes = QueuedEvent::Bytes(vec![7, 8]);
        let value = QueuedEvent::Value(Box::new(3_u32));
        assert_eq!(bytes.bytes(), Some(&[7, 8][..]));
        assert_eq!(value.bytes(), None);
        assert_eq!(format!("{bytes:?}"), "Bytes([7, 8])");
        assert_eq!(format!("{value:?}"), "Value(..)");
    }

    #[test]
    fn a_value_travels_as_itself_beside_the_bytes() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        assert!(push_plugin_event(vec![1]));
        assert!(push_plugin_value(Box::new(String::from("typed"))));
        assert!(plugin_events_pending());
        let mut drained = drain_plugin_events().into_iter();
        assert_eq!(
            drained.next().and_then(|e| e.bytes().map(<[u8]>::to_vec)),
            Some(vec![1])
        );
        match drained.next() {
            Some(QueuedEvent::Value(value)) => {
                assert_eq!(
                    value
                        .downcast::<String>()
                        .ok()
                        .as_deref()
                        .map(String::as_str),
                    Some("typed")
                );
            }
            other => panic!("expected the value back, got {other:?}"),
        }
        assert!(drained.next().is_none());
    }

    #[test]
    fn events_queue_report_pending_and_drain_in_order() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        assert!(!plugin_events_pending());
        assert!(push_plugin_event(vec![1]));
        assert!(push_plugin_event(vec![2, 3]));
        assert!(plugin_events_pending());
        assert_eq!(bytes_of(drain_plugin_events()), vec![vec![1], vec![2, 3]]);
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
    fn a_push_wakes_a_parked_loop() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_plugin_events();
        // Stand-in for a backend's parked `Wait` state: a thread blocks on a
        // condvar until the waker fires, then drains the bus - the same
        // wake-then-tick sequence the real loops run.
        let parked = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let waker_side = std::sync::Arc::clone(&parked);
        set_plugin_event_waker(crate::app::EventLoopWaker(std::sync::Arc::new(move || {
            let (flag, cv) = &*waker_side;
            *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        })));

        let loop_side = std::sync::Arc::clone(&parked);
        let parked_loop = std::thread::spawn(move || {
            let (flag, cv) = &*loop_side;
            let mut woken = flag.lock().unwrap_or_else(|e| e.into_inner());
            while !*woken {
                let (g, timed_out) = cv
                    .wait_timeout(woken, std::time::Duration::from_secs(10))
                    .unwrap_or_else(|e| e.into_inner());
                woken = g;
                assert!(!timed_out.timed_out(), "the push never woke the loop");
            }
            drain_plugin_events()
        });

        assert!(push_plugin_event(vec![5]));
        let delivered = parked_loop.join().expect("parked loop");
        assert_eq!(bytes_of(delivered), vec![vec![5]]);

        // Put the slot back so later tests see the uninstalled state.
        if let Ok(mut slot) = PLUGIN_EVENT_WAKER.lock() {
            *slot = None;
        }
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
