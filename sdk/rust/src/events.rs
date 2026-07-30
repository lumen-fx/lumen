//! Native Rust event handlers.
//!
//! The script runtime (`lumen-script-rhai`) routes UI events to Rhai
//! functions through a per-id handler registry on `RhaiHost`. This module
//! is the Rust-closure equivalent: [`RustHandlers`] holds `FnMut`
//! closures keyed by `(event kind, element id)`, [`collect_ui_events`]
//! folds this tick's [`ClickEvent`] / [`DoubleClickEvent`] /
//! [`LongPressEvent`] messages into a queue, and
//! [`dispatch_rust_handlers`] drains the queue, calling each matching
//! closure with an [`EventCtx`] that offers typed signal access through
//! [`PropertyStore`].
//!
//! Both systems are registered by the [`crate::simple::AppBuilder`] via
//! [`lumenc::RunOptions::app_hooks`], ordered *before* the reactive
//! binding readers (`apply_text_bindings` et al.) so a signal written by
//! a handler is reflected by `bind-text="..."` markup on the very tick the
//! event fired - the same same-tick guarantee the Rhai path gets from
//! `commit_external_properties`.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumen_core::input::{ClickEvent, DoubleClickEvent, LongPressEvent};
use lumen_core::property_store::{Property, PropertyKey, PropertyStore, PropertyValue};
use lumen_core::tick::TickStage;
use std::collections::{HashMap, HashSet};

/// One deferred `(kind, id, handler)` registration, shared by the
/// ECS-first [`crate::App`] and the [`crate::simple::AppBuilder`]. A `None`
/// id is the wildcard slot.
pub(crate) type HandlerEntry = (EventKind, Option<String>, Handler);

/// The UI event class a handler is registered against.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum EventKind {
    /// Pointer press + release on the same element.
    Click,
    /// Two clicks within the double-click window. On a double-click tick
    /// the plain [`EventKind::Click`] for the same element is suppressed,
    /// matching the script runtime's "press twice fast = the double
    /// action, not the single action *and* the double" semantics.
    DoubleClick,
    /// Press held past the long-press threshold.
    LongPress,
}

/// A boxed native event handler. Receives an [`EventCtx`] scoped to the
/// firing event; mutate signals through it to drive `bind-text` /
/// `bind-checked` / `bind-value` markup reactively.
pub type Handler = Box<dyn FnMut(&mut EventCtx<'_>) + Send + Sync + 'static>;

/// Context handed to native event handlers.
///
/// Wraps the main ECS [`World`] for the duration of one handler call and
/// exposes typed get/set over the global signal namespace backed by
/// [`PropertyStore`]. Writes land in the store immediately (and push onto
/// its dirty queue), so binding readers running later in the same tick
/// observe them.
pub struct EventCtx<'w> {
    world: &'w mut World,
    target: &'w str,
    kind: EventKind,
}

impl EventCtx<'_> {
    /// The `id="..."` attribute of the element the event fired on. Empty
    /// when the element carries no id.
    pub fn target(&self) -> &str {
        self.target
    }

    /// Which event class fired. Useful for handlers registered against
    /// several kinds.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// Typed read of a global signal. Returns `None` when the signal was
    /// never set. Conversions follow [`PropertyValue`]'s lossy coercion
    /// rules (an `I64` cell read as `f64` converts, a `Str` cell read as
    /// `i64` parses, ...).
    pub fn get<T>(&self, name: &str) -> Option<T>
    where
        T: TryFrom<PropertyValue> + Into<PropertyValue> + Clone,
    {
        Property::<T>::new(name).get(self.world.resource::<PropertyStore>())
    }

    /// Typed read with a fallback for absent signals.
    pub fn get_or<T>(&self, name: &str, default: T) -> T
    where
        T: TryFrom<PropertyValue> + Into<PropertyValue> + Clone,
    {
        self.get(name).unwrap_or(default)
    }

    /// Typed write of a global signal. Accepts anything convertible into
    /// a [`PropertyValue`] (`i64`, `f64`, `bool`, `&str`, `String`,
    /// [`lumen_core::components::Color`], ...). The write is visible to
    /// `bind-*` markup on this same tick.
    pub fn set<T>(&mut self, name: &str, value: T)
    where
        T: Into<PropertyValue>,
    {
        self.world
            .resource_mut::<PropertyStore>()
            .set(PropertyKey::global(name), value.into());
    }

    /// Shared borrow of the raw [`PropertyStore`] for reads the typed
    /// helpers don't cover (entity-scoped keys, cell generations, ...).
    pub fn store(&self) -> &PropertyStore {
        self.world.resource::<PropertyStore>()
    }

    /// Mutable borrow of the raw [`PropertyStore`].
    pub fn store_mut(&mut self) -> Mut<'_, PropertyStore> {
        self.world.resource_mut::<PropertyStore>()
    }

    /// Escape hatch: the full main [`World`]. Spawn entities, touch any
    /// resource, or query components directly.
    pub fn world_mut(&mut self) -> &mut World {
        self.world
    }
}

