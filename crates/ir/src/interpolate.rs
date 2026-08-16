//! Resolving the `{...}` placeholders a template carries.
//!
//! A `<for>` body and a fragment body are written with placeholders standing
//! in for what the iteration or the instantiation brings: `{row.name}`,
//! `{$index}`, `{$theme}`. Turning a template into an instance means walking
//! the subtree and replacing each of them with the value its scope holds.
//!
//! This lives here, below both consumers, because the desktop reconciler and
//! the web emitter write the same text into the same node a tick apart. Two
//! resolvers would drift, and a drift shows up as a page the browser rebuilds
//! on load rather than adopts.
//!
//! A placeholder whose scope is absent is left in the string verbatim, so the
//! walk that owns that scope resolves it later: a fragment walk resolves its
//! arguments and leaves `{row.name}` standing for the `<for>` walk inside it.

use std::collections::BTreeMap;

use lumen_core::property_store::PropertyStore;
use lumen_core::signals::ArrayItem;

use crate::layout_ir::{Element, InterpolationSlot};

/// Where a global placeholder reads its value.
///
/// The desktop reads the property store; the emitter reads the state a page
/// is rendered with. Both answer the same question, and the resolver asks it
/// the same way.
pub trait Globals {
    /// The value of the global signal `name`, or `None` when nothing has set
    /// it.
    fn global(&self, name: &str) -> Option<String>;
}

impl Globals for PropertyStore {
    fn global(&self, name: &str) -> Option<String> {
        self.get_global_str(name).map(|value| value.to_string())
    }
}

/// One iteration of a `<for>` body: the record its placeholders read and the
/// index they count from.
pub struct Row<'a> {
    /// The current iteration's row record, which
    /// [`InterpolationSlot::Row`] looks a field up in.
    pub item: &'a ArrayItem,
    /// 0-based iteration index, which [`InterpolationSlot::RowIndex`]
    /// resolves to.
    pub index: usize,
}

/// The scopes a placeholder can resolve against.
///
/// Each caller brings the scopes it has: a `<for>` row brings [`Self::row`],
/// a fragment instantiation brings [`Self::args`], and a `<for>` inside a
/// fragment body meets both in turn.
pub struct Scope<'a> {
    /// The iteration in progress, absent outside a `<for>` body.
    pub row: Option<Row<'a>>,
    /// The arguments a fragment instance was built with, already folded over
    /// the declared defaults. Absent outside a fragment body.
    pub args: Option<&'a BTreeMap<String, String>>,
    /// Where a global placeholder reads its value.
    pub globals: &'a dyn Globals,
    /// Told the name of a row field the iteration record does not have.
    /// Absent when the caller has nowhere to report it.
    pub missing_row_field: Option<&'a dyn Fn(&str)>,
}

impl<'a> Scope<'a> {
    /// A scope with globals alone: no iteration, no arguments, nothing to
    /// report to.
    pub fn new(globals: &'a dyn Globals) -> Self {
        Self {
            row: None,
            args: None,
            globals,
            missing_row_field: None,
        }
    }

    /// The same scope, iterating `item` at `index`.
    pub fn with_row(mut self, item: &'a ArrayItem, index: usize) -> Self {
        self.row = Some(Row { item, index });
        self
    }

    /// The same scope, inside a fragment instantiated with `args`.
    pub fn with_args(mut self, args: &'a BTreeMap<String, String>) -> Self {
        self.args = Some(args);
        self
    }

    /// The same scope, reporting a row field the record does not have.
    pub fn reporting_to(mut self, report: &'a dyn Fn(&str)) -> Self {
        self.missing_row_field = Some(report);
        self
    }
}

/// Walk a template subtree, resolving every placeholder in its string-valued
/// attributes and its text against `scope`.
///
/// The result is one instance of the template. Nothing else about the element
/// changes: a caller that needs the CSS cascade re-run over the instance runs
/// it afterwards, because what a rule matches depends on the `id` and `class`
/// values that landed here.
pub fn substitute_element(template: &Element, scope: &Scope<'_>) -> Element {
    let mut instance = template.clone();
    substitute_in_place(&mut instance, scope);
    instance.children = instance
        .children
        .iter()
        .map(|child| substitute_element(child, scope))
        .collect();
    instance
}

