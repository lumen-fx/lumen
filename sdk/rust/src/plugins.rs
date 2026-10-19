//! Plugin groups for an app's own plugins, modelled on bevy's `PluginGroup` /
//! `PluginGroupBuilder`.
//!
//! The engine needs no plugin list from the app: [`App::new`](crate::App::new)
//! runs the whole stack, and `lumen.toml` says what the app uses. These types
//! are for bundling the app's own [`Plugin`]s: implement
//! [`PluginGroup::build`], hand the built group to
//! [`App::add_plugins`](crate::App::add_plugins), and `disable` / `set` /
//! `add_after` compose plugins installed in order.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::lumen_core;

use crate::app::App;
use lumen_core::app::{App as EcsApp, Plugin};
use std::any::TypeId;

// --- Object-safe plugin wrapper ----------------------------------------------

/// Object-safe view of a [`Plugin`] so heterogeneous plugins can live in a
/// group's `Vec`. Blanket-implemented for every [`Plugin`]; you never write
/// this by hand.
pub trait BoxedPlugin: Send + 'static {
    /// Install the plugin, consuming its box.
    fn install(self: Box<Self>, app: &mut EcsApp);
    /// The concrete plugin type id, used for `disable` / `set` lookups.
    fn plugin_type_id(&self) -> TypeId;
    /// The plugin's [`Plugin::name`], for diagnostics.
    fn plugin_name(&self) -> &'static str;
}

impl<P: Plugin + Send + 'static> BoxedPlugin for P {
    fn install(self: Box<Self>, app: &mut EcsApp) {
        app.add_plugin(*self);
    }
    fn plugin_type_id(&self) -> TypeId {
        TypeId::of::<P>()
    }
    fn plugin_name(&self) -> &'static str {
        Plugin::name(self)
    }
}

// --- Plugin groups -------------------------------------------------------------

/// A named, ordered collection of [`Plugin`]s that install together.
///
/// Implement it for your own bundle, then pass `MyPlugins.build()` to
/// [`App::add_plugins`](crate::App::add_plugins):
///
/// ```
/// # extern crate lumenui_engine as lumenui;
/// use lumenui::prelude::*;
/// use lumenui::plugins::{PluginGroup, PluginGroupBuilder};
///
/// # struct Physics; impl Plugin for Physics { fn build(self, _: &mut EcsApp) {} }
/// # struct Telemetry; impl Plugin for Telemetry { fn build(self, _: &mut EcsApp) {} }
/// struct GamePlugins;
/// impl PluginGroup for GamePlugins {
///     fn build(self) -> PluginGroupBuilder {
///         PluginGroupBuilder::new("GamePlugins")
///             .add(Physics)
///             .add(Telemetry)
///     }
/// }
/// ```
pub trait PluginGroup: Sized {
    /// Enumerate the group's plugins into a [`PluginGroupBuilder`].
    fn build(self) -> PluginGroupBuilder;
}

struct GroupEntry {
    type_id: TypeId,
    name: &'static str,
    plugin: Box<dyn BoxedPlugin>,
    enabled: bool,
}

/// Ordered, editable list of plugins produced by [`PluginGroup::build`].
///
/// Mirrors bevy's `PluginGroupBuilder`: [`add`](Self::add) appends,
/// [`disable`](Self::disable) / [`enable`](Self::enable) toggle an entry by
/// type, [`set`](Self::set) swaps an entry's implementation in place, and
/// [`add_before`](Self::add_before) / [`add_after`](Self::add_after) insert
/// relative to an existing entry.
pub struct PluginGroupBuilder {
    group_name: &'static str,
    entries: Vec<GroupEntry>,
}

impl PluginGroupBuilder {
    /// Start an empty builder tagged with a group name (used in diagnostics).
    pub fn new(group_name: &'static str) -> Self {
        Self {
            group_name,
            entries: Vec::new(),
        }
    }

