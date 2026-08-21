//! Reactive named-value store backing `bind-text="..."` markup attributes and `<for each="...">` iteration.
//!
//! Wave-D status: [`Signals`] is now a thin **`#[deprecated]`** wrapper kept for
//! external embedders that still hold `Res<Signals>` references. Internal lumen
//! systems (`apply_text_bindings`, `apply_checked_bindings`, ...) read and write
//! through [`PropertyStore`] directly - that's the canonical typed reactive
//! store. The wrapper's `set` mirrors writes into [`PropertyStore`] via
//! [`push_external_property`] so the next tick's
//! [`crate::property_store::drain_external_properties`] pass commits them, and
//! `get` reads from the local `HashMap` populated by mirror-back when an
//! external write lands.
//!
//! - [`Signals`] holds scalar `String` values keyed by name (back-compat only).
//! - [`ArraySignals`] holds ordered vectors of record-shaped maps keyed by name.
//! - Scripts populate both via `signal_set` / `signal_array_set`, stringified at the boundary.
//! - Each tick, the `Bind*` and reconciler systems pull from [`PropertyStore`]
//!   (post wave-D) and copy into bound components or spawn/despawn `<for>` children.

// The `Resource` derive below generates its own `impl Resource for Signals`
// as a separate item that doesn't inherit the struct's `#[allow(deprecated)]`
// (derive-macro output isn't nested under the annotated item's attributes),
// so the module-level allow is needed to suppress the self-referential
// deprecation warning on `Signals`'s own definition.
#![allow(deprecated)]

use crate::components::{
    BindChecked, BindDisabled, BindScroll, BindText, BindValue, Disabled, ImeState, SliderValue,
    TextContent, TextInput, Toggleable,
};
use crate::input::{Focused, Scroll, ScrollOffset};
use crate::property_store::{PropertyKey, PropertyStore, PropertyValue, push_external_property};
use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Legacy reactive named-value store. **Deprecated** as of wave-D - internal lumen
/// systems now read/write through [`PropertyStore`] keyed on [`PropertyKey::Global`].
///
/// The struct is retained as a thin wrapper so external embedders and FFI consumers that
/// still hold `Res<Signals>` references compile and continue working: `set` writes through
/// to [`PropertyStore`] via the cross-thread [`push_external_property`] bus AND keeps a
/// local string copy for the legacy `&str`-returning `get` signature, and a per-tick
/// mirror system back-fills writes that landed directly on [`PropertyStore`] so
/// `signals.get(name)` keeps surfacing the latest value regardless of which side wrote it.
///
/// The `dirty` field is still populated by `Signals::set` for legacy derive-style callers,
/// but new code should call [`PropertyStore::dirty_global_names`] instead.
#[derive(Resource, Debug, Default, Clone)]
#[deprecated(
    since = "0.0.1",
    note = "use lumen_core::property_store::PropertyStore instead - Signals is now a thin wrapper that mirrors writes through `push_external_property` and reads from the typed store. New systems should take Res<PropertyStore> / ResMut<PropertyStore>."
)]
#[allow(deprecated)]
pub struct Signals {
    /// Stringified reactive signal values keyed by name. Populated by [`Self::set`]
    /// and by the per-tick [`mirror_property_store_globals_to_signals`] back-mirror
    /// so legacy `&str`-returning `get` callers keep working post wave-D.
    pub values: HashMap<String, String>,
    /// Set of signal names whose value changed during this tick. Kept for the
    /// `apply_derivations` legacy path; new derivation-style consumers should peek
    /// [`PropertyStore::dirty_global_names`] instead.
    pub dirty: HashSet<String>,
}

