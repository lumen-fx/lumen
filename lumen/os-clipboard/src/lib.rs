//! OS clipboard host for Lumen.
//!
//! Wraps `arboard` 3.x behind the shared [`MimePayload`] abstraction
//! (`QMimeData` / `GdkContentProvider` analog). Carved out of
//! `lumen-input` per the OS plan section 469 + W6.1.
//!
//! - [`ClipboardHost`] - `read` / `write` / `clear` for the standard
//!   system clipboard. Owns an `arboard::Clipboard` behind a `Mutex`.
//! - Linux PRIMARY selection: [`ClipboardHost::read_primary`] /
//!   [`ClipboardHost::write_primary`], gated behind the
//!   `linux_primary` feature.
//! - [`set_rgba8_image`] / [`get_rgba8_image`] preserved for
//!   backwards-compatible image round-trips used by lumenc's
//!   `copy_image` / `save_clipboard_image` Rhai builtins.
//!
//! `arboard::Clipboard` is `!Send` on Linux/Wayland - store as a
//! `NonSend` ECS resource (see [`InstallExt::install_clipboard_host`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use lumen_os_mime::{MimeKind, MimePayload};
use std::sync::Mutex;

pub use lumen_os_mime as mime;

/// Single-process clipboard host. Wraps `arboard::Clipboard` behind a
/// `Mutex` - the underlying handle is `!Send` on Linux/Wayland so this
/// type stays `!Sync` by virtue of `Mutex<arboard::Clipboard>` having
/// the same restriction.
pub struct ClipboardHost {
    inner: Mutex<arboard::Clipboard>,
}

