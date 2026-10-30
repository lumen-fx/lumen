//! The element trees a menu is built from.
//!
//! A `<menu id>` popup and an in-window menu bar are the same parts: a
//! `menu` element holding `menuitem` and `separator` elements, floating over
//! the page while its `__menu_open:<id>` signal is set. The parser builds a
//! popup from the `<menu>` markup an author writes; the runtime builds a
//! whole bar from the app's `<menubar>` where the platform has no native
//! one. Both build it here, so the two cannot drift apart.
//!
//! The menu and its items stay the elements they were written as, so a
//! script adds, removes and relabels items with the DOM calls it uses for
//! any element. The menu is mounted with the page, closed, rather than when
//! it first opens, so its items are there to find from the start. The bar's
//! own parts carry a class (`menubar`, `menubar-menu`, `menubar-button`), so
//! a skin or an app styles it like any element. The only declarations
//! written here are the ones that make a panel float: its position and its
//! inset.

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
        /// The keyboard shortcut the row shows beside its label, as the
        /// markup wrote it (`CommandOrControl+Z`). Shown only: binding it is
        /// the menu bar's business, or the app's.
        accel: Option<String>,
    },
    /// A rule between groups of items.
    Separator,
}

/// An accelerator (`CommandOrControl+Shift+S`, `Alt+F4`) as a menu shows it.
///
/// Off macOS it reads the way Windows and Linux menus write one: modifiers
/// and the key joined by `+`, `CommandOrControl` as `Ctrl`, the logo key as
/// `Super`. On macOS it is the modifier symbols in the system's order
/// (Control, Option, Shift, Command) followed by the key, so
/// `CommandOrControl+Shift+S` reads as Shift-Command-S. A single-letter key
/// is upper-cased; anything this does not know is kept as written.
#[must_use]
pub fn shortcut_label(accel: &str, mac: bool) -> String {
    let mut parts: Vec<&str> = accel.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let key = if key.chars().count() == 1 {
        key.to_uppercase()
    } else {
        key.to_string()
    };
    // Control, Alt/Option, Shift, Super/Command: the order macOS draws its
    // symbols in.
    let mut held = [false; 4];
    let mut unknown = Vec::new();
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "commandorcontrol" | "cmdorctrl" | "cmdorcontrol" | "commandorctrl" => {
                held[if mac { 3 } else { 0 }] = true;
            }
            "control" | "ctrl" => held[0] = true,
            "alt" | "option" => held[1] = true,
            "shift" => held[2] = true,
            "super" | "cmd" | "command" | "meta" => held[3] = true,
            _ => unknown.push(part),
        }
    }
    if mac {
        const SYMBOLS: [&str; 4] = ["\u{2303}", "\u{2325}", "\u{21e7}", "\u{2318}"];
        let mut out: String = unknown.concat();
        for (on, symbol) in held.iter().zip(SYMBOLS) {
            if *on {
                out.push_str(symbol);
            }
        }
        out.push_str(&key);
        return out;
    }
    const NAMES: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];
    let mut out: Vec<&str> = held
        .iter()
        .zip(NAMES)
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
    out.extend(unknown);
    out.push(&key);
    out.join("+")
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

/// The `menuitem` element for one item: `id` names it to scripts, `label`
/// is its text, and `accel` the shortcut it shows.
#[must_use]
pub fn menu_item(id: &str, label: &str, disabled: bool, accel: Option<&str>) -> Element {
    let mut item = Element {
        tag: "menuitem".to_string(),
        ..Default::default()
    };
    item.attrs.id = Some(id.to_string());
    item.attrs.text = Some(label.to_string());
    item.attrs.disabled = disabled;
    item.attrs.shortcut = accel.map(str::to_string);
    item
}

/// The part of a menu item that shows its keyboard shortcut, `text`: a
/// `label.menu-accel`, pushed to the row's far end by the user-agent sheet.
#[must_use]
pub fn menu_accel_part(text: &str) -> Element {
    let mut part = Element {
        tag: "label".to_string(),
        ..Default::default()
    };
    part.attrs.text = Some(text.to_string());
    part.attrs.classes = vec!["menu-accel".to_string()];
    part
}

/// The `separator` element: a rule between groups of items.
#[must_use]
pub fn menu_separator() -> Element {
    Element {
        tag: "separator".to_string(),
        ..Default::default()
    }
}

/// The popup for menu `menu_id` holding `rows`. See [`popup_menu_of`].
#[must_use]
pub fn popup_menu(menu_id: &str, rows: &[MenuRow]) -> Element {
    let items = rows
        .iter()
        .map(|row| match row {
            MenuRow::Item {
                id,
                label,
                disabled,
                accel,
            } => menu_item(id, label, *disabled, accel.as_deref()),
            MenuRow::Separator => menu_separator(),
        })
        .collect();
    popup_menu_of(menu_id, items)
}