#[allow(deprecated)]
impl Signals {
    /// Sets the signal `name` to `value`. Writes through to [`PropertyStore`] via
    /// the cross-thread [`push_external_property`] bus AND updates the local
    /// `values` map so legacy `Signals::get` callers observe the write
    /// synchronously. Records the name in [`Self::dirty`] when the value changed.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        let changed = match self.values.get(&name) {
            Some(prev) => prev != &value,
            None => true,
        };
        if changed {
            self.dirty.insert(name.clone());
            // Mirror to PropertyStore via the cross-thread bus. The drain runs once
            // per tick in `TickStage::CommandDrain`; downstream systems that read
            // PropertyStore (apply_text_bindings, derivations, etc.) see the write
            // on the next tick boundary, matching the legacy `Signals` semantics.
            push_external_property(
                PropertyKey::Global(Arc::<str>::from(name.as_str())),
                PropertyValue::Str(Arc::<str>::from(value.as_str())),
            );
        }
        self.values.insert(name, value);
    }

    /// Returns the signal's value, or `None` when undefined.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Typed convenience: stores `value` under `name` as `"true"` or
    /// `"false"`. The underlying repr is still a [`String`] (the
    /// reactive store is type-erased), but funnelling boolean writes
    /// through this helper means call sites can't drift to `"True"`,
    /// `"1"`, or other look-alike strings that the compiler's
    /// `<if eq="true">` body comparator wouldn't recognise.
    pub fn set_bool(&mut self, name: impl Into<String>, value: bool) {
        self.set(name, if value { "true" } else { "false" });
    }

    /// Typed convenience read for boolean signals written via
    /// [`Self::set_bool`]. Accepts the canonical `"true"` / `"false"`
    /// pair plus the common `"1"` / `"0"` alias for FFI / Rhai
    /// authors. Returns `None` when the signal is undefined OR carries
    /// a non-boolean value - the caller can then fall back to a
    /// default rather than treating an unrelated string as `false`.
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }
}

/// Per-tick mirror that copies the latest [`PropertyStore`] global string cells
/// back into the legacy [`Signals`] map. Runs in [`crate::tick::TickStage::Systems`]
/// after `drain_external_properties` so any write that landed on PropertyStore
/// directly (typed setters, ECS-side writes, FFI typed pushes) is visible to
/// readers that still consult `Res<Signals>`.
///
/// Only `PropertyValue::Str` cells are mirrored - boolean / numeric / colour cells
/// stay typed in PropertyStore; legacy `Signals::get` would have returned the
/// stringified repr anyway and direct callers should migrate to `PropertyStore`
/// for the typed value.
///
/// No-op when either resource is absent.
#[allow(deprecated)]
pub fn mirror_property_store_globals_to_signals(
    store: Option<Res<PropertyStore>>,
    signals: Option<ResMut<Signals>>,
) {
    let (Some(store), Some(mut signals)) = (store, signals) else {
        return;
    };
    if store.dirty_peek().is_empty() {
        return;
    }
    for key in store.dirty_peek() {
        if let PropertyKey::Global(name) = key
            && let Some(PropertyValue::Str(value)) = store.get(key)
        {
            let name_str = name.as_ref();
            let value_str = value.as_ref();
            let prev = signals.values.get(name_str);
            let changed = match prev {
                Some(p) => p != value_str,
                None => true,
            };
            if changed {
                signals.dirty.insert(name_str.to_string());
                signals
                    .values
                    .insert(name_str.to_string(), value_str.to_string());
            }
        }
    }
}

/// Producer half of the W1.6 theme-notify path: on every
/// `Changed<StyleManager>` tick, writes `"dark"` / `"light"` into
/// [`PropertyStore`] under the `__theme__` global key based on
/// [`crate::components::StyleManager::effective_dark`].
///
/// Post wave-D the write lands directly on [`PropertyStore`] (no Signals
/// round-trip); [`apply_theme_signal_to_root_classes`] consumes it via
/// `dirty_peek` on the next schedule step.
pub fn style_manager_to_signal(
    theme: Res<crate::components::StyleManager>,
    store: Option<ResMut<PropertyStore>>,
) {
    if !theme.is_changed() {
        return;
    }
    let Some(mut s) = store else {
        return;
    };
    let val = if theme.effective_dark {
        "dark"
    } else {
        "light"
    };
    let key = PropertyKey::Global(Arc::<str>::from("__theme__"));
    let already = matches!(s.get(&key), Some(PropertyValue::Str(curr)) if curr.as_ref() == val);
    if !already {
        s.set(key, PropertyValue::Str(Arc::<str>::from(val)));
    }
}

/// Pre-W4.6 alias retained as a thin wrapper so any external scheduler
/// that explicitly named the system keeps compiling. New callers should
/// use [`style_manager_to_signal`].
#[deprecated(
    since = "0.0.1",
    note = "Renamed to `style_manager_to_signal` (W4.6); the underlying `StyleManager` carries `effective_dark` in place of the old `OsTheme.is_dark`."
)]
pub fn os_theme_to_signal(
    theme: Res<crate::components::StyleManager>,
    store: Option<ResMut<PropertyStore>>,
) {
    style_manager_to_signal(theme, store);
}

