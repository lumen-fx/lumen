//! Writing a page's element tree as HTML.
//!
//! The tree is written with no whitespace of its own. An element's text is
//! the one text node inside it, so an indented document would put text
//! where the markup had none, and the browser runtime that adopts the
//! document would find a node the app does not have.
//!
//! A `<for>` block's children are the row template, never content. What goes
//! inside it is one instance of that template per row of the array signal the
//! page is rendered with, with the row's own values substituted in.
//!
//! An element is written with the values its `bind-*` attributes hold in that
//! same state, which [`crate::bindings`] resolves; what the state answers
//! nothing for keeps the fallback the markup carries.

use std::cell::RefCell;
use std::collections::BTreeSet;

use lumen_html::contract::{DATA_LM, DATA_LM_HIDDEN, DATA_LM_SELECTED, DIALOG_OPEN, NodePath};
use lumen_html::style::{Emission, rewrite_property};
use lumen_html::{escape_attr, escape_text, html_attrs, html_tag_for};
use lumen_ir::css::computed_style_map;
use lumen_ir::fragment::FRAGMENT_TAG;
use lumen_ir::interpolate::{Scope, substitute_element};
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec};

use crate::error::EmitError;
use crate::spec::{CssMode, PageSpec, SignalEnv, SiteSpec};
use crate::{bindings, urls};

/// What the walk needs to know that is not the element itself.
struct Walk<'a> {
    page: &'a str,
    signals: &'a SignalEnv,
    css_mode: CssMode,
    /// Site base path, which is what the shared files hang off.
    base: String,
    /// Base path of this locale's documents, which a link hangs off.
    tree: String,
    /// Page keys, longest first, for resolving a link.
    keys: Vec<String>,
    entry: &'a str,
    seen: BTreeSet<String>,
    /// Where the page could not be written the way the app meant it.
    warnings: &'a mut Vec<String>,
}

/// Write the page's element tree, starting at the page root.
pub fn emit_tree(
    page: &PageSpec,
    spec: &SiteSpec,
    warnings: &mut Vec<String>,
) -> Result<String, EmitError> {
    let mut out = String::new();
    let base = urls::normalize_base(&spec.web.base_path);
    let mut walk = Walk {
        page: &page.key,
        signals: &page.signals,
        css_mode: spec.web.css_mode,
        tree: urls::join(&base, &spec.locale.prefix()),
        base,
        keys: spec.keys(),
        entry: &spec.web.entry,
        seen: BTreeSet::new(),
        warnings,
    };
    emit_element(&mut out, &page.ir.root, &NodePath::root(), &mut walk)?;
    Ok(out)
}

fn emit_element(
    out: &mut String,
    element: &Element,
    path: &NodePath,
    walk: &mut Walk<'_>,
) -> Result<(), EmitError> {
    // A component whose body the build could stand in for is already the body
    // by the time the tree gets here. What is left carrying a use site is a
    // component that has to run, and the element is the marker the runtime
    // replaces with what the call returns. It is written as the empty box it
    // is: the node the call builds is not knowable here, and what the use site
    // wrote inside the marker goes with the marker when the replacement lands,
    // so writing that would put content in the page the app never has.
    let marker = element.frag_use.is_some();
    let ir_tag = if marker {
        FRAGMENT_TAG
    } else {
        element.tag.as_str()
    };
    let tag = html_tag_for(ir_tag).ok_or_else(|| EmitError::UnknownTag {
        page: walk.page.to_string(),
        tag: element.tag.clone(),
    })?;
    let path_text = path.to_string();
    if !walk.seen.insert(path_text.clone()) {
        return Err(EmitError::DuplicateNodePath {
            page: walk.page.to_string(),
            path: path_text,
        });
    }
    // What the element's bindings hold in the state this page is rendered
    // with. An element whose bindings the state answers nothing for is emitted
    // from its own attributes, which is what leaves the authored fallback in
    // the page.
    let bound = bindings::resolved(ir_tag, &element.attrs, walk.signals);
    let attrs = bound.as_ref().unwrap_or(&element.attrs);

    let mut hidden = false;
    let mut open = false;
    let mut children_are_content = !marker;
    match ir_tag {
        "if" => match element.attrs.if_mode {
            // A hidden branch stays in the document, so the runtime has
            // something to show when the signal turns true.
            IfModeSpec::Hide => hidden = !branch_taken(element, walk.signals),
            IfModeSpec::Render => children_are_content = branch_taken(element, walk.signals),
        },
        // `<dialog open="signal">` is an `<if mode="hide">` that is also a
        // real dialog: `open` names the signal, not the state. The state is
        // what a browser reads, so it is resolved here. A dialog with no
        // signal is always showing, which is what it does on the desktop.
        // Whether it shows is the `open` attribute alone; a browser hides a
        // closed dialog itself and the reset makes that rule one an author
        // sheet cannot outrank.
        "dialog" => {
            open = element
                .attrs
                .if_signal
                .as_ref()
                .is_none_or(|_| branch_taken(element, walk.signals));
        }
        "for" => children_are_content = false,
        _ => {}
    }

    out.push('<');
    out.push_str(tag.name);
    for (name, value) in tag.fixed {
        write_attr(out, name, value);
    }
    for (name, value) in html_attrs(ir_tag, attrs) {
        // A link and an asset reference are written as the IR holds them,
        // which is a page key and a path relative to the site root. Both
        // become URLs here, where the site's base path and page set are
        // known.
        match name {
            "href" => write_attr(
                out,
                name,
                &urls::page_href(&value, &walk.tree, &walk.keys, walk.entry),
            ),
            "src" => write_attr(out, name, &urls::asset_src(&value, &walk.base)),
            _ => write_attr(out, name, &value),
        }
    }
    write_attr(out, DATA_LM, &path_text);
    if walk.css_mode == CssMode::Computed {
        let style = computed_style(attrs);
        if !style.is_empty() {
            write_attr(out, "style", &style);
        }
    }
    if open {
        write_attr(out, DIALOG_OPEN, "");
    }
    // Which tab is current is a signal, and the strip button that matches it
    // is the one Lumen calls `:selected`. The runtime maintains this mark; the
    // page needs it too, or the current tab is unmarked until the runtime
    // loads and unmarked forever without it.
    if let Some((signal, value)) = &attrs.tab_strip
        && walk.signals.global(signal) == Some(value.as_str())
    {
        write_attr(out, DATA_LM_SELECTED, "");
    }
    if hidden {
        write_attr(out, DATA_LM_HIDDEN, "");
    }
    out.push('>');

    if tag.void {
        return Ok(());
    }
    if let Some(text) = &attrs.text
        && !text.is_empty()
        && !marker
    {
        out.push_str(&escape_text(text));
    }
    if children_are_content {
        for (index, child) in element.children.iter().enumerate() {
            emit_element(out, child, &path.child(index as u32), walk)?;
        }
    } else if ir_tag == "for" {
        emit_rows(out, element, path, walk)?;
    }
    out.push_str("</");
    out.push_str(tag.name);
    out.push('>');
    Ok(())
}

