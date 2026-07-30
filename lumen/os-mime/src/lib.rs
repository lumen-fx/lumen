//! Shared `MimePayload` and `Action` abstractions for the `lumen-os-*`
//! crates (`lumen-os-clipboard`, `lumen-os-dnd`, `lumen-os-menu`).
//!
//! Mirrors Qt's `QMimeData` / `QAction` and GTK 4's `GdkContentProvider`
//! / `GAction`. Pure-data - no OS or ECS dependency lives here so every
//! `lumen-os-*` crate can pull it without dragging in the others.
//!
//! See `docs/audits/os-integration.md` "Shared abstractions" (section 469-470).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use smallvec::SmallVec;
use std::sync::Arc;

/// A single MIME-typed payload entry kind.
///
/// Mirrors the well-known content types Qt's `QMimeData` exposes via
/// `text()`, `html()`, `urls()`, `imageData()`, `setData(mime, bytes)`.
/// Unknown / app-specific MIME types live under [`MimeKind::Custom`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MimeKind {
    /// `text/plain;charset=utf-8`.
    TextPlain,
    /// `text/html`.
    TextHtml,
    /// `text/uri-list` (RFC 2483) - newline-separated URIs / `file://`
    /// paths. Standard format used by file managers for drag-and-drop.
    TextUriList,
    /// `image/png`.
    ImagePng,
    /// `image/jpeg`.
    ImageJpeg,
    /// `application/octet-stream` - opaque bytes.
    ApplicationOctet,
    /// Any other MIME type (`application/json`, `application/x-myapp`,
    /// ...). The `Arc<str>` is shared so cheap to clone.
    Custom(Arc<str>),
}

impl MimeKind {
    /// Canonical MIME-type string for this kind.
    pub fn as_str(&self) -> &str {
        match self {
            MimeKind::TextPlain => "text/plain;charset=utf-8",
            MimeKind::TextHtml => "text/html",
            MimeKind::TextUriList => "text/uri-list",
            MimeKind::ImagePng => "image/png",
            MimeKind::ImageJpeg => "image/jpeg",
            MimeKind::ApplicationOctet => "application/octet-stream",
            MimeKind::Custom(s) => s,
        }
    }
}

impl From<&str> for MimeKind {
    fn from(s: &str) -> Self {
        match s {
            "text/plain" | "text/plain;charset=utf-8" => MimeKind::TextPlain,
            "text/html" => MimeKind::TextHtml,
            "text/uri-list" => MimeKind::TextUriList,
            "image/png" => MimeKind::ImagePng,
            "image/jpeg" => MimeKind::ImageJpeg,
            "application/octet-stream" => MimeKind::ApplicationOctet,
            other => MimeKind::Custom(Arc::from(other)),
        }
    }
}

/// A MIME-typed multi-format payload.
///
/// Used by both the clipboard ([`lumen-os-clipboard`]) and drag-and-drop
/// ([`lumen-os-dnd`]) - one payload can carry multiple representations
/// of the same data (the convention shared by `QMimeData` and
/// `GdkContentProvider`). Targets read whichever MIME they understand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimePayload {
    /// Stored representations in author-insertion order; recipients pick
    /// the first kind they understand.
    pub kinds: SmallVec<[(MimeKind, Vec<u8>); 4]>,
}

impl MimePayload {
    /// Empty payload, no entries.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when this payload carries no entries.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Push an `(kind, bytes)` entry. Returns `self` for builder-style
    /// chaining.
    pub fn with(mut self, kind: MimeKind, bytes: Vec<u8>) -> Self {
        self.kinds.push((kind, bytes));
        self
    }

    /// Look up the first entry matching `kind`. Returns `None` when not
    /// carried.
    pub fn get(&self, kind: &MimeKind) -> Option<&[u8]> {
        self.kinds
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, b)| b.as_slice())
    }

    /// True when this payload carries the named kind.
    pub fn has(&self, kind: &MimeKind) -> bool {
        self.kinds.iter().any(|(k, _)| k == kind)
    }

    /// Convenience: read the `text/plain` entry as UTF-8, falling back
    /// to an empty string when the bytes aren't valid UTF-8.
    pub fn text(&self) -> Option<String> {
        self.get(&MimeKind::TextPlain)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// Convenience: read the `text/uri-list` entry, splitting on `\r\n`
    /// per RFC 2483 (lines starting with `#` are comments and skipped).
    pub fn uris(&self) -> Vec<String> {
        let Some(bytes) = self.get(&MimeKind::TextUriList) else {
            return Vec::new();
        };
        std::str::from_utf8(bytes)
            .unwrap_or("")
            .split("\r\n")
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
            .map(|s| s.to_string())
            .collect()
    }
}

impl From<&str> for MimePayload {
    fn from(s: &str) -> Self {
        let mut p = MimePayload::new();
        p.kinds.push((MimeKind::TextPlain, s.as_bytes().to_vec()));
        p
    }
}

impl From<String> for MimePayload {
    fn from(s: String) -> Self {
        let mut p = MimePayload::new();
        p.kinds.push((MimeKind::TextPlain, s.into_bytes()));
        p
    }
}

impl From<Vec<u8>> for MimePayload {
    fn from(bytes: Vec<u8>) -> Self {
        let mut p = MimePayload::new();
        p.kinds.push((MimeKind::ApplicationOctet, bytes));
        p
    }
}