/// Consumer half of the W1.6 theme-notify path: reads
/// [`PropertyStore::dirty_peek`] for `PropertyKey::Global("__theme__")` and
/// applies `theme-light` / `theme-dark` to every root entity's
/// [`crate::components::LumenClasses`].
///
/// Uses `dirty_peek` (non-destructive) so the wave-1
/// [`crate::render_world::roll_up_frame_dirty`] can also observe the same
/// notify entry on this tick. Skips the iteration entirely when the queue
/// carries no theme write - no `Changed<LumenClasses>` bump on quiet ticks.
#[allow(clippy::type_complexity)]
pub fn apply_theme_signal_to_root_classes(
    store: Option<Res<PropertyStore>>,
    mut roots: Query<
        &mut crate::components::LumenClasses,
        bevy_ecs::query::Without<bevy_ecs::hierarchy::ChildOf>,
    >,
) {
    let Some(store) = store else {
        return;
    };
    let key: PropertyKey = PropertyKey::Global(Arc::<str>::from("__theme__"));
    if !store.dirty_peek().iter().any(|k| k == &key) {
        return;
    }
    let want_dark = matches!(store.get(&key), Some(PropertyValue::Str(s)) if s.as_ref() == "dark");
    let (add, drop) = if want_dark {
        ("theme-dark", "theme-light")
    } else {
        ("theme-light", "theme-dark")
    };
    for mut classes in &mut roots {
        let has_add = classes.0.iter().any(|c| c.as_ref() == add);
        let has_drop = classes.0.iter().any(|c| c.as_ref() == drop);
        if has_add && !has_drop {
            // Already in the target state - skip the borrow to avoid bumping `Changed<LumenClasses>`.
            continue;
        }
        classes.0.retain(|c| c.as_ref() != drop);
        if !has_add {
            classes.0.push(add.into());
        }
    }
}

/// Legacy theme application path - superseded by the W1.6 split of
/// [`style_manager_to_signal`] (producer) + [`apply_theme_signal_to_root_classes`]
/// (consumer). Retained as a `#[deprecated]` thin wrapper so external
/// callers that explicitly registered this system keep compiling; new
/// registrations should use the split pair instead.
#[deprecated(
    note = "Use `style_manager_to_signal` + `apply_theme_signal_to_root_classes` instead. The split removes the joint `Res<StyleManager>` / `ResMut<Signals>` / `Query<&mut LumenClasses>` borrow and replaces the `&mut *classes` reborrow trick with notify-queue gating."
)]
#[allow(clippy::type_complexity)]
pub fn apply_theme_class_to_root(
    theme: Res<crate::components::StyleManager>,
    store: Option<ResMut<PropertyStore>>,
    mut roots: Query<
        &mut crate::components::LumenClasses,
        bevy_ecs::query::Without<bevy_ecs::hierarchy::ChildOf>,
    >,
) {
    if !theme.is_changed() {
        return;
    }
    let want_dark = theme.effective_dark;
    let (add, drop) = if want_dark {
        ("theme-dark", "theme-light")
    } else {
        ("theme-light", "theme-dark")
    };
    for mut classes in &mut roots {
        let mut changed = false;
        classes.0.retain(|c| {
            if c.as_ref() == drop {
                changed = true;
                false
            } else {
                true
            }
        });
        if !classes.0.iter().any(|c| c.as_ref() == add) {
            classes.0.push(add.into());
            changed = true;
        }
        if !changed {
            let _ = &mut *classes;
        }
    }
    if let Some(mut s) = store {
        let val = if want_dark { "dark" } else { "light" };
        let key = PropertyKey::Global(Arc::<str>::from("__theme__"));
        let already = matches!(s.get(&key), Some(PropertyValue::Str(curr)) if curr.as_ref() == val);
        if !already {
            s.set(key, PropertyValue::Str(Arc::<str>::from(val)));
        }
    }
}

/// Clears the legacy [`Signals::dirty`] set at the end of [`crate::tick::TickStage::A11ySync`].
/// Argument is `Option<ResMut<Signals>>` so the system no-ops when the [`Signals`] resource is absent.
/// Post wave-D the canonical dirty queue lives on [`PropertyStore`] and is cleared by
/// [`crate::property_store::clear_property_store_dirty`].
#[allow(deprecated)]
pub fn clear_signal_dirty(signals: Option<ResMut<Signals>>) {
    if let Some(mut s) = signals
        && !s.dirty.is_empty()
    {
        s.dirty.clear();
    }
}

