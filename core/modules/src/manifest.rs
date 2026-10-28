//! A module's own description of itself: `lumen-module.toml`, at the root of
//! every module.
//!
//! Declaring a module in `[dependencies]` is the one thing an app opts into;
//! everything the module brings comes with it. The markup tags it answers
//! for are part of that, so the module lists them, not the app:
//!
//! ```toml
//! [module]
//! tags = ["canvas"]
//! ```
//!
//! A module that brings no markup writes `tags = []`. The file is data, read
//! without opening the module's library, so every compile reads it the same
//! way whether or not the library is built, loadable here, or for this
//! platform at all.
//!
//! The root is found the same way for every source: the directory a `path`
//! source names (or, when the path names a library file, the directory that
//! file is in), the package `lpm` unpacked for a `version` source, and for a
//! `bundled` one `modules/<name>/` under the directories a toolchain ships its
//! files in, then the crate under `std/` of the checkout this crate was built
//! in. A declared module whose descriptor cannot be found is an error naming
//! the module.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::language::{BUNDLED_MODULE_DIR, checkout_std_crates, search_roots};
use crate::{DepCfg, DependenciesCfg, ModuleSource, RESERVED_TAGS};

/// The descriptor's file name, at the root of a module.
pub const MODULE_MANIFEST: &str = "lumen-module.toml";

/// What a module's `lumen-module.toml` declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleManifest {
    /// The markup tags the module answers for, deduplicated, in the order
    /// written.
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    module: Section,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    tags: Vec<String>,
}

/// Read the descriptor `text` of the module `module`.
///
/// # Errors
///
/// The text is not a descriptor, or declares a tag this format refuses: an
/// empty name, one outside lowercase letters, digits and dashes, or one the
/// language owns. Every message names the module.
pub fn parse_manifest(module: &str, text: &str) -> Result<ModuleManifest, String> {
    let descriptor: Descriptor =
        toml::from_str(text).map_err(|e| format!("module '{module}': {MODULE_MANIFEST}: {e}"))?;
    let mut tags: Vec<String> = Vec::new();
    for tag in descriptor.module.tags {
        validate_tag(module, &tag)?;
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    Ok(ModuleManifest { tags })
}

/// Read the descriptor of the module `module`, rooted at `root`.
///
/// # Errors
///
/// The file is missing or unreadable, or [`parse_manifest`] refuses it.
pub fn read_manifest(module: &str, root: &Path) -> Result<ModuleManifest, String> {
    let path = root.join(MODULE_MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "module '{module}': cannot read {}: {e}; every module ships a {MODULE_MANIFEST} at \
             its root declaring the markup tags it brings",
            path.display()
        )
    })?;
    parse_manifest(module, &text)
}

/// Check one tag name a module declares, or say why it is refused.
pub(crate) fn validate_tag(module: &str, tag: &str) -> Result<(), String> {
    if tag.is_empty() {
        return Err(format!("module '{module}': a tag name must not be empty"));
    }
    if !tag
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "module '{module}': tag '{tag}' must be lowercase letters, digits, and dashes"
        ));
    }
    if RESERVED_TAGS.contains(&tag) {
        return Err(format!(
            "module '{module}': tag '{tag}' is a built-in tag and cannot be declared by a module"
        ));
    }
    Ok(())
}

/// The root directory of the module `dep` declares, for the app at `app_dir`.
///
/// `versions` holds the root of each `version` source the resolver unpacked,
/// keyed by declared name, and `lib_dir` is a `--lib-dir` a build was given,
/// searched for bundled modules before the toolchain's own directories.
///
/// # Errors
///
/// A `bundled` module no searched directory holds, or a `version` source with
/// no resolved root. The message names the module.
pub fn module_root(
    app_dir: &Path,
    dep: &DepCfg,
    versions: &BTreeMap<String, PathBuf>,
    lib_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    match &dep.source {
        ModuleSource::Path(path) => {
            let named = app_dir.join(path);
            if named.is_dir() {
                return Ok(named);
            }
            // A library file, written with its extension or as the stem the
            // loader spells per platform: its module lives beside it.
            Ok(named
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| app_dir.to_path_buf()))
        }
        ModuleSource::Version(req) => versions.get(&dep.name).cloned().ok_or_else(|| {
            format!(
                "module '{}': `version = \"{req}\"` is not resolved here, so its \
                 {MODULE_MANIFEST} cannot be read",
                dep.name
            )
        }),
        ModuleSource::Bundled => bundled_module(&dep.name, lib_dir).ok_or_else(|| {
            let searched: Vec<String> = search_roots(lib_dir)
                .iter()
                .map(|root| root.join(BUNDLED_MODULE_DIR).join(&dep.name))
                .map(|dir| dir.display().to_string())
                .collect();
            format!(
                "module '{}': no bundled module of that name is in reach, so its \
                 {MODULE_MANIFEST} cannot be read (looked in {})",
                dep.name,
                if searched.is_empty() {
                    "no toolchain directory".to_string()
                } else {
                    searched.join(", ")
                }
            )
        }),
    }
}

