//! Keeping the page in step with the world.
//!
//! Each system watches one component and writes the one thing a browser
//! reads for it. Every write is a comparison first: the value already in the
//! document is checked, and an equal one is left alone. That is what makes a
//! prerendered page load with no DOM mutations at all, and it is also just
//! correct, because a write the browser cannot distinguish from what is
//! there still costs a style invalidation.
//!
//! What is NOT here is anything the browser's own CSS engine owns. Colors,
//! sizes, spacing and hover states reach the page as a stylesheet; the world
//! keeps computing them for the desktop and nothing reads them here.

use bevy_ecs::prelude::*;
use lumen_core::components::{
    Disabled, InlineStyle, LumenAttributes, LumenClasses, LumenTag, Selected, SliderValue,
    TextContent, Toggleable, Visible,
};
use lumen_html::contract::{DATA_LM_CHECKED, DATA_LM_DISABLED, DATA_LM_HIDDEN, DATA_LM_SELECTED};
use lumen_html::is_disableable;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlDialogElement, HtmlInputElement, HtmlTextAreaElement};

use crate::nodes::NodeTable;

/// Set an attribute, or take it off, unless the element already says that.
fn set_attribute(element: &Element, name: &str, value: Option<&str>) {
    match value {
        Some(value) => {
            if element.get_attribute(name).as_deref() != Some(value) {
                let _ = element.set_attribute(name, value);
            }
        }
        None => {
            if element.has_attribute(name) {
                let _ = element.remove_attribute(name);
            }
        }
    }
}

/// Set a boolean attribute, which is present or absent and never false.
fn set_flag(element: &Element, name: &str, on: bool) {
    set_attribute(element, name, on.then_some(""));
}

/// The `style` value for an inline-style component, as a browser reads it.
pub(crate) fn style_value(style: &InlineStyle) -> String {
    let mut out = String::new();
    for (property, value) in &style.0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(property);
        out.push_str(": ");
        out.push_str(value);
        out.push(';');
    }
    out
}

/// An element's own text is the text node before its children, which is
/// where the emitter wrote it. A form control is the exception: its text is
/// its value, and it has no children to hold one.
fn set_text(element: &Element, text: &str) {
    if let Some(input) = element.dyn_ref::<HtmlInputElement>() {
        if input.value() != text {
            input.set_value(text);
        }
        return;
    }
    if let Some(area) = element.dyn_ref::<HtmlTextAreaElement>() {
        if area.value() != text {
            area.set_value(text);
        }
        return;
    }
    match element.first_child() {
        // A text node already there is the one to update.
        Some(node) if node.node_type() == web_sys::Node::TEXT_NODE => {
            if node.text_content().as_deref() != Some(text) {
                node.set_text_content(Some(text));
            }
        }
        // Nothing to update, and nothing worth adding for empty text.
        _ if text.is_empty() => {}
        first => {
            let Some(document) = element.owner_document() else {
                return;
            };
            let node = document.create_text_node(text);
            let _ = element.insert_before(&node, first.as_ref());
        }
    }
}

/// Project an element's text.
pub fn project_text(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &TextContent), Changed<TextContent>>,
) {
    for (entity, text) in &changed {
        if let Some(element) = table.element(entity) {
            set_text(element, &text.0);
        }
    }
}

/// Project the class list, which is how nearly all of an app's styling
/// reaches the page.
pub fn project_classes(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &LumenTag, &LumenClasses), Changed<LumenClasses>>,
) {
    for (entity, tag, classes) in &changed {
        if let Some(element) = table.element(entity) {
            let value = crate::nodes::class_value(&tag.0, Some(classes));
            set_attribute(element, "class", Some(&value));
        }
    }
}

/// Project the attributes that have no component of their own: `role`,
/// `aria-*`, `data-*`, and whatever else a script set.
pub fn project_attributes(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &LumenAttributes), Changed<LumenAttributes>>,
) {
    for (entity, attributes) in &changed {
        let Some(element) = table.element(entity) else {
            continue;
        };
        for (name, value) in &attributes.0 {
            set_attribute(element, name, Some(value));
        }
    }
}

/// Project the inline style, which is the tier that outranks the stylesheet
/// on both sides.
pub fn project_inline_style(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &InlineStyle), Changed<InlineStyle>>,
) {
    for (entity, style) in &changed {
        if let Some(element) = table.element(entity) {
            let value = style_value(style);
            set_attribute(element, "style", (!value.is_empty()).then_some(&value));
        }
    }
}