/// Copies `PropertyStore[Global(name)]` into [`TextContent`] for every [`BindText`] entity.
/// Entities whose property has no entry keep their existing text.
///
/// Editing-protection gate: entities currently carrying [`Focused`] or an active
/// [`ImeState`] preedit are skipped - overwriting `TextContent` mid-edit would
/// race the keystroke / IME path in [`crate::input::route_ime_events`] and
/// `lumen_input::type_into_focused`, wiping the user's in-progress typing or
/// leaving the caret dangling past the new (shorter) string. When `apply_text_bindings`
/// does overwrite an unfocused entity, any co-resident [`TextInput.cursor`] is
/// clamped to `<= new_text.len()` so the cursor cannot point past the buffer end.
#[allow(clippy::type_complexity)]
pub fn apply_text_bindings(
    store: Res<PropertyStore>,
    mut q: Query<
        (&BindText, &mut TextContent, Option<&mut TextInput>),
        (Without<Focused>, Without<ImeState>),
    >,
    new_binds: Query<(), Added<BindText>>,
) {
    // Idle-tick fast path: no signal changed this tick, so no bound
    // `TextContent` can need refreshing. A `set()` that changes a cell
    // always pushes onto the dirty queue (which isn't cleared until
    // A11ySync, after this system), so an empty queue means every binding
    // already reflects its source.
    //
    // The `new_binds` escape hatch is load-bearing: a reconciler (`<if>` /
    // `<for>` / tab-panel mount) can spawn a fresh `BindText` entity on a
    // tick where NO signal changed - e.g. switching to a tab whose panel
    // was despawned. Its `TextContent` was seeded empty at spawn, and the
    // dirty queue is empty, so without re-running here the new label would
    // stay blank until the next unrelated signal write. `Added<BindText>`
    // catches exactly those just-mounted rows; the full-loop re-scan below
    // is idempotent (equal writes are skipped) so re-running it is safe.
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (bind, mut tc, input) in &mut q {
        let key = PropertyKey::Global(Arc::<str>::from(bind.0.as_ref()));
        // Stringify scalar variants via the existing `From<PropertyValue> for Arc<str>` impl
        // - `Bool` -> `"true"` / `"false"`, `I64` / `F64` -> decimal - so a typed write to the
        // store still reaches `bind-text` markup. Non-scalar variants stringify to "".
        let Some(pv) = store.get(&key) else {
            continue;
        };
        let value: Arc<str> = Arc::<str>::from(pv.clone());
        let value_str = value.as_ref();
        if tc.0 != value_str {
            tc.0 = value_str.to_string();
            // Cursor / selection_anchor are raw byte offsets into TextContent;
            // a signal write that shortens the buffer (or replaces it entirely)
            // can leave them dangling past the new end. Clamp to a valid
            // boundary so the next route_ime_events / type_into_focused call
            // doesn't panic on `insert_str` / `drain`.
            if let Some(mut input) = input {
                if input.cursor > tc.0.len() {
                    input.cursor = tc.0.len();
                }
                if let Some(a) = input.selection_anchor
                    && a > tc.0.len()
                {
                    input.selection_anchor = None;
                }
            }
        }
    }
}

/// Pushes the latest [`TextContent`] back into the matching [`PropertyStore`] entry
/// for every entity carrying both [`BindText`] and [`crate::components::TextInput`].
/// Filtered by `Changed<TextContent>` so the push only fires on user edits.
#[allow(clippy::type_complexity)]
pub fn push_textinput_to_signal(
    mut store: ResMut<PropertyStore>,
    q: Query<
        (&BindText, Ref<TextContent>),
        (
            bevy_ecs::query::Changed<TextContent>,
            bevy_ecs::prelude::With<crate::components::TextInput>,
        ),
    >,
) {
    for (bind, tc) in &q {
        // Spawn default, not a user edit - see `push_toggle_to_signal`.
        if tc.is_added() {
            continue;
        }
        let key = PropertyKey::Global(Arc::<str>::from(bind.0.as_ref()));
        let want = tc.0.as_str();
        let stored: Option<Arc<str>> = store.get(&key).cloned().map(Arc::<str>::from);
        if stored.as_deref() != Some(want) {
            store.set(key, PropertyValue::Str(Arc::<str>::from(want)));
        }
    }
}

