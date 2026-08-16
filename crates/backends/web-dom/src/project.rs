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
use lumen_html::contract::{
    DATA_LM_CHECKED, DATA_LM_DISABLED, DATA_LM_HIDDEN, DATA_LM_SELECTED, DIALOG_OPEN,
};
use web_sys::Element;

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
/// where the emitter wrote it.
fn set_text(element: &Element, text: &str) {
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
/// `<if mode="hide">` branch and a closed dialog both are. A dialog says it
/// twice: the attribute the stylesheet matches, and the `open` a browser and
/// a screen reader read.
pub fn project_visibility(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &Visible, &LumenTag), Changed<Visible>>,
) {
    for (entity, visible, tag) in &changed {
        let Some(element) = table.element(entity) else {
            continue;
        };
        set_flag(element, DATA_LM_HIDDEN, !visible.0);
        if &*tag.0 == "dialog" {
            set_flag(element, DIALOG_OPEN, visible.0);
        }
    }
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
    for entity in &selected {
        flag(entity, DATA_LM_SELECTED, true);
    }
    for entity in unselected.read() {
        flag(entity, DATA_LM_SELECTED, false);
    }
    for entity in &disabled {
        flag(entity, DATA_LM_DISABLED, true);
    }
    for entity in enabled.read() {
        flag(entity, DATA_LM_DISABLED, false);
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
        if let Some(element) = table.element(entity) {
            set_attribute(element, "value", Some(&value.value.to_string()));
        }
    }
}
