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
//!
//! Once the app runs, its window is an entity in the main world carrying
//! [`Window`] and [`PrimaryWindow`]. [`Window`] is the state the app wants
//! the window to have; the window backend applies a change to the platform
//! window and writes back what the platform settled on. The app asks for the
//! window to close, or for the whole app to quit, with [`WindowRequest`] and
//! [`QuitRequest`]; the backend answers both through the
//! [`crate::input::CloseRequest`] veto path.

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::components::Color;

/// The app's window as the app wants it to be.
///
/// Lives on the window entity, next to [`PrimaryWindow`]. App code (the
/// `window.set_title` / `window.set_size` script calls, the C ABI, the SDKs,
/// a Rust system) writes the fields; the window backend applies what changed
/// to the platform window after the tick, and writes back the size the
/// platform settled on when the window is resized from outside the app.
/// What the window has right now is what the `window.title()` /
/// `window.size()` getters report, through [`crate::window_state`].
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Window {
    /// Window title.
    pub title: String,
    /// Inner size in logical pixels.
    pub size: Vec2,
}

/// Marks the app's first window: the one a launch opens, and the one
/// `window.*` calls apply to.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PrimaryWindow;

/// The primary window's entity, if the app has a window.
pub fn primary_window(world: &mut World) -> Option<Entity> {
    world
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .iter(world)
        .next()
}

/// A request from the app to its window backend.
///
/// The backend reads these after each tick and obeys them; a run with no
/// display obeys them the same way, so a script that closes its window
/// behaves alike headless and on screen.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// Close the window (`window.close()`). The close runs the same veto
    /// path the window's close button does: [`crate::input::CloseRequest`]
    /// with [`crate::input::CloseReason::WindowClose`], and an `on_close`
    /// hook can keep the window open.
    Close,
}

/// A request from the app to quit (`app.quit()`).
///
/// Every window is asked to close through the
/// [`crate::input::CloseRequest`] veto path, with
/// [`crate::input::CloseReason::AppQuit`]; a veto cancels the quit.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuitRequest;

/// Fallback GPU clear color painted before the very first frame - what a
/// user sees for an instant during window creation, and behind any pixel
/// the app's tree doesn't cover. This is the single Rust-side source of
/// truth for that fallback; `lumen_runtime::run::RunOptions::clear` (the
/// value every launch path reaches - CLI, FFI, SDK) defaults to it too,
/// so the two never drift apart the way they used to.
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