/// Copies `PropertyStore[Global(name)]` into [`Toggleable::checked`] for every [`BindChecked`] entity.
/// Recognises `Bool` directly plus the canonical `"true"` / `"1"` string aliases for back-compat.
pub fn apply_checked_bindings(
    store: Res<PropertyStore>,
    mut q: Query<(&BindChecked, &mut Toggleable)>,
    new_binds: Query<(), Added<BindChecked>>,
) {
    // Idle-tick fast path - see `apply_text_bindings` (incl. the
    // `new_binds` escape hatch for reconciler-mounted `<toggle bind-checked>`).
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (bind, mut t) in &mut q {
        let Some(want) = store.get_global_bool(&bind.0) else {
            continue;
        };
        if t.checked != want {
            t.checked = want;
        }
    }
}

/// Pushes [`Toggleable::checked`] back into the matching [`PropertyStore`] entry for every
/// [`BindChecked`] entity with `Changed<Toggleable>`. Writes the canonical `"true"` / `"false"`
/// string variant so the wave-D `<if eq="true">` body comparator and `Signals::get_bool`
/// callers keep recognising it.
/// Spawn-tick rows (`is_added()`) are skipped: the freshly-inserted component
/// carries the widget's spawn default, not a user edit, and pushing it would
/// clobber a script's `signal(name, default)` initial publish (authored markup
/// attrs seed the store separately, if-absent, at spawn).
pub fn push_toggle_to_signal(
    mut store: ResMut<PropertyStore>,
    q: Query<(&BindChecked, Ref<Toggleable>), bevy_ecs::query::Changed<Toggleable>>,
) {
    for (bind, t) in &q {
        if t.is_added() {
            continue;
        }
        let curr = store.get_global_bool(&bind.0);
        if curr != Some(t.checked) {
            store.set_global_bool(&bind.0, t.checked);
        }
    }
}

/// Copies `PropertyStore[Global(name)]` into the presence of the [`Disabled`]
/// marker for every [`BindDisabled`] entity: truthy inserts, falsy removes.
/// Recognises `Bool` plus the canonical `"true"` / `"1"` string aliases via
/// `get_global_bool`. A missing signal leaves the entity untouched (its
/// spawn-time `disabled` attribute keeps authority until the signal exists).
///
/// Downstream reactions - stripping `Hovered` / `Pressed` / focus and the
/// `:disabled` style swap - key off the marker add/remove
/// (`lumen_primitives::eject_interaction_on_disable` and
/// `apply_state_visuals`).
pub fn apply_disabled_bindings(
    store: Res<PropertyStore>,
    mut commands: Commands,
    q: Query<(Entity, &BindDisabled, Has<Disabled>)>,
    new_binds: Query<(), Added<BindDisabled>>,
) {
    // Idle-tick fast path - see `apply_text_bindings` (incl. the
    // `new_binds` escape hatch for reconciler-mounted `bind-disabled`).
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (entity, bind, is_disabled) in &q {
        let Some(want) = store.get_global_bool(&bind.0) else {
            continue;
        };
        if want != is_disabled {
            if want {
                commands.entity(entity).insert(Disabled);
            } else {
                commands.entity(entity).remove::<Disabled>();
            }
        }
    }
}

/// Parses `PropertyStore[Global(name)]` as `f32` and writes it into [`SliderValue::value`]
/// (clamped to `[min, max]`) for every [`BindValue`] entity. Unparseable values are skipped.
pub fn apply_value_bindings(
    store: Res<PropertyStore>,
    mut q: Query<(&BindValue, &mut SliderValue)>,
    new_binds: Query<(), Added<BindValue>>,
) {
    // Idle-tick fast path - see `apply_text_bindings` (incl. the
    // `new_binds` escape hatch for reconciler-mounted `<slider bind-value>`).
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (bind, mut sv) in &mut q {
        let key = PropertyKey::Global(Arc::<str>::from(bind.0.as_str()));
        let Some(pv) = store.get(&key) else {
            continue;
        };
        let parsed: Option<f32> = match pv {
            PropertyValue::F64(n) => Some(*n as f32),
            PropertyValue::I64(n) => Some(*n as f32),
            PropertyValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            PropertyValue::Str(s) => s.as_ref().parse::<f32>().ok(),
            _ => None,
        };
        let Some(parsed) = parsed else {
            continue;
        };
        let clamped = sv.clamp(parsed);
        if (sv.value - clamped).abs() > f32::EPSILON {
            sv.value = clamped;
        }
    }
}

