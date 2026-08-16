//! Writing a page's element tree as HTML.
//!
//! The tree is written with no whitespace of its own. An element's text is
//! the one text node inside it, so an indented document would put text
//! where the markup had none, and the browser runtime that adopts the
//! document would find a node the app does not have.
//!
//! A `<for>` block emits its anchor and nothing inside it. Its children are
//! the row template, not content, and the rows themselves come from the
//! state the page is rendered with, which the prerenderer supplies.

use std::collections::BTreeSet;

use lumen_html::contract::{DATA_LM, DATA_LM_HIDDEN, DIALOG_OPEN, NodePath};
use lumen_html::style::{Emission, rewrite_property};
use lumen_html::{escape_attr, escape_text, html_attrs, html_tag_for};
use lumen_ir::css::computed_style_map;
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec};

use crate::error::EmitError;
use crate::spec::{CssMode, PageSpec, SignalEnv, SiteSpec};
use crate::urls;

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
}

/// Write the page's element tree, starting at the page root.
pub fn emit_tree(page: &PageSpec, spec: &SiteSpec) -> Result<String, EmitError> {
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
    // A fragment use site stands in for a body that lives in the app's
    // fragment table. Expanding it is the compiler's step, not the
    // emitter's; a tree that still carries one has not been through it, and
    // writing the placeholder out would put an element in the page that the
    // app does not have.
    if let Some(frag) = &element.frag_use {
        return Err(EmitError::UnexpandedFragment {
            page: walk.page.to_string(),
            key: frag.key.clone(),
        });
    }
    let tag = html_tag_for(&element.tag).ok_or_else(|| EmitError::UnknownTag {
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

    let mut hidden = false;
    let mut open = false;
    let mut children_are_content = true;
    match element.tag.as_str() {
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
        "dialog" => {
            open = element
                .attrs
                .if_signal
                .as_ref()
                .is_none_or(|_| branch_taken(element, walk.signals));
            hidden = !open;
        }
        "for" => children_are_content = false,
        _ => {}
    }

    out.push('<');
    out.push_str(tag.name);
    for (name, value) in tag.fixed {
        write_attr(out, name, value);
    }
    for (name, value) in html_attrs(&element.tag, &element.attrs) {
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
        let style = computed_style(&element.attrs);
        if !style.is_empty() {
            write_attr(out, "style", &style);
        }
    }
    if open {
        write_attr(out, DIALOG_OPEN, "");
    }
    if hidden {
        write_attr(out, DATA_LM_HIDDEN, "");
    }
    out.push('>');

    if tag.void {
        return Ok(());
    }
    if let Some(text) = &element.attrs.text
        && !text.is_empty()
    {
        out.push_str(&escape_text(text));
    }
    if children_are_content {
        for (index, child) in element.children.iter().enumerate() {
            emit_element(out, child, &path.child(index as u32), walk)?;
        }
    }
    out.push_str("</");
    out.push_str(tag.name);
    out.push('>');
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
