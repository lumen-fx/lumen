//! Bevy-style run-condition adapters over Lumen's message and signal streams.
//!
//! Each returns a closure usable as a `bevy_ecs` run condition, so a system can
//! be gated declaratively:
//!
//! ```
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! fn reset(mut signals: Signals) {
//!     signals.set("count", 0i64);
//! }
//!
//! # let app = lumenui::App::from_markup("<root/>");
//! app.add_systems(TickStage::Systems, reset.run_if(on_click("reset")));
//! ```
//!
//! Conditions run inside the same [`TickStage`](lumen_core::tick::TickStage) as
//! their gated system. A system [`App::add_systems`](crate::App::add_systems)
//! puts in [`TickStage::Systems`](lumen_core::tick::TickStage::Systems) runs
//! after the tick's clicks exist, so [`on_click`] and [`on_toggle`] see them
//! on the tick they happen; a click counts for an id when it lands on that
//! element or inside it. [`on_change`] observes the [`PropertyStore`] dirty
//! queue, which holds writes committed earlier in the tick and is cleared at
//! the end of it: a signal a peer system writes later in the same stage is
//! observed on the following tick.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core};

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageReader;
use bevy_ecs::system::{Query, Res};
use lumen_core::components::{LumenId, Toggleable};
use lumen_core::input::ClickEvent;
use lumen_core::property_store::PropertyStore;

/// Whether `entity` is the element `id`, or inside it.
fn within(entity: Entity, id: &str, ids: &Query<&LumenId>, parents: &Query<&ChildOf>) -> bool {
    let mut at = Some(entity);
    while let Some(e) = at {
        if ids.get(e).is_ok_and(|l| l.0 == id) {
            return true;
        }
        at = parents.get(e).ok().map(ChildOf::parent);
    }
    false
}

/// Run condition: true on ticks where a [`ClickEvent`] landed on the element
/// whose `id="..."` equals `id`, or on anything inside it.
#[allow(clippy::type_complexity)]
pub fn on_click(
    id: impl Into<String>,
) -> impl FnMut(MessageReader<ClickEvent>, Query<&LumenId>, Query<&ChildOf>) -> bool + Clone {
    let id = id.into();
    move |mut clicks: MessageReader<ClickEvent>, ids: Query<&LumenId>, parents: Query<&ChildOf>| {
        clicks.read().any(|c| within(c.entity, &id, &ids, &parents))
    }
}

/// Run condition: true on ticks where a [`ClickEvent`] landed on a
/// [`Toggleable`] element whose `id="..."` equals `id`, or inside it - the
/// user toggled that control.
#[allow(clippy::type_complexity)]
pub fn on_toggle(
    id: impl Into<String>,
) -> impl FnMut(
    MessageReader<ClickEvent>,
    Query<&LumenId>,
    Query<&ChildOf>,
    Query<(), bevy_ecs::query::With<Toggleable>>,
) -> bool
+ Clone {
    let id = id.into();
    move |mut clicks: MessageReader<ClickEvent>,
          ids: Query<&LumenId>,
          parents: Query<&ChildOf>,
          toggles: Query<(), bevy_ecs::query::With<Toggleable>>| {
        // The control a click flips is the nearest toggleable element at or
        // above where it landed.
        clicks.read().any(|c| {
            let mut at = Some(c.entity);
            while let Some(e) = at {
                if toggles.get(e).is_ok() {
                    return ids.get(e).is_ok_and(|l| l.0 == id);
                }
                at = parents.get(e).ok().map(ChildOf::parent);
            }
            false
        })
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
