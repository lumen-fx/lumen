//! The element trees a menu is built from.
//!
//! A `<menu id>` popup and an in-window menu bar are the same parts: a panel
//! of item buttons and separator rules, floating over the page while its
//! `__menu_open:<id>` signal is set. The parser builds a popup from the
//! `<menu>` markup an author writes; the runtime builds a whole bar from the
//! app's `<menubar>` where the platform has no native one. Both build it
//! here, so the two cannot drift apart.
//!
//! Every part carries a class (`menu-panel`, `menu-item`, `menu-separator`,
//! `menubar`, `menubar-menu`, `menubar-button`), so a skin or an app styles
//! it like any element. The only declarations written here are the ones
//! that make a panel float: its position and its inset.

use crate::layout_ir::{
    Attributes, DropdownButtonSpec, Element, IfModeSpec, MenuBarSpec, MenuEntrySpec, WidgetRole,
};

/// One row of a menu panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuRow {
    /// A clickable item, relayed to scripts as `on_menu(id)`.
    Item {
        /// The item's id.
        id: String,
        /// The text the row shows.
        label: String,
        /// Unclickable, skipped by arrow navigation, `:disabled`-styled.
        disabled: bool,
    },
    /// A rule between groups of items.
    Separator,
}

/// The reserved signal holding the open state of menu `menu_id`.
#[must_use]
pub fn menu_open_signal(menu_id: &str) -> String {
    format!("__menu_open:{menu_id}")
}

/// Write a declaration the builder relies on. Every one written here is a
/// known property with a value it accepts.
fn set_built_style(attrs: &mut Attributes, property: &str, value: &str) {
    match crate::css::apply_markup_declaration(property, value, attrs) {
        Ok(true) => {}
        other => unreachable!(
            "a menu part wrote a style it cannot apply: {property}: {value} ({other:?})"
        ),
    }
}

/// The popup for menu `menu_id`: an `<if eq="true" mode="hide">` keyed on
/// the menu's open signal, around a floating `overlay.menu-panel` of rows.
///
/// The panel takes no part in its ancestors' layout. `right` and `bottom`
/// stay `auto`, so the panel shrink-wraps its items, and the items'
/// `width: 100%` from the user-agent sheet equalises them to the widest.
/// The open signal is seeded `false`, so the menu starts closed.
#[must_use]
pub fn popup_menu(menu_id: &str, rows: &[MenuRow]) -> Element {
    let open_signal = menu_open_signal(menu_id);
    let body = rows
        .iter()
        .map(|row| match row {
            MenuRow::Item {
                id,
                label,
                disabled,
            } => {
                // Row height and padding are theme metrics on `.menu-item`
                // in the user-agent sheet.
                let mut btn = Element {
                    tag: "button".to_string(),
                    ..Default::default()
                };
                btn.attrs.text = Some(label.clone());
                btn.attrs.classes = vec!["menu-item".to_string()];
                btn.attrs.menu_item = Some((open_signal.clone(), id.clone()));
                btn.attrs.disabled = *disabled;
                btn
            }
            MenuRow::Separator => {
                // A rule line, not a gap: `.menu-separator` in the
                // user-agent sheet keeps it one pixel tall rather than
                // taking the grow a written `<spacer>` gets.
                let mut sep = Element {
                    tag: "spacer".to_string(),
                    ..Default::default()
                };
                sep.attrs.classes = vec!["menu-separator".to_string()];
                sep
            }
        })
        .collect();
    let mut panel = Element {
        tag: "overlay".to_string(),
        children: body,
        ..Default::default()
    };
    set_built_style(&mut panel.attrs, "position", "absolute");
    set_built_style(&mut panel.attrs, "inset", "0 auto auto 0");
    panel.attrs.classes = vec!["menu-panel".to_string()];
    panel.attrs.id = Some(menu_id.to_string());
    // Outside-click dismissal and placement key off the open signal.
    panel.attrs.popup_panel = Some(open_signal.clone());

    let mut if_block = Element {
        tag: "if".to_string(),
        children: vec![panel],
        ..Default::default()
    };
    if_block.attrs.if_signal = Some(open_signal.clone());
    if_block.attrs.if_eq = Some("true".to_string());
    if_block.attrs.if_mode = IfModeSpec::Hide;
    if_block.attrs.signal_seed = Some((open_signal, "false".to_string()));
    if_block.attrs.widget = Some(WidgetRole::Menu);
    if_block
}

/// The id the menu bar gives its `index`-th menu.
#[must_use]
pub fn menubar_menu_id(index: usize) -> String {
    format!("menubar-{index}")
}

