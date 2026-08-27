//! Fragment instantiation - turning each use site into the subtree it names.
//!
//! The parser leaves a use site standing in the tree as one element carrying
//! a [`FragmentUse`](lumen_ir::layout_ir::FragmentUse); this pass replaces it
//! with the fragment's body, its arguments resolved. It works on the IR, so a
//! fragment declared in another file instantiates exactly like a local one:
//! all it takes is a table holding both.
//!
//! Expansion runs outside-in, one level of nesting per pass, repeated until
//! the tree holds no use site. That order is what makes per-instance ids
//! stack: an outer instance prefixes the id of the inner use site before the
//! inner one expands, so a body two levels down answers to `outer:inner:name`.
//!
//! [`link`] runs the same expansion over the table itself, so every body is
//! the whole subtree it stands for by the time it travels in an artifact.

use crate::layout_ir::{DeferredAttr, Element, InterpolationSlot, LintFinding, ParseError};
use crate::parser_html::{apply_deferred_attribute, classify_markers, push_unique};
use lumen_ir::fragment::{DEFAULT_SLOT, Fragment, FragmentTable, SLOT_TAG};
use std::collections::{BTreeMap, BTreeSet};

/// How many expansion passes a tree gets. One pass resolves one level of
/// nesting, so this is the deepest chain of fragments-inside-fragments a
/// document may build. A cycle is caught before expansion starts; this
/// catches a chain long enough that the author meant something else.
const MAX_PASSES: u32 = 64;

/// One argument bound at a use site, with its markers already classified so
/// the element it lands on inherits the right resolution scope.
struct Arg {
    name: String,
    value: String,
    slots: Vec<InterpolationSlot>,
}

/// The content a use site hands to the fragment's `<slot>`.
#[derive(Default, Clone)]
struct Fill {
    elements: Vec<Element>,
    text: Option<String>,
}

impl Fill {
    fn is_empty(&self) -> bool {
        self.elements.is_empty() && self.text.is_none()
    }
}

/// Replace every use site under `root` with the fragment it names.
pub(crate) fn inline(
    root: &mut Element,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<(), ParseError> {
    check(table)?;
    let mut list = std::mem::take(&mut root.children);
    let result = expand_until_settled(&mut list, table, lint_findings);
    root.children = list;
    result
}

/// Expand the use sites the fragment bodies themselves hold, so every body in
/// the table is the whole subtree it stands for.
///
/// A body reaches this pass with use sites left in it: an `lmn!` block writes
/// `<card/>` for a `<template>` the markup declares, and the block is read
/// before the markup is. Resolving them here is what lets the table travel in
/// the artifact and instantiate with nothing else in hand.
///
/// # Errors
///
/// [`ParseError`] when a body names a fragment the table does not hold, or
/// when the table itself is malformed.
pub fn link(table: &mut FragmentTable) -> Result<(), ParseError> {
    check(table)?;
    let declared = table.clone();
    let mut lint_findings = Vec::new();
    for fragment in table.iter_mut() {
        expand_until_settled(&mut fragment.body, &declared, &mut lint_findings)?;
    }
    Ok(())
}

/// Expand `list` one level of nesting per pass until nothing is left to
/// expand.
fn expand_until_settled(
    list: &mut Vec<Element>,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<(), ParseError> {
    for _ in 0..MAX_PASSES {
        let mut expanded = false;
        inline_list(list, table, lint_findings, &mut expanded)?;
        if !expanded {
            return Ok(());
        }
    }
    Err(ParseError::Xml(format!(
        "fragments are nested deeper than {MAX_PASSES} levels"
    )))
}

/// Expand the use sites in `list` and recurse into everything else. A body
/// spliced in here keeps its own use sites for the next pass.
///
/// A use site naming a component the build cannot stand in for is left where
/// it is, with its arguments put in the function's parameter order. That
/// element is the marker the runtime fills by calling the function.
fn inline_list(
    list: &mut Vec<Element>,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
    expanded: &mut bool,
) -> Result<(), ParseError> {
    let mut out: Vec<Element> = Vec::with_capacity(list.len());
    for mut el in std::mem::take(list) {
        match site(&el, table)? {
            Site::Finished => {
                out.extend(instantiate(el, table, lint_findings)?);
                *expanded = true;
            }
            Site::Fill(component) => {
                normalize_fill_args(&mut el, component)?;
                out.push(el);
            }
            Site::Plain => {
                inline_list(&mut el.children, table, lint_findings, expanded)?;
                out.push(el);
            }
        }
    }
    *list = out;
    Ok(())
}

/// What one element is to this pass.
enum Site<'a> {
    /// Not a use site; walk into it.
    Plain,
    /// A use site whose whole subtree the build knows, so it expands here.
    Finished,
    /// A use site naming a component that has to run. It stays in the tree as
    /// the marker the runtime fills.
    Fill(&'a lumen_ir::fragment::FragmentComponent),
}

/// Classify `el` for this pass.
fn site<'a>(el: &Element, table: &'a FragmentTable) -> Result<Site<'a>, ParseError> {
    let Some(use_site) = &el.frag_use else {
        return Ok(Site::Plain);
    };
    let name = use_site.key.as_str();
    if table.get(name).is_some() {
        return Ok(Site::Finished);
    }
    match table.component(name) {
        Some(component) if component.inlinable => Ok(Site::Finished),
        Some(component) => Ok(Site::Fill(component)),
        None => Err(ParseError::Xml(format!(
            "unknown fragment `{name}`: no <template name=\"{name}\"> and no candela `fn {name}` \
             returning an lmn! block is declared"
        ))),
    }
}