/// Pushes [`SliderValue::value`] back into the matching [`PropertyStore`] entry for every
/// [`BindValue`] entity with `Changed<SliderValue>`. Writes the stringified value so the
/// existing `<if>` / interpolation paths keep working.
pub fn push_slider_to_signal(
    mut store: ResMut<PropertyStore>,
    q: Query<(&BindValue, Ref<SliderValue>), bevy_ecs::query::Changed<SliderValue>>,
) {
    for (bind, sv) in &q {
        // Spawn default, not a user edit - see `push_toggle_to_signal`.
        if sv.is_added() {
            continue;
        }
        let serialised = format!("{}", sv.value);
        let stored: Option<Arc<str>> = store.get_global_str(&bind.0);
        if stored.as_deref() != Some(serialised.as_str()) {
            store.set_global_str(&bind.0, serialised);
        }
    }
}

/// Parses `PropertyStore[Global(name)]` as `f32` and writes it into the vertical
/// [`ScrollOffset`] of every [`BindScroll`] + [`Scroll`] entity (W6 T6).
///
/// Reactive scroll control with NO per-frame script hook: the script writes the
/// signal once; this dirty-gated reader (same fast-path + `Added` escape hatch as
/// [`apply_value_bindings`]) applies it on the write tick. The raw value is
/// written unclamped - `clamp_scroll_offsets` (`TickStage::A11ySync`, registered
/// by `lumen-primitives`) clips it to the content extent the same way it clips
/// user scrolling, so signal-driven and wheel-driven offsets share one clamp
/// rule.
///
/// Applying a signal value also zeroes any in-flight fling velocity on the
/// container: a reactive `scroll_to` must land exactly where the script said,
/// not get dragged onward by leftover momentum.
pub fn apply_scroll_bindings(
    store: Res<PropertyStore>,
    mut q: Query<(&BindScroll, &mut ScrollOffset, Option<&mut Scroll>)>,
    new_binds: Query<(), Added<BindScroll>>,
) {
    // Idle-tick fast path - see `apply_text_bindings` (incl. the
    // `new_binds` escape hatch for reconciler-mounted `<scroll bind-scroll>`).
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (bind, mut off, scroll) in &mut q {
        let key = PropertyKey::Global(Arc::<str>::from(bind.0.as_str()));
        let Some(pv) = store.get(&key) else {
            continue;
        };
        let parsed: Option<f32> = match pv {
            PropertyValue::F64(n) => Some(*n as f32),
            PropertyValue::I64(n) => Some(*n as f32),
            PropertyValue::Str(s) => s.as_ref().parse::<f32>().ok(),
            _ => None,
        };
        let Some(parsed) = parsed else {
            continue;
        };
        if (off.0.y - parsed).abs() > f32::EPSILON {
            off.0.y = parsed;
            if let Some(mut scroll) = scroll {
                scroll.velocity = glam::Vec2::ZERO;
            }
        }
    }
}

/// Pushes the settled vertical [`ScrollOffset`] back into the matching
/// [`PropertyStore`] entry for every [`BindScroll`] entity (W6 T6, the
/// two-way half).
///
/// Throttle contract: not per-frame. A user drag / wheel fling mutates the
/// offset every tick; pushing each intermediate value would spam the store
/// (and re-run every derivation) at frame rate. Instead the system arms a
/// pending entry while the offset keeps changing, and pushes once on
/// settle: the first tick where the offset did not change and the
/// container's fling velocity has slept. Spawn-tick rows (`is_added()`)
/// are skipped - the freshly-inserted offset is the widget default, not a
/// user scroll (same rule as [`push_toggle_to_signal`]). The value is
/// stringified like [`push_slider_to_signal`] so `<if>` comparators and
/// interpolation keep working; the equality check keeps the
/// signal->offset->signal round trip from echoing.
pub fn push_scroll_to_signal(
    mut store: ResMut<PropertyStore>,
    q: Query<(Entity, &BindScroll, Ref<ScrollOffset>, Option<&Scroll>)>,
    mut removed: RemovedComponents<BindScroll>,
    mut pending: Local<HashSet<Entity>>,
) {
    // Drop settle latches for despawned / unbound containers so the
    // Local set can't grow unbounded under a reconciler that churns
    // `<scroll bind-scroll>` subtrees.
    for gone in removed.read() {
        pending.remove(&gone);
    }
    for (entity, bind, off, scroll) in &q {
        if off.is_added() {
            continue;
        }
        if off.is_changed() {
            // Still moving - arm (or keep) the settle latch, push nothing.
            pending.insert(entity);
            continue;
        }
        if !pending.contains(&entity) {
            continue;
        }
        // Offset unchanged this tick; wait out any live fling so the
        // settled value (post rubber-band / clamp) is what lands.
        let velocity_live = scroll.is_some_and(|s| s.velocity.length_squared() > 1.0);
        if velocity_live {
            continue;
        }
        pending.remove(&entity);
        let serialised = format!("{}", off.0.y);
        let stored: Option<Arc<str>> = store.get_global_str(&bind.0);
        if stored.as_deref() != Some(serialised.as_str()) {
            store.set_global_str(&bind.0, serialised);
        }
    }
}