/// Registry of native handlers keyed by `(kind, element id)`.
///
/// A `None` id is the wildcard slot: it fires for events whose element id
/// has no dedicated handler - mirroring the script runtime, where a
/// per-id `on("click", id, f)` registration overrides the global
/// `on_click(id)` fallback for that id.
#[derive(Resource, Default)]
pub(crate) struct RustHandlers {
    map: HashMap<(EventKind, Option<String>), Vec<Handler>>,
}

impl RustHandlers {
    /// Append a handler under `(kind, id)`; `None` id = wildcard.
    pub(crate) fn add(&mut self, kind: EventKind, id: Option<String>, handler: Handler) {
        self.map.entry((kind, id)).or_default().push(handler);
    }
}

/// This tick's `(kind, element id)` pairs awaiting native dispatch.
/// Filled by [`collect_ui_events`], drained by [`dispatch_rust_handlers`].
#[derive(Resource, Default)]
pub(crate) struct PendingUiEvents(Vec<(EventKind, String)>);

/// Fold this tick's pointer-derived messages into [`PendingUiEvents`].
///
/// Applies the same double-click suppression as the script dispatcher:
/// when a [`DoubleClickEvent`] fires for an entity, that entity's plain
/// [`ClickEvent`]s this tick are dropped so a double-click counts as
/// exactly one double, not two clicks plus a double.
pub(crate) fn collect_ui_events(
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    mut longs: MessageReader<LongPressEvent>,
    ids: Query<&LumenId>,
    mut queue: ResMut<PendingUiEvents>,
) {
    let id_of =
        |entity: Entity| -> String { ids.get(entity).map(|i| i.0.clone()).unwrap_or_default() };
    let double_targets: HashSet<Entity> = doubles.read().map(|ev| ev.entity).collect();
    for click in clicks.read() {
        if double_targets.contains(&click.entity) {
            continue;
        }
        queue.0.push((EventKind::Click, id_of(click.entity)));
    }
    for entity in double_targets {
        queue.0.push((EventKind::DoubleClick, id_of(entity)));
    }
    for press in longs.read() {
        queue.0.push((EventKind::LongPress, id_of(press.entity)));
    }
}

/// Drain [`PendingUiEvents`] and invoke matching [`RustHandlers`].
///
/// Exclusive system: handlers receive `&mut World` through [`EventCtx`],
/// so the registry is temporarily taken out of the world to keep the
/// borrow unique, then merged back (a handler may have inserted new
/// entries through [`EventCtx::world_mut`]).
///
/// Registered in `TickStage::Systems`, ordered before the reactive
/// binding readers so handler writes are reflected in-tick.
pub(crate) fn dispatch_rust_handlers(world: &mut World) {
    let events = std::mem::take(&mut world.resource_mut::<PendingUiEvents>().0);
    if events.is_empty() {
        return;
    }
    let mut handlers = std::mem::take(&mut world.resource_mut::<RustHandlers>().map);
    for (kind, target) in events {
        // Per-id handlers win; the wildcard runs only when no per-id
        // handler matched (mirrors the Rhai `on(event, id, fn)` router).
        let keyed = (kind, Some(target.clone()));
        let slot = if handlers.contains_key(&keyed) {
            keyed
        } else {
            (kind, None)
        };
        if let Some(list) = handlers.get_mut(&slot) {
            let mut ctx = EventCtx {
                world: &mut *world,
                target: &target,
                kind,
            };
            for handler in list.iter_mut() {
                handler(&mut ctx);
            }
        }
    }
    // Merge back rather than overwrite so registrations a handler made
    // through `world_mut()` during dispatch survive.
    let mut registry = world.resource_mut::<RustHandlers>();
    for (key, mut list) in handlers {
        registry.map.entry(key).or_default().append(&mut list);
    }
}

