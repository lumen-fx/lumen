//! Bevy-style run-condition adapters over Lumen's message and signal streams.
//!
//! Each returns a closure usable as a `bevy_ecs` run condition, so a system can
//! be gated declaratively:
//!
//! ```
//! use lumen::prelude::*;
//!
//! fn reset(mut signals: Signals) {
//!     signals.set("count", 0i64);
//! }
//!
//! # let mut app = lumen::App::new();
//! app.add_systems(TickStage::Systems, reset.run_if(on_click("reset")));
//! ```
//!
//! Conditions run inside the same [`TickStage`](lumen_core::tick::TickStage) as
//! their gated system. [`on_change`] observes the [`PropertyStore`] dirty queue,
//! which is populated by writes committed *earlier* in the tick (input drain,
//! external bus) and cleared at end of tick - a signal a peer system writes
//! later in the same stage is observed on the following tick.

use bevy_ecs::message::MessageReader;
use bevy_ecs::system::{Query, Res};
use lumen_core::components::{LumenId, Toggleable};
use lumen_core::input::ClickEvent;
use lumen_core::property_store::PropertyStore;

/// Run condition: true on ticks where a [`ClickEvent`] targeted the element
/// whose `id="..."` equals `id`.
pub fn on_click(
    id: impl Into<String>,
) -> impl FnMut(MessageReader<ClickEvent>, Query<&LumenId>) -> bool + Clone {
    let id = id.into();
    move |mut clicks: MessageReader<ClickEvent>, ids: Query<&LumenId>| {
        clicks
            .read()
            .any(|c| ids.get(c.entity).map(|l| l.0 == id).unwrap_or(false))
    }
}

/// Run condition: true on ticks where a [`ClickEvent`] targeted a
/// [`Toggleable`] element whose `id="..."` equals `id` - i.e. the user toggled
/// that control.
pub fn on_toggle(
    id: impl Into<String>,
) -> impl FnMut(MessageReader<ClickEvent>, Query<(&LumenId, &Toggleable)>) -> bool + Clone {
    let id = id.into();
    move |mut clicks: MessageReader<ClickEvent>, q: Query<(&LumenId, &Toggleable)>| {
        clicks
            .read()
            .any(|c| q.get(c.entity).map(|(l, _)| l.0 == id).unwrap_or(false))
    }
}

/// Run condition: true on ticks where the global signal `name` was written
/// (its key is on the [`PropertyStore`] dirty queue).
///
/// See the module note on ordering: a same-stage peer's write lands on the next
/// tick's dirty queue, so gate reactive work on inputs drained before
/// [`TickStage::Systems`](lumen_core::tick::TickStage::Systems) for same-tick
/// response.
pub fn on_change(name: impl Into<String>) -> impl FnMut(Res<PropertyStore>) -> bool + Clone {
    let name = name.into();
    move |store: Res<PropertyStore>| store.dirty_global_names().any(|n| n == name)
}
