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

use crate::layout_ir::{DeferredAttr, Element, InterpolationSlot, LintFinding, ParseError};
use crate::parser_html::{apply_deferred_attribute, classify_markers, push_unique};
use lumen_ir::fragment::{DEFAULT_SLOT, Fragment, FragmentTable, SLOT_TAG};
use std::collections::BTreeSet;

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
    reject_cycles(table)?;
    for _ in 0..MAX_PASSES {
        let mut expanded = false;
        inline_list(&mut root.children, table, lint_findings, &mut expanded)?;
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
fn inline_list(
    list: &mut Vec<Element>,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
    expanded: &mut bool,
) -> Result<(), ParseError> {
    let mut out: Vec<Element> = Vec::with_capacity(list.len());
    for mut el in std::mem::take(list) {
        if el.frag_use.is_some() {
            out.extend(instantiate(el, table, lint_findings)?);
            *expanded = true;
        } else {
            inline_list(&mut el.children, table, lint_findings, expanded)?;
            out.push(el);
        }
    }
    *list = out;
    Ok(())
}

/// Build the subtree one use site stands for.
fn instantiate(
    mut site: Element,
    table: &FragmentTable,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<Vec<Element>, ParseError> {
    let use_site = *site.frag_use.take().expect("caller checked the site");
    let fragment = table.get(&use_site.key).ok_or_else(|| {
        ParseError::Xml(format!(
            "unknown fragment `{}`: no <template name=\"{}\"> is declared",
            use_site.key, use_site.key
        ))
    })?;

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

/// Depth-first walk of what `key` instantiates. `chain` is the path that
/// reached it, which is the cycle itself when `key` turns up on it again.
fn visit<'a>(
    key: &'a str,
    table: &'a FragmentTable,
    chain: &mut Vec<&'a str>,
    done: &mut BTreeSet<&'a str>,
) -> Result<(), ParseError> {
    if done.contains(key) {
        return Ok(());
    }
    if let Some(at) = chain.iter().position(|k| *k == key) {
        let mut cycle: Vec<&str> = chain[at..].to_vec();
        cycle.push(key);
        return Err(ParseError::Xml(format!(
            "fragment `{key}` instantiates itself: {}",
            cycle.join(" -> ")
        )));
    }
    let Some(fragment) = table.get(key) else {
        return Ok(());
    };
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