/// Put a fill marker's arguments in the order the call passes them.
///
/// A use site binds props by name and the call is positional, so the values
/// are laid out against the function's parameters here, while the parameter
/// list is still in hand. A parameter no prop names is passed empty, matching
/// what an omitted fragment argument resolves to.
///
/// # Errors
///
/// [`ParseError`] when a prop names no parameter of the function, which would
/// otherwise reach nothing.
fn normalize_fill_args(
    el: &mut Element,
    component: &lumen_ir::fragment::FragmentComponent,
) -> Result<(), ParseError> {
    let use_site = el.frag_use.as_mut().expect("caller checked the site");
    for (prop, _) in &use_site.args {
        if prop != "id" && !component.params.iter().any(|p| p == prop) {
            return Err(ParseError::Xml(format!(
                "component `{}` has no parameter `{prop}`; it declares ({})",
                component.name,
                component.params.join(", ")
            )));
        }
    }
    use_site.args = component
        .params
        .iter()
        .map(|param| {
            let value = use_site
                .args
                .iter()
                .find(|(name, _)| name == param)
                .map_or("", |(_, value)| value.as_str());
            (param.clone(), value.to_string())
        })
        .collect();
    Ok(())
}

/// Build the subtree one use site stands for.
fn instantiate(
    mut site: Element,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<Vec<Element>, ParseError> {
    let use_site = *site.frag_use.take().expect("caller checked the site");
    let fragment = lookup(table, &use_site.key)?;

    // The use site binds first, so an argument it passes wins over the
    // declared default for the same name.
    let mut args: Vec<Arg> = use_site.args.iter().map(|(k, v)| arg(k, v)).collect();
    for param in &fragment.params {
        if let Some(default) = &param.default
            && !args.iter().any(|a| a.name == param.name)
        {
            args.push(arg(&param.name, default));
        }
    }

    let mut body = fragment.body.clone();
    for el in &mut body {
        resolve(el, &args, lint_findings)?;
    }
    if let Some(prefix) = site.attrs.id.as_deref().filter(|id| !id.is_empty()) {
        let prefix = format!("{prefix}:");
        for el in &mut body {
            prefix_ids(el, &prefix);
        }
    }
    let fill = Fill {
        elements: site.children,
        text: site.attrs.text,
    };
    let _ = fill_slots(&mut body, &fill);
    Ok(body)
}

/// The fragment a use site naming `name` stands for.
///
/// One lookup for both spellings a use site has: a `<template>` name, which is
/// the table key itself, and a candela component name, which the fragment the
/// function returns carries.
fn lookup<'a>(table: &'a FragmentTable, name: &str) -> Result<&'a Fragment, ParseError> {
    table
        .get(name)
        .or_else(|| table.by_component(name).map(|(fragment, _)| fragment))
        .ok_or_else(|| {
            ParseError::Xml(format!(
                "unknown fragment `{name}`: no <template name=\"{name}\"> and no candela \
                 `fn {name}` returning an lmn! block is declared"
            ))
        })
}

/// Classify one argument value the way an authored attribute value is
/// classified, so a signal reference passed through a fragment resolves at
/// runtime like one written in place.
fn arg(name: &str, value: &str) -> Arg {
    let (value, slots) = classify_markers(value);
    Arg {
        name: name.to_string(),
        value,
        slots,
    }
}