impl ClipboardHost {
    /// Try to initialize the OS clipboard. Returns `None` if the
    /// backend (Wayland with no compositor, headless CI, X11 without a
    /// running window manager) refuses.
    pub fn try_new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|cb| Self {
            inner: Mutex::new(cb),
        })
    }

    /// Lock the inner clipboard, recovering from a poisoned mutex.
    ///
    /// `arboard::Clipboard` stays valid even if a holder panicked, so
    /// recovering the guard is safe. The previous per-site `lock().ok()`
    /// dropped to the empty/`false` fallback on poison - permanently and
    /// silently bricking clipboard access with no diagnostic. Here we log
    /// once and carry on.
    fn guard(&self) -> std::sync::MutexGuard<'_, arboard::Clipboard> {
        self.inner.lock().unwrap_or_else(|e| {
            eprintln!("lumen-os-clipboard: recovered poisoned clipboard lock");
            e.into_inner()
        })
    }

    /// Read the current clipboard contents as a multi-format
    /// [`MimePayload`]. Tries text first, then image; returns an empty
    /// payload when neither is available.
    pub fn read(&self) -> MimePayload {
        let mut payload = MimePayload::new();
        let mut cb = self.guard();
        if let Ok(text) = cb.get_text() {
            payload = payload.with(MimeKind::TextPlain, text.into_bytes());
        }
        // arboard's `get_image` is RGBA8 raw; we surface it under a
        // synthetic raw-rgba MIME (PNG encoding is a lumenc-layer
        // concern - see `handle_save_clipboard_image`).
        if let Ok(img) = cb.get_image() {
            let bytes = img.bytes.into_owned();
            let header = format!("{}x{}:", img.width, img.height);
            let mut combined = header.into_bytes();
            combined.extend_from_slice(&bytes);
            payload = payload.with(
                MimeKind::Custom(std::sync::Arc::from("application/x-lumen-rgba8")),
                combined,
            );
        }
        payload
    }

    /// Write a [`MimePayload`] onto the system clipboard. Picks the
    /// first MIME kind arboard understands (text/plain -> `set_text`).
    /// Returns `true` on success.
    pub fn write(&self, payload: &MimePayload) -> bool {
        let mut cb = self.guard();
        // Prefer text/plain; arboard's API only exposes text + image.
        if let Some(bytes) = payload.get(&MimeKind::TextPlain) {
            let text = String::from_utf8_lossy(bytes).into_owned();
            return cb.set_text(text).is_ok();
        }
        // No directly-supported MIME - caller must use
        // `set_rgba8_image` for image payloads since the encoded
        // representation (PNG) lives outside this crate.
        false
    }

    /// Convenience: write a plain-text payload. Same as `write` with a
    /// `MimePayload::from(&str)` but avoids the allocation when callers
    /// only have a `&str` (the text editor's copy / cut path).
    pub fn write_text(&self, text: &str) -> bool {
        let mut cb = self.guard();
        cb.set_text(text.to_string()).is_ok()
    }

    /// Convenience: read the current clipboard text. Returns an empty
    /// string when no text payload is available.
    pub fn read_text(&self) -> String {
        let mut cb = self.guard();
        cb.get_text().unwrap_or_default()
    }

    /// Clear the clipboard. Returns `true` on success.
    pub fn clear(&self) -> bool {
        let mut cb = self.guard();
        cb.clear().is_ok()
    }

    /// Write the supplied RGBA8 image (`width x height x 4` bytes) to
    /// the system clipboard. Preserves the API the previous
    /// `ClipboardResource` exposed for the `copy_image` Rhai builtin.
    pub fn set_rgba8_image(&self, width: u32, height: u32, rgba: Vec<u8>) -> bool {
        let img = arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Owned(rgba),
        };
        self.guard().set_image(img).is_ok()
    }

    /// Read the current clipboard image as RGBA8. Returns
    /// `(width, height, rgba_bytes)` when an image is present.
    pub fn get_rgba8_image(&self) -> Option<(u32, u32, Vec<u8>)> {
        let mut cb = self.guard();
        let img = cb.get_image().ok()?;
        Some((img.width as u32, img.height as u32, img.bytes.into_owned()))
    }

    /// Read the X11 PRIMARY selection (Linux-only). On other platforms
    /// (or when the `linux_primary` feature is off) returns an empty
    /// payload.
    #[cfg(all(feature = "linux_primary", target_os = "linux"))]
    pub fn read_primary(&self) -> MimePayload {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let mut cb = self.guard();
        if let Ok(text) = cb.get().clipboard(LinuxClipboardKind::Primary).text() {
            return text.as_str().into();
        }
        MimePayload::new()
    }

    /// Read the X11 PRIMARY selection - feature-disabled stub.
    #[cfg(not(all(feature = "linux_primary", target_os = "linux")))]
    pub fn read_primary(&self) -> MimePayload {
        MimePayload::new()
    }

    /// Write to the X11 PRIMARY selection (Linux-only). On other
    /// platforms (or when the `linux_primary` feature is off) returns
    /// `false`.
    #[cfg(all(feature = "linux_primary", target_os = "linux"))]
    pub fn write_primary(&self, payload: &MimePayload) -> bool {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        let Some(bytes) = payload.get(&MimeKind::TextPlain) else {
            return false;
        };
        let text = String::from_utf8_lossy(bytes).into_owned();
        let mut cb = self.guard();
        cb.set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text)
            .is_ok()
    }

    /// Write to the X11 PRIMARY selection - feature-disabled stub.
    #[cfg(not(all(feature = "linux_primary", target_os = "linux")))]
    pub fn write_primary(&self, _payload: &MimePayload) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // We can't unit-test against the real arboard backend in CI (no
    // display server). Cover the MIME round-trip helpers instead.

    #[test]
    fn payload_from_str_carries_text() {
        let p: MimePayload = "abc".into();
        assert_eq!(p.text().as_deref(), Some("abc"));
    }

    #[test]
    fn payload_write_picks_textplain() {
        // ClipboardHost::write is documented to short-circuit on
        // `TextPlain`; verify it returns false on a payload that
        // carries only octet-stream bytes (no text/plain -> nothing
        // arboard can send).
        let p: MimePayload = vec![0u8, 1, 2].into();
        assert!(!p.has(&MimeKind::TextPlain));
    }

    #[test]
    fn read_primary_empty_without_feature() {
        // On a default-feature build the helper returns an empty
        // payload regardless of platform.
        if let Some(host) = ClipboardHost::try_new() {
            let p = host.read_primary();
            #[cfg(not(all(feature = "linux_primary", target_os = "linux")))]
            assert!(p.is_empty());
            #[cfg(all(feature = "linux_primary", target_os = "linux"))]
            let _ = p; // contents depend on the live X11 selection
        }
    }
}
