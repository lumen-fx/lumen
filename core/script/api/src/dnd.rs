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
//! `payload` is the drag's text: its `text/plain` value (the source's
//! `drag-payload`, or its `id` when that is empty), else the value of the
//! first kind it carries, so a typed `drag-type` payload and a file drop's
//! `text/uri-list` arrive too. The DOM drag events
//! ([`crate::dom_events`]) carry every kind; these two global functions are
//! the short form beside them, as `on_click` is beside `click`.
//!
//! Mirrors HTML5 `ondrop(event)` - `event.target` is the drop zone,
//! `event.dataTransfer.getData()` is the payload - and Qt's `dropEvent`
//! carrying the source + `QMimeData`.

use crate::ScriptHost;
use bevy_ecs::component::Mutable;
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumen_core::node::AppDom;
use lumen_core::warn_line;
use lumen_os_dnd::mime::MimePayload;
use lumen_os_dnd::{DragStarted, DropAccepted};

/// The text a global drag function receives: the `text/plain` value, else
/// the first kind's.
pub fn payload_text(payload: &MimePayload) -> String {
    payload
        .text()
        .or_else(|| {
            payload
                .kinds
                .first()
                .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        })
        .unwrap_or_default()
}

use crate::runtime::{ScriptCommandEvent, prefix, route_event_two_args};

/// Forward each [`DropAccepted`] to the script as
/// `on_drop(target_id, payload)`. A reactive `on_drop` that reassigns a
/// signal drives the next reconcile - no per-frame polling.
pub fn dispatch_drops_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    dom: Option<Res<AppDom>>,
    mut events: MessageReader<DropAccepted>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    let _dom = dom.as_deref().map(AppDom::enter);
    for ev in events.read() {
        let target_id = ids.get(ev.target).map(|i| i.0.as_str()).unwrap_or("");
        let payload = payload_text(&ev.payload);
        if let Err(e) =
            route_event_two_args(&mut *host, "drop", "on_drop", target_id, &payload, &mut out)
        {
            warn_line!("{}: on_drop failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward each [`DragStarted`] to the script as
/// `on_drag_start(source_id, payload)`.
pub fn dispatch_drag_start_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    dom: Option<Res<AppDom>>,
    mut events: MessageReader<DragStarted>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    let _dom = dom.as_deref().map(AppDom::enter);
    for ev in events.read() {
        let source_id = ids.get(ev.source).map(|i| i.0.as_str()).unwrap_or("");
        let payload = payload_text(&ev.payload);
        if let Err(e) = route_event_two_args(
            &mut *host,
            "drag_start",
            "on_drag_start",
            source_id,
            &payload,
            &mut out,
        ) {
            warn_line!("{}: on_drag_start failed: {e}", prefix(host.lang()));
        }
    }
}
