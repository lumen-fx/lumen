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

use lumen_html::contract::{DATA_LM, DATA_LM_HIDDEN, NodePath};
use lumen_html::{escape_attr, escape_text, html_attrs, html_tag_for};
use lumen_ir::layout_ir::{Element, IfModeSpec};

use crate::error::EmitError;
use crate::spec::{PageSpec, SignalEnv};

/// What the walk needs to know that is not the element itself.
struct Walk<'a> {
    page: &'a str,
    signals: &'a SignalEnv,
    seen: BTreeSet<String>,
}

/// Write the page's element tree, starting at the page root.
pub fn emit_tree(page: &PageSpec) -> Result<String, EmitError> {
    let mut out = String::new();
    let mut walk = Walk {
        page: &page.key,
        signals: &page.signals,
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
    let mut children_are_content = true;
    match element.tag.as_str() {
        "if" => match element.attrs.if_mode {
            // A hidden branch stays in the document, so the runtime has
            // something to show when the signal turns true.
            IfModeSpec::Hide => hidden = !branch_taken(element, walk.signals),
            IfModeSpec::Render => children_are_content = branch_taken(element, walk.signals),
        },
        "for" => children_are_content = false,
        _ => {}
    }

    out.push('<');
    out.push_str(tag.name);
    for (name, value) in tag.fixed {
        write_attr(out, name, value);
    }
    for (name, value) in html_attrs(&element.tag, &element.attrs) {
        write_attr(out, name, &value);
    }
    write_attr(out, DATA_LM, &path_text);
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
