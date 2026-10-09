//! Process-global window-state mirror for the `window` script namespace.
//!
//! The OS window lives inside the winit event loop, not in the ECS, so the
//! `window.title()` / `window.size()` / `window.dpr()` getters and the
//! `window.set_title` / `window.set_size` setters cannot reach it through a
//! `&World`. They read and write this small cache instead.
//! [`publish_viewport`] copies the live size and device-pixel ratio here
//! from the main world's [`Viewport`], which every window backend and the
//! headless runner keep current; the setters write the requested title /
//! size, which the backend applies to the real window when one exists.
//! Headless runs have no window, so a setter followed by its getter
//! round-trips through the cache.

use std::sync::{Mutex, OnceLock};

use bevy_ecs::prelude::*;

use crate::render_world::Viewport;

#[derive(Debug, Clone)]
struct WindowState {
    title: String,
    width: f32,
    height: f32,
    dpr: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 0.0,
            height: 0.0,
            dpr: 1.0,
        }
    }
}

fn cell() -> &'static Mutex<WindowState> {
    static STATE: OnceLock<Mutex<WindowState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(WindowState::default()))
}

/// Current window title.
pub fn title() -> String {
    cell().lock().map(|s| s.title.clone()).unwrap_or_default()
}

/// Request a new window title (`window.set_title`).
pub fn set_title(title: &str) {
    if let Ok(mut s) = cell().lock() {
        s.title = title.to_string();
    }
}

/// Current window size in logical pixels (`window.size`).
pub fn size() -> (f32, f32) {
    cell()
        .lock()
        .map(|s| (s.width, s.height))
        .unwrap_or((0.0, 0.0))
}

/// Request a new window size in logical pixels (`window.set_size`).
pub fn set_size(width: f32, height: f32) {
    if let Ok(mut s) = cell().lock() {
        s.width = width;
        s.height = height;
    }
}

/// Current device-pixel ratio / scale factor (`window.dpr`).
pub fn dpr() -> f32 {
    cell().lock().map(|s| s.dpr).unwrap_or(1.0)
}

/// Publish the live device-pixel ratio (window backend, on scale change).
pub fn set_dpr(dpr: f32) {
    if let Ok(mut s) = cell().lock() {
        s.dpr = dpr;
    }
}

/// Copy the main world's [`Viewport`] into the cache whenever it changes,
/// so `window.size()` and `window.dpr()` read what the app is running at.
/// Registered by `App::new` at the start of each tick, ahead of every
/// script handler.
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
