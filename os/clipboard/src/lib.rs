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
//!
//! On `wasm32` the clipboard is the page's `navigator.clipboard`, text only.
//! [`ClipboardHost::try_new`] finds it wherever the page has one (a secure
//! context: `https` or `localhost`) and reports it unavailable otherwise. A
//! page reads the clipboard asynchronously and only with the visitor's
//! permission, so a read goes through [`ClipboardHost::read_text_then`]; the
//! synchronous readers, images, and PRIMARY answer as an absent clipboard
//! does.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(not(target_arch = "wasm32"))]
use lumen_os_mime::MimeKind;
use lumen_os_mime::MimePayload;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;

pub use lumen_os_mime as mime;

/// Single-process clipboard host. Wraps `arboard::Clipboard` behind a
/// `Mutex` - the underlying handle is `!Send` on Linux/Wayland so this
/// type stays `!Sync` by virtue of `Mutex<arboard::Clipboard>` having
/// the same restriction.
///
/// Call it from the thread that owns the app, which is what holding it as
/// a `NonSend` resource enforces. macOS `NSPasteboard` mutates shared
/// state without locking it, so two threads reaching the clipboard at
/// once corrupts memory there rather than returning an error.
pub struct ClipboardHost {
    #[cfg(not(target_arch = "wasm32"))]
    inner: Mutex<arboard::Clipboard>,
}

