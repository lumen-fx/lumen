//! Typed reactive property store with notify-on-write semantics.
//!
//! - [`PropertyStore`] is a [`Resource`] holding a [`HashMap`] of [`PropertyKey`] -> [`PropertyCell`].
//! - [`PropertyStore::set`] writes the cell, bumps its generation, and records the key onto the per-tick dirty queue.
//! - The per-tick dirty queue drives the observer systems: they read it with [`PropertyStore::dirty_peek`] each tick, and the end-of-tick `clear_property_store_dirty` system resets it. [`PropertyStore::drain_dirty`] is the consuming variant for callers that want the entries.
//! - [`PropertyStore::freeze_notify`] / [`PropertyStore::thaw_notify`] gate the dirty queue across batched writes (e.g. `<for>` reconciler rewriting 1000 rows).
//!
//! The legacy [`crate::signals::Signals`] resource is a thin wrapper over this store keyed on `PropertyKey::Global(name)`.

use crate::components::Color;
use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use glam::Vec2;
use smallvec::SmallVec;
use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, OnceLock};

/// Identifier for an observer registered against a [`PropertyKey`]. Newtype-wrapped `u64`.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct ListenerId(pub u64);

/// Identifier for a one-way / two-way [`PropertyStore::bind`] binding. Newtype-wrapped `u64`.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct BindingId(pub u64);

/// Stable key identifying a property cell.
///
/// - `Global(name)`: the formerly-`Signals[name]` namespace.
/// - `Entity(e, name)`: an entity-scoped property (QML's `foo.text` analogue) - wave 1 wires it.
#[derive(Clone, Debug)]
pub enum PropertyKey {
    /// Global, name-keyed property - replaces `Signals[name]`.
    Global(Arc<str>),
    /// Entity-scoped property; the name is the [`crate::traits::Bindable::NAME`] of the bindable component.
    Entity(Entity, Arc<str>),
}

impl PartialEq for PropertyKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PropertyKey::Global(a), PropertyKey::Global(b)) => **a == **b,
            (PropertyKey::Entity(ea, na), PropertyKey::Entity(eb, nb)) => ea == eb && **na == **nb,
            _ => false,
        }
    }
}

impl Eq for PropertyKey {}

impl std::hash::Hash for PropertyKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            PropertyKey::Global(name) => {
                0u8.hash(state);
                (**name).hash(state);
            }
            PropertyKey::Entity(e, name) => {
                1u8.hash(state);
                e.hash(state);
                (**name).hash(state);
            }
        }
    }
}

impl PropertyKey {
    /// Returns a new `Global` key from any string-like input.
    pub fn global(name: impl Into<Arc<str>>) -> Self {
        PropertyKey::Global(name.into())
    }

    /// Returns a new `Entity`-scoped key.
    pub fn entity(e: Entity, name: impl Into<Arc<str>>) -> Self {
        PropertyKey::Entity(e, name.into())
    }
}

/// Typed property value. The `Custom` variant covers Rust types not enumerated here.
#[derive(Clone)]
pub enum PropertyValue {
    /// Boolean payload.
    Bool(bool),
    /// Signed 64-bit integer payload.
    I64(i64),
    /// 64-bit float payload.
    F64(f64),
    /// Shared string payload.
    Str(Arc<str>),
    /// RGBA color payload.
    Color(Color),
    /// 2-vector payload (typically a logical-pixel position or size).
    Vec2(Vec2),
    /// Escape hatch for types not covered by the enumerated variants. The inner `Arc` is shared cheaply across clones.
    Custom(Arc<dyn Any + Send + Sync>),
}

impl std::fmt::Debug for PropertyValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropertyValue::Bool(b) => f.debug_tuple("Bool").field(b).finish(),
            PropertyValue::I64(n) => f.debug_tuple("I64").field(n).finish(),
            PropertyValue::F64(n) => f.debug_tuple("F64").field(n).finish(),
            PropertyValue::Str(s) => f.debug_tuple("Str").field(s).finish(),
            PropertyValue::Color(c) => f.debug_tuple("Color").field(c).finish(),
            PropertyValue::Vec2(v) => f.debug_tuple("Vec2").field(v).finish(),
            PropertyValue::Custom(_) => f.debug_tuple("Custom").field(&"<dyn Any>").finish(),
        }
    }
}

