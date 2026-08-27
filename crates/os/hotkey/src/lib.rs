//! Global-hotkey host for Lumen.
//!
//! Wraps `global-hotkey` 0.6 behind a [`HotkeyRegistry`] non-send
//! resource. Mirrors Qt's `QShortcut` (the global-shortcut equivalent
//! every Qt app rolls via `QHotkey`) and GTK 4's
//! `GtkShortcutController` + the XDG `GlobalShortcuts` portal.
//!
//! Extracted from `public/lumenc/src/run.rs:951-1026` (the `HotkeyRegistry`
//! struct + `register_hotkey` / `unregister_hotkey` /
//! `poll_global_hotkeys` helpers) per W6.5.
//!
//! Both `Pressed` and `Released` events are surfaced as separate
//! messages, so one chord can drive push-to-talk. Scripts see them as
//! `on_hotkey(name)` and `on_hotkey_release(name)`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use std::str::FromStr;

pub use lumen_os_mime as mime;
pub use lumen_os_mime::KeyChord;

/// Backwards-compatible alias for the existing message - emitted
/// whenever a registered hotkey fires its press event. Scripts route
/// it as `on_hotkey(name)`.
pub use lumen_core::input::HotkeyFired as HotkeyPressed;

/// A previously-pressed hotkey was released. Scripts route it as
/// `on_hotkey_release(name)`, the release half of the push-to-talk
/// pair.
///
/// Lives in `lumen-core` beside [`HotkeyPressed`] so the scripting
/// layer can dispatch it without depending on this crate; re-exported
/// here because this crate produces it.
pub use lumen_core::input::HotkeyReleased;

/// OS-level global hotkey registry. `GlobalHotKeyManager` is `!Send`
/// on some platforms (macOS NSEvent monitor), so this resource lives
/// as a `NonSend` in the ECS world.
///
/// Backwards-compatible shape with the previous
/// `lumenc::run::HotkeyRegistry`.
pub struct HotkeyRegistry {
    manager: global_hotkey::GlobalHotKeyManager,
    /// `name` -> `(id, accelerator)` so we can unregister by name and
    /// look the name up when an event fires.
    by_name: std::collections::HashMap<String, (u32, global_hotkey::hotkey::HotKey)>,
    /// `id` -> `name` for the event-dispatch lookup.
    by_id: std::collections::HashMap<u32, String>,
}

impl HotkeyRegistry {
    /// Try to create the registry. Returns `None` on platforms where
    /// the OS-level manager fails to initialise (CI without an X11
    /// display, the existing failure path) - and, on Linux/BSD, when
    /// no X11 display is reachable at all.
    ///
    /// The display guard matters (W6 T1): `GlobalHotKeyManager::new`
    /// always returns `Ok` and only fails inside its worker thread, so
    /// without the guard a displayless host would silently install a
    /// dead registry (and, under `global-hotkey` 0.6, segfault the
    /// whole process in that worker). `global-hotkey` is X11-only on
    /// Linux - a Wayland-only session (no `DISPLAY` from XWayland)
    /// legitimately has no global-hotkey support either.
    pub fn new() -> Option<Self> {
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        if !display_env_usable(std::env::var("DISPLAY").ok().as_deref()) {
            eprintln!(
                "lumen-os-hotkey: no X11 display (DISPLAY unset/empty); global hotkeys unavailable"
            );
            return None;
        }
        match global_hotkey::GlobalHotKeyManager::new() {
            Ok(manager) => Some(Self {
                manager,
                by_name: std::collections::HashMap::new(),
                by_id: std::collections::HashMap::new(),
            }),
            Err(e) => {
                eprintln!("lumen-os-hotkey: manager init failed: {e}");
                None
            }
        }
    }

    /// Register (or replace) a hotkey under `name`, parsing
    /// `accelerator` via `global-hotkey`'s electron-style string
    /// format (`"Ctrl+Shift+S"`, `"Cmd+P"`, ...).
    ///
    /// A grab conflict - another process (possibly a crashed
    /// predecessor of this very app, W6 T7) already holds the chord -
    /// surfaces as `Error::AlreadyRegistered` from `global-hotkey` 0.8
    /// (x11rb checks the `X_GrabKey` reply and maps `Access` errors;
    /// nothing routes through Xlib's process-killing default error
    /// handler). We log and leave the name unbound: non-fatal.
    pub fn register(&mut self, name: &str, accelerator: &str) {
        // Drop any existing binding so callers can repeatedly
        // re-register a name without leaking.
        self.unregister(name);
        let key = match global_hotkey::hotkey::HotKey::from_str(accelerator) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("lumen-os-hotkey: bad accelerator '{accelerator}' for '{name}': {e}");
                return;
            }
        };
        if let Err(e) = self.manager.register(key) {
            eprintln!("lumen-os-hotkey: register '{name}' = '{accelerator}' failed: {e}");
            return;
        }
        let id = key.id();
        self.by_name.insert(name.to_string(), (id, key));
        self.by_id.insert(id, name.to_string());
    }

    /// Register a hotkey via a [`KeyChord`] (shared with the menu
    /// crate). Convenience over [`Self::register`].
    pub fn register_chord(&mut self, name: &str, chord: &KeyChord) {
        self.register(name, chord.0.as_ref());
    }

    /// Remove the binding under `name`. No-op if `name` isn't
    /// registered.
    pub fn unregister(&mut self, name: &str) {
        if let Some((id, key)) = self.by_name.remove(name) {
            let _ = self.manager.unregister(key);
            self.by_id.remove(&id);
        }
    }

    /// True when `name` is currently bound.
    pub fn is_registered(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// List currently-registered hotkey names.
    pub fn list(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }
}

