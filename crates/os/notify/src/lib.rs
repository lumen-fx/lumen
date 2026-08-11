//! Desktop-notification host for Lumen.
//!
//! Wraps `notify-rust` 4 behind a [`NotificationService`] resource +
//! [`NotificationActionInvoked`] message. Mirrors `GNotification` /
//! `GApplication::send_notification` and
//! `QSystemTrayIcon::showMessage`.
//!
//! A notification carries a title, body, optional icon, urgency, and a
//! list of action buttons. Pressing a button emits
//! [`NotificationActionInvoked`], which [`poll_notification_actions`]
//! drains each tick; scripts see it as
//! `on_notification_action(id, action_id)`. Button presses report back
//! on freedesktop desktops only, the one backend `notify-rust` gives an
//! activation callback for.
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

impl Urgency {
    /// Parse the name a script passes to `notify_ex`. Unknown and empty
    /// names resolve to [`Urgency::Normal`], so an app that leaves the
    /// argument blank gets the default rather than a failed call.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "low" => Urgency::Low,
            "critical" => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
}

/// Parse an action-button spec into [`Action`]s: `|`-separated
/// `id:Label` buttons, the same shape a tray menu takes. See
/// [`lumen_os_mime::parse_action_spec`] for the full grammar. A
/// notification draws no separator, so the spec's `-` entry has no use
/// here.
pub fn parse_actions(spec: &str) -> Vec<Action> {
    lumen_os_mime::parse_action_spec(spec)
}

/// The per-notification settings that are neither text nor buttons.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotifyOptions {
    /// Themed icon name or icon path.
    pub icon: Option<String>,
    /// Urgency hint.
    pub urgency: Urgency,
}

/// Parse an options spec into [`NotifyOptions`].
///
/// Same `|`-separated `key:value` shape as the action spec. Recognised
/// keys are `icon` and `urgency`; anything else is ignored, so a spec
/// written against a newer runtime degrades instead of failing. An empty
/// spec yields the defaults: no icon, normal urgency.
///
/// Only the value is split off, so an icon path carrying a drive letter
/// (`icon:C:\icons\app.png`) survives intact.
pub fn parse_options(spec: &str) -> NotifyOptions {
    let mut out = NotifyOptions::default();
    for entry in spec.split('|').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((key, value)) = entry.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "icon" if !value.is_empty() => out.icon = Some(value.to_string()),
            "urgency" => out.urgency = Urgency::from_name(value),
            _ => {}
        }
    }
    out
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
/// Lives in `lumen-core` so the scripting layer can dispatch it
/// without depending on this crate; re-exported here because this
/// crate produces it.
pub use lumen_core::input::NotificationActionInvoked;

/// Process-global queue of `(notification_id, action_id)` pairs the
/// activation waiters have observed, drained once per tick by
/// [`poll_notification_actions`].
fn action_queue() -> &'static std::sync::Mutex<Vec<(String, String)>> {
    static Q: std::sync::OnceLock<std::sync::Mutex<Vec<(String, String)>>> =
        std::sync::OnceLock::new();
    Q.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Push one observed activation. Poison recovery instead of a silent
/// drop: the queue is a plain `Vec`, so a panicking holder leaves it
/// perfectly usable, and swallowing the poison would lose every future
/// action with no diagnostic.
///
/// Compiled where the waiter that calls it is.
#[cfg(all(unix, not(target_os = "macos")))]
fn push_action(id: String, action_id: String) {
    let mut q = action_queue().lock().unwrap_or_else(|e| e.into_inner());
    q.push((id, action_id));
}

/// Drain the activation queue each tick and emit one
/// [`NotificationActionInvoked`] per observed button press.
pub fn poll_notification_actions(mut out: MessageWriter<NotificationActionInvoked>) {
    let drained: Vec<(String, String)> = {
        let mut q = action_queue().lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    };
    for (id, action_id) in drained {
        out.write(NotificationActionInvoked { id, action_id });
    }
}