impl PropertyValue {
    /// Structural equality across enumerated variants.
    ///
    /// `Custom` is treated as never-equal - there is no general equality across `dyn Any`. Treating it as always-changed
    /// is the safe conservative choice for the dirty queue (a redundant notify is fine; a missed notify is not).
    pub fn eq_value(&self, other: &Self) -> bool {
        match (self, other) {
            (PropertyValue::Bool(a), PropertyValue::Bool(b)) => a == b,
            (PropertyValue::I64(a), PropertyValue::I64(b)) => a == b,
            (PropertyValue::F64(a), PropertyValue::F64(b)) => a.to_bits() == b.to_bits(),
            (PropertyValue::Str(a), PropertyValue::Str(b)) => **a == **b,
            (PropertyValue::Color(a), PropertyValue::Color(b)) => a == b,
            (PropertyValue::Vec2(a), PropertyValue::Vec2(b)) => a == b,
            _ => false,
        }
    }
}

// --- Conversion impls so `Bindable::Value: Into<PropertyValue> + From<PropertyValue>` works for the common types.

impl From<bool> for PropertyValue {
    fn from(v: bool) -> Self {
        PropertyValue::Bool(v)
    }
}
impl From<i64> for PropertyValue {
    fn from(v: i64) -> Self {
        PropertyValue::I64(v)
    }
}
impl From<f64> for PropertyValue {
    fn from(v: f64) -> Self {
        PropertyValue::F64(v)
    }
}
impl From<f32> for PropertyValue {
    fn from(v: f32) -> Self {
        PropertyValue::F64(v as f64)
    }
}
impl From<Arc<str>> for PropertyValue {
    fn from(v: Arc<str>) -> Self {
        PropertyValue::Str(v)
    }
}
impl From<String> for PropertyValue {
    fn from(v: String) -> Self {
        PropertyValue::Str(v.into())
    }
}
impl From<&str> for PropertyValue {
    fn from(v: &str) -> Self {
        PropertyValue::Str(v.into())
    }
}
impl From<Color> for PropertyValue {
    fn from(v: Color) -> Self {
        PropertyValue::Color(v)
    }
}
impl From<Vec2> for PropertyValue {
    fn from(v: Vec2) -> Self {
        PropertyValue::Vec2(v)
    }
}

/// Inverse impls - used by [`crate::traits::Bindable::write`] to receive a typed value from the store.
///
/// These fall back to the type's `Default` when the stored value has the wrong variant. Callers who want strict typing
/// should read the cell directly and pattern-match.
impl From<PropertyValue> for bool {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::Bool(b) => b,
            PropertyValue::I64(n) => n != 0,
            PropertyValue::Str(s) => matches!(&*s, "true" | "1"),
            _ => false,
        }
    }
}

impl From<PropertyValue> for i64 {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::I64(n) => n,
            PropertyValue::F64(n) => n as i64,
            PropertyValue::Bool(b) => b as i64,
            PropertyValue::Str(s) => s.parse().unwrap_or_default(),
            _ => 0,
        }
    }
}

impl From<PropertyValue> for f64 {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::F64(n) => n,
            PropertyValue::I64(n) => n as f64,
            PropertyValue::Bool(b) => b as i64 as f64,
            PropertyValue::Str(s) => s.parse().unwrap_or_default(),
            _ => 0.0,
        }
    }
}

impl From<PropertyValue> for f32 {
    fn from(v: PropertyValue) -> Self {
        f64::from(v) as f32
    }
}

impl From<PropertyValue> for Arc<str> {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::Str(s) => s,
            PropertyValue::Bool(b) => if b { "true" } else { "false" }.into(),
            PropertyValue::I64(n) => n.to_string().into(),
            PropertyValue::F64(n) => n.to_string().into(),
            PropertyValue::Color(_) | PropertyValue::Vec2(_) | PropertyValue::Custom(_) => {
                "".into()
            }
        }
    }
}

impl From<PropertyValue> for String {
    fn from(v: PropertyValue) -> Self {
        Arc::<str>::from(v).to_string()
    }
}

impl From<PropertyValue> for Color {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::Color(c) => c,
            _ => Color::default(),
        }
    }
}

impl From<PropertyValue> for Vec2 {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::Vec2(v) => v,
            _ => Vec2::ZERO,
        }
    }
}

/// One stored property: the typed value, optional binding source, registered listeners, and a monotonic generation counter.
#[derive(Debug)]
pub struct PropertyCell {
    /// Current value.
    pub value: PropertyValue,
    /// Listener handles fanned out on each [`PropertyStore::set`].
    pub listeners: SmallVec<[ListenerId; 4]>,
    /// Optional one-way binding source. Populated by [`PropertyStore::bind`]; wave 1 wires the propagation system.
    pub binding: Option<BindingId>,
    /// Monotonic write counter; bumped on every successful `set`.
    pub generation: u64,
}

