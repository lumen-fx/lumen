//! Script-facing drag-and-drop events.
//!
//! Surfaces the in-app DnD pipeline (`lumen-os-dnd`) to app scripts the
//! same way pointer events reach them - one uniform host seam, not a
//! Rhai-only builtin, so every binding / SDK gets it (like `page()` /
//! `http()`):
//!
//! * [`DropAccepted`] -> `on_drop(target_id, payload)` with per-id routing
//!   via `on("drop", "<target_id>", "<fn>")`.
//! * [`DragStarted`] -> `on_drag_start(source_id, payload)` with per-id
//!   routing via `on("drag_start", "<source_id>", "<fn>")`.
//!
//! `payload` is the source's text payload (its `drag-payload` attr, or
//! its `id` when the payload was derived from the element id).
//!
//! Mirrors HTML5 `ondrop(event)` - `event.target` is the drop zone,
//! `event.dataTransfer.getData()` is the payload - and Qt's `dropEvent`
//! carrying the source + `QMimeData`. Kept in its own submodule so the
//! shared `lib.rs` touch stays to two `add_systems` lines.

use crate::ScriptHost;
use bevy_ecs::component::Mutable;
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumen_os_dnd::{DragStarted, DropAccepted};

use crate::runtime::{ScriptCommandEvent, prefix, route_event_two_args};

/// Forward each [`DropAccepted`] to the script as
/// `on_drop(target_id, payload)`. A reactive `on_drop` that reassigns a
/// signal drives the next reconcile - no per-frame polling.
pub fn dispatch_drops_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<DropAccepted>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let target_id = ids.get(ev.target).map(|i| i.0.as_str()).unwrap_or("");
        let payload = ev.payload.text().unwrap_or_default();
        if let Err(e) =
            route_event_two_args(&mut *host, "drop", "on_drop", target_id, &payload, &mut out)
        {
            eprintln!("{}: on_drop failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward each [`DragStarted`] to the script as
/// `on_drag_start(source_id, payload)`.
pub fn dispatch_drag_start_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<DragStarted>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let source_id = ids.get(ev.source).map(|i| i.0.as_str()).unwrap_or("");
        let payload = ev.payload.text().unwrap_or_default();
        if let Err(e) = route_event_two_args(
            &mut *host,
            "drag_start",
            "on_drag_start",
            source_id,
            &payload,
            &mut out,
        ) {
            eprintln!("{}: on_drag_start failed: {e}", prefix(host.lang()));
        }
    }
}