/// Write one instance of a `<for>` block's row template per row of the array
/// it iterates.
///
/// A row element's identity is its FLAT position in the block's child list,
/// not the row number: the reconciler spawns one entity per template element
/// per row as flat siblings, and the runtime numbers those siblings by
/// position when it looks for the element each one belongs to. A block whose
/// template is two elements therefore starts its second row at slot 2.
///
/// The rows are not put through the cascade again. The browser has its own
/// over the same stylesheet, which is why the reconciler leaves a row
/// unresolved in a page too.
fn emit_rows(
    out: &mut String,
    element: &Element,
    path: &NodePath,
    walk: &mut Walk<'_>,
) -> Result<(), EmitError> {
    let Some(name) = &element.attrs.each else {
        return Ok(());
    };
    // The signal environment outlives the walk, so reading the rows out of it
    // does not stand in the way of writing the document.
    let signals = walk.signals;
    let Some(rows) = signals.rows(name).filter(|rows| !rows.is_empty()) else {
        return Ok(());
    };
    let body = &element.children;
    if body.is_empty() {
        return Ok(());
    }
    // Which rows a virtualized block mounts comes from the offset of the
    // `<scroll>` it sits in, which a build machine cannot know. A guessed
    // prefix would be markup the runtime takes straight back out.
    if element.attrs.virtualized {
        walk.warnings.push(format!(
            "page `{}`: the virtualized `<for each=\"{name}\">` is emitted with no rows, \
             because which rows are in view is not known until the page is scrolled",
            walk.page
        ));
        return Ok(());
    }

    let missing: RefCell<BTreeSet<String>> = RefCell::new(BTreeSet::new());
    let report = |field: &str| {
        missing.borrow_mut().insert(field.to_string());
    };
    for (index, item) in rows.iter().enumerate() {
        let scope = Scope::new(signals)
            .with_row(item, index)
            .reporting_to(&report);
        for (offset, template) in body.iter().enumerate() {
            let slot = index * body.len() + offset;
            let instance = substitute_element(template, &scope);
            emit_element(out, &instance, &path.row(slot as u32), walk)?;
        }
    }
    for field in missing.into_inner() {
        walk.warnings.push(format!(
            "page `{}`: `<for each=\"{name}\">` reads row field `{field}`, which its records do \
             not carry; it renders empty",
            walk.page
        ));
    }
    Ok(())
}

/// What Lumen's cascade resolved for this element, as an inline style.
///
/// Only what a browser can be told this way survives: a value that stands
/// for a state (a hover fill) or for a knob with no CSS property behind it
/// has nowhere to land on an element.
fn computed_style(attrs: &Attributes) -> String {
    let mut out = String::new();
    for (name, value) in computed_style_map(attrs) {
        let Emission::Plain(decls) = rewrite_property(&name, &value) else {
            continue;
        };
        for decl in decls {
            if !out.is_empty() {
                out.push(';');
            }
            out.push_str(&decl.name);
            out.push(':');
            out.push_str(&decl.value);
        }
    }
    out
}

/// Whether an `<if>` block's condition holds in the state being rendered.
///
/// With `eq` the signal has to equal that value; without it, any truthy
/// value will do. This is the rule the desktop reconciler applies.
fn branch_taken(element: &Element, signals: &SignalEnv) -> bool {
    let Some(signal) = &element.attrs.if_signal else {
        return false;
    };
    match &element.attrs.if_eq {
        Some(expected) => signals.global(signal) == Some(expected.as_str()),
        None => signals.is_truthy(signal),
    }
}

fn write_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(&escape_attr(value));
    out.push('"');
}
