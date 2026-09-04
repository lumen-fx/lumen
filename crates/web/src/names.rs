//! Naming a file after what is in it.
//!
//! A static host is told nothing about how long to keep a file, so it keeps
//! it, and a redeploy that writes the same name is served from the cache the
//! visitor already has. A name carrying the hash of the file's own bytes
//! changes whenever the bytes change, so a redeploy is a new URL and there is
//! nothing to invalidate.
//!
//! Nothing here decides which files are named this way. Whoever holds a
//! file's bytes names it and puts the name in the [`SiteSpec`], which is how
//! every name already reaches the documents and the manifest.

use lumen_html::contract::Dir;

use crate::site::manifest;
use crate::spec::SiteSpec;

/// FNV-1a, 64 bits.
///
/// A cache-busting name needs distinctness and nothing else, so this is the
/// same content key Lumen already uses for a fragment.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `path` with the hash of `bytes` in it: `styles.css` becomes
/// `styles.<hash>.css`.
///
/// The hash goes before the final extension of the last segment, and any
/// directory part is left alone. The extension stays last because that is
/// what a server reads the content type off, and a name with no extension
/// takes the hash at the end.
pub fn content_name(path: &str, bytes: &[u8]) -> String {
    let hash = format!("{:016x}", fnv1a64(bytes));
    let (dir, name) = match path.rfind('/') {
        Some(at) => path.split_at(at + 1),
        None => ("", path),
    };
    let hashed = match name.rfind('.') {
        Some(at) if at > 0 => format!("{}.{hash}{}", &name[..at], &name[at..]),
        _ => format!("{name}.{hash}"),
    };
    format!("{dir}{hashed}")
}

/// A marker for the one manifest a site writes, so the document's fetch of it
/// changes whenever its contents do.
///
/// The manifest keeps its name: it is the one file that names all the others,
/// and a site rendered per request has no document to read a hashed name out
/// of. The documents bust it instead, with this on the URL they fetch.
///
/// A site emitted in several locales writes one manifest and a tree of
/// documents per locale, and the locale fields are the only ones those trees
/// disagree about. They are normalized away so every tree marks the file it
/// shares with the same value.
pub fn build_id(spec: &SiteSpec) -> String {
    let mut manifest = manifest(spec);
    manifest.locale = String::new();
    manifest.dir = Dir::default();
    manifest.locales = Vec::new();
    let json = serde_json::to_string(&manifest).unwrap_or_default();
    format!("{:016x}", fnv1a64(json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{LocaleSpec, PageSpec};
    use lumen_ir::layout_ir::LayoutIR;

    #[test]
    fn the_hash_goes_before_the_extension_and_the_directory_is_left_alone() {
        assert_eq!(
            content_name("styles.css", b"body{}"),
            format!("styles.{:016x}.css", fnv1a64(b"body{}"))
        );
        assert_eq!(
            content_name("assets/img/logo.png", b"png"),
            format!("assets/img/logo.{:016x}.png", fnv1a64(b"png"))
        );
        // A name with no extension of its own takes the hash at the end.
        assert_eq!(
            content_name("assets/LICENSE", b"text"),
            format!("assets/LICENSE.{:016x}", fnv1a64(b"text"))
        );
    }

    #[test]
    fn different_bytes_are_a_different_name_and_the_same_bytes_are_the_same_one() {
        assert_ne!(
            content_name("styles.css", b"a{}"),
            content_name("styles.css", b"b{}")
        );
        assert_eq!(
            content_name("styles.css", b"a{}"),
            content_name("styles.css", b"a{}")
        );
    }

    #[test]
    fn every_locale_tree_marks_the_manifest_it_shares_the_same_way() {
        let tree = |locale: &str, alternates: Vec<String>| SiteSpec {
            pages: vec![PageSpec::new("index", LayoutIR::default())],
            locale: LocaleSpec {
                alternates,
                default_locale: "en-US".to_string(),
                ..LocaleSpec::new(locale)
            },
            ..SiteSpec::default()
        };
        let root = tree("en-US", vec!["ar-EG".to_string()]);
        let other = tree("ar-EG", vec!["en-US".to_string()]);
        assert_eq!(build_id(&root), build_id(&other));

        // What the manifest names is what the marker follows.
        let mut changed = tree("en-US", vec!["ar-EG".to_string()]);
        changed.web.css = "styles.0123456789abcdef.css".to_string();
        assert_ne!(build_id(&root), build_id(&changed));
    }
}