/// Reactive typed property store. Replaces the legacy `Signals: HashMap<String, String>` with a notify-on-write store.
///
/// Read [`crate::signals::Signals`] for backward-compatible callers; new code should use this store directly.
#[derive(Resource, Default, Debug)]
pub struct PropertyStore {
    /// All stored cells.
    cells: HashMap<PropertyKey, PropertyCell>,
    /// Per-tick dirty queue; drained by [`Self::drain_dirty`].
    dirty: SmallVec<[PropertyKey; 16]>,
    /// Depth counter for [`Self::freeze_notify`] / [`Self::thaw_notify`].
    /// When non-zero, `set` still writes the cell but skips appending to `dirty`.
    notify_freeze_depth: u32,
    /// Monotonic listener id allocator.
    next_listener: u64,
    /// Monotonic binding id allocator.
    next_binding: u64,
}

impl PropertyStore {
    /// Writes `value` into the cell for `key`. Bumps the cell's generation and, unless [`Self::freeze_notify`] is active,
    /// appends `key` to the dirty queue.
    ///
    /// Returns `true` when the stored value actually changed (semantically equal writes skip the dirty push - matches
    /// GObject's `notify` behaviour).
    pub fn set(&mut self, key: PropertyKey, value: PropertyValue) -> bool {
        let changed = match self.cells.get(&key) {
            Some(cell) => !cell.value.eq_value(&value),
            None => true,
        };
        let cell = self
            .cells
            .entry(key.clone())
            .or_insert_with(|| PropertyCell {
                value: value.clone(),
                listeners: SmallVec::new(),
                binding: None,
                generation: 0,
            });
        if changed {
            cell.value = value;
            cell.generation = cell.generation.wrapping_add(1);
            if self.notify_freeze_depth == 0 {
                self.dirty.push(key);
            }
        }
        changed
    }

    /// Returns a shared reference to the cell's value, or `None` when the key is absent.
    pub fn get(&self, key: &PropertyKey) -> Option<&PropertyValue> {
        self.cells.get(key).map(|c| &c.value)
    }

    /// Returns a shared reference to the full cell, or `None` when the key is absent.
    pub fn cell(&self, key: &PropertyKey) -> Option<&PropertyCell> {
        self.cells.get(key)
    }

    /// Iterate over every `(key, value)` pair in storage, in arbitrary
    /// order. Used by [`mirror_property_store_to_typed_cache`] to
    /// snapshot the typed scalar cells into a process-wide cache that
    /// FFI accessors read from any thread.
    pub fn iter(&self) -> impl Iterator<Item = (&PropertyKey, &PropertyValue)> {
        self.cells.iter().map(|(k, c)| (k, &c.value))
    }

    /// Returns `true` when `key` has a cell.
    pub fn contains(&self, key: &PropertyKey) -> bool {
        self.cells.contains_key(key)
    }

    /// Returns the number of listeners registered for `key` (currently always 0 - wave 1 wires the listener API).
    pub fn listener_count(&self, key: &PropertyKey) -> usize {
        self.cells.get(key).map(|c| c.listeners.len()).unwrap_or(0)
    }

    /// Allocates and registers a fresh listener id against `key`. The cell is auto-created with a [`PropertyValue::Bool(false)`] sentinel when absent;
    /// the next `set` overwrites it.
    pub fn add_listener(&mut self, key: PropertyKey) -> ListenerId {
        self.next_listener = self.next_listener.wrapping_add(1);
        let id = ListenerId(self.next_listener);
        let cell = self.cells.entry(key).or_insert_with(|| PropertyCell {
            value: PropertyValue::Bool(false),
            listeners: SmallVec::new(),
            binding: None,
            generation: 0,
        });
        cell.listeners.push(id);
        id
    }

    /// Registers a one-way binding from `src` to `dst`. Returns the [`BindingId`] stored on the destination cell.
    /// Wave 1 wires the actual propagation system; this method only records the wiring intent today.
    pub fn bind(&mut self, dst: PropertyKey, _src: PropertyKey) -> BindingId {
        self.next_binding = self.next_binding.wrapping_add(1);
        let id = BindingId(self.next_binding);
        let cell = self.cells.entry(dst).or_insert_with(|| PropertyCell {
            value: PropertyValue::Bool(false),
            listeners: SmallVec::new(),
            binding: None,
            generation: 0,
        });
        cell.binding = Some(id);
        id
    }