/// Resolve every placeholder in one element's own strings, leaving its
/// children alone.
fn substitute_in_place(element: &mut Element, scope: &Scope<'_>) {
    let substitute = |text: &str| resolve(text, &element.interpolations, scope);
    if let Some(text) = &element.attrs.text {
        element.attrs.text = Some(substitute(text));
    }
    if let Some(id) = &element.attrs.id {
        element.attrs.id = Some(substitute(id));
    }
    if let Some(src) = &element.attrs.src {
        element.attrs.src = Some(substitute(src));
    }
    if let Some(role) = &element.attrs.style_role {
        element.attrs.style_role = Some(substitute(role));
    }
    if let Some(placeholder) = &element.attrs.placeholder {
        element.attrs.placeholder = Some(substitute(placeholder));
    }
    if let Some(payload) = &element.attrs.drag_payload {
        element.attrs.drag_payload = Some(substitute(payload));
    }
    element.attrs.classes = element
        .attrs
        .classes
        .iter()
        .map(|class| substitute(class))
        .collect();
}

/// Replace every `{...}` token in `text` with what its scope holds.
///
/// `slots` is the placeholder catalog the parser recorded on the element the
/// string came from, which is what tells a fragment parameter apart from a
/// global signal of the same name.
///
/// The rules:
///
/// - [`InterpolationSlot::Row`] - the iteration record's field. A field the
///   record does not have resolves empty and is reported through
///   [`Scope::missing_row_field`]. With no row scope the placeholder is left
///   standing for the `<for>` walk that has one.
/// - [`InterpolationSlot::RowIndex`] - the iteration index, or left standing
///   outside a row scope.
/// - [`InterpolationSlot::Arg`] - the instantiating argument, else empty. A
///   `Global` naming a declared parameter resolves the same way, which is how
///   an argument shadows a global signal of that name.
/// - [`InterpolationSlot::Global`] - the global signal, or the iteration
///   record's field of that name when no signal is set, which is what makes
///   the bare `{field}` spelling work inside a `<for>` body.
/// - [`InterpolationSlot::SelfField`] / [`InterpolationSlot::ParentField`] -
///   empty. Per-entity properties have no consumer yet.
pub fn resolve(text: &str, slots: &[InterpolationSlot], scope: &Scope<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let Some(rel) = text[i..].find('{') else {
            out.push_str(&text[i..]);
            break;
        };
        let open = i + rel;
        out.push_str(&text[i..open]);
        let Some(end) = text[open..].find('}') else {
            out.push_str(&text[open..]);
            break;
        };
        let close = open + end;
        let inner = &text[open + 1..close];
        let trimmed = inner.trim();
        let slot = claim_arg(InterpolationSlot::from(trimmed), slots, scope);
        let resolved = match &slot {
            InterpolationSlot::Row(field) => scope.row.as_ref().map(|row| {
                row.item.get(field).cloned().unwrap_or_else(|| {
                    if let Some(report) = scope.missing_row_field {
                        report(field);
                    }
                    String::new()
                })
            }),
            InterpolationSlot::RowIndex => scope.row.as_ref().map(|row| row.index.to_string()),
            InterpolationSlot::Arg(name) => scope
                .args
                .map(|args| args.get(name).cloned().unwrap_or_default()),
            InterpolationSlot::Global(name) => scope.globals.global(name),
            InterpolationSlot::SelfField(field) | InterpolationSlot::ParentField(field) => {
                tracing::debug!(
                    target: "lumen_ir::interpolate",
                    "$self / $parent field `{field}` substituted as empty - \
                     per-entity properties have no consumer yet"
                );
                Some(String::new())
            }
        };
        // A global with no signal set falls through to the iteration record,
        // which is what the bare `{field}` spelling inside a `<for>` body
        // means. The scopes that consulted the record already, and the
        // argument scope that never should, do not fall through.
        let value = resolved.or_else(|| {
            if matches!(
                slot,
                InterpolationSlot::RowIndex | InterpolationSlot::Row(_) | InterpolationSlot::Arg(_)
            ) {
                return None;
            }
            scope
                .row
                .as_ref()
                .and_then(|row| row.item.get(trimmed).cloned())
        });
        match value {
            Some(value) => out.push_str(&value),
            // Nothing resolved it: keep it verbatim so an authoring typo
            // surfaces as the literal `{x}` rather than as an empty string.
            None => {
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        i = close + 1;
    }
    out
}

/// Re-read a placeholder as a fragment argument when the surrounding fragment
/// scope claims its name.
///
/// A parameter reference is spelled exactly like a global signal reference,
/// so the classifier that sees only the token always says `Global`. Two things
/// separate them, and either is enough: the compiler records an
/// [`InterpolationSlot::Arg`] on the element when it knows the enclosing
/// parameter list, and the instance's argument map holds every declared
/// parameter.
fn claim_arg(
    slot: InterpolationSlot,
    slots: &[InterpolationSlot],
    scope: &Scope<'_>,
) -> InterpolationSlot {
    let InterpolationSlot::Global(name) = &slot else {
        return slot;
    };
    let claimed = slots.contains(&InterpolationSlot::Arg(name.clone()))
        || scope.args.is_some_and(|args| args.contains_key(name));
    if claimed {
        return InterpolationSlot::Arg(name.clone());
    }
    slot
}

#[cfg(test)]
mod tests {
    //! Every case the reconciler's own resolver was checked against before
    //! this moved here, so the text a page is emitted with and the text the
    //! browser writes a tick later come from the same rules.
    //!
    //! One scope, two sources. A `<for>` row brings the row record, a
    //! fragment instance brings the arguments, and a `<for>` inside a
    //! fragment body meets both: each walk resolves what its own scope knows
    //! and leaves the rest of the placeholders standing.
    use std::cell::RefCell;

    use lumen_core::property_store::PropertyStore;

    use super::*;
    use crate::layout_ir::Attributes;

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn item(pairs: &[(&str, &str)]) -> ArrayItem {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn resolved(
        body: &str,
        slots: &[InterpolationSlot],
        row: Option<(&ArrayItem, usize)>,
        args: Option<&BTreeMap<String, String>>,
        store: &PropertyStore,
    ) -> String {
        let mut scope = Scope::new(store);
        if let Some((item, index)) = row {
            scope = scope.with_row(item, index);
        }
        if let Some(args) = args {
            scope = scope.with_args(args);
        }
        resolve(body, slots, &scope)
    }

    #[test]
    fn an_argument_resolves_from_the_instance_binding() {
        let store = PropertyStore::default();
        let bound = args(&[("title", "Recent")]);
        let out = resolved(
            "{$title}!",
            &[InterpolationSlot::Arg("title".to_string())],
            None,
            Some(&bound),
            &store,
        );
        assert_eq!(out, "Recent!");
    }

    #[test]
    fn an_argument_beats_a_global_of_the_same_name() {
        let mut store = PropertyStore::default();
        store.set_global_str("title", "from the signal");
        let bound = args(&[("title", "from the argument")]);
        let out = resolved("{$title}", &[], None, Some(&bound), &store);
        assert_eq!(out, "from the argument");
    }

    #[test]
    fn a_global_outside_the_parameter_list_still_reads_the_store() {
        let mut store = PropertyStore::default();
        store.set_global_str("theme", "dark");
        let bound = args(&[("title", "Recent")]);
        let out = resolved("{$theme}", &[], None, Some(&bound), &store);
        assert_eq!(out, "dark");
    }

    #[test]
    fn row_placeholders_survive_the_argument_walk() {
        let store = PropertyStore::default();
        let bound = args(&[("prefix", "Row")]);
        let slots = [
            InterpolationSlot::Arg("prefix".to_string()),
            InterpolationSlot::Row("name".to_string()),
        ];
        let out = resolved(
            "{$prefix}: {row.name} #{$index}",
            &slots,
            None,
            Some(&bound),
            &store,
        );
        assert_eq!(
            out, "Row: {row.name} #{$index}",
            "the walk with no row scope leaves the row placeholders for the reconciler"
        );
    }

    #[test]
    fn the_row_walk_finishes_what_the_argument_walk_left() {
        let store = PropertyStore::default();
        let row = item(&[("name", "alpha")]);
        let slots = [
            InterpolationSlot::Arg("prefix".to_string()),
            InterpolationSlot::Row("name".to_string()),
        ];
        let out = resolved(
            "Row: {row.name} #{$index}",
            &slots,
            Some((&row, 2)),
            None,
            &store,
        );
        assert_eq!(out, "Row: alpha #2");
    }

    #[test]
    fn both_scopes_resolve_in_one_walk() {
        let store = PropertyStore::default();
        let bound = args(&[("prefix", "Row")]);
        let row = item(&[("name", "alpha")]);
        let slots = [
            InterpolationSlot::Arg("prefix".to_string()),
            InterpolationSlot::Row("name".to_string()),
        ];
        let out = resolved(
            "{$prefix}: {row.name} #{$index}",
            &slots,
            Some((&row, 0)),
            Some(&bound),
            &store,
        );
        assert_eq!(out, "Row: alpha #0");
    }

    #[test]
    fn a_parameter_nothing_bound_resolves_empty() {
        let store = PropertyStore::default();
        let bound = args(&[("title", "")]);
        let out = resolved(
            "[{$title}]",
            &[InterpolationSlot::Arg("title".to_string())],
            None,
            Some(&bound),
            &store,
        );
        assert_eq!(out, "[]");
    }

    #[test]
    fn a_bare_name_reads_the_row_record_when_no_signal_holds_it() {
        let store = PropertyStore::default();
        let row = item(&[("name", "alpha")]);
        let out = resolved("{name}", &[], Some((&row, 0)), None, &store);
        assert_eq!(
            out, "alpha",
            "the legacy bare-field spelling still means the row's field"
        );
    }

    #[test]
    fn a_field_the_record_does_not_have_is_reported_and_resolves_empty() {
        let store = PropertyStore::default();
        let row = item(&[("name", "alpha")]);
        let missing = RefCell::new(Vec::new());
        let report = |field: &str| missing.borrow_mut().push(field.to_string());
        let scope = Scope::new(&store).with_row(&row, 0).reporting_to(&report);
        let out = resolve(
            "[{row.title}]",
            &[InterpolationSlot::Row("title".to_string())],
            &scope,
        );
        assert_eq!(out, "[]");
        assert_eq!(missing.into_inner(), vec!["title".to_string()]);
    }

    #[test]
    fn a_placeholder_nothing_answers_stays_in_the_text() {
        let store = PropertyStore::default();
        let out = resolved("a {$absent} b {unclosed", &[], None, None, &store);
        assert_eq!(
            out, "a {$absent} b {unclosed",
            "a global no signal holds is left standing, and a brace with no end is text"
        );
    }

    #[test]
    fn a_subtree_is_substituted_down_to_its_leaves() {
        let store = PropertyStore::default();
        let row = item(&[("name", "alpha")]);
        let leaf = Element {
            tag: "label".to_string(),
            attrs: Attributes {
                text: Some("{row.name} #{$index}".to_string()),
                ..Attributes::default()
            },
            interpolations: vec![
                InterpolationSlot::Row("name".to_string()),
                InterpolationSlot::RowIndex,
            ],
            ..Element::default()
        };
        let template = Element {
            tag: "row".to_string(),
            attrs: Attributes {
                classes: vec!["item-{$index}".to_string()],
                ..Attributes::default()
            },
            children: vec![leaf],
            interpolations: vec![InterpolationSlot::RowIndex],
            ..Element::default()
        };

        let scope = Scope::new(&store).with_row(&row, 3);
        let instance = substitute_element(&template, &scope);

        assert_eq!(instance.attrs.classes, vec!["item-3".to_string()]);
        assert_eq!(
            instance.children[0].attrs.text.as_deref(),
            Some("alpha #3"),
            "a leaf several levels down is substituted too"
        );
        assert_eq!(
            template.children[0].attrs.text.as_deref(),
            Some("{row.name} #{$index}"),
            "and the template it came from is untouched"
        );
    }
}