/// Whether a signal's stringified value counts as true.
///
/// Empty, `"false"` and `"0"` are false; anything else is true. This is the
/// rule an `<if>` gate evaluates. Anything that decides the same branch
/// somewhere else, such as a build step rendering it ahead of the runtime,
/// applies this rule rather than one of its own.
///
/// A signal that was never written is false. Callers holding an
/// `Option<&str>` spell that out with `is_some_and`, which keeps the missing
/// case visible at the call site.
pub fn signal_is_truthy(value: &str) -> bool {
    !matches!(value, "" | "false" | "0")
}

/// The boolean a signal's stringified value states, when it states one.
///
/// Stricter than [`signal_is_truthy`] on purpose: a binding that drives a
/// state a widget already has, such as `bind-checked` or `bind-disabled`,
/// leaves that state alone unless the signal says which way it goes.
/// `"anything"` is a value nobody meant as a boolean, so the widget keeps
/// what its markup gave it rather than silently turning on.
///
/// [`crate::property_store::PropertyStore::get_global_bool`] reads a typed
/// `Bool` cell directly and passes a string one through here, so a value that
/// arrives as text decides the same way wherever it is read: in a running
/// app, and in a build writing the page ahead of one.
pub fn signal_as_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// One row of a reactive array; field name -> stringified value.
pub type ArrayItem = HashMap<String, String>;

/// Reactive array store paired with [`Signals`]; ordered vectors of [`ArrayItem`] keyed by name.
/// Written through `ScriptCommand::SetArray`; the reconciler system spawns/despawns markup children to match the stored vector.
///
/// Wave-D follow-up: a future migration may collapse this into
/// `PropertyValue::Custom(Arc<ArrayItems>)` so the typed property store becomes
/// the single source of truth - left as a separate resource for now since the
/// record-shaped data model doesn't map cleanly onto the scalar typed cells.
#[derive(Resource, Debug, Default, Clone)]
pub struct ArraySignals(pub HashMap<String, Vec<ArrayItem>>);

impl ArraySignals {
    /// Replaces the contents of the named array with `items`.
    pub fn set(&mut self, name: impl Into<String>, items: Vec<ArrayItem>) {
        self.0.insert(name.into(), items);
    }

    /// Returns the named array as a slice, or `None` when absent.
    pub fn get(&self, name: &str) -> Option<&[ArrayItem]> {
        self.0.get(name).map(Vec::as_slice)
    }

    /// No-op placeholder retained for forward-compatibility with an incremental-diff reconciler.
    pub fn touch(&mut self, name: &str) {
        let _ = name;
    }
}

// ============================================================
// External signal channel.
//
// Provides a thread-safe ingress for `push_external_signal`, `push_external_array`, and `push_external_clear`.
// The runtime installs [`drain_external_signals`] as a per-tick system that applies enqueued mutations to [`ArraySignals`].
// Scalar Signal writes route through [`push_external_property`] post wave-D so the cell lands in
// [`PropertyStore`] directly; the legacy Array / Clear payloads stay on this channel until ArraySignals
// itself migrates.
// ============================================================

/// One enqueued mutation applied by [`drain_external_signals`] on the next tick. Values are pre-stringified.
#[derive(Debug, Clone)]
pub enum ExternalMutation {
    /// Overwrite a scalar signal.
    Signal {
        /// Signal name.
        name: String,
        /// Pre-formatted stringified value.
        value: String,
    },
    /// Replace an array signal with new rows.
    Array {
        /// Signal name.
        name: String,
        /// New rows, each a flat `field -> value` map matching the `<for>` template placeholders.
        items: Vec<ArrayItem>,
    },
    /// Clear the signal (scalar becomes empty string; array becomes empty vec).
    Clear {
        /// Signal name.
        name: String,
    },
}

static EXTERNAL_TX: OnceLock<Sender<ExternalMutation>> = OnceLock::new();
static EXTERNAL_RX: OnceLock<Mutex<Receiver<ExternalMutation>>> = OnceLock::new();

