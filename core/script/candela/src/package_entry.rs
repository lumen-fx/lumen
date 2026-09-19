//! The file a candela package is entered through.
//!
//! A dependency package resolves imports from its entry file: `import "shapes";`
//! reads the entry, and `import "shapes/circle";` reads `circle.cdl` from the
//! entry's directory. `candela::Engine::with_import_root` reads the entry out of
//! the package's `candela.toml` itself, but the ahead-of-time build has no
//! engine and assembles its own [`candela::ImportResolver`], so it has to read
//! the same key. candela does not export its manifest reader, so this reads the
//! one key an import needs and leaves the rest of the manifest to candela.

use std::path::Path;

use candela::compiler::imports::DEFAULT_PACKAGE_ENTRY;

/// The entry of the package rooted at `dir`, relative to `dir`.
///
/// A package with no manifest, an unreadable one, or one that names no entry
/// is entered through the default, which is what candela does as well.
pub(crate) fn of(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("candela.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|doc| {
            doc.get("package")?
                .as_table()?
                .get("entry")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| DEFAULT_PACKAGE_ENTRY.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::DEFAULT_PACKAGE_ENTRY;
    use super::of;

    /// A fresh, empty package directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-script-candela-entry-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn manifest(name: &str, body: &str) -> PathBuf {
        let dir = scratch(name);
        fs::write(dir.join("candela.toml"), body).expect("write manifest");
        dir
    }

    #[test]
    fn a_package_without_a_manifest_is_entered_through_the_default() {
        assert_eq!(of(&scratch("none")), DEFAULT_PACKAGE_ENTRY);
    }

    #[test]
    fn a_manifest_that_names_an_entry_is_entered_through_it() {
        let dir = manifest(
            "named",
            "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\nentry = \"shapes.cdl\"\n",
        );
        assert_eq!(of(&dir), "shapes.cdl");
    }

    #[test]
    fn a_manifest_that_names_no_entry_is_entered_through_the_default() {
        let dir = manifest(
            "unnamed",
            "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\n",
        );
        assert_eq!(of(&dir), DEFAULT_PACKAGE_ENTRY);
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_entered_through_the_default() {
        let dir = manifest("broken", "[package");
        assert_eq!(of(&dir), DEFAULT_PACKAGE_ENTRY);
    }
}
