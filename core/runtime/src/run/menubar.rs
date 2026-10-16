//! The menu bar a platform with no native one draws in the window.
//!
//! macOS and Windows hand an app's `<menubar>` to the OS. Linux desktops
//! have no menu bar convention for an app to join (GNOME has none at all),
//! so there the same markup becomes an in-window bar at the top of the root:
//! one button per `<menu>`, each opening that menu as a popup panel. Its
//! items reach `on_menu` exactly as the native bar's do, and an item's
//! `accel` works as a shortcut while the window has focus.
//!
//! The parser leaves `<menubar>` out of the layout tree, and the cascade has
//! already run when the runtime decides, so the bar is styled here against
//! the app's combined stylesheet before it joins the tree. Every later
//! restyle resolves it with the rest of the document.

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::{Entity, World};
use lumen_core::style_overrides::StyleOverrides;
use lumen_ir::css::{MediaContext, apply_css_with_media};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR, MenuEntrySpec};
use lumen_primitives::{InWindowMenuBar, MenuAccelerators};

/// Whether this platform shows an app's menu bar itself.
pub(crate) const NATIVE_MENUBAR: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Put the in-window menu bar at the top of `ir`'s root, styled, when the
/// app declares a `<menubar>` and `native` says the platform has no bar of
/// its own. Returns the shortcuts its items declare, for
/// [`MenuAccelerators`], or `None` when nothing was mounted.
pub(crate) fn mount_in_window_menubar(ir: &mut LayoutIR, native: bool) -> Option<MenuAccelerators> {
    let spec = ir
        .menubar
        .as_ref()
        .filter(|s| !native && !s.menus.is_empty())?;
    let accelerators = MenuAccelerators::from_entries(spec.menus.iter().flat_map(|menu| {
        menu.items.iter().filter_map(|entry| match entry {
            MenuEntrySpec::Item {
                id,
                accelerator: Some(accel),
                ..
            } => Some((id.as_str(), accel.as_str())),
            _ => None,
        })
    }));
    let bar = lumen_ir::menu::in_window_menubar(spec);
    // The bar is resolved where it is going to sit: as the first child of
    // a root carrying the real root's tag, id and classes, so `root > *`,
    // `:first-child` and a theme class on the root all reach it.
    let mut staged = LayoutIR {
        root: Element {
            tag: ir.root.tag.clone(),
            attrs: Attributes {
                id: ir.root.attrs.id.clone(),
                classes: ir.root.attrs.classes.clone(),
                ..Attributes::default()
            },
            children: vec![bar],
            ..Default::default()
        },
        ..LayoutIR::default()
    };
    if let Some(sheet) = &ir.combined_stylesheet
        && let Err(e) = apply_css_with_media(&mut staged, sheet, &MediaContext::default())
    {
        tracing::warn!("menubar: the stylesheet failed to apply to the menu bar: {e}");
    }
    if let Some(bar) = staged.root.children.pop() {
        ir.root.children.insert(0, bar);
    }
    Some(accelerators)
}

/// Finish what [`mount_in_window_menubar`] started once the tree is spawned
/// at `root`: mark the bar so the root keeps its height clear, and install
/// its shortcuts. With no bar mounted the shortcut table is emptied, so a
/// hot reload that drops the `<menubar>` drops its shortcuts too.
pub(crate) fn finish_in_window_menubar(
    world: &mut World,
    root: Entity,
    mounted: Option<MenuAccelerators>,
) {
    let Some(accelerators) = mounted else {
        world.insert_resource(MenuAccelerators::default());
        return;
    };
    world.insert_resource(accelerators);
    let bar = world
        .get::<Children>(root)
        .and_then(|c| c.iter().next().copied());
    if let Some(bar) = bar {
        world.entity_mut(bar).insert(InWindowMenuBar);
    }
    if world.get::<StyleOverrides>(root).is_none() {
        world.entity_mut(root).insert(StyleOverrides::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_ir::layout_ir::{MenuBarSpec, MenuSpec};

    fn app() -> LayoutIR {
        let mut ir = LayoutIR::default();
        ir.root.tag = "root".to_string();
        ir.root.children.push(Element {
            tag: "column".to_string(),
            ..Default::default()
        });
        ir.menubar = Some(MenuBarSpec {
            menus: vec![MenuSpec {
                label: "File".to_string(),
                items: vec![MenuEntrySpec::Item {
                    id: "open".to_string(),
                    label: "Open".to_string(),
                    accelerator: Some("Ctrl+O".to_string()),
                }],
            }],
        });
        ir
    }

    /// Without a native bar the menus become the root's first child, and the
    /// items' shortcuts are collected.
    #[test]
    fn without_a_native_bar_the_menus_join_the_top_of_the_root() {
        let mut ir = app();
        let accels = mount_in_window_menubar(&mut ir, false).expect("mounted");
        assert_eq!(ir.root.children.len(), 2);
        assert_eq!(ir.root.children[0].attrs.classes, vec!["menubar"]);
        assert_eq!(ir.root.children[1].tag, "column");
        assert_eq!(accels.len(), 1);
    }

    /// With a native bar the tree is left alone.
    #[test]
    fn with_a_native_bar_the_tree_is_left_alone() {
        let mut ir = app();
        assert!(mount_in_window_menubar(&mut ir, true).is_none());
        assert_eq!(ir.root.children.len(), 1);
    }

    /// The bar is styled against the app's stylesheet before it mounts.
    #[test]
    fn the_bar_takes_the_app_stylesheet() {
        use lumen_ir::css::{
            Combinator, CompoundSelector, Declaration, LegacySelectorShim, Origin, Rule,
            SelectorBuf, Stylesheet,
        };
        let mut ir = app();
        ir.combined_stylesheet = Some(Stylesheet {
            rules: vec![Rule {
                selectors: vec![SelectorBuf {
                    chain: vec![(
                        Combinator::Subject,
                        CompoundSelector {
                            tag: None,
                            id: None,
                            classes: vec!["menubar-button".to_string()],
                            pseudo_classes: Vec::new(),
                        },
                    )],
                }],
                declarations: vec![Declaration {
                    name: "padding".to_string(),
                    value: "3px 9px".to_string(),
                    important: false,
                }],
                origin: Origin::Author,
                source_order: 0,
                media: None,
                selector: LegacySelectorShim {
                    tag: None,
                    classes: vec!["menubar-button".to_string()],
                },
            }],
            ..Default::default()
        });
        mount_in_window_menubar(&mut ir, false);
        let button = &ir.root.children[0].children[0].children[0];
        assert_eq!(button.attrs.classes, vec!["menubar-button"]);
        let padding = button.attrs.padding.expect("padding resolved");
        assert_eq!((padding.top, padding.left), (3.0, 9.0));
    }
}
