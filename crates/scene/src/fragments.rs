//! Fragments at run time: the app's declared fragment set, the marker a
//! fragment instance carries, and the argument binding an instantiation
//! resolves its placeholders against.
//!
//! A fragment is compiled once and instantiated many times. The compiler
//! puts every declaration in the artifact's
//! [`FragmentTable`](lumen_ir::fragment::FragmentTable); [`FragmentLibrary`]
//! is that table as a world resource, so anything holding the world can
//! instantiate by key. The instantiation itself lives with the rest of the
//! DOM command applier, which is the only caller.
//!
//! Arguments are static: they substitute once, when the instance is built.
//! A value that changes while the app runs is a `bind-*` attribute inside
//! the fragment body, which the spawn path seeds and the per-tick binding
//! systems drive. [`FragmentInstance`] records what an instance was built
//! with so a later change can rebind arguments too; nothing reads it back
//! yet.

use bevy_ecs::component::Component;
use bevy_ecs::resource::Resource;
use lumen_ir::fragment::{DEFAULT_SLOT, Fragment, FragmentTable, SLOT_TAG};
use lumen_ir::layout_ir::Element;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The app's declared fragments, keyed by fragment key.
///
/// Installed from the artifact (or from the parsed sources) when the app is
/// built. Cheap to clone: instantiation takes a handle on the table so the
/// world is free for the spawn that follows.
#[derive(Resource, Clone, Default)]
pub struct FragmentLibrary(Arc<FragmentTable>);

impl FragmentLibrary {
    /// Wrap a compiled table.
    pub fn new(table: FragmentTable) -> Self {
        Self(Arc::new(table))
    }

    /// Look a fragment up by key.
    pub fn get(&self, key: &str) -> Option<&Fragment> {
        self.0.get(key)
    }

    /// Whether the app declares no fragments at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Marks the root of an instantiated fragment with the key it came from and
/// the arguments it was built with.
///
/// Recorded so per-instance arguments can become live later; the current
/// runtime substitutes them once and never reads this back.
#[derive(Component, Debug, Clone)]
pub struct FragmentInstance {
    /// Key of the fragment this subtree came from.
    pub key: String,
    /// Arguments the instance resolved, defaults already folded in.
    pub args: BTreeMap<String, String>,
}

/// Marks a spawned `<slot>`: the element a use site's child replaces.
#[derive(Component, Debug, Clone)]
pub struct SlotPlaceholder(pub String);

/// The slot name an element occupies, or `None` when it is not a slot.
pub fn slot_name_of(el: &Element) -> Option<String> {
    if el.tag != SLOT_TAG {
        return None;
    }
    Some(
        el.attrs
            .slot_name
            .clone()
            .unwrap_or_else(|| DEFAULT_SLOT.to_string()),
    )
}

/// Why a fragment could not be instantiated.
#[derive(Debug, PartialEq, Eq)]
pub enum FragmentFault {
    /// No fragment is declared under the requested key.
    UnknownKey,
    /// The body has a number of roots other than one.
    RootCount(usize),
    /// The single root is itself a slot, so the instance would have no root
    /// of its own once the slot is filled.
    RootIsSlot,
    /// The caller passed a child for a slot the body does not declare.
    UnknownSlot(String),
}

impl std::fmt::Display for FragmentFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey => write!(f, "no fragment is declared under this key"),
            Self::RootCount(n) => write!(
                f,
                "the body has {n} roots; instantiation returns one node, so a fragment \
                 reached this way needs exactly one root element"
            ),
            Self::RootIsSlot => {
                write!(f, "the body root is a slot, which leaves no root to return")
            }
            Self::UnknownSlot(name) => {
                write!(
                    f,
                    "no slot named `{name}` in the body; the child stays detached"
                )
            }
        }
    }
}

/// The element an instance spawns from, or why it cannot.
pub fn instance_body(fragment: &Fragment) -> Result<&Element, FragmentFault> {
    let [root] = fragment.body.as_slice() else {
        return Err(FragmentFault::RootCount(fragment.body.len()));
    };
    if slot_name_of(root).is_some() {
        return Err(FragmentFault::RootIsSlot);
    }
    Ok(root)
}

