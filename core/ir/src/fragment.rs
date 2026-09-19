//! Fragments - the reusable markup subtree.
//!
//! A fragment is a named piece of markup with parameters, declared once and
//! instantiated anywhere. Lumen has one such entity, and both authoring forms
//! compile to it: a `<template>` in a `.lmn` file, and an `lmn!` block in a
//! script. The two forms differ in where the text came from, which
//! [`FragmentKind`] records; everything downstream of the compiler sees the
//! same [`Fragment`].
//!
//! The declaration side lives here. The use site lives on the element that
//! instantiates a fragment, as [`crate::layout_ir::FragmentUse`], so a tree
//! carries its own instantiation points and the table stays a flat lookup.
//!
//! [`FragmentTable`] is the whole set an app declares, and it travels in the
//! compiled artifact next to the layout tree. It is ordered by key so the
//! artifact bytes depend only on what the app declares, not on the order the
//! compiler happened to visit files in: the same sources build the same
//! bytes.

use crate::layout_ir::Element;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// Tag of the element that marks where a use site's children land inside a
/// fragment body.
pub const SLOT_TAG: &str = "slot";

/// Slot name a `<slot>` with no `name` attribute occupies, and the name a
/// use site's unnamed children are passed under.
pub const DEFAULT_SLOT: &str = "default";

/// Tag standing for a use site the build could not finish, which the runtime
/// fills by calling the function it names.
///
/// The element itself keeps the name a use site wrote, because that is what
/// the call needs. This is what the element is *as a box*: an empty one, for
/// as long as it takes the first tick to replace it.
pub const FRAGMENT_TAG: &str = "fragment";

/// Which authoring form declared a fragment.
///
/// The distinction is kept because diagnostics point back at source: a
/// `<template>` collision reports a markup file and a line, an `lmn!`
/// collision reports the script that expanded.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FragmentKind {
    /// Declared by a `<template>` element in markup.
    #[default]
    Template,
    /// Declared by an `lmn!` block in a script.
    Markup,
}

/// A candela function name that reaches a fragment.
///
/// A candela function returns an `lmn!` block, and a use site names that
/// function as a tag. Nothing parses markup while an app runs, so the tree a
/// use site stands for is baked before the app starts; `inlinable` is whether
/// baking it is the whole story.
///
/// It is set when instantiating the fragment is the same as calling the
/// function: the block is the function's whole body, and every value the block
/// reads is one the caller passed. The build then puts the body at the use
/// site and the subtree is there from the first frame.
///
/// It is clear when the function has to run, because it works a value out or
/// picks between blocks. Every block it may return is compiled just the same;
/// what stands at the use site is a marker the runtime fills on the first tick,
/// by calling the function with [`params`](Self::params) and putting the node
/// it returns in the marker's place.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FragmentComponent {
    /// The candela function's name, as a use site writes it.
    pub name: String,
    /// The function's parameters, in declaration order. A use site binds props
    /// by name; a call passes them in this order.
    pub params: Vec<String>,
    /// Whether the baked body is what the call would have returned.
    pub inlinable: bool,
}

/// One declared parameter of a fragment.
///
/// A parameter name is what an argument at the use site binds to, and what
/// [`crate::layout_ir::InterpolationSlot::Arg`] names inside the body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FragmentParam {
    /// Parameter name, without sigil.
    pub name: String,
    /// Value used when the use site passes no argument. A parameter with
    /// `None` that the use site leaves out keeps its marker in the tree,
    /// where the global signal scope resolves it at runtime.
    pub default: Option<String>,
}

/// Where a fragment was declared, for diagnostics.
///
/// A fragment can carry more than one origin: the same declaration reaching
/// the table twice (an include pulled in from two places, a script expanded
/// per call site) merges instead of colliding, and every place it came from
/// is kept so a later error can name all of them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FragmentOrigin {
    /// Source file the declaration was read from.
    pub file: String,
    /// 1-based line of the declaration.
    pub line: u32,
    /// 1-based column of the declaration.
    pub col: u32,
}

/// A named, parameterized markup subtree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fragment {
    /// Lookup key. A use site names this, and it is the key the fragment
    /// occupies in a [`FragmentTable`].
    pub key: String,
    /// Declared parameters, in source order.
    pub params: Vec<FragmentParam>,
    /// The subtree instantiated at each use site. More than one root is
    /// allowed: a fragment can expand to a sequence of siblings.
    pub body: Vec<Element>,
    /// Every place this fragment was declared. Never empty for a fragment
    /// that came from source; a synthesized fragment may have none.
    pub origins: Vec<FragmentOrigin>,
    /// Which authoring form declared it.
    pub kind: FragmentKind,
    /// Candela function names that reach this fragment. Empty for a
    /// `<template>`, which is reached by its own key.
    pub components: Vec<FragmentComponent>,
}

