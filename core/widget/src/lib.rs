//! Widget trait + attribute bag - the thin surface implemented by
//! `#[derive(Widget)]` (see the sibling `lumen-widget-macros` crate).
//!
//! ## Why this crate
//!
//! Authoring a custom Lumen widget today repeats the same five-step
//! boilerplate per primitive (hover, scroll, tooltip, transition,
//! validation ... all of `lumen-primitives`):
//!
//! 1. Define a marker [`bevy_ecs::component::Component`].
//! 2. Define a `Plugin` struct.
//! 3. In `Plugin::build`, register N systems against one or more
//!    [`lumen_core::tick::TickStage`] sets.
//! 4. Tell the markup parser about the new tag. The plugin the derive
//!    generates calls `<Type>::register()` in its `build`, publishing
//!    `Widget::parser_tag` into the registry the parser consults after
//!    its built-in table.
//! 5. Provide a spawn fn that turns the element spawned for a parsed
//!    `<tag attr="...">` into the widget: the marker + the author-specified
//!    props.
//!
//! The first three steps are repeated verbatim per widget. This crate
//! defines the minimal surface that the `Widget` derive macro
//! implements so authors only write the unique parts: the prop /
//! state fields and the per-tick system bodies.
//!
//! ## The trait
//!
//! ```ignore
//! use lumen_widget::{Attributes, Widget};
//! use lumen_widget_macros::Widget;
//! use bevy_ecs::prelude::*;
//!
//! #[derive(Component, Widget, Default)]
//! #[widget(tag = "tooltip")]
//! pub struct Tooltip {
//!     #[widget(prop)] pub text: String,
//!     #[widget(state)] pub visible: bool,
//! }
//! ```
//!
//! The derive emits:
//!
//! - `impl Plugin for TooltipPlugin` - a zero-sized struct that the
//!   author adds to their `App`. Default `build` body inserts the
//!   marker resource and registers any author-supplied systems via the
//!   `#[widget(systems = "fn1,fn2")]` attribute (forward-looking).
//! - `impl Widget for Tooltip` - connects the marker component, the
//!   parser tag, and the spawn glue.
//! - A `spawn` fn that walks an [`Attributes`] bag, populates the prop
//!   fields, and inserts the component on the supplied entity.
//!
//! ## Scope of v1
//!
//! The macro covers Plugin scaffolding + entity-spawn glue. Widget-
//! specific systems (the `record_hover_started` / `spawn_popup` chain
//! for tooltips) stay hand-written. A future iteration may grow the
//! `#[widget(systems = ...)]` attribute to register them automatically.
//!
//! ## Parser integration
//!
//! The lumenc HTML parser checks its built-in tag table
//! (`KNOWN_TAGS` in `public/lumenc/src/parse/html.rs`) first and falls back
//! to [`is_widget_tag_registered`]. The plugin the derive generates
//! calls `<Type>::register()` when it is added, so adding that plugin
//! before the parser runs is enough for `<my-thing>` markup to be
//! accepted; no edit to the built-in table is needed.
//!
//! The parser keeps every attribute such an element was written with, and
//! the spawner, once it has spawned the element, looks the tag up with
//! [`widget_spawner`] and hands the element and its [`Attributes`] to the
//! widget's [`Widget::spawn`]. So `<my-thing greeting="hi"/>` ends up an
//! element carrying the widget's component with `greeting` set.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bevy_ecs::prelude::*;

/// What builds the widget an element with a registered tag stands for: the
/// tag's [`Widget::spawn`].
pub type WidgetSpawnFn = fn(Entity, &Attributes, &mut World);

/// Process-wide registry of user-installed widget tags. The lumenc HTML
/// parser consults [`is_widget_tag_registered`] after its built-in
/// `KNOWN_TAGS` miss so custom widgets registered by the host app
/// (typically through `T::register()` emitted by the `Widget` derive)
/// are accepted instead of rejected as `UnknownTag`, and the spawner asks
/// [`widget_spawner`] what to build on the element it spawned for one.
///
/// A tag registered by name alone maps to `None`: something else (a runtime
/// module's systems, say) gives the element its behavior.
static REGISTERED_WIDGET_TAGS: OnceLock<Mutex<HashMap<&'static str, Option<WidgetSpawnFn>>>> =
    OnceLock::new();