/// Project whether a node is shown.
///
/// A hidden node keeps its markup and loses its box, which is what an
/// `<if mode="hide">` branch is.
///
/// A dialog is the browser's own: showing it modally is what puts it in the
/// top layer over everything else on the page, makes the rest of the
/// document inert, and gives Escape somewhere to land. All of that is what a
/// Lumen dialog is on the desktop, so none of it is written here; the
/// element is told to show or to close and it maintains its own `open`.
pub fn project_visibility(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &Visible), Changed<Visible>>,
) {
    for (entity, visible) in &changed {
        let Some(element) = table.element(entity) else {
            continue;
        };
        match element.dyn_ref::<HtmlDialogElement>() {
            Some(dialog) => show_dialog(dialog, visible.0),
            None => set_flag(element, DATA_LM_HIDDEN, !visible.0),
        }
    }
}

/// Show a dialog modally, or close it.
///
/// A page rendered with the dialog already showing carries a plain `open`,
/// because static markup has no way to say modal. Showing it modally is
/// refused while it is open at all, so the one the document came with is
/// closed first; on every later show there is nothing to close.
fn show_dialog(dialog: &HtmlDialogElement, show: bool) {
    if !show {
        dialog.close();
        return;
    }
    if dialog.open() {
        dialog.close();
    }
    let _ = dialog.show_modal();
}

/// Project the states a Lumen widget carries as components and a browser
/// carries as attributes: which tab is current, whether a toggle is on,
/// whether a control is disabled, where a slider sits.
pub fn project_control_state(
    table: NonSend<NodeTable>,
    selected: Query<Entity, Added<Selected>>,
    mut unselected: RemovedComponents<Selected>,
    toggled: Query<(Entity, &Toggleable), Changed<Toggleable>>,
    disabled: Query<Entity, Added<Disabled>>,
    mut enabled: RemovedComponents<Disabled>,
    values: Query<(Entity, &SliderValue), Changed<SliderValue>>,
) {
    let flag = |entity: Entity, name: &str, on: bool| {
        if let Some(element) = table.element(entity) {
            set_flag(element, name, on);
        }
    };
    // What Lumen calls selected is what a radio calls checked, and the markup
    // wrote a starting value there. Moving both keeps the mark from claiming
    // a control is on after the one beside it took over.
    let select = |entity: Entity, on: bool| {
        let Some(element) = table.element(entity) else {
            return;
        };
        set_flag(element, DATA_LM_SELECTED, on);
        if matches!(element.get_attribute("type").as_deref(), Some("radio")) {
            set_flag(element, DATA_LM_CHECKED, on);
        }
    };
    for entity in &selected {
        select(entity, true);
    }
    for entity in unselected.read() {
        select(entity, false);
    }
    // A control the browser knows how to disable is disabled, which is what
    // takes it out of the tab order and stops it answering a click at all.
    // The mark beside it is the one the stylesheet reads, and every other
    // kind of element has only that.
    let disable = |entity: Entity, on: bool| {
        let Some(element) = table.element(entity) else {
            return;
        };
        set_flag(element, DATA_LM_DISABLED, on);
        if is_disableable(&element.tag_name().to_ascii_lowercase()) {
            set_flag(element, "disabled", on);
        }
    };
    for entity in &disabled {
        disable(entity, true);
    }
    for entity in enabled.read() {
        disable(entity, false);
    }
    for (entity, toggle) in &toggled {
        flag(entity, DATA_LM_CHECKED, toggle.checked);
        if let Some(element) = table.element(entity)
            && element.has_attribute("role")
        {
            set_attribute(element, "aria-checked", Some(&toggle.checked.to_string()));
        }
    }
    for (entity, value) in &values {
        let Some(element) = table.element(entity) else {
            continue;
        };
        let text = value.value.to_string();
        // A range input stops reading its `value` attribute the moment the
        // visitor moves it, so what a script writes afterwards has to go to
        // the value the browser is actually showing. A `<progress>` has no
        // such value and reads the attribute always.
        match element.dyn_ref::<HtmlInputElement>() {
            Some(input) => {
                if input.value() != text {
                    input.set_value(&text);
                }
            }
            None => set_attribute(element, "value", Some(&text)),
        }
    }
}