/// An in-window menu bar holding the menus of `spec`, for a platform with
/// no native one.
///
/// A `row.menubar`, floating at the top of the root, holds one
/// `column.menubar-menu` per menu: a
/// `button.menubar-button` showing the menu's label, and the menu's popup
/// under it. The button opens and closes its menu the way a dropdown header
/// opens its list, and the popup machinery anchors the panel below it and
/// flips it at the window edges. The `n`-th menu's id is
/// [`menubar_menu_id`]`(n)`, so a script reaches it with `open_menu`.
#[must_use]
pub fn in_window_menubar(spec: &MenuBarSpec) -> Element {
    let menus = spec
        .menus
        .iter()
        .enumerate()
        .map(|(index, menu)| {
            let menu_id = menubar_menu_id(index);
            let rows: Vec<MenuRow> = menu
                .items
                .iter()
                .map(|entry| match entry {
                    MenuEntrySpec::Item { id, label, .. } => MenuRow::Item {
                        id: id.clone(),
                        label: label.clone(),
                        disabled: false,
                    },
                    MenuEntrySpec::Separator => MenuRow::Separator,
                })
                .collect();
            let mut button = Element {
                tag: "button".to_string(),
                attrs: Box::new(Attributes {
                    tab_index: Some(0),
                    ..Attributes::default()
                }),
                ..Default::default()
            };
            button.attrs.text = Some(menu.label.clone());
            button.attrs.classes = vec!["menubar-button".to_string()];
            // A header with no options: it only opens and closes the panel
            // bound to the same signal, which is what anchors the panel
            // below it.
            button.attrs.dropdown_button = Some(DropdownButtonSpec {
                open_signal: menu_open_signal(&menu_id),
                value_signal: String::new(),
                options: Vec::new(),
            });
            let mut column = Element {
                tag: "column".to_string(),
                children: vec![button, popup_menu(&menu_id, &rows)],
                ..Default::default()
            };
            column.attrs.classes = vec!["menubar-menu".to_string()];
            column
        })
        .collect();
    let mut bar = Element {
        tag: "row".to_string(),
        children: menus,
        ..Default::default()
    };
    // It floats at the top of the root rather than taking a place in the
    // root's flow, so the root's own `justify` and `align` lay the app's
    // content out as they would with no bar. The runtime keeps the bar's
    // height clear above that content.
    set_built_style(&mut bar.attrs, "position", "absolute");
    set_built_style(&mut bar.attrs, "inset", "0 0 auto 0");
    bar.attrs.classes = vec!["menubar".to_string()];
    bar.attrs.widget = Some(WidgetRole::MenuBar);
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_ir::MenuSpec;

    fn spec() -> MenuBarSpec {
        MenuBarSpec {
            menus: vec![
                MenuSpec {
                    label: "File".to_string(),
                    items: vec![
                        MenuEntrySpec::Item {
                            id: "open".to_string(),
                            label: "Open".to_string(),
                            accelerator: Some("CommandOrControl+O".to_string()),
                        },
                        MenuEntrySpec::Separator,
                        MenuEntrySpec::Item {
                            id: "quit".to_string(),
                            label: "Quit".to_string(),
                            accelerator: None,
                        },
                    ],
                },
                MenuSpec {
                    label: "Help".to_string(),
                    items: vec![MenuEntrySpec::Item {
                        id: "about".to_string(),
                        label: "About".to_string(),
                        accelerator: None,
                    }],
                },
            ],
        }
    }

    /// The bar is one menu column per `<menu>`, each a labelled button over
    /// that menu's popup, and the button opens the popup's own signal.
    #[test]
    fn the_bar_holds_a_button_and_a_popup_per_menu() {
        let bar = in_window_menubar(&spec());
        assert_eq!(bar.tag, "row");
        assert_eq!(bar.attrs.classes, vec!["menubar"]);
        assert_eq!(bar.attrs.widget, Some(WidgetRole::MenuBar));
        assert_eq!(bar.children.len(), 2);
        let file = &bar.children[0];
        assert_eq!(file.attrs.classes, vec!["menubar-menu"]);
        let button = &file.children[0];
        assert_eq!(button.attrs.text.as_deref(), Some("File"));
        assert_eq!(button.attrs.classes, vec!["menubar-button"]);
        let opens = button
            .attrs
            .dropdown_button
            .as_ref()
            .expect("opens a panel");
        assert_eq!(opens.open_signal, "__menu_open:menubar-0");
        assert!(opens.options.is_empty());

        let popup = &file.children[1];
        assert_eq!(
            popup.attrs.if_signal.as_deref(),
            Some("__menu_open:menubar-0")
        );
        let panel = &popup.children[0];
        assert_eq!(panel.attrs.classes, vec!["menu-panel"]);
        assert_eq!(panel.attrs.id.as_deref(), Some("menubar-0"));
        let rows: Vec<_> = panel
            .children
            .iter()
            .map(|r| (r.attrs.classes[0].as_str(), r.attrs.menu_item.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "menu-item",
                    Some(("__menu_open:menubar-0".to_string(), "open".to_string()))
                ),
                ("menu-separator", None),
                (
                    "menu-item",
                    Some(("__menu_open:menubar-0".to_string(), "quit".to_string()))
                ),
            ]
        );
    }
}
