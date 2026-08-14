//! Backend-neutral description of the app's window and its native menu bar.
//!
//! Every launch path (the CLI, the C-ABI, the SDKs) resolves the same
//! values - size, title, clear color, chrome, menu bar - before a window
//! exists, and a window backend consumes them when it opens one. Keeping
//! the description here means the composition point can build it without
//! naming a backend, and a second window backend can consume it without
//! inventing a parallel type.
//!
//! The menu tree ([`MenuModel`], [`Menu`], [`MenuEntry`]) is pure data: it
//! says what the menu bar contains, not how a platform draws it.
//! `lumen-os-menu` re-exports these types and owns the native attach.

use crate::components::Color;

/// Fallback GPU clear color painted before the very first frame - what a
/// user sees for an instant during window creation, and behind any pixel
/// the app's tree doesn't cover. This is the single Rust-side source of
/// truth for that fallback; `lumen_runtime::run::RunOptions::clear` (the
/// value that actually reaches every real launch path - CLI, FFI, SDK)
/// defaults to it too, so the two never drift apart the way they used to.
///
/// A resolved `--lumen-window-bg` custom property (from the app's own
/// `:root` or its active skin) overrides this at build time - see
/// `lumen_runtime::run::app_build::build_app`. This constant is only what
/// remains when no layer defines that token, or a caller constructs
/// [`WindowOptions`] directly without going through `RunOptions` at all.
pub const DEFAULT_CLEAR: Color = Color::rgb(0.07, 0.07, 0.09);

/// Everything a window backend needs to open the app's window.
pub struct WindowOptions {
    /// Initial window inner size in logical pixels.
    pub size: (u32, u32),
    /// Window title.
    pub title: String,
    /// Background clear color (writes [`crate::render_world::Viewport::clear`]
    /// on init). See [`DEFAULT_CLEAR`] for the fallback this defaults to.
    pub clear: Color,
    /// Maximize the window on launch. Useful for full-bleed app demos.
    pub maximized: bool,
    /// Suppress OS window chrome (title bar, borders, close/min/max
    /// buttons). Custom title bars and per-platform window drag must
    /// be implemented inside the app - see the `<title-bar drag>`
    /// region (TODO S31).
    pub frameless: bool,
    /// Initial outer position in physical pixels. `None` lets the OS
    /// place the window (the default). Wired up so callers can
    /// restore the last-known window position from disk (P1.3).
    pub start_position: Option<(i32, i32)>,
    /// Callback invoked once per close with the current window
    /// position, inner size (logical), and maximized flag. `None`
    /// (the default) disables state persistence. Implementations
    /// typically serialise to disk via
    /// `lumenc::window_state::save`.
    pub on_close_state: Option<Box<dyn FnOnce(WindowGeometry) + Send>>,
    /// Optional native menu bar spec. Built by the embedder from
    /// the markup `<menubar>` element. `None` = no menu bar.
    pub menubar: Option<MenuModel>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            size: (800, 600),
            title: "Lumen".into(),
            clear: DEFAULT_CLEAR,
            maximized: false,
            frameless: false,
            start_position: None,
            on_close_state: None,
            menubar: None,
        }
    }
}

/// Snapshot of geometry handed to [`WindowOptions::on_close_state`] so
/// the embedder can persist `(position, size, maximized)` to disk.
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    /// Outer window position in physical pixels (winit
    /// `Window::outer_position`).
    pub position: Option<(i32, i32)>,
    /// Inner window size in logical pixels.
    pub size: (u32, u32),
    /// Last maximized state.
    pub maximized: bool,
}

/// The app's native menu bar, as parsed from `<menubar>`.
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
        /// `id="..."` - dispatched as `MenuClicked { id }` to scripts.
        id: String,
        /// Display label.
        label: String,
        /// Optional accelerator string in muda format.
        accelerator: Option<String>,
    },
    /// `<separator />`.
    Separator,
}