fn init_external_channel() -> &'static Sender<ExternalMutation> {
    EXTERNAL_TX.get_or_init(|| {
        let (tx, rx) = unbounded();
        let _ = EXTERNAL_RX.set(Mutex::new(rx));
        tx
    })
}

/// Idempotently initialises the external signal channel. Safe to call multiple times.
pub fn init_external_signals() {
    let _ = init_external_channel();
    // Also wire the typed-property channel so any caller that touches
    // `init_external_signals` for the legacy path implicitly also gets
    // the wave-D typed bus (FFI callers, async tasks).
    crate::property_store::init_external_properties();
}

/// Sends a scalar signal write from any thread. Wave-D: routes through
/// [`push_external_property`] so the value lands typed in [`PropertyStore`] under
/// `PropertyKey::Global(name)` on the next tick.
///
/// Returns `false` when the channel has disconnected.
pub fn push_external_signal(name: impl Into<String>, value: impl Into<String>) -> bool {
    let name_str = name.into();
    let value_str = value.into();
    let key = PropertyKey::Global(Arc::<str>::from(name_str.as_str()));
    let value = PropertyValue::Str(Arc::<str>::from(value_str.as_str()));
    push_external_property(key, value)
}

/// Sends an array mutation from any thread. Returns `false` when the channel has disconnected.
/// Array signals stay on the legacy [`ExternalMutation`] path until [`ArraySignals`] migrates to [`PropertyStore`].
pub fn push_external_array(name: impl Into<String>, items: Vec<ArrayItem>) -> bool {
    let tx = init_external_channel();
    tx.send(ExternalMutation::Array {
        name: name.into(),
        items,
    })
    .is_ok()
}

/// Sends a clear mutation from any thread. Returns `false` when the channel has disconnected.
/// The scalar half routes through [`push_external_property`] (empty string); the array half stays
/// on the legacy channel.
pub fn push_external_clear(name: impl Into<String>) -> bool {
    let tx = init_external_channel();
    let name_str = name.into();
    // Scalar clear: empty string on PropertyStore.
    let _ = push_external_property(
        PropertyKey::Global(Arc::<str>::from(name_str.as_str())),
        PropertyValue::Str(Arc::<str>::from("")),
    );
    // Array clear: legacy channel.
    tx.send(ExternalMutation::Clear { name: name_str }).is_ok()
}

/// Empties the external array channel, throwing away whatever it holds.
///
/// The array half of [`crate::property_store::discard_external_properties`]:
/// one process-global channel, so an app built after another one inherits the
/// mutations the first left queued unless they are dropped between the two.
pub fn discard_external_signals() {
    let Some(rx_lock) = EXTERNAL_RX.get() else {
        return;
    };
    let Ok(rx) = rx_lock.lock() else {
        return;
    };
    while rx.try_recv().is_ok() {}
}

/// Per-tick system that drains queued external Array / Clear mutations into [`ArraySignals`].
/// Scalar signal writes were routed through [`push_external_property`] in wave-D and are committed
/// to [`PropertyStore`] by [`crate::property_store::drain_external_properties`]; only the legacy
/// Array path remains here.
///
/// No-ops on an empty channel.
pub fn drain_external_signals(mut arrays: ResMut<ArraySignals>) {
    let Some(rx_lock) = EXTERNAL_RX.get() else {
        return;
    };
    let Ok(rx) = rx_lock.lock() else {
        return;
    };
    loop {
        match rx.try_recv() {
            Ok(ExternalMutation::Signal { name, value }) => {
                // Pre wave-D callers may still hand us a stringified scalar write; bounce it
                // through the typed bus so PropertyStore observes it on the next drain.
                let _ = push_external_property(
                    PropertyKey::Global(Arc::<str>::from(name.as_str())),
                    PropertyValue::Str(Arc::<str>::from(value.as_str())),
                );
            }
            Ok(ExternalMutation::Array { name, items }) => {
                arrays.set(name, items);
            }
            Ok(ExternalMutation::Clear { name }) => {
                arrays.set(name, Vec::new());
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::signal_is_truthy;

    #[test]
    fn only_empty_false_and_zero_are_falsy() {
        for value in ["", "false", "0"] {
            assert!(!signal_is_truthy(value), "`{value}` should be falsy");
        }
        for value in ["true", "1", "yes", "no", "False", "0.0", "00", " "] {
            assert!(signal_is_truthy(value), "`{value}` should be truthy");
        }
    }
}
