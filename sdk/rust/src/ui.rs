//! What a handler works with: the [`Ui`] context, the [`Record`]s a `<for>`
//! list is made of, and the [`CompiledUi`] an app is built from.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core};

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::*;
use bevy_ecs::world::Mut;
use lumen_core::property_store::{Property, PropertyKey, PropertyStore, PropertyValue};
use lumen_core::signals::ArraySignals;

use crate::dom::{self, Node, NodeQuery};
use crate::signal::Signal;

/// The app, as a handler sees it: its signals, its lists, its element tree,
/// its menus, and the state it keeps in resources.
///
/// Every write lands on the tick the handler runs, so what the handler wrote
/// is on screen when the tick paints.
pub struct Ui<'w> {
    world: &'w mut World,
}

impl<'w> Ui<'w> {
    pub(crate) fn new(world: &'w mut World) -> Self {
        Self { world }
    }

    // -- Signals -----------------------------------------------------------

    /// The signal `name` as `T`; the type's default when it was never set
    /// or does not convert. A number field's text reads as a number.
    pub fn get<T: Signal + Default>(&self, name: &str) -> T {
        self.try_get(name).unwrap_or_default()
    }

    /// The signal `name` as `T`, or `None` when it was never set or does not
    /// convert.
    pub fn try_get<T: Signal>(&self, name: &str) -> Option<T> {
        Property::<T>::new(name).get(self.world.resource::<PropertyStore>())
    }

    /// Write `value` into the signal `name`.
    pub fn set(&mut self, name: &str, value: impl Into<PropertyValue>) {
        self.world
            .resource_mut::<PropertyStore>()
            .set(PropertyKey::global(name), value.into());
    }

    /// Read the signal `name` (its default when unset), apply `f`, and write
    /// the result back.
    pub fn update<T: Signal + Default>(&mut self, name: &str, f: impl FnOnce(T) -> T) {
        let next = f(self.get(name));
        self.set(name, next);
    }

    /// Flip the boolean signal `name` (unset reads as `false`) and return
    /// its new value.
    pub fn toggle(&mut self, name: &str) -> bool {
        let next = !self.get::<bool>(name);
        self.set(name, next);
        next
    }

    // -- Lists -------------------------------------------------------------

    /// Replace the array signal `name`, which a `<for each="$name">` renders
    /// one row per record of.
    pub fn set_list(&mut self, name: &str, rows: impl IntoIterator<Item = Record>) {
        let items = rows.into_iter().map(|r| r.0).collect();
        self.arrays().set(name, items);
    }

    /// Append one record to the array signal `name`.
    pub fn push_row(&mut self, name: &str, row: Record) {
        let mut items = self
            .world
            .get_resource::<ArraySignals>()
            .and_then(|a| a.get(name))
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        items.push(row.0);
        self.arrays().set(name, items);
    }

    /// How many records the array signal `name` holds.
    pub fn list_len(&self, name: &str) -> usize {
        self.world
            .get_resource::<ArraySignals>()
            .and_then(|a| a.get(name))
            .map_or(0, <[_]>::len)
    }

    fn arrays(&mut self) -> Mut<'_, ArraySignals> {
        if !self.world.contains_resource::<ArraySignals>() {
            self.world.init_resource::<ArraySignals>();
        }
        self.world.resource_mut::<ArraySignals>()
    }

    // -- The element tree ----------------------------------------------------

    /// The element with `id="..."`.
    pub fn get_by_id(&self, id: &str) -> Option<Node> {
        dom::get_by_id(id)
    }

    /// Every element matching a CSS selector, in document order.
    pub fn query(&self, selector: &str) -> NodeQuery {
        dom::query(selector)
    }

    /// A new detached element; attach it with [`Node::append`].
    pub fn create(&self, tag: &str) -> Node {
        dom::spawn(tag)
    }

    // -- Menus and pages ---------------------------------------------------

    /// Open the `<menu id>` popup where its styles place it.
    pub fn open_menu(&mut self, id: &str) {
        let mut store = self.world.resource_mut::<PropertyStore>();
        store.set_global_str(&format!("__menu_at:{id}"), "");
        store.set_global_bool(&format!("__menu_open:{id}"), true);
    }

    /// Open the `<menu id>` popup with its top-left corner at `(x, y)` in
    /// window coordinates, moved inward where it would leave the window. A
    /// context-menu handler passes [`Event::position`](crate::Event::position).
    pub fn open_menu_at(&mut self, id: &str, x: f32, y: f32) {
        let mut store = self.world.resource_mut::<PropertyStore>();
        store.set_global_str(&format!("__menu_at:{id}"), format!("{x} {y}"));
        store.set_global_bool(&format!("__menu_open:{id}"), true);
    }

    /// Close the `<menu id>` popup.
    pub fn close_menu(&mut self, id: &str) {
        self.world
            .resource_mut::<PropertyStore>()
            .set_global_bool(&format!("__menu_open:{id}"), false);
    }

    /// Open the `<menu id>` popup when it is closed and close it when it is
    /// open.
    pub fn toggle_menu(&mut self, id: &str) {
        let open = self
            .world
            .resource::<PropertyStore>()
            .get_global_bool(&format!("__menu_open:{id}"))
            .unwrap_or(false);
        if open {
            self.close_menu(id);
        } else {
            self.open_menu(id);
        }
    }

    /// Show the page at `path` (`"settings"`, `"/user/7"`, `"/"`).
    pub fn navigate(&mut self, path: impl Into<String>) {
        lumen_core::nav::navigate(path.into());
    }

    /// Open a native file dialog; `then` runs on a later tick, after it
    /// closes, with the chosen paths (none when it was cancelled).
    pub fn show_dialog(
        &mut self,
        dialog: crate::dialog::FileDialog,
        then: impl FnOnce(&mut Ui<'_>, Vec<PathBuf>) + Send + 'static,
    ) -> crate::dialog::RequestId {
        dialog.show(self.world, move |world, paths| {
            then(&mut Ui::new(world), paths);
        })
    }

    // -- App state -----------------------------------------------------------

    /// The app's resource `T`, inserted with
    /// [`App::insert_resource`](crate::App::insert_resource).
    ///
    /// # Panics
    ///
    /// When the app never inserted one.
    pub fn resource<T: Resource>(&self) -> &T {
        self.world.resource::<T>()
    }

    /// The app's resource `T`, to change.
    ///
    /// # Panics
    ///
    /// When the app never inserted one.
    pub fn resource_mut<T: Resource<Mutability = bevy_ecs::component::Mutable>>(
        &mut self,
    ) -> Mut<'_, T> {
        self.world.resource_mut::<T>()
    }

    /// The app's resource `T`, or `None` when it has none.
    pub fn get_resource<T: Resource>(&self) -> Option<&T> {
        self.world.get_resource::<T>()
    }

    /// Run `f` with the resource `T` taken out of the app, so it can change
    /// `T` and the rest of the app (its signals, its tree) in one go.
    ///
    /// # Panics
    ///
    /// When the app never inserted one.
    pub fn resource_scope<T, R>(&mut self, f: impl FnOnce(&mut Ui<'_>, Mut<'_, T>) -> R) -> R
    where
        T: Resource<Mutability = bevy_ecs::component::Mutable>,
    {
        self.world
            .resource_scope(|world, value: Mut<'_, T>| f(&mut Ui::new(world), value))
    }

    /// The ECS world itself, for what the methods above do not cover.
    pub fn world(&mut self) -> &mut World {
        self.world
    }
}