/// Bind the fragment's parameters: every declared parameter takes its
/// default, then the use site's arguments override. An argument naming
/// something the fragment does not declare is kept, so a body written
/// against a wider parameter list than its declaration still resolves.
pub fn bind_args(fragment: &Fragment, args: &[(String, String)]) -> BTreeMap<String, String> {
    let mut bound: BTreeMap<String, String> = fragment
        .params
        .iter()
        .map(|p| (p.name.clone(), p.default.clone().unwrap_or_default()))
        .collect();
    for (name, value) in args {
        bound.insert(name.clone(), value.clone());
    }
    bound
}

/// Report a fragment fault once per `(key, fault)` pair. A command issued
/// every tick reports the first time and stays quiet after, so a broken key
/// is visible without flooding the log.
pub fn report_once(key: &str, fault: &FragmentFault) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
    if guard.insert((key.to_string(), fault.to_string())) {
        eprintln!("spawn_fragment `{key}`: {fault}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_ir::fragment::{FragmentKind, FragmentParam};
    use lumen_ir::layout_ir::Attributes;

    fn fragment(params: Vec<FragmentParam>, body: Vec<Element>) -> Fragment {
        Fragment {
            key: "card".to_string(),
            params,
            body,
            origins: Vec::new(),
            kind: FragmentKind::Template,
        }
    }

    fn param(name: &str, default: Option<&str>) -> FragmentParam {
        FragmentParam {
            name: name.to_string(),
            default: default.map(str::to_string),
        }
    }

    fn el(tag: &str) -> Element {
        Element {
            tag: tag.to_string(),
            ..Element::default()
        }
    }

    #[test]
    fn a_use_site_argument_beats_the_declared_default() {
        let f = fragment(
            vec![
                param("title", Some("Untitled")),
                param("tone", Some("calm")),
            ],
            vec![el("column")],
        );
        let bound = bind_args(&f, &[("title".to_string(), "Recent".to_string())]);
        assert_eq!(bound.get("title").map(String::as_str), Some("Recent"));
        assert_eq!(bound.get("tone").map(String::as_str), Some("calm"));
    }

    #[test]
    fn a_parameter_with_no_default_and_no_argument_binds_empty() {
        let f = fragment(vec![param("title", None)], vec![el("column")]);
        let bound = bind_args(&f, &[]);
        assert_eq!(bound.get("title").map(String::as_str), Some(""));
    }

    #[test]
    fn one_root_is_the_instance_body() {
        let f = fragment(Vec::new(), vec![el("column")]);
        assert_eq!(instance_body(&f).expect("single root").tag, "column");
    }

    #[test]
    fn several_roots_report_their_count() {
        let f = fragment(Vec::new(), vec![el("label"), el("label")]);
        assert_eq!(instance_body(&f).err(), Some(FragmentFault::RootCount(2)));
    }

    #[test]
    fn an_empty_body_is_a_root_count_of_zero() {
        let f = fragment(Vec::new(), Vec::new());
        assert_eq!(instance_body(&f).err(), Some(FragmentFault::RootCount(0)));
    }

    #[test]
    fn a_slot_root_is_rejected() {
        let f = fragment(Vec::new(), vec![el(SLOT_TAG)]);
        assert_eq!(instance_body(&f).err(), Some(FragmentFault::RootIsSlot));
    }

    #[test]
    fn an_unnamed_slot_occupies_the_default_name() {
        assert_eq!(slot_name_of(&el(SLOT_TAG)).as_deref(), Some(DEFAULT_SLOT));
        assert_eq!(slot_name_of(&el("column")), None);
    }

    #[test]
    fn a_named_slot_keeps_its_name() {
        let mut slot = el(SLOT_TAG);
        slot.attrs = Attributes {
            slot_name: Some("footer".to_string()),
            ..Attributes::default()
        };
        assert_eq!(slot_name_of(&slot).as_deref(), Some("footer"));
    }
}