    /// Pushes the freeze counter so subsequent `set` calls write the cell without appending to `dirty`.
    /// Pair with [`Self::thaw_notify`]; nested freeze/thaw is supported.
    pub fn freeze_notify(&mut self) {
        self.notify_freeze_depth = self.notify_freeze_depth.saturating_add(1);
    }

    /// Pops the freeze counter. When the depth reaches zero, no implicit `dirty` flush happens - callers that need to
    /// fire deferred notifies should manually re-set the affected keys after thaw.
    pub fn thaw_notify(&mut self) {
        self.notify_freeze_depth = self.notify_freeze_depth.saturating_sub(1);
    }

    /// Drains the per-tick dirty queue, returning the dirtied keys. Downstream systems call this once per tick.
    pub fn drain_dirty(&mut self) -> SmallVec<[PropertyKey; 16]> {
        std::mem::take(&mut self.dirty)
    }

    /// Returns a snapshot of the current dirty queue without draining it.
    pub fn dirty_peek(&self) -> &[PropertyKey] {
        &self.dirty
    }

    /// Clear the per-tick dirty queue without consuming it. Sibling to [`Self::drain_dirty`]
    /// for callers that only want the reset side-effect - used by the end-of-tick
    /// `clear_property_store_dirty` system to drop accumulated entries after every consumer
    /// (derivations, theme propagation, ...) has already peeked.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Number of stored cells. Useful for tests and devtools.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Returns `true` when no cells are stored.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    // --- Wave-D ergonomic helpers (Global-keyed `Str` cells) -----------------
    //
    // These let migrating callers swap `signals.get(name)` for
    // `store.get_global_str(name)` without authoring the `PropertyKey::Global` /
    // `PropertyValue::Str` ceremony at every call site. They're additive - the
    // raw [`Self::get`] / [`Self::set`] APIs continue to work and are still
    // required for non-Global / non-Str use.

    /// Reads a globally-keyed string cell, stringifying scalar variants on demand.
    /// Returns `None` when the cell is absent.
    ///
    /// Coercion matches the [`From<PropertyValue> for Arc<str>`] impl above:
    /// `Bool` -> `"true"` / `"false"`; `I64` / `F64` -> decimal repr; non-scalar
    /// variants (`Color`, `Vec2`, `Custom`) yield `Some("")` - callers that
    /// need the typed value should pattern-match the raw cell.
    pub fn get_global_str(&self, name: &str) -> Option<Arc<str>> {
        let key = PropertyKey::Global(Arc::<str>::from(name));
        self.get(&key).cloned().map(Arc::<str>::from)
    }

    /// Writes a string into a globally-keyed cell. Convenience for the common
    /// `set(PropertyKey::Global(name), PropertyValue::Str(value))` pattern.
    pub fn set_global_str(&mut self, name: &str, value: impl Into<Arc<str>>) -> bool {
        self.set(
            PropertyKey::Global(Arc::<str>::from(name)),
            PropertyValue::Str(value.into()),
        )
    }

