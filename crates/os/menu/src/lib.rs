//! Native menu-bar abstraction for Lumen.
//!
//! Wraps `muda` 0.19 behind a [`MenuModel`] / [`MenuBuilder`] +
//! [`Action`](lumen_os_mime::Action) shape that mirrors `QAction` /
//! `GAction` (per os-integration audit section 154-164, section 470). Extracted from
//! `lumen-window-winit::lib.rs:1051-1124` per W6.3.
//!
//! - [`MenuModel`] / [`MenuEntry`] are the pure-data tree the markup
//!   layer hands to the runtime. Backwards-compatible with the old
//!   `MenuBarOptions` / `MenuOptions` types: those are kept as type
//!   aliases inside `lumen-window-winit` so the markup wiring keeps
//!   working unchanged.
//! - [`attach_native_menubar`] builds a `muda::Menu` and binds it.
//!   macOS attaches to the app; Windows attaches to the supplied HWND.
//!   Linux is a no-op stub (muda's Linux backend depends on
//!   `libxdo-dev`).
//! - [`poll_native_menu_events`] drains muda's global event channel
//!   and writes [`lumen_core::input::MenuClicked`] (alias
//!   [`ActionInvoked`]).

// `deny` rather than `forbid`: the Windows `init_for_hwnd` FFI call needs
// one explicitly-annotated `unsafe` block (see `attach_native_menubar`),
// and `forbid` cannot be locally overridden even with `#[allow]`.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use lumen_core::input::MenuClicked;

pub use lumen_os_mime as mime;
pub use lumen_os_mime::{Action, KeyChord};

/// Re-export of [`lumen_core::input::MenuClicked`] under the QAction /
/// GAction-style name. Identical type; emitted whenever a native menu
/// item fires.
pub type ActionInvoked = MenuClicked;

/// Backend-facing description of the native menu bar (parsed from
/// markup at the lumenc layer). Backwards-compatible with
/// `lumen_window_winit::MenuBarOptions`.
#[derive(Debug, Clone, Default)]
pub struct MenuModel {
    /// Top-level submenus, in source order.
    pub menus: Vec<Menu>,
}

/// One submenu (`<menu label="File">...</menu>`).
#[derive(Debug, Clone)]
pub struct Menu {
    /// Display label shown in the menu bar.
    pub label: String,
    /// Items inside the submenu.
    pub items: Vec<MenuEntry>,
}

/// One submenu entry - either a clickable item or a separator.
#[derive(Debug, Clone)]
pub enum MenuEntry {
    /// `<menuitem id label accel?>`.
    Item {
        /// `id="..."` - dispatched as `ActionInvoked { id }` to scripts.
        id: String,
        /// Display label.
        label: String,
        /// Optional accelerator string in muda format.
        accelerator: Option<String>,
    },
    /// `<separator />`.
    Separator,
}

impl From<&Action> for MenuEntry {
    fn from(action: &Action) -> Self {
        MenuEntry::Item {
            id: action.id.as_ref().to_string(),
            label: action.label.as_ref().to_string(),
            accelerator: action.shortcut.as_ref().map(|c| c.0.as_ref().to_string()),
        }
    }
}

/// Build a [`MenuModel`] imperatively.
///
/// Mirrors `QMenuBar::addMenu` / `g_menu_append_submenu`. Items refer
/// to action ids; backends translate the click into an
/// [`ActionInvoked`] message at run-time.
#[derive(Default)]
pub struct MenuBuilder {
    model: MenuModel,
}

impl MenuBuilder {
    /// New empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a top-level submenu with the given label and items.
    pub fn menu(mut self, label: impl Into<String>, items: Vec<MenuEntry>) -> Self {
        self.model.menus.push(Menu {
            label: label.into(),
            items,
        });
        self
    }

    /// Append an [`Action`] as a clickable menu entry. Convenience for
    /// callers that have an `Action` table already. Thin shim over the
    /// [`From<&Action>`] impl, preserved for public-API stability.
    pub fn action_entry(action: &Action) -> MenuEntry {
        MenuEntry::from(action)
    }

    /// Finalize the model.
    pub fn build(self) -> MenuModel {
        self.model
    }
}

/// Default action ids the macOS app-menu items should use when an app
/// wants to override (About / Quit / Hide / Hide-Others / Services).
/// Per `QAction::MenuRole` convention.
pub mod default_action_ids {
    /// `application.about` - the macOS About item.
    pub const ABOUT: &str = "application.about";
    /// `application.preferences` - the macOS Preferences / Settings
    /// item.
    pub const PREFERENCES: &str = "application.preferences";
    /// `application.services` - macOS Services submenu.
    pub const SERVICES: &str = "application.services";
    /// `application.hide` - Hide.
    pub const HIDE: &str = "application.hide";
    /// `application.hide_others` - Hide Others.
    pub const HIDE_OTHERS: &str = "application.hide_others";
    /// `application.show_all` - Show All.
    pub const SHOW_ALL: &str = "application.show_all";
    /// `application.quit` - Quit.
    pub const QUIT: &str = "application.quit";
    /// `edit.copy`.
    pub const COPY: &str = "edit.copy";
    /// `edit.cut`.
    pub const CUT: &str = "edit.cut";
    /// `edit.paste`.
    pub const PASTE: &str = "edit.paste";
    /// `edit.select_all`.
    pub const SELECT_ALL: &str = "edit.select_all";
}

