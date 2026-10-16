//! The page's half of `<split>`: measuring the panes off the browser's
//! boxes, and putting what a resize holds back into the page.
//!
//! The gutters drag and step in the shared split systems, which start from
//! [`Split::panes`]. On the desktop layout fills that in; here the browser
//! laid the page out, so it is read off the elements: the split's computed
//! `flex-direction`, each pane's box, and its computed `min-*` / `max-*`.
//! That read forces the browser to finish its layout, so it runs only when
//! a split is new or the pointer or a key just went down, which is when a
//! gutter can start to move.
//!
//! A resize holds the panes' `flex-basis`. The page's stylesheet does not
//! know about that, so each held basis is written on the pane's own style,
//! and taken off again when nothing holds it.

use bevy_ecs::hierarchy::Children;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::{InlineStyle, Length};
use lumen_core::input::{KeyPressed, PointerPressed};
use lumen_core::style_overrides::StyleOverrides;
use lumen_primitives::{Split, SplitAxis, SplitGutter, SplitPane, SplitPaneGeometry};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

use crate::nodes::NodeTable;

/// A computed length as pixels along an axis `of` long: `px` and `%`, and
/// `None` for the keywords (`auto`, `none`).
fn computed_px(value: &str, of: f32) -> Option<f32> {
    let value = value.trim();
    if let Some(px) = value.strip_suffix("px") {
        return px.trim().parse().ok();
    }
    let pct: f32 = value.strip_suffix('%')?.trim().parse().ok()?;
    Some(pct / 100.0 * of)
}

/// The computed value of `property` on `element`.
fn computed(element: &Element, property: &str) -> String {
    web_sys::window()
        .and_then(|w| w.get_computed_style(element).ok().flatten())
        .and_then(|s| s.get_property_value(property).ok())
        .unwrap_or_default()
}

/// Read every split off the page when one is new, or when the pointer or a
/// key went down this tick and a gutter may be about to move.
pub fn measure_splits_from_dom(
    table: NonSend<NodeTable>,
    mut presses: MessageReader<PointerPressed>,
    mut keys: MessageReader<KeyPressed>,
    mut splits: Query<(Entity, &mut Split, &Children)>,
    panes: Query<(), With<SplitPane>>,
    marks: Query<(), With<SplitGutter>>,
) {
    let input = presses.read().count() + keys.read().count() > 0;
    for (entity, mut split, children) in &mut splits {
        if !input && !split.panes.is_empty() {
            continue;
        }
        let Some(element) = table.element(entity) else {
            continue;
        };
        let axis = if computed(element, "flex-direction").starts_with("column") {
            SplitAxis::Column
        } else {
            SplitAxis::Row
        };
        let rect = element.get_bounding_client_rect();
        let length = match axis {
            SplitAxis::Row => rect.width() as f32,
            SplitAxis::Column => rect.height() as f32,
        };
        let (min_name, max_name) = match axis {
            SplitAxis::Row => ("min-width", "max-width"),
            SplitAxis::Column => ("min-height", "max-height"),
        };
        let mut measured = Split {
            axis,
            ..Split::default()
        };
        for &child in children {
            if marks.contains(child) {
                measured.gutters.push(child);
                continue;
            }
            if !panes.contains(child) {
                continue;
            }
            let Some(pane) = table.element(child) else {
                continue;
            };
            let r = pane.get_bounding_client_rect();
            let (start, size) = match axis {
                SplitAxis::Row => (r.left() as f32, r.width() as f32),
                SplitAxis::Column => (r.top() as f32, r.height() as f32),
            };
            measured.panes.push(SplitPaneGeometry {
                entity: child,
                start,
                size,
                min: computed_px(&computed(pane, min_name), length)
                    .unwrap_or(0.0)
                    .max(0.0),
                max: computed_px(&computed(pane, max_name), length).unwrap_or(f32::INFINITY),
                // The page's reset makes every box `border-box`.
                frame: 0.0,
            });
        }
        if *split != measured {
            *split = measured;
        }
    }
}

/// A pane whose held basis may have moved, or whose `style` attribute was
/// rewritten.
type BasisChanged = (
    With<SplitPane>,
    Or<(Changed<StyleOverrides>, Changed<InlineStyle>)>,
);

/// Write the `flex-basis` a resize holds on each pane onto the pane's own
/// style, and take it off when nothing holds it. Also after the inline style
/// was rewritten, which replaces the element's whole `style` attribute.
pub fn project_split_basis(
    table: NonSend<NodeTable>,
    changed: Query<(Entity, &StyleOverrides), BasisChanged>,
) {
    for (entity, overrides) in &changed {
        let Some(element) = table
            .element(entity)
            .and_then(|e| e.dyn_ref::<HtmlElement>())
        else {
            continue;
        };
        let style = element.style();
        let held = overrides
            .held_by::<SplitPane>()
            .and_then(|v| v.basis)
            .and_then(|b| match b {
                Length::Px(px) => Some(format!("{px}px")),
                Length::Percent(p) => Some(format!("{p}%")),
                Length::Auto => None,
            });
        let current = style.get_property_value("flex-basis").unwrap_or_default();
        match held {
            Some(value) if current != value => {
                let _ = style.set_property("flex-basis", &value);
            }
            Some(_) => {}
            None if !current.is_empty() => {
                let _ = style.remove_property("flex-basis");
            }
            None => {}
        }
    }
}

/// True when a key on `target` is one a focused gutter takes, so the page
/// must not also scroll for it.
pub(crate) fn is_gutter_key(target: Option<&Element>, key: &str) -> bool {
    matches!(
        key,
        "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" | "Home" | "End"
    ) && target.is_some_and(|t| {
        t.class_list()
            .contains(lumen_ir::layout_ir::WidgetPart::SplitGutter.class())
    })
}

#[cfg(test)]
mod tests {
    use super::computed_px;

    #[test]
    fn computed_lengths_read_as_pixels() {
        assert_eq!(computed_px("120px", 400.0), Some(120.0));
        assert_eq!(computed_px("25%", 400.0), Some(100.0));
        assert_eq!(computed_px("auto", 400.0), None);
        assert_eq!(computed_px("none", 400.0), None);
    }
}