/// Resolve `args` into `el` and everything below it.
fn resolve(
    el: &mut Element,
    args: &[Arg],
    lint_findings: &mut Vec<LintFinding>,
) -> Result<(), ParseError> {
    let deferred: Vec<DeferredAttr> = std::mem::take(&mut el.attrs.deferred);
    for attr in &deferred {
        let mut used = Vec::new();
        let value = substitute(&attr.value, args, &mut used);
        apply_deferred_attribute(&el.tag, attr, &value, &mut el.attrs, lint_findings)?;
    }

    if el
        .interpolations
        .iter()
        .any(|slot| matches!(slot, InterpolationSlot::Arg(_)))
    {
        let mut used = Vec::new();
        for text in [
            el.attrs.text.as_mut(),
            el.attrs.placeholder.as_mut(),
            el.attrs.src.as_mut(),
            el.attrs.id.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            *text = substitute(text, args, &mut used);
        }
        for class in &mut el.attrs.classes {
            *class = substitute(class, args, &mut used);
        }
        rescope(el, args, &used);
    }

    if let Some(use_site) = &mut el.frag_use {
        for (_, value) in &mut use_site.args {
            let mut used = Vec::new();
            *value = substitute(value, args, &mut used);
        }
    }

    for child in &mut el.children {
        resolve(child, args, lint_findings)?;
    }
    Ok(())
}

/// Rewrite the element's marker list around what the arguments just did to
/// it: a bound parameter is gone from the text, an unbound one falls through
/// to the global signal scope, and whatever the argument values themselves
/// carried is now this element's to resolve.
fn rescope(el: &mut Element, args: &[Arg], used: &[usize]) {
    let bound: BTreeSet<&str> = args.iter().map(|a| a.name.as_str()).collect();
    let mut slots: Vec<InterpolationSlot> = Vec::with_capacity(el.interpolations.len());
    for slot in std::mem::take(&mut el.interpolations) {
        match slot {
            InterpolationSlot::Arg(name) if bound.contains(name.as_str()) => {}
            InterpolationSlot::Arg(name) => {
                push_unique(&mut slots, InterpolationSlot::Global(name));
            }
            other => push_unique(&mut slots, other),
        }
    }
    for index in used {
        for slot in &args[*index].slots {
            push_unique(&mut slots, slot.clone());
        }
    }
    el.interpolations = slots;
}