/// The popup for menu `menu_id` holding `items` (`menuitem`, `separator`,
/// or a `<for>` building either): an `<if eq="true">` keyed on the menu's
/// open signal, around a floating `menu` element.
///
/// The `<if>` mounts its body with the page and only shows it while the menu
/// is open, so the menu's items are elements from the start. The panel takes
/// no part in its ancestors' layout. `right` and `bottom` stay `auto`, so the
/// panel shrink-wraps its items, and the items' `width: 100%` from the
/// user-agent sheet equalises them to the widest. The open signal is seeded
/// `false`, so the menu starts closed.
#[must_use]
pub fn popup_menu_of(menu_id: &str, items: Vec<Element>) -> Element {
    let open_signal = menu_open_signal(menu_id);
    let mut panel = Element {
        tag: "menu".to_string(),
        children: items,
        ..Default::default()
    };
    set_built_style(&mut panel.attrs, "position", "absolute");
    set_built_style(&mut panel.attrs, "inset", "0 auto auto 0");
    panel.attrs.id = Some(menu_id.to_string());
    // Outside-click dismissal, placement, and the items' wiring key off the
    // open signal.
    panel.attrs.popup_panel = Some(open_signal.clone());

    let mut if_block = Element {
        tag: "if".to_string(),
        children: vec![panel],
        ..Default::default()
    };
    if_block.attrs.if_signal = Some(open_signal.clone());
    if_block.attrs.if_eq = Some("true".to_string());
    if_block.attrs.if_mode = IfModeSpec::Mounted;
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
                    MenuEntrySpec::Item {
                        id,
                        label,
                        accelerator,
                    } => MenuRow::Item {
                        id: id.clone(),
                        label: label.clone(),
                        disabled: false,
                        accel: accelerator.clone(),
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
        assert_eq!(
            popup.attrs.if_mode,
            IfModeSpec::Mounted,
            "items exist closed"
        );
        let panel = &popup.children[0];
        assert_eq!(panel.tag, "menu");
        assert_eq!(panel.attrs.id.as_deref(), Some("menubar-0"));
        assert_eq!(
            panel.attrs.popup_panel.as_deref(),
            Some("__menu_open:menubar-0")
        );
        let rows: Vec<_> = panel
            .children
            .iter()
            .map(|r| (r.tag.as_str(), r.attrs.id.as_deref()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("menuitem", Some("open")),
                ("separator", None),
                ("menuitem", Some("quit")),
            ]
        );
    }

    /// A shortcut reads the way the platform's menus write one: `Ctrl+`
    /// names off macOS, the modifier symbols in the system's order on it.
    #[test]
    fn a_shortcut_reads_the_platform_s_way() {
        assert_eq!(shortcut_label("CommandOrControl+z", false), "Ctrl+Z");
        assert_eq!(
            shortcut_label("Shift+CmdOrCtrl+S", false),
            "Ctrl+Shift+S",
            "modifiers in one order whatever order they were written in"
        );
        assert_eq!(shortcut_label("Alt+F4", false), "Alt+F4");
        assert_eq!(
            shortcut_label("Super+Option+Space", false),
            "Alt+Super+Space"
        );
        assert_eq!(shortcut_label("CommandOrControl+Z", true), "\u{2318}Z");
        assert_eq!(
            shortcut_label("Shift+CommandOrControl+S", true),
            "\u{21e7}\u{2318}S"
        );
        assert_eq!(
            shortcut_label("Control+Alt+Delete", true),
            "\u{2303}\u{2325}Delete"
        );
        assert_eq!(shortcut_label("F1", true), "F1");
    }

    /// An item is a `menuitem` element whose text is its label and whose
    /// accelerator rides on it for the runtime to draw.
    #[test]
    fn an_item_is_an_element_carrying_its_shortcut() {
        let popup = popup_menu(
            "edit",
            &[
                MenuRow::Item {
                    id: "undo".to_string(),
                    label: "Undo".to_string(),
                    disabled: true,
                    accel: Some("CommandOrControl+Z".to_string()),
                },
                MenuRow::Item {
                    id: "prefs".to_string(),
                    label: "Preferences".to_string(),
                    disabled: false,
                    accel: None,
                },
            ],
        );
        let panel = &popup.children[0];
        let undo = &panel.children[0];
        assert_eq!(undo.tag, "menuitem");
        assert_eq!(undo.attrs.text.as_deref(), Some("Undo"));
        assert!(undo.attrs.disabled);
        assert_eq!(undo.attrs.shortcut.as_deref(), Some("CommandOrControl+Z"));
        assert!(undo.children.is_empty());
        assert_eq!(panel.children[1].attrs.shortcut, None);
    }

    /// The in-window menu bar's menus carry the accelerators their items
    /// declare, the way an OS menu bar shows them.
    #[test]
    fn the_bar_s_menus_carry_their_shortcuts() {
        let bar = in_window_menubar(&spec());
        let panel = &bar.children[0].children[1].children[0];
        assert_eq!(
            panel.children[0].attrs.shortcut.as_deref(),
            Some("CommandOrControl+O")
        );
        assert_eq!(panel.children[2].attrs.shortcut, None, "quit has none");
    }
}