/// One row of a `<for>` list: field names to values, which the row's
/// `{row.field}` placeholders read.
///
/// ```
/// # extern crate lumenui_engine as lumenui;
/// use lumenui::Record;
///
/// let row = Record::new().with("id", 3).with("name", "Ink").with("visible", true);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record(HashMap<String, String>);

impl Record {
    /// An empty record.
    pub fn new() -> Self {
        Self::default()
    }

    /// This record with the field `name` set to `value`, as text.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.0.insert(name.into(), value.to_string());
        self
    }

    /// Set the field `name` to `value`, as text.
    pub fn set(&mut self, name: impl Into<String>, value: impl ToString) {
        self.0.insert(name.into(), value.to_string());
    }
}

impl From<HashMap<String, String>> for Record {
    fn from(fields: HashMap<String, String>) -> Self {
        Self(fields)
    }
}

impl<K: Into<String>, V: ToString> FromIterator<(K, V)> for Record {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.to_string()))
                .collect(),
        )
    }
}

/// The app's UI, compiled to a `.lmna` artifact from what `lumen.toml` names.
///
/// [`lumenui::ui!()`](crate::ui) makes one from what the app's build script
/// compiled; [`CompiledUi::compile`] compiles an app directory on the spot.
#[derive(Clone, Debug)]
pub struct CompiledUi {
    pub(crate) bytes: Cow<'static, [u8]>,
    /// The directory the UI was compiled from.
    pub(crate) built_in: Option<PathBuf>,
    /// Watch `built_in` for edits and reload from it.
    pub(crate) watch: bool,
}

impl CompiledUi {
    /// What [`lumenui::ui!()`](crate::ui) expands to: the artifact the build
    /// script wrote, the crate directory it compiled, and whether the build
    /// is a debug one, which watches that directory and reloads on an edit.
    #[doc(hidden)]
    pub fn from_build(bytes: &'static [u8], crate_dir: &'static str, debug: bool) -> Self {
        Self {
            bytes: Cow::Borrowed(bytes),
            built_in: Some(PathBuf::from(crate_dir)),
            watch: debug,
        }
    }

    /// Compile the app in `dir` now, the way its build script would: for a
    /// test or a tool that runs an app it was not built with.
    ///
    /// # Errors
    ///
    /// The compiler's error, when the UI does not compile.
    pub fn compile(dir: impl AsRef<Path>) -> crate::Result<Self> {
        let dir = dir.as_ref();
        let compiled = crate::lumenc::compile_app_ui(dir).map_err(crate::Error::Run)?;
        let bytes = crate::lumenc::artifact::serialize(&compiled)
            .map_err(|e| crate::Error::Setup(format!("encode the compiled UI: {e}")))?;
        Ok(Self {
            bytes: Cow::Owned(bytes),
            built_in: Some(dir.to_path_buf()),
            watch: false,
        })
    }

    /// The directory the UI was compiled from, when known.
    pub fn source_dir(&self) -> Option<&Path> {
        self.built_in.as_deref()
    }

    /// Turn reloading on an edit to the UI's sources on or off. On by
    /// default in a debug build, off in a release build.
    #[must_use]
    pub fn hot_reload(mut self, on: bool) -> Self {
        self.watch = on;
        self
    }
}
