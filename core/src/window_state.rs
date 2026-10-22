//! Process-global mirror of the app's windows for the `window` script
//! namespace and the C ABI.
//!
//! The `window.title()` / `window.size()` / `window.dpr()` getters run inside
//! a script host or a C-ABI call, with no `&World` to read, so they read this
//! small mirror instead, the way DOM queries read the published DOM index.
//! The mirror only ever holds what each window has: [`publish_windows`]
//! copies the live size and device-pixel ratio from every window's
//! [`WindowViewport`], and the window backend records a title once it has
//! applied it ([`publish_title`]).
//!
//! The getters answer for the *current* window: the window of the event
//! being handled, which the event dispatcher sets around each handler with
//! [`set_current`], and the primary window outside any handler (startup,
//! timers, `on_ready`).
//!
//! The setters never write here. `window.set_title` / `window.set_size`
//! change the [`Window`] component; the window backend applies the change
//! and the mirror follows what the platform settled on.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bevy_ecs::prelude::*;

use crate::window::{PrimaryWindow, Window, WindowViewport};

#[derive(Debug, Clone)]
struct Entry {
    title: String,
    width: f32,
    height: f32,
    dpr: f32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 0.0,
            height: 0.0,
            dpr: 1.0,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// What each open window has, by packed entity bits.
    windows: HashMap<u64, Entry>,
    /// The primary window's bits, `0` when the app has none.
    primary: u64,
    /// The window of the event being handled, `0` outside a handler.
    current: u64,
    /// The title of an app with no window at all (a browser page, a bare
    /// app in a test), which only the getter reports.
    windowless_title: String,
}

fn cell() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    cell().lock().ok().map(|mut s| f(&mut s))
}

fn current_in(s: &State) -> u64 {
    if s.current != 0 && s.windows.contains_key(&s.current) {
        s.current
    } else {
        s.primary
    }
}

/// The window the getters answer for: the current window when one is set
/// and still open, else the primary window. Packed entity bits, `0` when the
/// app has no window.
pub fn current() -> u64 {
    with(|s| current_in(s)).unwrap_or(0)
}

/// The current window as an entity, if the app has one.
pub fn current_window() -> Option<Entity> {
    Some(current())
        .filter(|bits| *bits != 0)
        .and_then(Entity::try_from_bits)
}

/// Make `window` the current window: the event dispatcher calls this with
/// the window of the event before running its handlers, and with `None`
/// after them.
pub fn set_current(window: Option<Entity>) {
    with(|s| s.current = window.map_or(0, Entity::to_bits));
}

/// Resolve a C-ABI or script window handle: `0` means the current window;
/// any other value must name a window that is open. `None` for a handle
/// that names no open window.
pub fn resolve(handle: u64) -> Option<u64> {
    with(|s| {
        if handle == 0 {
            Some(current_in(s)).filter(|bits| *bits != 0)
        } else {
            s.windows.contains_key(&handle).then_some(handle)
        }
    })
    .flatten()
}

/// The primary window's entity as packed bits, `0` when the app has none.
pub fn primary_handle() -> u64 {
    with(|s| s.primary).unwrap_or(0)
}

/// The title the current window has.
pub fn title() -> String {
    title_of(current())
}

/// The title `window` (packed entity bits) has; `0` is an app with no
/// window, which reports the title it was given.
pub fn title_of(window: u64) -> String {
    with(|s| {
        s.windows
            .get(&window)
            .map(|e| e.title.clone())
            .unwrap_or_else(|| s.windowless_title.clone())
    })
    .unwrap_or_default()
}

/// The current window's size in logical pixels (`window.size`).
pub fn size() -> (f32, f32) {
    size_of(current())
}

/// `window`'s size in logical pixels.
pub fn size_of(window: u64) -> (f32, f32) {
    with(|s| {
        s.windows
            .get(&window)
            .map(|e| (e.width, e.height))
            .unwrap_or((0.0, 0.0))
    })
    .unwrap_or((0.0, 0.0))
}

/// The current window's device-pixel ratio / scale factor (`window.dpr`).
pub fn dpr() -> f32 {
    dpr_of(current())
}

/// `window`'s device-pixel ratio.
pub fn dpr_of(window: u64) -> f32 {
    with(|s| s.windows.get(&window).map(|e| e.dpr).unwrap_or(1.0)).unwrap_or(1.0)
}

/// Record the title `window` has. Called by whatever applies titles: the
/// window backend once the platform window has it, or the headless stand-in.
/// `None` records the title of an app with no window (the DOM applier in a
/// browser page or a bare test app).
pub fn publish_title(window: Option<Entity>, title: &str) {
    with(|s| match window {
        Some(window) => {
            let entry = s.windows.entry(window.to_bits()).or_default();
            if entry.title != title {
                entry.title = title.to_string();
            }
        }
        None => s.windowless_title = title.to_string(),
    });
}

/// Copy every window's live size and scale into the mirror whenever it
/// changes, drop windows that closed, and track the primary window.
/// Registered by `App::new` at the start of each tick, ahead of every script
/// handler, and at its end, so a size applied during the tick is what the
/// getters read after it.
pub fn publish_windows(
    changed: Query<(Entity, &WindowViewport), Changed<WindowViewport>>,
    windows: Query<(Entity, Has<PrimaryWindow>), With<Window>>,
    mut removed: RemovedComponents<Window>,
) {
    let gone: Vec<Entity> = removed.read().collect();
    if changed.is_empty() && gone.is_empty() {
        return;
    }
    with(|s| {
        for (window, viewport) in &changed {
            let entry = s.windows.entry(window.to_bits()).or_default();
            entry.width = viewport.size.x;
            entry.height = viewport.size.y;
            entry.dpr = viewport.scale_factor;
        }
        for window in gone {
            s.windows.remove(&window.to_bits());
        }
        s.primary = windows
            .iter()
            .find(|(_, primary)| *primary)
            .map_or(0, |(e, _)| e.to_bits());
    });
}