/// The toolchain's own root of the bundled module `name`: `modules/<name>/`
/// under the `--lib-dir`, the directory holding the running executable, the
/// shared engine's directory, then `LUMEN_LIB_DIR`, and last the crate under
/// `std/` of the source tree this crate was built from whose package is
/// `name`, when that tree is still on disk.
pub fn bundled_module(name: &str, lib_dir: Option<&Path>) -> Option<PathBuf> {
    search_roots(lib_dir)
        .into_iter()
        .map(|root| root.join(BUNDLED_MODULE_DIR).join(name))
        .find(|root| root.is_dir())
        .or_else(|| {
            checkout_std_crates()
                .into_iter()
                .find_map(|(package, root)| (package == name).then_some(root))
        })
}

/// `deps` without the `version` sources `versions` holds no root for.
///
/// For a caller that resolves no registry package itself and is handed what
/// was resolved, such as the runtime. A `version` entry it was not handed a
/// root for is a script library the resolver sorted elsewhere, or a module
/// nobody resolved, which the loader reports when it fails to open it; either
/// way there is no descriptor here to read.
pub fn without_unresolved(
    deps: &DependenciesCfg,
    versions: &BTreeMap<String, PathBuf>,
) -> DependenciesCfg {
    DependenciesCfg(
        deps.0
            .iter()
            .filter(|dep| {
                !matches!(dep.source, ModuleSource::Version(_)) || versions.contains_key(&dep.name)
            })
            .cloned()
            .collect(),
    )
}