/// Install the native-handler dispatch pipeline onto a built ECS app.
///
/// Inserts the [`RustHandlers`] registry (seeded from `handlers`) and the
/// [`PendingUiEvents`] queue, then schedules [`collect_ui_events`] ->
/// [`dispatch_rust_handlers`] into [`TickStage::Systems`], ordered *before*
/// the reactive binding readers so a handler's signal write is reflected by
/// `bind-*` markup on the very tick the event fired.
///
/// Shared by both the ECS-first [`crate::App`] (its `on_click` / `on` family)
/// and the [`crate::simple::AppBuilder`] so the two surfaces dispatch through
/// exactly one code path. A no-op when `handlers` is empty - an app with no
/// native handlers pays for no extra systems.
pub(crate) fn install_rust_handlers(app: &mut lumen_core::app::App, handlers: Vec<HandlerEntry>) {
    if handlers.is_empty() {
        return;
    }
    let mut registry = RustHandlers::default();
    for (kind, id, handler) in handlers {
        registry.add(kind, id, handler);
    }
    app.world.insert_resource(registry);
    app.world.insert_resource(PendingUiEvents::default());
    app.add_systems(
        TickStage::Systems,
        collect_ui_events.before(dispatch_rust_handlers),
    );
    app.add_systems(
        TickStage::Systems,
        dispatch_rust_handlers
            .before(lumen_core::signals::apply_text_bindings)
            .before(lumen_core::signals::apply_checked_bindings)
            .before(lumen_core::signals::apply_value_bindings),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::tick::TickStage;

    /// Build a bare `lumen_core` App, wire the two systems the SDK
    /// installs, seed a click on an identified entity, and confirm the
    /// handler fires with same-tick PropertyStore visibility.
    #[test]
    fn click_routes_to_rust_handler_and_writes_store() {
        let mut app = lumen_core::app::App::new();
        let mut reg = RustHandlers::default();
        reg.add(
            EventKind::Click,
            Some("go".into()),
            Box::new(|ctx| {
                let n = ctx.get_or::<i64>("count", 0) + 1;
                ctx.set("count", n);
                assert_eq!(ctx.target(), "go");
                assert_eq!(ctx.kind(), EventKind::Click);
            }),
        );
        app.world.insert_resource(reg);
        app.world.insert_resource(PendingUiEvents::default());
        app.add_systems(
            TickStage::Systems,
            collect_ui_events.before(dispatch_rust_handlers),
        );
        app.add_systems(TickStage::Systems, dispatch_rust_handlers);

        let entity = app.world.spawn(LumenId("go".to_string())).id();
        app.world.write_message(ClickEvent {
            entity,
            position: glam_vec2_zero(),
            button: lumen_core::input::PointerButton::Primary,
        });
        app.tick();
        let store = app.world.resource::<PropertyStore>();
        assert_eq!(Property::<i64>::new("count").get(store), Some(1));
    }

    /// Wildcard handlers fire when no per-id handler matches; per-id
    /// registrations override the wildcard for their id.
    #[test]
    fn per_id_overrides_wildcard() {
        let mut app = lumen_core::app::App::new();
        let mut reg = RustHandlers::default();
        reg.add(
            EventKind::Click,
            None,
            Box::new(|ctx| ctx.set("hit", "wildcard")),
        );
        reg.add(
            EventKind::Click,
            Some("special".into()),
            Box::new(|ctx| ctx.set("hit", "special")),
        );
        app.world.insert_resource(reg);
        app.world.insert_resource(PendingUiEvents::default());
        app.add_systems(
            TickStage::Systems,
            collect_ui_events.before(dispatch_rust_handlers),
        );
        app.add_systems(TickStage::Systems, dispatch_rust_handlers);

        let special = app.world.spawn(LumenId("special".to_string())).id();
        app.world.write_message(ClickEvent {
            entity: special,
            position: glam_vec2_zero(),
            button: lumen_core::input::PointerButton::Primary,
        });
        app.tick();
        assert_eq!(
            app.world
                .resource::<PropertyStore>()
                .get_global_str("hit")
                .as_deref(),
            Some("special")
        );

        let plain = app.world.spawn_empty().id();
        app.world.write_message(ClickEvent {
            entity: plain,
            position: glam_vec2_zero(),
            button: lumen_core::input::PointerButton::Primary,
        });
        app.tick();
        assert_eq!(
            app.world
                .resource::<PropertyStore>()
                .get_global_str("hit")
                .as_deref(),
            Some("wildcard")
        );
    }

    fn glam_vec2_zero() -> glam::Vec2 {
        glam::Vec2::ZERO
    }
}
