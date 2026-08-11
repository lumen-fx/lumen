//! Embedded-font registration for [`cosmic_text::FontSystem`].
//!
//! Mirrors `QFontDatabase::addApplicationFont` (Qt) and
//! `pango_font_map_add_font_file` / `gdk_display_load_font` (GTK +
//! Pango). The system shaper already discovers system fonts at
//! construction; this module lets the app push extra fonts at runtime,
//! typically the bytes of a `.ttf` / `.otf` / `.woff2` shipped
//! inside a `.lpak` bundle.
//!
//! The split lives in its own module so the asset loader can call into
//! it without growing the `lib.rs` shape registration surface.

use cosmic_text::FontSystem;

/// Register a single font from raw bytes against an existing
/// `FontSystem`. Mirrors `QFontDatabase::addApplicationFont` -
/// returns `true` when at least one face was loaded.
///
/// The bytes can come from any source (`include_bytes!`, a `.lpak`
/// bundle read, a manual disk fetch). cosmic-text's `fontdb` clones
/// the data internally, so the caller's buffer can be dropped
/// immediately after the call returns.
pub fn register_bytes(font_system: &mut FontSystem, _name: &str, bytes: Vec<u8>) -> bool {
    // `_name` is forwarded to match the spec - fontdb keys faces by
    // their parsed family / style metadata, not by an arbitrary
    // caller-supplied name, so we don't actually store it. Keeping the
    // parameter in the signature future-proofs against an alternative
    // backend that needs an explicit handle.
    let db = font_system.db_mut();
    let before = db.faces().count();
    db.load_font_data(bytes);
    let after = db.faces().count();
    after > before
}

/// Convenience: register every `.ttf` / `.otf` / `.woff2` entry in a
/// generic iterator. Returns the number of faces successfully loaded.
///
/// Used by the asset-server integration to drain a `LumenBundle`'s
/// font entries into the system shaper without leaking the bundle
/// type into this crate.
pub fn register_iter<'a, I>(font_system: &mut FontSystem, fonts: I) -> usize
where
    I: IntoIterator<Item = (&'a str, Vec<u8>)>,
{
    let mut loaded = 0;
    for (name, bytes) in fonts {
        if !is_font_extension(name) {
            continue;
        }
        if register_bytes(font_system, name, bytes) {
            loaded += 1;
        }
    }
    loaded
}

/// Whether `name` ends in a recognised font extension. Case-insensitive.
pub fn is_font_extension(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".ttf")
        || lower.ends_with(".otf")
        || lower.ends_with(".woff")
        || lower.ends_with(".woff2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_font_extension_matches_common_suffixes() {
        assert!(is_font_extension("Inter.ttf"));
        assert!(is_font_extension("Inter.TTF"));
        assert!(is_font_extension("path/to/Roboto.otf"));
        assert!(is_font_extension("a.woff"));
        assert!(is_font_extension("a.woff2"));
        assert!(!is_font_extension("icon.png"));
        assert!(!is_font_extension("main.rhai"));
    }

    #[test]
    fn register_bytes_with_empty_buffer_is_noop() {
        // An empty buffer cannot parse to a face; we expect `false`
        // (no new faces registered) and no panic.
        let mut fs = FontSystem::new();
        let before = fs.db_mut().faces().count();
        let added = register_bytes(&mut fs, "empty", Vec::new());
        assert!(!added);
        assert_eq!(fs.db_mut().faces().count(), before);
    }
}