/// Replace every `{name}` marker in `text` that an argument binds, recording
/// which arguments were read into `used`.
///
/// One left-to-right pass, so a value that itself looks like a marker is
/// text from here on rather than something a later argument can rewrite.
fn substitute(text: &str, args: &[Arg], used: &mut Vec<usize>) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}').map(|i| open + i) else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &rest[open + 1..close];
        match args.iter().position(|a| a.name == name) {
            Some(index) => {
                out.push_str(&args[index].value);
                if !used.contains(&index) {
                    used.push(index);
                }
            }
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Prefix every id in the subtree, so two instances of one fragment address
/// their parts apart.
fn prefix_ids(el: &mut Element, prefix: &str) {
    if let Some(id) = &mut el.attrs.id {
        id.insert_str(0, prefix);
    }
    for child in &mut el.children {
        prefix_ids(child, prefix);
    }
}

/// Put `fill` in place of every default slot marker in `list`. A marker
/// naming another slot keeps its fallback: markup has no way to say which
/// slot its content is for, so all of it is for the default one.
///
/// Returns text a fill left for the element holding the marker to adopt: the
/// IR has no text node, so text content belongs to its parent.
fn fill_slots(list: &mut Vec<Element>, fill: &Fill) -> Option<String> {
    let mut out: Vec<Element> = Vec::with_capacity(list.len());
    let mut text = None;
    for mut el in std::mem::take(list) {
        if el.tag == SLOT_TAG {
            let is_default = el.attrs.slot_name.as_deref().unwrap_or(DEFAULT_SLOT) == DEFAULT_SLOT;
            let (elements, fill_text) = if fill.is_empty() || !is_default {
                (el.children, el.attrs.text)
            } else {
                (fill.elements.clone(), fill.text.clone())
            };
            out.extend(elements);
            text = text.or(fill_text);
        } else {
            if let Some(adopted) = fill_slots(&mut el.children, fill)
                && el.attrs.text.is_none()
            {
                el.attrs.text = Some(adopted);
            }
            out.push(el);
        }
    }
    *list = out;
    text
}

/// Everything that has to hold of a table before anything expands against it.
fn check(table: &FragmentTable) -> Result<(), ParseError> {
    reject_name_collisions(table)?;
    reject_cycles(table)
}

/// Reject a name two declarations both answer to.
///
/// A use site writes one name, so a candela component sharing a name with a
/// `<template>`, or with another component, leaves nothing to pick between
/// them. Both declarations are named in the message; renaming either settles
/// it.
///
/// A `<template>` sharing a name with any component is a collision: a use
/// site would reach the template and the function would never run.
///
/// Two fragments claiming one component name are only a collision when the
/// build can stand in for the call, because that is when the body picked
/// decides what renders. A function with several blocks contributes one entry
/// per block under its own name and is reached by that name either way, so
/// those are the same declaration rather than competing ones.
fn reject_name_collisions(table: &FragmentTable) -> Result<(), ParseError> {
    let mut claimed: BTreeMap<&str, &Fragment> = BTreeMap::new();
    for (key, fragment) in table.iter() {
        for component in &fragment.components {
            if let Some(other) = table.get(&component.name)
                && other.key != *key
            {
                return Err(collision(&component.name, other, fragment));
            }
            if !component.inlinable {
                continue;
            }
            match claimed.insert(component.name.as_str(), fragment) {
                Some(other) if other.key != *key => {
                    return Err(collision(&component.name, other, fragment));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The message for one contested name, pointing at both declarations.
fn collision(name: &str, first: &Fragment, second: &Fragment) -> ParseError {
    ParseError::Xml(format!(
        "`{name}` is declared twice: {} and {}. A use site writes one name, so rename one of them",
        origins(first),
        origins(second)
    ))
}

/// Where a fragment was declared, for a message about it.
fn origins(fragment: &Fragment) -> String {
    if fragment.origins.is_empty() {
        return "<generated>".to_string();
    }
    fragment
        .origins
        .iter()
        .map(|o| format!("{}:{}:{}", o.file, o.line, o.col))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What to call a fragment in a message: the component name a reader wrote,
/// or the key when nothing named it. A block's own key is a content hash,
/// which says nothing to the person who has to break the cycle.
fn readable<'a>(table: &'a FragmentTable, key: &'a str) -> &'a str {
    table
        .get(key)
        .and_then(|fragment| fragment.components.first())
        .map_or(key, |component| component.name.as_str())
}

/// Reject a fragment that instantiates itself, directly or through a chain.
///
/// Checked on the table rather than during expansion: the whole cycle is
/// visible here, and naming it is the only useful thing an error about one
/// can say.
fn reject_cycles(table: &FragmentTable) -> Result<(), ParseError> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for (key, _) in table.iter() {
        let mut chain: Vec<&str> = Vec::new();
        visit(key, table, &mut chain, &mut done)?;
    }
    Ok(())
}

/// Depth-first walk of what `name` instantiates. `chain` is the path that
/// reached it, which is the cycle itself when it turns up on the path again.
///
/// The walk travels on fragment keys rather than on the names use sites write,
/// so a component reached under its function name and under its key is the one
/// node it is.
fn visit<'a>(
    name: &str,
    table: &'a FragmentTable,
    chain: &mut Vec<&'a str>,
    done: &mut BTreeSet<&'a str>,
) -> Result<(), ParseError> {
    let Some(fragment) = table
        .get(name)
        .or_else(|| table.by_component(name).map(|(f, _)| f))
    else {
        return Ok(());
    };
    let key = fragment.key.as_str();
    if done.contains(key) {
        return Ok(());
    }
    if let Some(at) = chain.iter().position(|k| *k == key) {
        let mut cycle: Vec<&str> = chain[at..].to_vec();
        cycle.push(key);
        let named: Vec<&str> = cycle.iter().map(|k| readable(table, k)).collect();
        return Err(ParseError::Xml(format!(
            "fragment `{}` instantiates itself: {}",
            readable(table, key),
            named.join(" -> ")
        )));
    }
    chain.push(key);
    for used in uses(fragment) {
        visit(used, table, chain, done)?;
    }
    chain.pop();
    done.insert(key);
    Ok(())
}

/// Every fragment key this fragment's body instantiates.
fn uses(fragment: &Fragment) -> Vec<&str> {
    fn walk<'a>(el: &'a Element, out: &mut Vec<&'a str>) {
        if let Some(use_site) = &el.frag_use {
            out.push(use_site.key.as_str());
        }
        for child in &el.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for el in &fragment.body {
        walk(el, &mut out);
    }
    out
}