/// Failures from building a [`FragmentTable`].
#[derive(Debug, thiserror::Error)]
pub enum FragmentError {
    /// Two different fragments claim the same key. The build stops here
    /// rather than picking one, because either choice silently changes what
    /// half the use sites render.
    #[error("fragment key `{key}` is declared twice with different content ({first} and {second})")]
    Collision {
        /// The contested key.
        key: String,
        /// Where the fragment already in the table came from.
        first: String,
        /// Where the rejected declaration came from.
        second: String,
    },
}

/// Every fragment an app declares, keyed by [`Fragment::key`].
///
/// Ordered rather than hashed: the artifact encodes the table field by field,
/// so a stable order is what makes two builds of the same sources produce the
/// same bytes.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FragmentTable {
    fragments: BTreeMap<String, Fragment>,
}

impl FragmentTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `fragment` under its own key.
    ///
    /// A key already holding the same declaration absorbs the new origins and
    /// keeps one entry, which is what an include reached from two paths
    /// produces. A key holding a different declaration is an error: keys are
    /// content-addressed, so two different bodies under one key means the
    /// content that produced the key was not the content being stored.
    pub fn insert(&mut self, fragment: Fragment) -> Result<(), FragmentError> {
        match self.fragments.entry(fragment.key.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(fragment);
                Ok(())
            }
            Entry::Occupied(mut slot) => {
                let existing = slot.get_mut();
                let same = match (fingerprint(existing), fingerprint(&fragment)) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if !same {
                    return Err(FragmentError::Collision {
                        key: fragment.key,
                        first: describe_origins(&existing.origins),
                        second: describe_origins(&fragment.origins),
                    });
                }
                for origin in fragment.origins {
                    if !existing.origins.contains(&origin) {
                        existing.origins.push(origin);
                    }
                }
                for component in fragment.components {
                    if !existing.components.contains(&component) {
                        existing.components.push(component);
                    }
                }
                Ok(())
            }
        }
    }

    /// Look a fragment up by key.
    pub fn get(&self, key: &str) -> Option<&Fragment> {
        self.fragments.get(key)
    }

    /// Every fragment, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Fragment)> {
        self.fragments.iter()
    }

    /// Every fragment, in key order, for a pass that rewrites bodies.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Fragment> {
        self.fragments.values_mut()
    }

    /// The fragment a candela component name reaches, with whether the baked
    /// body is what calling the function would have returned.
    ///
    /// A name no component carries yields `None`, which the caller reports as
    /// an unknown tag.
    pub fn by_component(&self, name: &str) -> Option<(&Fragment, bool)> {
        self.fragments.values().find_map(|fragment| {
            fragment
                .components
                .iter()
                .find(|component| component.name == name)
                .map(|component| (fragment, component.inlinable))
        })
    }

    /// The candela component a use site naming `name` reaches.
    ///
    /// A function that has to run is reached by name alone, so this answers
    /// without reference to the block it happens to be attached to.
    pub fn component(&self, name: &str) -> Option<&FragmentComponent> {
        self.fragments.values().find_map(|fragment| {
            fragment
                .components
                .iter()
                .find(|component| component.name == name)
        })
    }

    /// Every name a use site may write: each fragment key, plus each candela
    /// component name.
    pub fn names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self.fragments.keys().cloned().collect();
        for fragment in self.fragments.values() {
            for component in &fragment.components {
                names.insert(component.name.clone());
            }
        }
        names
    }

    /// How many fragments are declared.
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    /// Whether the table declares nothing.
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// Fold `other` into this table, applying the same collision rule per
    /// key. Used to combine what markup declared with what the scripts
    /// declared.
    pub fn merge(&mut self, other: FragmentTable) -> Result<(), FragmentError> {
        for (_, fragment) in other.fragments {
            self.insert(fragment)?;
        }
        Ok(())
    }
}

/// Canonical bytes for the parts of a fragment that decide what it renders.
///
/// [`Element`] carries no `PartialEq` (its attribute bag is a wide bag of
/// float and enum specs), so the artifact's own codec stands in for
/// structural equality. It writes fields in declaration order with no names,
/// which makes equal bytes mean equal content. An encoding failure yields
/// `None`, which the caller reads as "cannot prove these are the same" and
/// reports as a collision rather than merging on an unchecked assumption.
fn fingerprint(fragment: &Fragment) -> Option<Vec<u8>> {
    bincode::serialize(&(&fragment.kind, &fragment.params, &fragment.body)).ok()
}

