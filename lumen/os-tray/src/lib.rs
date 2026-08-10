//! System-tray host for Lumen.
//!
//! Wraps `tray-icon` 0.24 (macOS + Windows) and `ksni` 0.3 (Linux) behind a [`TrayService`] +
//! per-tray [`TrayConfig`] shape. Mirrors `QSystemTrayIcon` (`setIcon` / `setToolTip` /
//! `setContextMenu` / activation reasons: Trigger / DoubleClick / MiddleClick / Context) and the
//! AppIndicator / StatusNotifierItem model on Linux.
//!
//! ## GNOME / KDE on Linux
//!
//! `ksni` speaks the StatusNotifierItem D-Bus protocol - the modern KDE Plasma format. It works on
//! Plasma 5/6 natively. On GNOME the user must install the
//! [AppIndicator / KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/)
//! extension; without it the tray icon binds successfully on the bus but doesn't render. When no
//! StatusNotifierWatcher is registered (some lightweight WMs) registration logs at debug and the
//! [`TrayService`] silently records the config without surfacing an icon.
//!
//! `TrayMenu` reuses [`lumen_os_mime::Action`] so a single Action drives menus, tray menus, and
//! notification buttons - the shared abstraction described in section 470 of the audit.

#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use std::path::Path;

pub use lumen_os_mime as mime;
pub use lumen_os_mime::Action;

/// Backwards-compatible alias for the existing `TrayClicked` message -
/// emitted for the left / Trigger activation reason.
pub use lumen_core::input::TrayClicked;

/// One tray entry: id, icon path, optional tooltip, optional menu.
///
/// Mirrors `QSystemTrayIcon::{setIcon, setToolTip, setContextMenu}`.
/// The icon is given as a path so the resource layer can decode it
/// once and hand the raw bytes to `tray_icon::Icon::from_rgba`.
#[derive(Clone, Debug)]
pub struct TrayConfig {
    /// Stable id used for the resulting [`TrayClicked`] dispatch and
    /// for `TrayService::unregister`.
    pub id: String,
    /// Filesystem path to a PNG (or any `image`-supported format) used
    /// as the tray icon.
    pub icon_path: std::path::PathBuf,
    /// Optional hover tooltip.
    pub tooltip: Option<String>,
    /// Optional menu attached to the tray icon. Reuses
    /// [`lumen_os_mime::Action`] so the menu shares the action surface
    /// with `lumen-os-menu`.
    pub menu: Option<TrayMenu>,
    /// macOS template-image flag - when true the icon is treated as
    /// monochrome and recoloured for the active menu-bar theme.
    pub template: bool,
}

/// Lightweight menu attached to a tray icon. A flat `Vec<Action>` is
/// enough for tray context menus on every platform - submenus are
/// possible via tray-icon's `muda::Submenu` but rare; defer to a
/// follow-up if needed.
#[derive(Clone, Debug, Default)]
pub struct TrayMenu {
    /// Top-level actions in source order. Separators are encoded as an
    /// [`Action`] with id `"separator"` (matching the `lumen-os-menu`
    /// convention so a single Action vocabulary works everywhere).
    pub items: Vec<Action>,
}

impl TrayMenu {
    /// Convenience constructor.
    pub fn new(items: Vec<Action>) -> Self {
        Self { items }
    }
}

/// Tray-host resource. Holds the live `tray_icon::TrayIcon` (Windows / macOS) or `ksni::Handle`
/// (Linux). `NonSend` because the platform crates own a per-thread receiver on macOS.
///
/// Linux entries are full participants - when ksni successfully registers we get a real SNI item;
/// otherwise the entry sits in `linux_handles` as `None` and surfaces as a no-op (the API stays
/// behaviourally identical to the macOS / Windows path so cross-platform code doesn't branch).
#[derive(Default)]
pub struct TrayService {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    icons: std::collections::HashMap<String, tray_icon::TrayIcon>,
    #[cfg(target_os = "linux")]
    linux_handles:
        std::collections::HashMap<String, Option<ksni::blocking::Handle<linux::LumenTray>>>,
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    _stub: std::collections::HashMap<String, ()>,
}

