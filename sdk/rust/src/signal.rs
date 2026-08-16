//! Typed signal access for Lumen systems.
//!
//! Lumen state lives in a single reactive [`PropertyStore`] resource. This
//! module gives systems a *typed* view over it instead of the stringly
//! `get`/`set` of the v1 SDK:
//!
//! * [`Signals`] - a [`SystemParam`] any system can take. `signals.get::<i64>("count")`
//!   / `signals.set("count", 3)` read and write typed cells without touching
//!   [`PropertyKey`] / [`PropertyValue`] ceremony.
//! * [`crate::signals!`] - a declarative macro minting a zero-sized *handle
//!   struct* whose associated functions return typed [`Property`] handles, so
//!   signal names are checked once at the definition site rather than spelled
//!   as bare strings at every call.
//!
//! Prefer [`Property`] handles (via the macro) when the same signal is touched
//! from several systems; prefer [`Signals`] for one-off reads and writes.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core};

use bevy_ecs::system::{ResMut, SystemParam};
use lumen_core::property_store::{Property, PropertyKey, PropertyStore, PropertyValue};

/// Bound on any type that round-trips through the [`PropertyStore`]. Satisfied
/// by `i64`, `f64`, `f32`, `bool`, `String`, [`lumen_core::components::Color`],
/// and [`glam::Vec2`] out of the box.
pub trait Signal: TryFrom<PropertyValue> + Into<PropertyValue> + Clone {}
impl<T> Signal for T where T: TryFrom<PropertyValue> + Into<PropertyValue> + Clone {}

/// Typed [`SystemParam`] over the global [`PropertyStore`] namespace.
///
/// Take it in any system to read and write signals with real Rust types:
///
/// ```
/// use lumenui::prelude::*;
///
/// fn bump(mut signals: Signals) {
///     let n = signals.get_or::<i64>("count", 0) + 1;
///     signals.set("count", n);
/// }
/// # lumenui::bevy_ecs::system::assert_is_system(bump);
/// ```
#[derive(SystemParam)]
pub struct Signals<'w> {
    store: ResMut<'w, PropertyStore>,
}