fn registry() -> &'static Mutex<HashMap<&'static str, Option<WidgetSpawnFn>>> {
    REGISTERED_WIDGET_TAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register `tag` so the lumenc HTML parser accepts `<tag ...>` markup.
///
/// For a tag whose element needs no [`Widget`] built on it. A widget calls
/// [`register_widget`] instead, which also says what to build. Subsequent
/// calls with the same tag are no-ops, and never take a registered widget's
/// spawn away. The supplied string must live for the lifetime of the
/// process.
pub fn register_widget_tag(tag: &'static str) {
    if let Ok(mut map) = registry().lock() {
        map.entry(tag).or_insert(None);
    }
}

/// Register `W`: the lumenc HTML parser accepts `<tag ...>` for its
/// [`Widget::parser_tag`], and every element spawned for that tag is handed,
/// with its authored attributes, to [`Widget::spawn`].
///
/// Called by `Widget` derive output (`<Type>::register()`, which the
/// generated plugin's `build` calls) at app startup, before the parser runs.
/// Registering again is a no-op; a second widget registered for the same tag
/// replaces the first.
pub fn register_widget<W: Widget>() {
    if let Ok(mut map) = registry().lock() {
        map.insert(W::parser_tag(), Some(W::spawn));
    }
}

/// Register a tag whose name is only known at run time, so `<tag ...>`
/// markup is accepted.
///
/// The derive knows its tag at compile time and passes a `&'static str`;
/// a name read out of a file does not exist until the file is read. This
/// takes the name by reference, checks the set first, and leaks the string
/// only when the tag is new - a bounded, once-per-tag cost, because the
/// registry outlives every caller and the parser consults it for the life
/// of the process.
pub fn register_widget_tag_owned(tag: &str) {
    if is_widget_tag_registered(tag) {
        return;
    }
    if let Ok(mut map) = registry().lock() {
        // Checked again under the lock: two threads racing the same new tag
        // must leak one string, not two.
        if !map.contains_key(tag) {
            map.insert(Box::leak(tag.to_string().into_boxed_str()), None);
        }
    }
}

/// Returns `true` when `tag` was registered via [`register_widget_tag`],
/// [`register_widget`] or [`register_widget_tag_owned`] earlier in this
/// process. Consulted by `public/lumenc/src/parse/html.rs`'s `KNOWN_TAGS`
/// fallback path.
pub fn is_widget_tag_registered(tag: &str) -> bool {
    REGISTERED_WIDGET_TAGS
        .get()
        .and_then(|m| m.lock().ok())
        .is_some_and(|m| m.contains_key(tag))
}

/// The [`Widget::spawn`] registered for `tag`, if a [`Widget`] was
/// registered for it with [`register_widget`].
pub fn widget_spawner(tag: &str) -> Option<WidgetSpawnFn> {
    REGISTERED_WIDGET_TAGS
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|m| m.get(tag).copied().flatten())
}

/// Untyped attribute bag handed to [`Widget::spawn`]: every attribute the
/// element was written with in markup.
///
/// A flat `(name, value)` map keyed by attribute name, the values as
/// authored. The parser owns the concrete representation; this crate keeps an
/// enum-free wrapper so authors don't pull a parser dep just to write a
/// widget.
#[derive(Debug, Default, Clone)]
pub struct Attributes {
    inner: HashMap<String, String>,
}

impl Attributes {
    /// Build an empty attribute bag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a single attribute. Last write wins.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.inner.insert(key.into(), value.into());
        self
    }

    /// Look up an attribute by name.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    /// Look up an attribute, falling back to the supplied default.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    /// Parse an attribute into `T` via [`std::str::FromStr`]. Returns
    /// `None` when the attribute is absent or fails to parse.
    pub fn parse<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.get(key)?.parse().ok()
    }

    /// Number of attributes set.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when no attributes are set.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl<K, V> From<Vec<(K, V)>> for Attributes