    /// Reads a globally-keyed boolean. Recognises `Bool` directly, plus the
    /// canonical `"true"` / `"false"` / `"1"` / `"0"` string aliases that
    /// [`crate::signals::Signals::set_bool`] used to write. Other variants
    /// (numeric, colour, ...) yield `None` so callers can fall back to a default
    /// rather than treating an unrelated value as `false`.
    pub fn get_global_bool(&self, name: &str) -> Option<bool> {
        let key = PropertyKey::Global(Arc::<str>::from(name));
        match self.get(&key)? {
            PropertyValue::Bool(b) => Some(*b),
            PropertyValue::Str(s) => match s.as_ref() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Writes a boolean as the canonical `"true"` / `"false"` string variant.
    /// Mirrors [`crate::signals::Signals::set_bool`] semantics so downstream
    /// readers that still consult string variants (`<if eq="true">` body
    /// comparator) keep working post-migration.
    pub fn set_global_bool(&mut self, name: &str, value: bool) -> bool {
        self.set_global_str(name, if value { "true" } else { "false" })
    }

    /// Iterates the per-tick dirty queue and yields just the global property
    /// names. Replaces direct iteration over the legacy `Signals::dirty` set.
    pub fn dirty_global_names(&self) -> impl Iterator<Item = &str> {
        self.dirty.iter().filter_map(|k| match k {
            PropertyKey::Global(name) => Some(name.as_ref()),
            PropertyKey::Entity(_, _) => None,
        })
    }
}

/// End-of-tick system that clears the [`PropertyStore`] dirty queue. Runs in
/// [`crate::tick::TickStage::A11ySync`] after every consumer that peeks the
/// queue (theme propagation, derivations, render-world frame dirty roll-up).
///
/// Without this the queue grows monotonically and `dirty_global_names` returns
/// stale entries on subsequent ticks. The system is the wave-D replacement for
/// `clear_signal_dirty` - same role, different backing store.
pub fn clear_property_store_dirty(store: Option<ResMut<PropertyStore>>) {
    if let Some(mut s) = store
        && !s.dirty.is_empty()
    {
        s.clear_dirty();
    }
}

// --- `Property<T>` typed handle ----------------------------------------------
//
// W7.x ergonomics: avoid stringifying typed signals across the Rust API. The
// handle wraps a `PropertyKey` with a phantom `T` so `store.get` / `store.set`
// round-trip through the existing `From`/`Into` conversions without bespoke
// per-type helpers.
//
// Conversions reuse the `From<PropertyValue> for T` impls landed above, which
// auto-derive `TryFrom<PropertyValue> for T` via the stdlib blanket impl with
// `Error = Infallible`. That matches the lossy fallback the legacy scripts
// already expect (e.g. reading an `I64` cell as `f64` coerces).

/// Typed property handle. Wraps a [`PropertyKey`] with a phantom `T` so reads
/// and writes never stringify.
///
/// Construct with [`Self::new`] (global key) or [`Self::entity`] (entity-scoped
/// key). Round-trip through [`Self::get`] / [`Self::set`] against a
/// [`PropertyStore`] borrow.
///
/// ```ignore
/// use lumen_core::prelude::*;
/// let count: Property<i64> = Property::new("count");
/// let mut store = PropertyStore::default();
/// count.set(&mut store, 42);
/// assert_eq!(count.get(&store), Some(42));
/// ```
#[derive(Clone, Debug)]
pub struct Property<T> {
    key: PropertyKey,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Property<T>
where
    T: TryFrom<PropertyValue> + Into<PropertyValue> + Clone,
{
    /// Constructs a handle keyed on the global namespace (`PropertyKey::Global`).
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            key: PropertyKey::Global(name.into()),
            _marker: PhantomData,
        }
    }

    /// Constructs a handle keyed on the entity-scoped namespace.
    pub fn entity(e: Entity, name: impl Into<Arc<str>>) -> Self {
        Self {
            key: PropertyKey::Entity(e, name.into()),
            _marker: PhantomData,
        }
    }

    /// Borrows the underlying [`PropertyKey`]. Useful when the caller needs to
    /// pass the key into raw [`PropertyStore`] APIs.
    pub fn key(&self) -> &PropertyKey {
        &self.key
    }

    /// Reads the typed value from `store`. Returns `None` when the key is
    /// absent. When the stored cell is a variant `T` doesn't natively
    /// represent, the conversion falls back to `T`'s `Default`-ish coercion
    /// (matches the existing `From<PropertyValue> for T` semantics).
    pub fn get(&self, store: &PropertyStore) -> Option<T> {
        store
            .get(&self.key)
            .cloned()
            .and_then(|v| T::try_from(v).ok())
    }

    /// Writes `value` into the cell, bumping the cell's generation and
    /// appending to the dirty queue (subject to [`PropertyStore::freeze_notify`]).
    pub fn set(&self, store: &mut PropertyStore, value: T) {
        store.set(self.key.clone(), value.into());
    }
}

// --- External typed-property bus ---------------------------------------------
//
// Round 4 typed-signal closure: cross-thread callers (the C-ABI crate, async tasks)
// that want to write a *typed* `PropertyValue` (Int64/Float64/Bool/Color/Vec2)
// directly into the `PropertyStore` use this channel. The existing
// `lumen_core::signals::push_external_signal` path is `Str`-only -
// `drain_external_signals` skips non-Str variants. The bus + drain below
// land typed writes without round-tripping through `Signals`.

static EXTERNAL_PROPERTY_TX: OnceLock<Sender<(PropertyKey, PropertyValue)>> = OnceLock::new();
static EXTERNAL_PROPERTY_RX: OnceLock<Mutex<Receiver<(PropertyKey, PropertyValue)>>> =
    OnceLock::new();

fn init_external_property_channel() -> &'static Sender<(PropertyKey, PropertyValue)> {
    EXTERNAL_PROPERTY_TX.get_or_init(|| {
        let (tx, rx) = unbounded();
        let _ = EXTERNAL_PROPERTY_RX.set(Mutex::new(rx));
        tx
    })
}