impl From<Vec<std::path::PathBuf>> for MimePayload {
    fn from(paths: Vec<std::path::PathBuf>) -> Self {
        // text/uri-list is the standard DnD / clipboard format for file
        // paths on every desktop OS (Win32 CF_HDROP gets translated
        // through this on Wayland / X11 / macOS).
        let mut buf = String::new();
        for p in &paths {
            if !buf.is_empty() {
                buf.push_str("\r\n");
            }
            // Best-effort: percent-encoding is the receiver's problem
            // here; most apps accept raw `file://` paths.
            buf.push_str("file://");
            buf.push_str(&p.to_string_lossy());
        }
        let mut payload = MimePayload::new();
        payload
            .kinds
            .push((MimeKind::TextUriList, buf.into_bytes()));
        payload
    }
}

/// A keyboard-shortcut chord. Stored as a `muda` / `global-hotkey`-style
/// accelerator string (e.g. `"Ctrl+S"`, `"Cmd+Shift+P"`).
///
/// Kept opaque so the menu / hotkey backends parse via their own crate
/// API (`muda::accelerator::Accelerator::from_str`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord(pub Arc<str>);

impl From<&str> for KeyChord {
    fn from(s: &str) -> Self {
        KeyChord(Arc::from(s))
    }
}

impl From<String> for KeyChord {
    fn from(s: String) -> Self {
        KeyChord(Arc::from(s.as_str()))
    }
}

/// A high-level user-invokable command. One `Action` ties together the
/// menu item, toolbar button, and keyboard shortcut that all do the
/// same thing - the model Qt's `QAction` and GTK 4's `GAction` use.
///
/// Lives in `ActionRegistry` (or a similar resource) per the OS plan
/// (section 470). Backends translate the action into a menu item, hotkey, or
/// toolbar button as appropriate.
#[derive(Clone, Debug)]
pub struct Action {
    /// Stable id used to dispatch `ActionInvoked { id }` and to look the
    /// action up across hot-reloads.
    pub id: Arc<str>,
    /// Human-visible label.
    pub label: Arc<str>,
    /// Optional icon identifier (asset path or theme name).
    pub icon: Option<Arc<str>>,
    /// Optional keyboard shortcut.
    pub shortcut: Option<KeyChord>,
    /// `true` when the action can be invoked.
    pub enabled: bool,
    /// `Some(true|false)` for a check / radio action; `None` for a
    /// plain action.
    pub checked: Option<bool>,
}

impl Action {
    /// Construct a plain enabled action with no shortcut, icon, or
    /// check state.
    pub fn new(id: impl Into<Arc<str>>, label: impl Into<Arc<str>>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            enabled: true,
            checked: None,
        }
    }

    /// Builder: set the keyboard shortcut.
    pub fn with_shortcut(mut self, chord: impl Into<KeyChord>) -> Self {
        self.shortcut = Some(chord.into());
        self
    }

    /// Builder: set the icon.
    pub fn with_icon(mut self, icon: impl Into<Arc<str>>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Builder: set the enabled flag.
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    /// Builder: turn into a check action with the supplied initial
    /// state.
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_payload_from_str_roundtrips_text() {
        let p: MimePayload = "hello".into();
        assert_eq!(p.text().as_deref(), Some("hello"));
        assert!(p.has(&MimeKind::TextPlain));
        assert!(!p.has(&MimeKind::TextHtml));
    }

    #[test]
    fn mime_payload_from_bytes_is_octet() {
        let p: MimePayload = vec![1u8, 2, 3].into();
        assert_eq!(p.get(&MimeKind::ApplicationOctet), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn mime_payload_from_paths_emits_uri_list() {
        let paths = vec![
            std::path::PathBuf::from("/tmp/a.txt"),
            std::path::PathBuf::from("/tmp/b.txt"),
        ];
        let p: MimePayload = paths.into();
        let uris = p.uris();
        assert_eq!(uris.len(), 2);
        assert!(uris[0].starts_with("file:///tmp/a.txt"));
        assert!(uris[1].starts_with("file:///tmp/b.txt"));
    }

    #[test]
    fn mime_kind_from_str_canonicalises() {
        assert_eq!(MimeKind::from("text/plain"), MimeKind::TextPlain);
        assert_eq!(MimeKind::from("image/png"), MimeKind::ImagePng);
        let custom = MimeKind::from("application/x-myapp");
        match custom {
            MimeKind::Custom(s) => assert_eq!(&*s, "application/x-myapp"),
            _ => panic!("expected Custom variant"),
        }
    }

    #[test]
    fn action_builder_chain() {
        let a = Action::new("file.save", "Save")
            .with_shortcut("Ctrl+S")
            .enabled(true);
        assert_eq!(&*a.id, "file.save");
        assert_eq!(&*a.label, "Save");
        assert_eq!(a.shortcut.as_ref().map(|c| c.0.as_ref()), Some("Ctrl+S"));
        assert!(a.enabled);
        assert!(a.checked.is_none());
    }

    #[test]
    fn action_check_state() {
        let a = Action::new("view.bold", "Bold").checked(true);
        assert_eq!(a.checked, Some(true));
    }

    #[test]
    fn empty_payload_has_no_text() {
        let p = MimePayload::new();
        assert!(p.is_empty());
        assert_eq!(p.text(), None);
        assert!(p.uris().is_empty());
    }
}