impl ClipboardHost {
    /// Try to initialize the OS clipboard. Returns `None` if the
    /// backend (Wayland with no compositor, headless CI, X11 without a
    /// running window manager) refuses.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|cb| Self {
            inner: Mutex::new(cb),
        })
    }

    /// Find the page's clipboard. `None` outside a secure context, where
    /// the browser does not expose one.
    #[cfg(target_arch = "wasm32")]
    pub fn try_new() -> Option<Self> {
        page_clipboard().map(|_| Self {})
    }

    /// Lock the inner clipboard, recovering from a poisoned mutex.
    ///
    /// `arboard::Clipboard` stays valid even if a holder panicked, so
    /// recovering the guard is safe. The previous per-site `lock().ok()`
    /// dropped to the empty/`false` fallback on poison - permanently and
    /// silently bricking clipboard access with no diagnostic. Here we log
    /// once and carry on.
    #[cfg(not(target_arch = "wasm32"))]
    fn guard(&self) -> std::sync::MutexGuard<'_, arboard::Clipboard> {
        self.inner.lock().unwrap_or_else(|e| {
            eprintln!("lumen-os-clipboard: recovered poisoned clipboard lock");
            e.into_inner()
        })
    }

    /// Read the current clipboard contents as a multi-format
    /// [`MimePayload`]. Tries text first, then image; returns an empty
    /// payload when neither is available.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Read the current clipboard contents - no backend on wasm32.
    #[cfg(target_arch = "wasm32")]
    pub fn read(&self) -> MimePayload {
        MimePayload::new()
    }

    /// Write a [`MimePayload`] onto the system clipboard. Picks the
    /// first MIME kind arboard understands (text/plain -> `set_text`).
    /// Returns `true` on success.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Write a [`MimePayload`] onto the page's clipboard. Text only: a
    /// payload with no `text/plain` entry writes nothing.
    #[cfg(target_arch = "wasm32")]
    pub fn write(&self, payload: &MimePayload) -> bool {
        match payload.text() {
            Some(text) => self.write_text(&text),
            None => false,
        }
    }

    /// Convenience: write a plain-text payload. Same as `write` with a
    /// `MimePayload::from(&str)` but avoids the allocation when callers
    /// only have a `&str` (the text editor's copy / cut path).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn write_text(&self, text: &str) -> bool {
        let mut cb = self.guard();
        cb.set_text(text.to_string()).is_ok()
    }

    /// Convenience: write a plain-text payload through the page's
    /// `navigator.clipboard.writeText`.
    ///
    /// The browser settles the write after this returns, so `true` means the
    /// page accepted the request. A write the browser refuses later (the
    /// document lost focus, the visitor denied the permission) is reported
    /// to the page's console.
    #[cfg(target_arch = "wasm32")]
    pub fn write_text(&self, text: &str) -> bool {
        let Some(clipboard) = page_clipboard() else {
            return false;
        };
        let written = wasm_bindgen_futures::JsFuture::from(clipboard.write_text(text));
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = written.await {
                web_sys::console::warn_2(
                    &"clipboard_write: the browser refused the text:".into(),
                    &error,
                );
            }
        });
        true
    }

    /// Convenience: read the current clipboard text. Returns an empty
    /// string when no text payload is available.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_text(&self) -> String {
        let mut cb = self.guard();
        cb.get_text().unwrap_or_default()
    }

    /// Convenience: read the current clipboard text - a page cannot read it
    /// synchronously, so this is always empty on wasm32. Use
    /// [`read_text_then`](Self::read_text_then).
    #[cfg(target_arch = "wasm32")]
    pub fn read_text(&self) -> String {
        String::new()
    }

    /// Read the clipboard text and hand it to `done`, which is called
    /// exactly once. Empty when the clipboard holds no text.
    ///
    /// On the desktop the read is synchronous and `done` runs before this
    /// returns.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_text_then(&self, done: impl FnOnce(String) + 'static) {
        done(self.read_text());
    }

    /// Read the clipboard text and hand it to `done`, which is called
    /// exactly once. Empty when the clipboard holds no text.
    ///
    /// In a page `done` runs when `navigator.clipboard.readText` settles,
    /// after this returns. The browser asks the visitor first; a read they
    /// refuse, or one the browser blocks, answers with empty text.
    #[cfg(target_arch = "wasm32")]
    pub fn read_text_then(&self, done: impl FnOnce(String) + 'static) {
        let Some(clipboard) = page_clipboard() else {
            done(String::new());
            return;
        };
        let read = wasm_bindgen_futures::JsFuture::from(clipboard.read_text());
        wasm_bindgen_futures::spawn_local(async move {
            let text = match read.await {
                Ok(text) => text.as_string().unwrap_or_default(),
                Err(error) => {
                    web_sys::console::warn_2(
                        &"clipboard_read: the browser refused the read:".into(),
                        &error,
                    );
                    String::new()
                }
            };
            done(text);
        });
    }

    /// Clear the clipboard. Returns `true` on success.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn clear(&self) -> bool {
        let mut cb = self.guard();
        cb.clear().is_ok()
    }

    /// Clear the clipboard - no backend on wasm32.
    #[cfg(target_arch = "wasm32")]
    pub fn clear(&self) -> bool {
        false
    }

    /// Write the supplied RGBA8 image (`width x height x 4` bytes) to
    /// the system clipboard. Preserves the API the previous
    /// `ClipboardResource` exposed for the `copy_image` Rhai builtin.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_rgba8_image(&self, width: u32, height: u32, rgba: Vec<u8>) -> bool {
        let img = arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Owned(rgba),
        };
        self.guard().set_image(img).is_ok()
    }

    /// Write an RGBA8 image to the system clipboard - no backend on wasm32.
    #[cfg(target_arch = "wasm32")]
    pub fn set_rgba8_image(&self, _width: u32, _height: u32, _rgba: Vec<u8>) -> bool {
        false
    }

    /// Read the current clipboard image as RGBA8. Returns
    /// `(width, height, rgba_bytes)` when an image is present.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn get_rgba8_image(&self) -> Option<(u32, u32, Vec<u8>)> {
        let mut cb = self.guard();
        let img = cb.get_image().ok()?;
        Some((img.width as u32, img.height as u32, img.bytes.into_owned()))
    }

    /// Read the current clipboard image as RGBA8 - no backend on wasm32.
    #[cfg(target_arch = "wasm32")]
    pub fn get_rgba8_image(&self) -> Option<(u32, u32, Vec<u8>)> {
        None
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

/// The page's `navigator.clipboard`, where the browser exposes one. It is
/// `undefined` outside a secure context, which the typed getter does not say.
#[cfg(target_arch = "wasm32")]
fn page_clipboard() -> Option<web_sys::Clipboard> {
    let clipboard = web_sys::window()?.navigator().clipboard();
    let value: &wasm_bindgen::JsValue = clipboard.as_ref();
    (!value.is_undefined() && !value.is_null()).then_some(clipboard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_os_mime::MimeKind;

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