/// Idempotently initialises the cross-thread typed-property channel. Safe to call multiple times.
pub fn init_external_properties() {
    let _ = init_external_property_channel();
}

/// Sends a typed `(PropertyKey, PropertyValue)` write from any thread.
///
/// Picked up on the next tick by [`drain_external_properties`]; bypasses the
/// legacy `Signals` mirror entirely so the receiving cell stores the typed
/// variant (`I64`, `F64`, `Bool`, `Color`, ...) directly.
///
/// Returns `false` when the channel has disconnected.
pub fn push_external_property(key: PropertyKey, value: PropertyValue) -> bool {
    init_external_property_channel().send((key, value)).is_ok()
}

/// Snapshot the cells currently visible to the external bus by polling the
/// channel non-destructively. Used by FFI typed reads that want to consult
/// pending pre-run writes without owning a `PropertyStore`. The returned
/// map is empty when the channel is unset or empty.
///
/// This is a best-effort accessor - concurrent senders may add entries
/// after the snapshot returns. Read consistency across a single FFI call
/// is sufficient for round-trip embedder scenarios (set N, read N).
pub fn external_property_snapshot() -> HashMap<PropertyKey, PropertyValue> {
    let Some(rx_lock) = EXTERNAL_PROPERTY_RX.get() else {
        return HashMap::new();
    };
    let Ok(rx) = rx_lock.lock() else {
        return HashMap::new();
    };
    // We can't peek non-destructively at a crossbeam receiver. Drain into
    // a buffer, then re-send the entries back through the channel so the
    // tick-side drain still sees them. Acceptable cost: one round-trip
    // per FFI read while the embedder is in pre-run config mode.
    let mut buf: Vec<(PropertyKey, PropertyValue)> = Vec::new();
    while let Ok(entry) = rx.try_recv() {
        buf.push(entry);
    }
    let tx = init_external_property_channel();
    let mut snapshot = HashMap::new();
    for (k, v) in &buf {
        snapshot.insert(k.clone(), v.clone());
    }
    for entry in buf {
        let _ = tx.send(entry);
    }
    snapshot
}

/// Returns `true` when the cross-thread typed-property channel currently
/// holds undrained writes.
///
/// Non-destructive: peeks the receiver's queue length without consuming
/// any entries. The window backend calls this after `App::tick()` so it
/// can self-schedule a follow-up frame when a write is still sitting in
/// the bus (e.g. a background thread pushed after this tick's
/// [`drain_external_properties`] already ran) - otherwise the value would
/// wait for the next unrelated OS event to wake the loop.
///
/// Returns `false` when the channel was never initialised, is empty, or
/// its lock is poisoned.
pub fn external_properties_pending() -> bool {
    EXTERNAL_PROPERTY_RX
        .get()
        .and_then(|rx_lock| rx_lock.lock().ok().map(|rx| !rx.is_empty()))
        .unwrap_or(false)
}

/// Per-tick system that drains every queued typed-property write into
/// [`PropertyStore`]. Each entry calls [`PropertyStore::set`] so the cell
/// gets the typed variant directly - no stringification, no `Signals`
/// round-trip.
///
/// Pair with [`init_external_properties`] at startup; the runtime
/// (`lumenc` or a custom embedder) registers this in
/// [`crate::tick::TickStage::CommandDrain`] alongside
/// [`crate::command::apply_property_commands`].
pub fn drain_external_properties(mut store: ResMut<PropertyStore>) {
    let Some(rx_lock) = EXTERNAL_PROPERTY_RX.get() else {
        return;
    };
    let Ok(rx) = rx_lock.lock() else {
        return;
    };
    while let Ok((key, value)) = rx.try_recv() {
        store.set(key, value);
    }
}