/// Render a fragment's origins for an error message. A fragment with no
/// recorded origin reads as `<generated>`.
fn describe_origins(origins: &[FragmentOrigin]) -> String {
    if origins.is_empty() {
        return "<generated>".to_string();
    }
    origins
        .iter()
        .map(|o| format!("{}:{}:{}", o.file, o.line, o.col))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_ir::Attributes;

    fn origin(file: &str, line: u32) -> FragmentOrigin {
        FragmentOrigin {
            file: file.to_string(),
            line,
            col: 1,
        }
    }

    fn body(text: &str) -> Vec<Element> {
        vec![Element {
            tag: "label".to_string(),
            attrs: Attributes {
                text: Some(text.to_string()),
                ..Attributes::default()
            },
            ..Element::default()
        }]
    }

    fn fragment(key: &str, text: &str, origins: Vec<FragmentOrigin>) -> Fragment {
        Fragment {
            key: key.to_string(),
            params: vec![FragmentParam {
                name: "label".to_string(),
                default: None,
            }],
            body: body(text),
            origins,
            kind: FragmentKind::Template,
            components: Vec::new(),
        }
    }

    #[test]
    fn identical_declarations_merge_origins() {
        let mut table = FragmentTable::new();
        table
            .insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("first declaration");
        table
            .insert(fragment("card", "hi", vec![origin("b.lmn", 9)]))
            .expect("the same body under the same key merges");

        assert_eq!(table.len(), 1);
        let card = table.get("card").expect("card is declared");
        assert_eq!(card.origins.len(), 2);
        assert_eq!(card.origins[1].file, "b.lmn");
    }

    #[test]
    fn repeating_one_origin_does_not_duplicate_it() {
        let mut table = FragmentTable::new();
        table
            .insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("first declaration");
        table
            .insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("same declaration, same place");

        assert_eq!(table.get("card").expect("card").origins.len(), 1);
    }

    #[test]
    fn different_bodies_under_one_key_error() {
        let mut table = FragmentTable::new();
        table
            .insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("first declaration");
        let err = table
            .insert(fragment("card", "bye", vec![origin("b.lmn", 9)]))
            .expect_err("a different body under the same key is a collision");

        let FragmentError::Collision { key, first, second } = err;
        assert_eq!(key, "card");
        assert_eq!(first, "a.lmn:3:1");
        assert_eq!(second, "b.lmn:9:1");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn different_params_under_one_key_error() {
        let mut table = FragmentTable::new();
        table
            .insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("first declaration");
        let mut other = fragment("card", "hi", vec![origin("b.lmn", 9)]);
        other.params[0].default = Some("none".to_string());
        table
            .insert(other)
            .expect_err("the same body with different parameters is a collision");
    }

    #[test]
    fn merge_applies_the_same_rule() {
        let mut left = FragmentTable::new();
        left.insert(fragment("card", "hi", vec![origin("a.lmn", 3)]))
            .expect("first declaration");
        let mut right = FragmentTable::new();
        right
            .insert(fragment("card", "hi", vec![origin("b.lmn", 9)]))
            .expect("first declaration");
        right
            .insert(fragment("row", "x", vec![origin("b.lmn", 20)]))
            .expect("second key");

        left.merge(right).expect("compatible tables merge");
        assert_eq!(left.len(), 2);
        assert_eq!(left.get("card").expect("card").origins.len(), 2);
    }

    #[test]
    fn a_component_name_reaches_the_fragment_it_is_on() {
        let mut table = FragmentTable::new();
        let mut fragment = fragment("79114ba6b591efb1", "hi", vec![origin("main.cdl", 3)]);
        fragment.kind = FragmentKind::Markup;
        fragment.components.push(FragmentComponent {
            name: "Home".to_string(),
            params: Vec::new(),
            inlinable: true,
        });
        table.insert(fragment).expect("first declaration");

        let (found, inlinable) = table.by_component("Home").expect("Home reaches it");
        assert_eq!(found.key, "79114ba6b591efb1");
        assert!(inlinable);
        assert!(table.by_component("Away").is_none());
        assert_eq!(
            table.names(),
            ["79114ba6b591efb1".to_string(), "Home".to_string()].into()
        );
    }

    #[test]
    fn one_body_written_by_two_functions_carries_both_names() {
        let mut table = FragmentTable::new();
        for name in ["Home", "Away"] {
            let mut fragment = fragment("shared", "hi", vec![origin("main.cdl", 3)]);
            fragment.components.push(FragmentComponent {
                name: name.to_string(),
                params: Vec::new(),
                inlinable: true,
            });
            table.insert(fragment).expect("the same body merges");
        }

        assert_eq!(table.len(), 1);
        assert!(table.by_component("Home").is_some());
        assert!(table.by_component("Away").is_some());
    }

    #[test]
    fn iteration_is_key_ordered() {
        let mut table = FragmentTable::new();
        for key in ["zeta", "alpha", "mid"] {
            table
                .insert(fragment(key, key, vec![origin("a.lmn", 1)]))
                .expect("distinct keys");
        }
        let keys: Vec<&str> = table.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["alpha", "mid", "zeta"]);
    }
}
