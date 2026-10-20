//! Process-global mirror of the window for the `window` script namespace.
//!
//! The `window.title()` / `window.size()` / `window.dpr()` getters run inside
//! a script host or a C-ABI call, with no `&World` to read, so they read this
//! small mirror instead, the way DOM queries read the published DOM index.
//! The mirror only ever holds what the window has: [`publish_viewport`] copies
//! the live size and device-pixel ratio from the main world's [`Viewport`],
//! which every window backend and the headless runner keep current, and the
//! backend calls [`publish_title`] once it has applied a title.
//!
//! The setters never write here. `window.set_title` / `window.set_size` change
//! the [`crate::window::Window`] component; the window backend applies the
//! change and the mirror follows what the platform settled on.

use std::sync::{Mutex, OnceLock};

use bevy_ecs::prelude::*;

use crate::render_world::Viewport;

#[derive(Debug, Clone)]
struct WindowState {
    title: String,
    width: f32,
    height: f32,
    dpr: f32,
    primary: u64,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 0.0,
            height: 0.0,
            dpr: 1.0,
            primary: 0,
        }
    }
}

fn cell() -> &'static Mutex<WindowState> {
    static STATE: OnceLock<Mutex<WindowState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(WindowState::default()))
}

/// The title the window has.
pub fn title() -> String {
    cell().lock().map(|s| s.title.clone()).unwrap_or_default()
}

/// Record the title the window has. Called by whatever applies titles: the
/// window backend once the platform window has it, the headless runner, or
/// the DOM applier in an app with no window.
pub fn publish_title(title: &str) {
    if let Ok(mut s) = cell().lock()
        && s.title != title
    {
        s.title = title.to_string();
    }
}

/// The window's size in logical pixels (`window.size`).
pub fn size() -> (f32, f32) {
    cell()
        .lock()
        .map(|s| (s.width, s.height))
        .unwrap_or((0.0, 0.0))
}

/// The device-pixel ratio / scale factor (`window.dpr`).
pub fn dpr() -> f32 {
    cell().lock().map(|s| s.dpr).unwrap_or(1.0)
}

/// The primary window's entity as packed bits, `0` when the app has none.
/// The C ABI checks a window handle against it.
pub fn primary_handle() -> u64 {
    cell().lock().map(|s| s.primary).unwrap_or(0)
}

/// Record the primary window's entity (or that there is none).
pub fn publish_primary(window: Option<Entity>) {
    if let Ok(mut s) = cell().lock() {
        s.primary = window.map_or(0, Entity::to_bits);
    }
}

/// Copy the main world's [`Viewport`] into the mirror whenever it changes,
/// so `window.size()` and `window.dpr()` read what the app is running at.
/// Registered by `App::new` at the start of each tick, ahead of every
/// script handler, and at its end, so a size applied during the tick is
/// what the getters read after it.
pub fn publish_viewport(viewport: Option<Res<Viewport>>) {
    let Some(viewport) = viewport else {
        return;
    };
    if !viewport.is_changed() {
        return;
    }
    if let Ok(mut s) = cell().lock() {
        s.width = viewport.size.x;
        s.height = viewport.size.y;
        s.dpr = viewport.scale_factor;
    }
}
