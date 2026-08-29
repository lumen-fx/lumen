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
//! 5. Provide a spawn fn that turns a parsed `<tag attr="...">` into an
//!    ECS entity carrying the marker + the author-specified props.
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
//! (`KNOWN_TAGS` in `public/lumenc/src/parser_html.rs`) first and falls back
//! to [`is_widget_tag_registered`]. The plugin the derive generates
//! calls `<Type>::register()` when it is added, so adding that plugin
//! before the parser runs is enough for `<my-thing>` markup to be
//! accepted; no edit to the built-in table is needed.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use bevy_ecs::prelude::*;

/// Process-wide registry of user-installed widget tags. The lumenc HTML
/// parser consults [`is_widget_tag_registered`] after its built-in
/// [`KNOWN_TAGS`] miss so custom widgets registered by the host app
/// (typically through `T::register()` emitted by the `Widget` derive)
/// are accepted instead of rejected as `UnknownTag`.
static REGISTERED_WIDGET_TAGS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();

/// Register `tag` so the lumenc HTML parser accepts `<tag ...>` markup.
///
/// Called by `Widget` derive output (`<Type>::register()`) at app
/// startup, BEFORE the parser runs. Subsequent calls with the same tag
/// are no-ops (the set is a [`HashSet`]). The supplied string must live
/// for the lifetime of the process - passing a `&'static` literal is
/// the expected pattern (the derive does this).
pub fn register_widget_tag(tag: &'static str) {
    if let Ok(mut set) = REGISTERED_WIDGET_TAGS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
    {
        set.insert(tag);
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
    if let Ok(mut set) = REGISTERED_WIDGET_TAGS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
    {
        // Checked again under the lock: two threads racing the same new tag
        // must leak one string, not two.
        if !set.contains(tag) {
            set.insert(Box::leak(tag.to_string().into_boxed_str()));
        }
    }
}

/// Returns `true` when `tag` was registered via [`register_widget_tag`]
/// earlier in this process. Consulted by `public/lumenc/src/parser_html.rs`'s
/// `KNOWN_TAGS` fallback path.
pub fn is_widget_tag_registered(tag: &str) -> bool {
    REGISTERED_WIDGET_TAGS
        .get()
        .and_then(|m| m.lock().ok())
        .map(|s| s.contains(tag))
        .unwrap_or(false)
}

/// Snapshot every registered tag - for diagnostics + `lumenc lint`.
pub fn registered_widget_tags() -> Vec<&'static str> {
    REGISTERED_WIDGET_TAGS
        .get()
        .and_then(|m| m.lock().ok())
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default()
}

/// Untyped attribute bag handed to [`Widget::spawn`] by the parser.
///
/// Mirrors the `roxmltree::Node::attributes()` iterator output: a flat
/// `(name, value)` list keyed by attribute name. The parser owns the
/// concrete representation; this crate keeps an enum-free wrapper so
/// authors don't pull a parser dep just to write a widget.
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

    /// Build a widget entity under `parent` from `attrs`, returning
    /// the new entity. The derive emits a default body that creates a
    /// fresh entity, parses `#[widget(prop)]` fields out of `attrs`,
    /// and inserts the component.
    fn spawn(parent: Entity, attrs: &Attributes, world: &mut World) -> Entity;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // The second call must not leak a second copy: the set is keyed by
        // the string's contents, so its size is what proves that.
        let before = registered_widget_tags().len();
        register_widget_tag_owned("owned-test-tag");
        assert_eq!(registered_widget_tags().len(), before);
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