impl Signals<'_> {
    /// Reads the global signal `name` as `T`, or `None` when it was never set.
    ///
    /// Conversions follow [`PropertyValue`]'s coercions (an `I64` cell read as
    /// `f64` converts; a `Str` cell read as `i64` parses).
    pub fn get<T: Signal>(&self, name: &str) -> Option<T> {
        Property::<T>::new(name).get(&self.store)
    }

    /// Reads the global signal `name` as `T`, falling back to `default`.
    pub fn get_or<T: Signal>(&self, name: &str, default: T) -> T {
        self.get(name).unwrap_or(default)
    }

    /// Reads the global signal `name` as `T`, falling back to `T::default()`.
    /// Terser than [`get_or`](Self::get_or) when zero / empty is the natural
    /// fallback: `signals.get_or_default::<i64>("count")`.
    pub fn get_or_default<T: Signal + Default>(&self, name: &str) -> T {
        self.get(name).unwrap_or_default()
    }

    /// Writes `value` into the global signal `name`. Visible to `bind-*` markup
    /// and later systems on the same tick.
    pub fn set<T: Into<PropertyValue>>(&mut self, name: &str, value: T) {
        self.store.set(PropertyKey::global(name), value.into());
    }

    /// Read-modify-write a signal in one call: reads `name` as `T` (or
    /// `T::default()` when unset), applies `f`, and writes the result back.
    ///
    /// Collapses the ubiquitous `let n = get_or(..); set(.., n + 1)` dance:
    ///
    /// ```
    /// use lumenui::prelude::*;
    ///
    /// fn bump(mut signals: Signals) {
    ///     signals.update::<i64>("count", |n| n + 1);
    /// }
    /// # lumenui::bevy_ecs::system::assert_is_system(bump);
    /// ```
    pub fn update<T: Signal + Default>(&mut self, name: &str, f: impl FnOnce(T) -> T) {
        let next = f(self.get_or_default::<T>(name));
        self.set(name, next);
    }

    /// Like [`update`](Self::update) but with an explicit fallback for an
    /// absent signal instead of `T::default()`.
    pub fn update_or<T: Signal>(&mut self, name: &str, default: T, f: impl FnOnce(T) -> T) {
        let next = f(self.get_or::<T>(name, default));
        self.set(name, next);
    }

    /// Flip a boolean signal (absent reads as `false`) and return the new
    /// value. Handy for `<toggle>` / show-hide state driven from a system.
    pub fn toggle(&mut self, name: &str) -> bool {
        let next = !self.get_or::<bool>(name, false);
        self.set(name, next);
        next
    }

    /// Mints a typed [`Property`] handle for `name` without reading or writing.
    pub fn handle<T: Signal>(&self, name: &str) -> Property<T> {
        Property::new(name)
    }

    /// Returns `true` when a cell exists for the global signal `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.store.contains(&PropertyKey::global(name))
    }

    /// Shared borrow of the underlying [`PropertyStore`] for reads the typed
    /// helpers do not cover (entity-scoped keys, cell generations).
    pub fn store(&self) -> &PropertyStore {
        &self.store
    }

    /// Mutable borrow of the underlying [`PropertyStore`].
    pub fn store_mut(&mut self) -> &mut PropertyStore {
        &mut self.store
    }

    // -- File-based-pages navigation ------------------------------------------
    //
    // Navigation rides the shared `lumen_core::nav` bus - the SAME command
    // surface the script `page()` builtin and the C-ABI `lumen_navigate`
    // reach. A system navigates by calling these; the runtime's resolver
    // switches the active page (and its `route.path` / `route.segment`
    // signals) on the next tick.

    /// Navigate the active page to `path` (`"settings"`, `"/user/7"`, `"/"`).
    /// Resolved by longest existing `.lmn` prefix - the framework does not
    /// pattern-match segments. Equivalent to the script `page("...")`.
    pub fn navigate(&self, path: impl Into<String>) {
        lumen_core::nav::navigate(path.into());
    }

    /// The current active page key (the resolved `.lmn` stem). Empty before
    /// the first page mounts. You can also read the reserved `route.path`
    /// signal directly (`signals.get::<String>("route.path")`).
    pub fn current_page(&self) -> String {
        lumen_core::nav::current()
    }

    /// Step one entry back in the in-memory history stack.
    pub fn navigate_back(&self) {
        lumen_core::nav::back();
    }

    /// Step one entry forward in the in-memory history stack.
    pub fn navigate_forward(&self) {
        lumen_core::nav::forward();
    }
}

/// Declares a typed *signal handle struct* mapping struct fields to global
/// signal names.
///
/// Each field `name: T` becomes an associated function `Name::name() -> Property<T>`
/// returning a typed handle bound to the global signal `"name"`. This keeps the
/// signal's name and type in one place instead of re-spelling the string at
/// every `get`/`set`:
///
/// ```
/// use lumenui::prelude::*;
///
/// signals! {
///     /// Handles for the counter app's signals.
///     pub struct Counter {
///         count: i64,
///         label: String,
///     }
/// }
///
/// fn read(store: &PropertyStore) -> i64 {
///     Counter::count().get(store).unwrap_or(0)
/// }
/// ```
///
/// This is the declarative (`macro_rules!`) equivalent of a `#[derive(Signals)]`
/// proc macro - it needs no separate crate and produces the same typed-handle
/// surface.
#[macro_export]
macro_rules! signals {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fmeta:meta])*
                $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[doc = "Typed signal-handle struct generated by [`signals!`](lumenui::signals)."]
        #[derive(Clone, Copy, Debug, Default)]
        $vis struct $name;

        impl $name {
            $(
                $(#[$fmeta])*
                #[doc = concat!("Typed handle for the global signal `", stringify!($field), "`.")]
                $vis fn $field() -> $crate::Property<$ty> {
                    $crate::Property::new(stringify!($field))
                }
            )*
        }
    };
}
