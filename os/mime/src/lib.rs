//! Shared `MimePayload` and `Action` abstractions for the `lumen-os-*`
//! crates (`lumen-os-clipboard`, `lumen-os-dnd`, `lumen-os-menu`).
//!
//! Mirrors Qt's `QMimeData` / `QAction` and GTK 4's `GdkContentProvider`
//! / `GAction`. Pure-data - no OS or ECS dependency lives here so every
//! `lumen-os-*` crate can pull it without dragging in the others.

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

impl MimeKind {
    /// The type without its parameters, the spelling the web's
    /// `DataTransfer.types` lists: `text/plain` for [`MimeKind::TextPlain`],
    /// `application/x-asset` for a custom kind of that name.
    pub fn essence(&self) -> &str {
        match self {
            MimeKind::TextPlain => "text/plain",
            other => {
                let s = other.as_str();
                s.split(';').next().unwrap_or(s).trim()
            }
        }
    }

    /// Whether this kind is what an `accept` entry names: the same type, or
    /// any type under a `type/*` wildcard (`image/*` takes `image/png`), as
    /// an HTML `<input accept>` list reads. Case is ignored and parameters
    /// do not count.
    pub fn matches(&self, pattern: &str) -> bool {
        let pattern = pattern.split(';').next().unwrap_or(pattern).trim();
        let essence = self.essence();
        match pattern.strip_suffix("/*") {
            Some("*") => true,
            Some(major) => essence
                .split_once('/')
                .is_some_and(|(m, _)| m.eq_ignore_ascii_case(major)),
            None => essence.eq_ignore_ascii_case(pattern),
        }
    }
}

impl From<&str> for MimeKind {
    fn from(s: &str) -> Self {
        // Lowercased, as the web's `DataTransfer` stores a type.
        let lower = s.trim().to_ascii_lowercase();
        match lower.split(';').next().unwrap_or_default().trim() {
            // `text` and `url` are the web's two short names.
            "text/plain" | "text" => MimeKind::TextPlain,
            "text/html" => MimeKind::TextHtml,
            "text/uri-list" | "url" => MimeKind::TextUriList,
            "image/png" => MimeKind::ImagePng,
            "image/jpeg" => MimeKind::ImageJpeg,
            "application/octet-stream" => MimeKind::ApplicationOctet,
            _ => MimeKind::Custom(Arc::from(lower)),
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

    /// Put `bytes` under `kind`, replacing what the payload already carried
    /// under it, the way the web's `DataTransfer.setData` does. A new kind
    /// goes after the ones already there.
    pub fn set(&mut self, kind: MimeKind, bytes: Vec<u8>) {
        match self.kinds.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, b)) => *b = bytes,
            None => self.kinds.push((kind, bytes)),
        }
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

/// Action id reserved for a menu separator. A backend that can draw one
/// (a tray menu, a menu bar) turns an action carrying this id into its
/// platform separator.
pub const SEPARATOR_ID: &str = "separator";

impl From<&str> for Action {
    /// Read one `id:label` entry of an action spec. An entry with no
    /// `:` uses its whole text as both id and label, and a lone `-` is
    /// the separator.
    fn from(entry: &str) -> Self {
        let entry = entry.trim();
        if entry == "-" {
            return Action::new(SEPARATOR_ID, "-");
        }
        match entry.split_once(':') {
            Some((id, label)) => Action::new(id.trim(), label.trim()),
            None => Action::new(entry, entry),
        }
    }
}

/// Read an action spec: `|`-separated `id:label` entries, where `-` is a
/// separator.
///
/// One spelling serves every surface a script builds from a string, so an
/// author learns the shape once and a tray menu and a notification's
/// buttons read alike. Empty entries are skipped, which makes a trailing
/// `|` harmless and an empty spec an empty list.
pub fn parse_action_spec(spec: &str) -> Vec<Action> {
    spec.split('|')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(Action::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_chord_from_str_keeps_the_accelerator() {
        let c: KeyChord = "Ctrl+S".into();
        assert_eq!(c.0.as_ref(), "Ctrl+S");
    }

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

    /// The web's spellings read the same as Lumen's: parameters and case
    /// do not count, `text` and `url` are the short names, and an app's own
    /// kind is stored lowercased.
    #[test]
    fn mime_kind_from_str_reads_the_web_spellings() {
        assert_eq!(MimeKind::from("text"), MimeKind::TextPlain);
        assert_eq!(MimeKind::from("URL"), MimeKind::TextUriList);
        assert_eq!(
            MimeKind::from("Text/Plain; charset=utf-8"),
            MimeKind::TextPlain
        );
        assert_eq!(
            MimeKind::from("Application/X-Asset"),
            MimeKind::Custom(Arc::from("application/x-asset"))
        );
        assert_eq!(MimeKind::TextPlain.essence(), "text/plain");
        assert_eq!(
            MimeKind::from("application/x-a;v=1").essence(),
            "application/x-a"
        );
    }

    /// An accept entry names one type or every type under a major type.
    #[test]
    fn mime_kind_matches_an_accept_entry() {
        assert!(MimeKind::ImagePng.matches("image/*"));
        assert!(MimeKind::ImagePng.matches("IMAGE/PNG"));
        assert!(!MimeKind::ImagePng.matches("image/jpeg"));
        assert!(!MimeKind::TextPlain.matches("image/*"));
        assert!(MimeKind::TextPlain.matches("text/plain;charset=utf-8"));
        assert!(MimeKind::from("application/x-asset").matches("application/*"));
        assert!(MimeKind::from("application/x-asset").matches("*/*"));
    }

    /// Setting a kind the payload carries replaces it in place.
    #[test]
    fn mime_payload_set_replaces_a_kind() {
        let mut p = MimePayload::from("a");
        p.set(MimeKind::from("application/x-b"), b"b".to_vec());
        p.set(MimeKind::TextPlain, b"c".to_vec());
        assert_eq!(p.kinds.len(), 2);
        assert_eq!(p.text().as_deref(), Some("c"));
        assert_eq!(p.kinds[1].0.essence(), "application/x-b");
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
    fn action_spec_reads_ids_labels_and_separators() {
        let items = parse_action_spec("open:Open|-|quit: Quit ");
        assert_eq!(items.len(), 3);
        assert_eq!(&*items[0].id, "open");
        assert_eq!(&*items[0].label, "Open");
        assert_eq!(&*items[1].id, SEPARATOR_ID);
        assert_eq!(&*items[2].id, "quit");
        assert_eq!(&*items[2].label, "Quit");
    }

    #[test]
    fn action_spec_tolerates_blanks_and_bare_entries() {
        assert!(parse_action_spec("").is_empty());
        assert!(parse_action_spec("  |  ").is_empty());
        let items = parse_action_spec("Quit|");
        assert_eq!(items.len(), 1);
        assert_eq!(&*items[0].id, "Quit");
        assert_eq!(&*items[0].label, "Quit");
    }

    #[test]
    fn action_entry_keeps_a_windows_path_label_intact() {
        // Only the first `:` splits, so a drive letter survives.
        let a = Action::from(r"icon:C:\icons\app.png");
        assert_eq!(&*a.id, "icon");
        assert_eq!(&*a.label, r"C:\icons\app.png");
    }

    #[test]
    fn empty_payload_has_no_text() {
        let p = MimePayload::new();
        assert!(p.is_empty());
        assert_eq!(p.text(), None);
        assert!(p.uris().is_empty());
    }
}