where
    K: Into<String>,
    V: Into<String>,
{
    fn from(pairs: Vec<(K, V)>) -> Self {
        let mut a = Attributes::new();
        for (k, v) in pairs {
            a.set(k, v);
        }
        a
    }
}

impl<K, V, const N: usize> From<[(K, V); N]> for Attributes
where
    K: Into<String>,
    V: Into<String>,
{
    fn from(pairs: [(K, V); N]) -> Self {
        let mut a = Attributes::new();
        for (k, v) in pairs {
            a.set(k, v);
        }
        a
    }
}

/// Thin surface implemented by `#[derive(Widget)]`.
///
/// The derive emits the plugin struct + the parser-glue impl; widget
/// authors only fill in the per-widget systems.
pub trait Widget: Sized + Send + Sync + 'static {
    /// Display name - defaults to the Rust type name in the derive.
    fn name() -> &'static str;

    /// Markup tag handled by the widget. The derive picks this up from
    /// `#[widget(tag = "...")]`. `<Type>::register()` publishes it into
    /// the registry behind [`is_widget_tag_registered`], which is what
    /// makes the lumenc parser accept the tag.
    fn parser_tag() -> &'static str;

    /// Make `entity` this widget, reading its props out of `attrs`.
    ///
    /// The spawner calls this for every element whose tag the widget was
    /// registered for ([`register_widget`]), once the element's own
    /// components are in place and before its children spawn: `entity` is
    /// that element and `attrs` every attribute it was written with. The
    /// derive emits a body that parses the `#[widget(prop)]` fields out of
    /// `attrs` over the struct's `Default` and inserts the component on
    /// `entity`. Outside markup, spawn the entity first
    /// (`world.spawn_empty().id()`) and hand it in.
    fn spawn(entity: Entity, attrs: &Attributes, world: &mut World);
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tag set is process-global and every test in the binary shares it,
    // so a test asks about the tag it registered rather than the whole set,
    // which anything running beside it can change.

    #[test]
    fn attributes_round_trip() {
        let mut a = Attributes::new();
        a.set("text", "Save").set("delay", "500");
        assert_eq!(a.get("text"), Some("Save"));
        assert_eq!(a.parse::<u32>("delay"), Some(500));
        assert_eq!(a.parse::<u32>("missing"), None);
        assert_eq!(a.get_or("missing", "fallback"), "fallback");
        assert_eq!(a.len(), 2);
        assert!(!a.is_empty());
    }

    #[test]
    fn attributes_from_array() {
        let a: Attributes = [("text", "Hi"), ("delay", "100")].into();
        assert_eq!(a.get("text"), Some("Hi"));
        assert_eq!(a.parse::<u32>("delay"), Some(100));
    }

    #[test]
    fn an_owned_tag_registers_once_and_is_accepted() {
        assert!(!is_widget_tag_registered("owned-test-tag"));
        register_widget_tag_owned(&String::from("owned-test-tag"));
        assert!(is_widget_tag_registered("owned-test-tag"));
        // A second call for a tag already in the set is a no-op.
        register_widget_tag_owned("owned-test-tag");
        assert!(is_widget_tag_registered("owned-test-tag"));
    }

    #[test]
    fn racing_registrations_of_one_tag_all_succeed() {
        // The set is checked before the lock is taken, so two callers can
        // both find the tag absent. Only one of them may put it in.
        let threads: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| register_widget_tag_owned("owned-race-tag")))
            .collect();
        for t in threads {
            t.join().expect("the registration does not panic");
        }
        assert!(is_widget_tag_registered("owned-race-tag"));
    }

    #[test]
    fn attributes_from_vec() {
        let a: Attributes = vec![
            ("text".to_string(), "Hi".to_string()),
            ("count".to_string(), "3".to_string()),
        ]
        .into();
        assert_eq!(a.get("count"), Some("3"));
    }
}