// Linux: ksni registers each tray via a background runtime task/thread that
// keeps running until its `Handle` is shut down. Without this Drop the
// registration and its thread leak whenever the service is torn down (app
// exit, resource replacement). macOS / Windows `tray_icon::TrayIcon` already
// removes itself on its own Drop, so no explicit teardown is needed there.
#[cfg(target_os = "linux")]
impl Drop for TrayService {
    fn drop(&mut self) {
        for (_, handle) in self.linux_handles.drain() {
            if let Some(handle) = handle {
                handle.shutdown();
            }
        }
    }
}

impl TrayService {
    /// Empty service.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a tray entry.
    ///
    /// `dir` is used to resolve a relative `icon_path` to an absolute
    /// path before reading the file. Errors log to stderr and leave
    /// the registry untouched - mirroring the pre-extract behaviour.
    pub fn register(&mut self, cfg: &TrayConfig, dir: &Path) {
        self.register_inner(cfg, dir);
    }

    /// Remove a tray entry. No-op if `id` was never registered.
    pub fn unregister(&mut self, id: &str) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.icons.remove(id);
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(Some(handle)) = self.linux_handles.remove(id) {
                handle.shutdown();
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            self._stub.remove(id);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn register_inner(&mut self, cfg: &TrayConfig, dir: &Path) {
        let p = Path::new(&cfg.icon_path);
        let resolved = if p.is_relative() {
            dir.join(p)
        } else {
            p.to_path_buf()
        };
        let img = match image::open(&resolved) {
            Ok(i) => i.to_rgba8(),
            Err(e) => {
                eprintln!("lumen-os-tray: tray '{}': {e}", resolved.display());
                return;
            }
        };
        let (w, h) = (img.width(), img.height());
        let icon = match tray_icon::Icon::from_rgba(img.into_raw(), w, h) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("lumen-os-tray: icon build failed: {e}");
                return;
            }
        };
        let attrs = tray_icon::TrayIconAttributes {
            icon: Some(icon),
            tooltip: cfg.tooltip.clone(),
            ..Default::default()
        };
        let tray = match tray_icon::TrayIcon::with_id(&cfg.id, attrs) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("lumen-os-tray: init '{}': {e}", cfg.id);
                return;
            }
        };
        self.icons.insert(cfg.id.clone(), tray);
    }

    #[cfg(target_os = "linux")]
    fn register_inner(&mut self, cfg: &TrayConfig, dir: &Path) {
        let p = Path::new(&cfg.icon_path);
        let resolved = if p.is_relative() {
            dir.join(p)
        } else {
            p.to_path_buf()
        };
        let img = match image::open(&resolved) {
            Ok(i) => i.to_rgba8(),
            Err(e) => {
                eprintln!("lumen-os-tray: tray '{}': {e}", resolved.display());
                return;
            }
        };
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut argb = img.into_raw();
        // ksni::Icon expects ARGB32. The `image` crate decodes RGBA8 - convert in-place.
        // Pixel layout: [R, G, B, A] -> [A, R, G, B].
        for chunk in argb.chunks_exact_mut(4) {
            chunk.rotate_right(1);
        }
        let handle = linux::register(cfg.id.clone(), cfg.tooltip.clone(), w, h, argb);
        self.linux_handles.insert(cfg.id.clone(), handle);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    fn register_inner(&mut self, cfg: &TrayConfig, _dir: &Path) {
        eprintln!(
            "lumen-os-tray: tray '{}' skipped - platform unsupported",
            cfg.id
        );
        self._stub.insert(cfg.id.clone(), ());
    }
}

/// Drain the global tray-icon channel each tick and re-emit
/// matching clicks as [`TrayClicked`] messages. Mirrors the previous
/// `poll_tray_events` system from `lumenc/src/run.rs:1505`.
///
/// Backwards-compatible: only the left-click / Trigger reason is
/// surfaced today so widget-garden's `on_tray(id)` continues to work
/// unchanged. The audit calls for splitting into left / right /
/// double / middle in a follow-up - the Action-routed `TrayMenu`
/// already lays the surface down.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn poll_tray_events(mut out: MessageWriter<TrayClicked>) {
    while let Ok(ev) = tray_icon::TrayIconEvent::receiver().try_recv() {
        if let tray_icon::TrayIconEvent::Click { id, .. } = ev {
            out.write(TrayClicked { id: id.0 });
        }
    }
}