/// Second, in-`Systems`-stage drain of the external typed-property bus,
/// for same-tick commit of main-thread script writes.
///
/// [`drain_external_properties`] is registered in
/// [`crate::tick::TickStage::CommandDrain`], which is chained *before*
/// [`crate::tick::TickStage::Systems`]. A main-thread script that writes a
/// signal during event dispatch (`on_click` -> `signals.count.set(..)`)
/// pushes onto the bus from inside a `Systems`-stage system, i.e. too late
/// for that tick's CommandDrain drain - so without this the new value
/// would sit in the bus until the *next* tick, adding a whole frame of
/// input latency before a `bind="text:count"` reader reflects it.
///
/// The embedder registers this in `TickStage::Systems`, ordered *after*
/// the script dispatch systems and *before* the reactive binding readers
/// ([`crate::signals::apply_text_bindings`] et al.), so those writes land
/// in [`PropertyStore`] on the very tick the click fired. Cross-thread
/// writers keep using the same bus untouched; entries that happen to be
/// queued here are simply committed a little earlier than CommandDrain
/// would - strictly lower latency, never a correctness change. Distinct
/// system type from [`drain_external_properties`] so registering both in
/// the same schedule stays unambiguous.
pub fn commit_external_properties(store: ResMut<PropertyStore>) {
    drain_external_properties(store);
}

// --- Typed-property mirror cache (PropertyStore -> cross-thread view) ---------
//
// FFI typed-read accessors (lumen_signal_get_int64 / _float64 / _bool / _color
// in the root `lumen` crate) run on any thread - `Res<PropertyStore>` requires the ECS
// scheduler. `mirror_property_store_to_typed_cache` runs at TickStage end
// and copies every globally-keyed typed cell into a process-wide
// `Mutex<HashMap>` that the FFI consults from any thread. Round 6 closes the
// loop: FFI reads now see PropertyStore writes from any source (script /
// ECS / FFI), not only writes that flowed through the `push_external_property`
// bus.

static TYPED_PROPERTY_MIRROR: OnceLock<Mutex<HashMap<PropertyKey, PropertyValue>>> =
    OnceLock::new();

fn typed_property_mirror() -> &'static Mutex<HashMap<PropertyKey, PropertyValue>> {
    TYPED_PROPERTY_MIRROR.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cross-thread snapshot of every typed [`PropertyStore`] cell mirrored
/// by [`mirror_property_store_to_typed_cache`]. Returns the cache as
/// owned data so callers can drop their lock immediately. FFI typed-read
/// accessors consume this on their fallback path.
pub fn typed_property_snapshot() -> HashMap<PropertyKey, PropertyValue> {
    typed_property_mirror()
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default()
}

