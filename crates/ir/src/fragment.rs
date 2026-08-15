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
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

/// Tag of the element that marks where a use site's children land inside a
/// fragment body.
pub const SLOT_TAG: &str = "slot";

/// Slot name a `<slot>` with no `name` attribute occupies, and the name a
/// use site's unnamed children are passed under.
pub const DEFAULT_SLOT: &str = "default";

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