/// Linux click-poll: drains the ksni click channel and emits [`TrayClicked`]
/// messages.
///
/// The runtime does not schedule this system on Linux, so a tray click there
/// does not reach the app; an embedder that wants it must add the system
/// itself.
#[cfg(target_os = "linux")]
pub fn poll_tray_events(mut out: MessageWriter<TrayClicked>) {
    while let Some(id) = linux::pop_click() {
        out.write(TrayClicked { id });
    }
}

/// No-op stub on non-tray-icon targets.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn poll_tray_events(_out: MessageWriter<TrayClicked>) {}

#[cfg(target_os = "linux")]
mod linux {
    //! ksni Linux backend.
    //!
    //! Each tray registration spawns a ksni runtime task; activations push the tray's id into a
    //! process-global click queue that [`poll_tray_events`] drains. ksni's runtime errors (no
    //! StatusNotifierWatcher on the bus, headless CI) downgrade to a `None` handle so the API
    //! surface is identical to a successful registration.

    use std::sync::{Mutex, OnceLock};

    fn click_queue() -> &'static Mutex<Vec<String>> {
        static Q: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
        Q.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub fn pop_click() -> Option<String> {
        // Recover from a poisoned lock instead of returning `None` forever:
        // the queue is a plain `Vec<String>`, so a panic in another holder
        // leaves it in a perfectly usable state. Swallowing poison here
        // would silently drop every future tray click with no diagnostic.
        let mut g = click_queue().lock().unwrap_or_else(|e| e.into_inner());
        if g.is_empty() {
            None
        } else {
            Some(g.remove(0))
        }
    }

    fn push_click(id: String) {
        // Same poison-recovery rationale as `pop_click`.
        let mut g = click_queue().lock().unwrap_or_else(|e| e.into_inner());
        g.push(id);
    }

    /// The `ksni::Tray` impl backing one Lumen tray entry.
    pub struct LumenTray {
        pub id: String,
        pub tooltip: Option<String>,
        pub width: i32,
        pub height: i32,
        pub argb: Vec<u8>,
    }

    impl ksni::Tray for LumenTray {
        fn id(&self) -> String {
            self.id.clone()
        }

        fn title(&self) -> String {
            self.tooltip.clone().unwrap_or_else(|| self.id.clone())
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: self.tooltip.clone().unwrap_or_default(),
                ..Default::default()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![ksni::Icon {
                width: self.width,
                height: self.height,
                data: self.argb.clone(),
            }]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            push_click(self.id.clone());
        }
    }

    pub fn register(
        id: String,
        tooltip: Option<String>,
        width: i32,
        height: i32,
        argb: Vec<u8>,
    ) -> Option<ksni::blocking::Handle<LumenTray>> {
        let tray = LumenTray {
            id: id.clone(),
            tooltip,
            width,
            height,
            argb,
        };
        // Use the blocking variant - Lumen's tick is serial and ksni's own runtime drives the SNI loop
        // on a background thread regardless of which spawn variant we call.
        match <LumenTray as ksni::blocking::TrayMethods>::spawn(tray) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::debug!("lumen-os-tray: ksni spawn '{id}' failed: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_service_constructs() {
        let _s = TrayService::new();
        let _s2 = TrayService::default();
    }

    #[test]
    fn unregister_missing_is_noop() {
        let mut s = TrayService::new();
        s.unregister("nope"); // must not panic
    }

    #[test]
    fn tray_menu_with_actions() {
        let m = TrayMenu::new(vec![
            Action::new("file.open", "Open"),
            Action::new("file.save", "Save"),
        ]);
        assert_eq!(m.items.len(), 2);
        assert_eq!(&*m.items[0].id, "file.open");
    }

    #[test]
    fn tray_config_carries_template_flag() {
        let cfg = TrayConfig {
            id: "main".to_string(),
            icon_path: std::path::PathBuf::from("icon.png"),
            tooltip: Some("hi".to_string()),
            menu: None,
            template: true,
        };
        assert!(cfg.template);
        assert_eq!(cfg.tooltip.as_deref(), Some("hi"));
    }
}