/// Every markup tag the modules in `deps` declare, read from each one's
/// `lumen-module.toml`, in table order and deduplicated. Two modules may name
/// the same tag; the parser only needs to know the tag exists.
///
/// # Errors
///
/// A module's root or descriptor cannot be found, or its descriptor is
/// refused (see [`module_root`], [`read_manifest`]).
pub fn module_tags(
    app_dir: &Path,
    deps: &DependenciesCfg,
    versions: &BTreeMap<String, PathBuf>,
    lib_dir: Option<&Path>,
) -> Result<Vec<String>, String> {
    let mut tags: Vec<String> = Vec::new();
    for dep in &deps.0 {
        let root = module_root(app_dir, dep, versions, lib_dir)?;
        for tag in read_manifest(&dep.name, &root)?.tags {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-module-manifest-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn deps(doc: &str) -> DependenciesCfg {
        toml::from_str(doc).expect("the test table parses")
    }

    #[test]
    fn tags_parse_and_deduplicate_in_order() {
        let manifest = parse_manifest(
            "m",
            "[module]\ntags = [\"gauge\", \"spark-line\", \"gauge\"]\n",
        )
        .unwrap();
        assert_eq!(manifest.tags, ["gauge", "spark-line"]);
    }

    #[test]
    fn an_empty_list_declares_nothing() {
        let manifest = parse_manifest("m", "[module]\ntags = []\n").unwrap();
        assert!(manifest.tags.is_empty());
    }

    #[test]
    fn the_tags_key_is_required_and_unknown_keys_are_refused() {
        let err = parse_manifest("m", "[module]\n").unwrap_err();
        assert!(err.contains("module 'm'"), "{err}");
        assert!(err.contains("tags"), "{err}");
        let err = parse_manifest("m", "[module]\ntags = []\nname = \"x\"\n").unwrap_err();
        assert!(err.contains("unknown field `name`"), "{err}");
    }

    #[test]
    fn a_tag_outside_the_allowed_characters_is_refused() {
        for bad in ["Gauge", "spark_line", "spark line", ""] {
            let err = parse_manifest("m", &format!("[module]\ntags = [\"{bad}\"]\n")).unwrap_err();
            assert!(
                err.contains("lowercase letters") || err.contains("must not be empty"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_built_in_tag_cannot_be_claimed() {
        let err = parse_manifest("m", "[module]\ntags = [\"button\"]\n").unwrap_err();
        assert!(err.contains("built-in tag"), "{err}");
    }

    #[test]
    fn a_path_source_naming_a_directory_is_the_root() {
        let app = scratch("path-dir");
        std::fs::create_dir_all(app.join("widgets")).unwrap();
        std::fs::write(
            app.join("widgets").join(MODULE_MANIFEST),
            "[module]\ntags = [\"gauge\"]\n",
        )
        .unwrap();
        let deps = deps("widgets = { path = \"widgets\" }\n");
        assert_eq!(
            module_tags(&app, &deps, &BTreeMap::new(), None).unwrap(),
            ["gauge"]
        );
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn a_path_source_naming_a_library_reads_the_descriptor_beside_it() {
        let app = scratch("path-lib");
        std::fs::create_dir_all(app.join("modules")).unwrap();
        std::fs::write(
            app.join("modules").join(MODULE_MANIFEST),
            "[module]\ntags = [\"shape-view\"]\n",
        )
        .unwrap();
        // With the extension, and as the stem the loader spells per platform.
        for path in ["modules/libshape.so", "modules/shape"] {
            let deps = deps(&format!("shape = {{ path = \"{path}\" }}\n"));
            assert_eq!(
                module_tags(&app, &deps, &BTreeMap::new(), None).unwrap(),
                ["shape-view"],
                "{path}"
            );
        }
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn a_module_without_a_descriptor_is_an_error_naming_it() {
        let app = scratch("missing");
        std::fs::create_dir_all(app.join("widgets")).unwrap();
        let deps = deps("widgets = { path = \"widgets\" }\n");
        let err = module_tags(&app, &deps, &BTreeMap::new(), None).unwrap_err();
        assert!(err.contains("module 'widgets'"), "{err}");
        assert!(err.contains(MODULE_MANIFEST), "{err}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn an_unresolved_version_and_an_absent_bundled_module_name_the_module() {
        let app = scratch("unresolved");
        let err = module_tags(&app, &deps("md = \"1.2\"\n"), &BTreeMap::new(), None).unwrap_err();
        assert!(err.contains("module 'md'"), "{err}");
        assert!(err.contains("not resolved"), "{err}");

        let err = module_tags(
            &app,
            &deps("lumen-no-such-module = { bundled = true }\n"),
            &BTreeMap::new(),
            None,
        )
        .unwrap_err();
        assert!(err.contains("module 'lumen-no-such-module'"), "{err}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn a_version_source_reads_the_resolved_root_and_a_lib_dir_answers_a_bundled_one() {
        let app = scratch("roots");
        let package = app.join("pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join(MODULE_MANIFEST),
            "[module]\ntags = [\"md-view\"]\n",
        )
        .unwrap();
        let lib = app.join("lib");
        let bundled = lib.join(BUNDLED_MODULE_DIR).join("zz-bundled");
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join(MODULE_MANIFEST),
            "[module]\ntags = [\"md-view\", \"zz-view\"]\n",
        )
        .unwrap();
        let versions = BTreeMap::from([("md".to_string(), package)]);
        let deps = deps("md = \"1.2\"\nzz-bundled = { bundled = true }\n");
        assert_eq!(
            module_tags(&app, &deps, &versions, Some(&lib)).unwrap(),
            ["md-view", "zz-view"]
        );
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn an_unresolved_version_source_is_passed_over_by_a_caller_that_resolves_nothing() {
        let deps = deps("lib = \"0.1\"\nmd = \"1.2\"\nw = { path = \"w\" }\n");
        let versions = BTreeMap::from([("md".to_string(), PathBuf::from("/pkgs/md"))]);
        let kept: Vec<String> = without_unresolved(&deps, &versions)
            .0
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(kept, ["md", "w"]);
    }

    /// Every module under `std/` of this checkout ships a descriptor that
    /// reads, so a `bundled = true` declaration of any of them compiles.
    #[test]
    fn every_checkout_module_ships_a_descriptor() {
        for (package, root) in checkout_std_crates() {
            let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) else {
                continue;
            };
            let manifest: toml::Table = toml::from_str(&text).expect("Cargo.toml parses");
            let is_module = manifest
                .get("dependencies")
                .and_then(|d| d.get("lumen-module"))
                .is_some();
            if !is_module {
                continue;
            }
            read_manifest(&package, &root).unwrap_or_else(|e| panic!("{e}"));
        }
    }
}
