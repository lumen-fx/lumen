//! Desktop-notification host for Lumen.
//!
//! Wraps `notify-rust` 4 behind a [`NotificationService`] resource +
//! [`NotificationActionInvoked`] message. Mirrors `GNotification` /
//! `GApplication::send_notification` and
//! `QSystemTrayIcon::showMessage`.
//!
//! Extracted from `lumenc/src/run.rs:1377-1389` (the
//! `ScriptCommand::Notify` branch) per W6.5. The v1 surface preserves
//! the existing fire-and-forget behaviour - the actions list and
//! [`NotificationActionInvoked`] reader-side wiring lay the
//! follow-up's surface down.
//!
//! `Action`s come from [`lumen_os_mime::Action`] so one Action drives
//! menu items, hotkeys, tray menus, and notification buttons (the
//! shared abstraction described in audit section 470 - equivalent to
//! `GAction` driving every interaction surface in GIO).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use std::sync::Arc;

pub use lumen_os_mime as mime;
pub use lumen_os_mime::Action;

/// Urgency level for a notification, mapped per-backend.
///
/// Mirrors `notify_rust::Urgency` / GTK's `GNotification` priority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    /// Low-priority background information.
    Low,
    /// Standard notification (default).
    #[default]
    Normal,
    /// Urgent / persistent alert.
    Critical,
}

impl From<Urgency> for notify_rust::Urgency {
    fn from(u: Urgency) -> Self {
        match u {
            Urgency::Low => notify_rust::Urgency::Low,
            Urgency::Normal => notify_rust::Urgency::Normal,
            Urgency::Critical => notify_rust::Urgency::Critical,
        }
    }
}

/// One notification request.
///
/// Mirrors `notify_rust::Notification::{summary, body, icon, urgency,
/// action}` plus a stable id used to route the eventual
/// [`NotificationActionInvoked`] message back to the script.
#[derive(Clone, Debug, Default)]
pub struct Notification {
    /// Stable id used as the routing key for
    /// [`NotificationActionInvoked`].
    pub id: String,
    /// Bold title text shown above the body.
    pub title: String,
    /// Body text.
    pub body: String,
    /// Optional icon name (themable) or path. Maps to
    /// `notify-rust::Notification::icon`.
    pub icon: Option<String>,
    /// Urgency hint, mapped per-backend.
    pub urgency: Urgency,
    /// Inline action buttons. Reuses [`Action`] from `lumen-os-mime`
    /// so the same Action drives menus and notifications.
    pub actions: Vec<Action>,
}

/// Stable handle returned by [`NotificationService::send`]. Today it
/// just echoes the request id - a follow-up will hold the
/// `notify_rust::NotificationHandle` so apps can update / dismiss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationId(pub Arc<str>);

impl From<&str> for NotificationId {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}

impl From<String> for NotificationId {
    fn from(s: String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}

/// ECS message: an action button on a previously-spawned notification
/// fired. Routed by the scripting layer as
/// `on_notification_action(id, action_id)`.
///
/// v1 doesn't actually wire backend callbacks (notify-rust's
/// `wait_for_action` is per-handle and blocking); the type is here so
/// downstream code can already write the handler.
#[derive(Message, Clone, Debug)]
pub struct NotificationActionInvoked {
    /// Notification id (matches [`Notification::id`]).
    pub id: String,
    /// Action id (matches the action's [`Action::id`]).
    pub action_id: String,
}

/// Notification-host resource. Stateless today - `send` is a pure
/// fire-and-forget call into notify-rust. A follow-up holds live
/// `NotificationHandle`s here so `dismiss(id)` / `update(id, ...)`
/// become possible.
#[derive(Resource, Default, Clone)]
pub struct NotificationService {
    /// Optional app id used by the macOS / Windows backends. `None` means
    /// "use the binary's bundle id". The runtime never sets this, so it is
    /// `None` under `lumenc run`; an embedder can fill it in with
    /// [`Self::with_app_id`].
    pub app_id: Option<String>,
}

impl NotificationService {
    /// Empty service, no app id.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the app id used by `notify_rust::set_application` (Win) /
    /// `subtitle` (mac). Returns `self` for builder chaining.
    pub fn with_app_id(mut self, id: impl Into<String>) -> Self {
        self.app_id = Some(id.into());
        self
    }

    /// Fire a notification. Returns the id for routing the eventual
    /// [`NotificationActionInvoked`] back to the right handler.
    /// Errors log to stderr - matches the previous behaviour exactly.
    pub fn send(&self, n: &Notification) -> NotificationId {
        let mut builder = notify_rust::Notification::new();
        builder.summary(&n.title).body(&n.body);
        if let Some(icon) = &n.icon {
            builder.icon(icon);
        }
        // XDG maps urgency to a hint and Windows maps it to a toast scenario.
        // macOS has no equivalent, so notify-rust does not compile the setter
        // there and the call has to be gated rather than the whole type.
        #[cfg(not(target_os = "macos"))]
        builder.urgency(n.urgency.into());
        for a in &n.actions {
            // notify-rust's add action signature is (identifier, label).
            builder.action(a.id.as_ref(), a.label.as_ref());
        }
        // Apply the configured app id. Windows toasts key off the
        // AppUserModelID (`Notification::app_id`, a no-op on other OSes);
        // macOS uses the process-global `set_application` (bundle id) and
        // has no per-notification setter. Linux derives identity from the
        // `.desktop` / bus name, so there is nothing to set here.
        if let Some(id) = self.app_id.as_deref().filter(|s| !s.is_empty()) {
            #[cfg(target_os = "windows")]
            {
                builder.app_id(id);
            }
            #[cfg(target_os = "macos")]
            {
                if let Err(e) = notify_rust::set_application(id) {
                    eprintln!("lumen-os-notify: set_application({id}) failed: {e}");
                }
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                let _ = id;
            }
        }
        if let Err(e) = builder.show() {
            eprintln!("lumen-os-notify: notify failed: {e}");
        }
        NotificationId::from(n.id.as_str())
    }

    /// Convenience: build + send a plain title / body notification.
    /// Matches the pre-extract `notify(title, body)` Rhai shape.
    pub fn send_simple(&self, title: &str, body: &str) -> NotificationId {
        let n = Notification {
            id: String::new(),
            title: title.to_string(),
            body: body.to_string(),
            ..Default::default()
        };
        self.send(&n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_default_is_normal() {
        assert_eq!(Urgency::default(), Urgency::Normal);
    }

    #[test]
    fn urgency_maps_into_notify_rust() {
        let _l: notify_rust::Urgency = Urgency::Low.into();
        let _n: notify_rust::Urgency = Urgency::Normal.into();
        let _c: notify_rust::Urgency = Urgency::Critical.into();
    }

    #[test]
    fn id_from_str_and_string() {
        let a: NotificationId = "abc".into();
        let b: NotificationId = String::from("abc").into();
        assert_eq!(a, b);
        assert_eq!(&*a.0, "abc");
    }

    #[test]
    fn notification_default_is_empty() {
        let n = Notification::default();
        assert!(n.id.is_empty());
        assert!(n.actions.is_empty());
        assert_eq!(n.urgency, Urgency::Normal);
    }

    #[test]
    fn service_app_id_builder() {
        let s = NotificationService::new().with_app_id("com.example.lumen");
        assert_eq!(s.app_id.as_deref(), Some("com.example.lumen"));
    }
}