/// Notification-host resource. Holds no live notification: `send`
/// hands the spec to notify-rust and returns. A follow-up keeps
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
        match builder.show() {
            Ok(handle) => self.watch_actions(n, handle),
            Err(e) => eprintln!("lumen-os-notify: notify failed: {e}"),
        }
        NotificationId::from(n.id.as_str())
    }

    /// Park a waiter on the live notification so a button press reaches
    /// the app.
    ///
    /// `wait_for_action` blocks until the user picks a button or the
    /// popup expires, so it runs on its own thread; the thread ends with
    /// the notification. Only notifications that declared buttons get a
    /// waiter, so a plain `notify(title, body)` still costs nothing.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn watch_actions(&self, n: &Notification, handle: notify_rust::NotificationHandle) {
        if n.actions.is_empty() {
            return;
        }
        let id = n.id.clone();
        let spawned = std::thread::Builder::new()
            .name("lumen-os-notify/action".to_string())
            .spawn(move || {
                handle.wait_for_action(|action| {
                    // The spec's synthetic "closed" / "__closed" action
                    // fires on dismissal; forward it verbatim so a script
                    // can tell dismissal from a real button.
                    push_action(id, action.to_string());
                });
            });
        if let Err(e) = spawned {
            tracing::debug!("lumen-os-notify: action waiter spawn failed: {e}");
        }
    }

    /// Activation waiting is freedesktop-only: `notify-rust` exposes no
    /// callback for the macOS or Windows backends, so buttons render but
    /// never report back.
    ///
    /// Generic over the handle because `show()` resolves to a different
    /// success type per backend; this arm ignores whichever it gets.
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn watch_actions<H>(&self, _n: &Notification, _handle: H) {}

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
    fn urgency_from_name_defaults_to_normal() {
        assert_eq!(Urgency::from_name("low"), Urgency::Low);
        assert_eq!(Urgency::from_name(" Critical "), Urgency::Critical);
        assert_eq!(Urgency::from_name("normal"), Urgency::Normal);
        // An empty or unknown name must not fail the call.
        assert_eq!(Urgency::from_name(""), Urgency::Normal);
        assert_eq!(Urgency::from_name("shouty"), Urgency::Normal);
    }

    #[test]
    fn action_spec_parses_ids_and_labels() {
        let actions = parse_actions("open:Open|dismiss:Dismiss");
        assert_eq!(actions.len(), 2);
        assert_eq!(&*actions[0].id, "open");
        assert_eq!(&*actions[0].label, "Open");
        assert_eq!(&*actions[1].id, "dismiss");
        assert!(parse_actions("").is_empty());
    }

    #[test]
    fn options_spec_reads_icon_and_urgency() {
        let o = parse_options("icon:document-save|urgency:critical");
        assert_eq!(o.icon.as_deref(), Some("document-save"));
        assert_eq!(o.urgency, Urgency::Critical);
    }

    #[test]
    fn options_spec_keeps_a_windows_path_intact() {
        let o = parse_options(r"icon:C:\icons\app.png");
        assert_eq!(o.icon.as_deref(), Some(r"C:\icons\app.png"));
    }

    #[test]
    fn options_spec_defaults_and_ignores_unknown_keys() {
        assert_eq!(parse_options(""), NotifyOptions::default());
        let o = parse_options("timeout:5000|urgency:low");
        assert!(o.icon.is_none());
        assert_eq!(o.urgency, Urgency::Low);
    }

    #[test]
    fn action_queue_drains_once() {
        // Queued the way the freedesktop waiter queues, which is the only
        // producer and is not compiled on every target.
        action_queue()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(("n1".to_string(), "open".to_string()));
        let drained: Vec<(String, String)> = {
            let mut q = action_queue().lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *q)
        };
        assert!(drained.contains(&("n1".to_string(), "open".to_string())));
        let again = action_queue().lock().unwrap_or_else(|e| e.into_inner());
        assert!(again.is_empty(), "a drained action must not repeat");
    }

    #[test]
    fn service_app_id_builder() {
        let s = NotificationService::new().with_app_id("com.example.lumen");
        assert_eq!(s.app_id.as_deref(), Some("com.example.lumen"));
    }
}