    fn index_of<P: 'static>(&self) -> Option<usize> {
        let tid = TypeId::of::<P>();
        self.entries.iter().position(|e| e.type_id == tid)
    }

    fn entry_of<P: Plugin + Send + 'static>(plugin: P) -> GroupEntry {
        GroupEntry {
            type_id: TypeId::of::<P>(),
            name: Plugin::name(&plugin),
            plugin: Box::new(plugin),
            enabled: true,
        }
    }

    /// Append `plugin` to the end of the group.
    #[must_use]
    #[allow(clippy::should_implement_trait)] // bevy-parity method name
    pub fn add<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        self.entries.push(Self::entry_of(plugin));
        self
    }

    /// Insert `plugin` immediately before the existing entry of type `Target`.
    /// Falls back to appending when `Target` is absent.
    #[must_use]
    pub fn add_before<Target: 'static, P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        let entry = Self::entry_of(plugin);
        match self.index_of::<Target>() {
            Some(i) => self.entries.insert(i, entry),
            None => self.entries.push(entry),
        }
        self
    }

    /// Insert `plugin` immediately after the existing entry of type `Target`.
    /// Falls back to appending when `Target` is absent.
    #[must_use]
    pub fn add_after<Target: 'static, P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        let entry = Self::entry_of(plugin);
        match self.index_of::<Target>() {
            Some(i) => self.entries.insert(i + 1, entry),
            None => self.entries.push(entry),
        }
        self
    }

    /// Replace the entry of type `P` with a fresh instance, keeping its
    /// position. Appends when `P` is absent.
    #[must_use]
    pub fn set<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        match self.index_of::<P>() {
            Some(i) => self.entries[i] = Self::entry_of(plugin),
            None => self.entries.push(Self::entry_of(plugin)),
        }
        self
    }

    /// Disable the entry of type `P`; it stays in the list (preserving order
    /// for later `add_before` / `add_after`) but is skipped at install.
    #[must_use]
    pub fn disable<P: 'static>(mut self) -> Self {
        if let Some(i) = self.index_of::<P>() {
            self.entries[i].enabled = false;
        }
        self
    }

    /// Re-enable a previously [`disable`](Self::disable)d entry of type `P`.
    #[must_use]
    pub fn enable<P: 'static>(mut self) -> Self {
        if let Some(i) = self.index_of::<P>() {
            self.entries[i].enabled = true;
        }
        self
    }

    /// The group's name.
    pub fn group_name(&self) -> &'static str {
        self.group_name
    }

    /// Names of the plugins that would install, in order.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        self.entries.iter().filter(|e| e.enabled).map(|e| e.name)
    }

    /// Install every enabled plugin into `app`, in order.
    pub fn finish(self, app: &mut EcsApp) {
        for entry in self.entries {
            if entry.enabled {
                entry.plugin.install(app);
            }
        }
    }
}

// --- AppPlugins: what App::add_plugins accepts -------------------------------

/// Anything installable via [`App::add_plugins`](crate::App::add_plugins): a
/// [`PluginGroupBuilder`], or a tuple of them.
pub trait AppPlugins {
    /// Apply this plugin set to `app`.
    fn apply(self, app: &mut App);
}

impl AppPlugins for PluginGroupBuilder {
    fn apply(self, app: &mut App) {
        app.push_deferred(move |ecs| self.finish(ecs));
    }
}

macro_rules! tuple_app_plugins {
    ($($T:ident),+) => {
        impl<$($T: AppPlugins),+> AppPlugins for ($($T,)+) {
            #[allow(non_snake_case)]
            fn apply(self, app: &mut App) {
                let ($($T,)+) = self;
                $($T.apply(app);)+
            }
        }
    };
}
tuple_app_plugins!(A);
tuple_app_plugins!(A, B);
tuple_app_plugins!(A, B, C);
tuple_app_plugins!(A, B, C, D);
tuple_app_plugins!(A, B, C, D, E);