/// Per-tick system that reflects every typed cell in [`PropertyStore`] into
/// the process-wide [`typed_property_snapshot`] cache. Pair with
/// [`drain_external_properties`] in [`crate::tick::TickStage::A11ySync`]
/// (or any late-tick stage) so the mirror sees writes that arrived this
/// tick.
///
/// Strings and `Custom` variants are skipped - the FFI surface only
/// exposes scalar accessors. Vec2 + Color flow through. The cache holds
/// `PropertyValue` clones; consumers downcast at read time.
pub fn mirror_property_store_to_typed_cache(store: Option<Res<PropertyStore>>) {
    let Some(store) = store else {
        return;
    };
    // Idle-tick fast path: nothing was written this tick, so the mirror
    // already reflects the store. Every `set()` that changes a cell pushes
    // its key onto the dirty queue, so an empty queue guarantees the cache
    // is current - no lock, no rebuild. This system is ordered before
    // `clear_property_store_dirty` so the queue is still populated here.
    if store.dirty_peek().is_empty() {
        return;
    }
    let Ok(mut cache) = typed_property_mirror().lock() else {
        return;
    };
    // Update only the cells dirtied this tick rather than clearing and
    // re-inserting the whole map every tick.
    for key in store.dirty_peek() {
        let Some(value) = store.get(key) else {
            continue;
        };
        match value {
            PropertyValue::I64(_)
            | PropertyValue::F64(_)
            | PropertyValue::Bool(_)
            | PropertyValue::Color(_)
            | PropertyValue::Vec2(_) => {
                cache.insert(key.clone(), value.clone());
            }
            // Strings + Custom skipped - the FFI scalar accessors do not
            // consume them. `bind-text` markup reads the string straight
            // out of this store (`apply_text_bindings`), so it needs no
            // mirror entry.
            PropertyValue::Str(_) | PropertyValue::Custom(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_round_trips() {
        let mut s = PropertyStore::default();
        let k = PropertyKey::global("foo");
        s.set(k.clone(), PropertyValue::I64(42));
        assert!(matches!(s.get(&k), Some(PropertyValue::I64(42))));
    }

    #[test]
    fn unchanged_set_does_not_dirty() {
        let mut s = PropertyStore::default();
        let k = PropertyKey::global("foo");
        assert!(s.set(k.clone(), PropertyValue::I64(1)));
        let _ = s.drain_dirty();
        assert!(!s.set(k.clone(), PropertyValue::I64(1)));
        assert!(s.drain_dirty().is_empty());
    }

    #[test]
    fn external_bus_pending_reports_queued_writes() {
        use std::sync::Arc;
        // Resource-level guard for Fix 1's self-scheduling: a queued
        // cross-thread / main-thread-script write must make
        // `external_properties_pending()` read true, because that is the
        // condition the window backend uses to re-arm the redraw. If this
        // ever returned false with a write in the channel, the app would
        // park with a stale frame until an unrelated OS event arrived.
        //
        // The channel is process-global, so we only assert the monotonic
        // direction (a push makes it pending) to stay robust under the
        // parallel test runner. `set(v) then get == v` after a manual
        // drain confirms the write commits, mirroring the tick-side drain.
        let k = PropertyKey::Global(Arc::<str>::from("bus_pending_probe_key"));
        assert!(push_external_property(k.clone(), PropertyValue::I64(7)));
        assert!(
            external_properties_pending(),
            "a queued external write must report pending so the backend re-arms the redraw"
        );

        // Commit the queued write via the same path the tick-side drain
        // takes, and confirm it lands in the store (drains the channel so
        // this probe key doesn't leak into sibling tests).
        let mut store = PropertyStore::default();
        if let Some(rx_lock) = EXTERNAL_PROPERTY_RX.get()
            && let Ok(rx) = rx_lock.lock()
        {
            while let Ok((key, value)) = rx.try_recv() {
                store.set(key, value);
            }
        }
        assert!(matches!(store.get(&k), Some(PropertyValue::I64(7))));
    }

    #[test]
    fn changed_set_appends_to_dirty() {
        let mut s = PropertyStore::default();
        let k = PropertyKey::global("foo");
        s.set(k.clone(), PropertyValue::I64(1));
        s.set(k.clone(), PropertyValue::I64(2));
        let drained = s.drain_dirty();
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn freeze_then_thaw_skips_dirty() {
        let mut s = PropertyStore::default();
        let k = PropertyKey::global("foo");
        s.freeze_notify();
        s.set(k.clone(), PropertyValue::I64(1));
        s.set(k.clone(), PropertyValue::I64(2));
        assert!(s.drain_dirty().is_empty());
        s.thaw_notify();
        s.set(k.clone(), PropertyValue::I64(3));
        assert_eq!(s.drain_dirty().len(), 1);
    }

    #[test]
    fn generation_bumps_per_change() {
        let mut s = PropertyStore::default();
        let k = PropertyKey::global("foo");
        s.set(k.clone(), PropertyValue::I64(1));
        let g1 = s.cell(&k).unwrap().generation;
        s.set(k.clone(), PropertyValue::I64(2));
        let g2 = s.cell(&k).unwrap().generation;
        assert_eq!(g2, g1 + 1);
    }

    #[test]
    fn property_handle_round_trips_i64() {
        let mut store = PropertyStore::default();
        let count = Property::<i64>::new("count");
        count.set(&mut store, 42);
        assert_eq!(count.get(&store), Some(42));
        count.set(&mut store, -7);
        assert_eq!(count.get(&store), Some(-7));
    }

    #[test]
    fn property_handle_round_trips_bool_and_f64() {
        let mut store = PropertyStore::default();
        let flag = Property::<bool>::new("flag");
        flag.set(&mut store, true);
        assert_eq!(flag.get(&store), Some(true));
        let amount = Property::<f64>::new("amount");
        amount.set(&mut store, 3.5);
        assert_eq!(amount.get(&store), Some(3.5));
    }

    #[test]
    fn property_handle_absent_key_returns_none() {
        let store = PropertyStore::default();
        let p = Property::<i64>::new("nope");
        assert_eq!(p.get(&store), None);
    }

    #[test]
    fn property_handle_exposes_key() {
        let p = Property::<i64>::new("count");
        match p.key() {
            PropertyKey::Global(name) => assert_eq!(&**name, "count"),
            _ => panic!("expected Global key"),
        }
    }

    #[test]
    fn entity_keys_disambiguate_from_global() {
        let mut s = PropertyStore::default();
        let g = PropertyKey::global("foo");
        let e = PropertyKey::entity(Entity::from_raw_u32(1).unwrap(), "foo");
        s.set(g.clone(), PropertyValue::I64(1));
        s.set(e.clone(), PropertyValue::I64(2));
        assert!(matches!(s.get(&g), Some(PropertyValue::I64(1))));
        assert!(matches!(s.get(&e), Some(PropertyValue::I64(2))));
    }
}