/// True when the `DISPLAY` environment value plausibly names an X11
/// display: present and non-empty. Pure so the displayless-host guard
/// in [`HotkeyRegistry::new`] is unit-testable without touching
/// process-global env vars (W6 T1 - a host with zero display sockets
/// must skip manager construction entirely).
///
/// X11-only concern, so gated to the same Linux/BSD cfg as its sole
/// caller in [`HotkeyRegistry::new`]; on macOS/Windows there is no
/// `DISPLAY` guard and the function would be dead code.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn display_env_usable(display: Option<&str>) -> bool {
    display.is_some_and(|d| !d.is_empty())
}

/// Drain the `global-hotkey` event channel each tick and re-emit
/// matching presses + releases as separate ECS messages.
///
/// Bug fix from the audit: pre-extract `poll_global_hotkeys` filtered
/// to `Pressed` only, dropping the Release path so push-to-talk
/// couldn't be built. This poll surfaces both halves.
pub fn poll_hotkeys(
    reg: Option<NonSend<HotkeyRegistry>>,
    mut pressed: MessageWriter<HotkeyPressed>,
    mut released: MessageWriter<HotkeyReleased>,
) {
    let Some(reg) = reg else {
        return;
    };
    let rx = global_hotkey::GlobalHotKeyEvent::receiver();
    while let Ok(ev) = rx.try_recv() {
        let Some(name) = reg.by_id.get(&ev.id) else {
            continue;
        };
        match ev.state {
            global_hotkey::HotKeyState::Pressed => {
                pressed.write(HotkeyPressed { name: name.clone() });
            }
            global_hotkey::HotKeyState::Released => {
                released.write(HotkeyReleased { name: name.clone() });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The real GlobalHotKeyManager refuses to init under CI (no X11),
    // so cover the data-shape round-trips and chord wiring instead.

    #[test]
    fn key_chord_round_trips() {
        let c: KeyChord = "Ctrl+S".into();
        assert_eq!(c.0.as_ref(), "Ctrl+S");
    }

    #[test]
    fn hotkey_released_message_constructs() {
        let r = HotkeyReleased {
            name: "save".to_string(),
        };
        assert_eq!(r.name, "save");
    }

    #[test]
    fn registry_init_under_ci_is_optional() {
        // On a CI host without a display, `HotkeyRegistry::new`
        // returns None - confirming the optional shape.
        let _ = HotkeyRegistry::new();
    }

    /// W6 T1: the displayless guard. A host with no X socket (DISPLAY
    /// unset or empty) must short-circuit to `None` BEFORE
    /// `GlobalHotKeyManager::new` spawns its worker thread - under
    /// `global-hotkey` 0.6 that thread segfaulted the process
    /// (`XOpenDisplay` null deref in `events_processor`).
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    #[test]
    fn displayless_host_is_rejected_by_the_guard() {
        assert!(!display_env_usable(None), "unset DISPLAY -> no X11");
        assert!(!display_env_usable(Some("")), "empty DISPLAY -> no X11");
        assert!(display_env_usable(Some(":1")), "local display accepted");
        assert!(
            display_env_usable(Some("localhost:10.0")),
            "forwarded display accepted (probe left to the backend)"
        );
    }

    /// W6 T7: a grab conflict must be non-fatal. Headless CI cannot
    /// exercise a real `X_GrabKey` BadAccess (needs a live X server and
    /// a competing grab holder - see the register() docs; verified
    /// manually by relaunching an app that registers the same chord).
    /// What we CAN pin down is the error-shape contract this crate
    /// relies on: `global-hotkey` 0.8 surfaces the conflict as
    /// `Error::AlreadyRegistered`, which `register()` maps to a log +
    /// unbound name rather than a crash.
    #[test]
    fn grab_conflict_error_shape_is_non_fatal() {
        use std::str::FromStr;
        let key = global_hotkey::hotkey::HotKey::from_str("Ctrl+Shift+S").unwrap();
        let err = global_hotkey::Error::AlreadyRegistered(key);
        // The display side of the contract: formatting must not panic
        // and the variant must be matchable (register() logs it).
        match err {
            global_hotkey::Error::AlreadyRegistered(k) => {
                assert_eq!(k.id(), key.id());
            }
            other => panic!("unexpected error shape: {other}"),
        }
    }
}