/// Build a native muda menu from `spec` and attach it. `window_handle`
/// is required on Windows (the HWND target); macOS ignores it
/// (`init_for_nsapp`); Linux is a no-op stub.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn attach_native_menubar(
    spec: &MenuModel,
    #[allow(unused_variables)] window_handle: Option<&dyn raw_window_handle::HasWindowHandle>,
) {
    use muda::accelerator::Accelerator;
    let menu = muda::Menu::new();
    for submenu in &spec.menus {
        let sub = muda::Submenu::new(&submenu.label, true);
        for entry in &submenu.items {
            match entry {
                MenuEntry::Item {
                    id,
                    label,
                    accelerator,
                } => {
                    let accel = accelerator
                        .as_deref()
                        .and_then(|s| s.parse::<Accelerator>().ok());
                    let item = muda::MenuItem::with_id(id.as_str(), label, true, accel);
                    let _ = sub.append(&item);
                }
                MenuEntry::Separator => {
                    let _ = sub.append(&muda::PredefinedMenuItem::separator());
                }
            }
        }
        let _ = menu.append(&sub);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window_handle;
        menu.init_for_nsapp();
    }
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::RawWindowHandle;
        if let Some(h) = window_handle
            && let Ok(handle) = h.window_handle()
            && let RawWindowHandle::Win32(win32) = handle.as_raw()
        {
            // SAFETY: `init_for_hwnd` requires a valid Win32 HWND. The
            // caller is responsible for ensuring `window_handle` is
            // valid for the lifetime of the menu - usually the
            // platform window outlives the menu binding (attach runs
            // once at resume).
            #[allow(unsafe_code)]
            let _ = unsafe { menu.init_for_hwnd(win32.hwnd.get()) };
        }
    }
    // Drain any pre-existing events the global MenuEvent::receiver
    // buffered while we were constructing the menu - guarantees the
    // first user click after attach lands cleanly.
    while muda::MenuEvent::receiver().try_recv().is_ok() {}
}

/// Linux stub - see [`attach_native_menubar`] above for the platform
/// gating rationale.
///
/// Deliberately inert: muda's Linux backend needs `libxdo-dev`, which we
/// don't take as a hard dependency, so there is no OS-level menubar and no
/// [`MenuClicked`] will ever fire from a native menu here. Linux apps that
/// want a menu should render an in-window menubar widget and dispatch its
/// clicks through the normal input path instead of relying on this host.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn attach_native_menubar(_spec: &MenuModel, _window_handle: Option<&()>) {}

/// Drain the global muda menu event channel and fire one
/// [`ActionInvoked`] message per event. Called once per tick from the
/// window backend; no-op on platforms where the muda dep is absent.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn poll_native_menu_events(world: &mut bevy_ecs::world::World) {
    let mut ids: Vec<String> = Vec::new();
    while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
        ids.push(ev.id.0);
    }
    if ids.is_empty() {
        return;
    }
    // `MenuClicked` may not be registered (an app that never wired native
    // menus). `resource_mut` would panic; degrade to a no-op instead.
    let Some(mut writer) = world.get_resource_mut::<bevy_ecs::message::Messages<MenuClicked>>()
    else {
        return;
    };
    for id in ids {
        writer.write(MenuClicked { id });
    }
}

/// Linux stub - see [`poll_native_menu_events`] above for the platform
/// gating rationale.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn poll_native_menu_events(_world: &mut bevy_ecs::world::World) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_builder_collects_menus() {
        let model = MenuBuilder::new()
            .menu(
                "File",
                vec![
                    MenuEntry::Item {
                        id: "file.new".into(),
                        label: "New".into(),
                        accelerator: Some("Ctrl+N".into()),
                    },
                    MenuEntry::Separator,
                    MenuEntry::Item {
                        id: "file.quit".into(),
                        label: "Quit".into(),
                        accelerator: None,
                    },
                ],
            )
            .menu("Edit", vec![])
            .build();
        assert_eq!(model.menus.len(), 2);
        assert_eq!(model.menus[0].label, "File");
        assert_eq!(model.menus[0].items.len(), 3);
        assert_eq!(model.menus[1].label, "Edit");
    }

    #[test]
    fn action_entry_from_action() {
        let a = Action::new("file.save", "Save").with_shortcut("Ctrl+S");
        let entry = MenuBuilder::action_entry(&a);
        match entry {
            MenuEntry::Item {
                id,
                label,
                accelerator,
            } => {
                assert_eq!(id, "file.save");
                assert_eq!(label, "Save");
                assert_eq!(accelerator.as_deref(), Some("Ctrl+S"));
            }
            _ => panic!("expected Item"),
        }
    }

    #[test]
    fn default_action_ids_stable() {
        // Pin the strings so users overriding by id don't get silent
        // breakage from rename churn.
        assert_eq!(default_action_ids::QUIT, "application.quit");
        assert_eq!(default_action_ids::ABOUT, "application.about");
        assert_eq!(default_action_ids::COPY, "edit.copy");
    }
}
