//! Browser add-ons an app depends on, found for the target a build is for.
//!
//! A `[dependencies]` entry is an add-on when the directory it resolves to
//! holds a `lumen-addon.toml`: a `path` source names that directory, a
//! `version` source is the package `lpm` unpacked, and a `bundled` one is the
//! toolchain's own copy under `addons/<name>/`. Everything else in the table
//! is a runtime library, and stays the loader's business. So is an add-on
//! whose descriptor says `native = true`, in a build for any target but the
//! web: there the dependency is the runtime module the add-on stands in for.
//!
//! What comes back is what the rest of a build needs from an add-on: the
//! description the compiled app carries, the files a site ships, and, for one
//! that carries candela sugar, the import root its script is compiled from.

use std::path::{Path, PathBuf};

use lumen_modules::addon::{AddonPackage, is_addon, read_addon, serves};
use lumen_modules::{DependenciesCfg, ModuleSource, Target};
use lumen_runtime::CompileDeps;

use crate::package::lpm::Resolved;

/// The directory a bundled add-on is looked for under, inside each place the
/// toolchain keeps its files.
pub const BUNDLED_ADDON_DIR: &str = "addons";

/// What a build for one target took from outside the app directory.
#[derive(Debug, Clone)]
pub struct TargetDeps {
    /// What the compile reads: the target, the import roots, the add-ons.
    pub compile: CompileDeps,
    /// The add-ons, with the files a site ships for each.
    pub packages: Vec<AddonPackage>,
    /// Everything `lpm` resolved for the target.
    pub resolved: Resolved,
}

/// Resolve what the app at `dir` depends on for a build for `target`: its
/// registry packages and its add-ons.
///
/// `lib_dir` is the `--lib-dir` a build was given, which is searched for
/// bundled add-ons before the toolchain's own directories.
///
/// # Errors
///
/// `lumen.toml` does not parse, a registry requirement does not resolve, or an
/// add-on's descriptor is refused.
pub fn target_deps(
    dir: &Path,
    target: Target,
    lib_dir: Option<&Path>,
) -> Result<TargetDeps, String> {
    let cfg =
        lumen_runtime::LumenToml::load_or_default(dir).map_err(|e| format!("lumen.toml: {e}"))?;
    let resolved = crate::registry_packages(dir, target)?;
    let packages = addons_of(
        dir,
        &cfg.dependencies_for(target),
        target,
        &resolved,
        lib_dir,
    )?;
    let mut import_roots = resolved.candela_roots.clone();
    import_roots.extend(packages.iter().filter_map(candela_root));
    let compile = CompileDeps {
        target,
        import_roots,
        addons: packages.iter().map(|p| p.addon.clone()).collect(),
    };
    Ok(TargetDeps {
        compile,
        packages,
        resolved,
    })
}

/// The add-ons a build for `target` takes from `deps`, read in table order,
/// each carrying the `config` table its entry gave it.
///
/// # Errors
///
/// An add-on's descriptor is refused.
pub fn addons_of(
    dir: &Path,
    deps: &DependenciesCfg,
    target: Target,
    resolved: &Resolved,
    lib_dir: Option<&Path>,
) -> Result<Vec<AddonPackage>, String> {
    let mut packages = Vec::new();
    for dep in &deps.0 {
        let Some(root) = addon_dir(dir, &dep.name, &dep.source, target, resolved, lib_dir) else {
            continue;
        };
        let mut package = read_addon(&dep.name, &root)?;
        package.config = dep.config.clone();
        packages.push(package);
    }
    Ok(packages)
}

/// `deps` without the add-ons a build for `target` takes: the runtime
/// libraries that build stages and its loader opens.
pub fn libraries_of(
    dir: &Path,
    deps: &DependenciesCfg,
    target: Target,
    resolved: &Resolved,
    lib_dir: Option<&Path>,
) -> DependenciesCfg {
    DependenciesCfg(
        deps.0
            .iter()
            .filter(|dep| {
                addon_dir(dir, &dep.name, &dep.source, target, resolved, lib_dir).is_none()
            })
            .cloned()
            .collect(),
    )
}

/// The directory the dependency `name` resolves to, when that is an add-on a
/// build for `target` takes.
fn addon_dir(
    dir: &Path,
    name: &str,
    source: &ModuleSource,
    target: Target,
    resolved: &Resolved,
    lib_dir: Option<&Path>,
) -> Option<PathBuf> {
    let root = match source {
        ModuleSource::Path(path) => Some(dir.join(path)),
        ModuleSource::Version(_) => resolved.addons.get(name).cloned(),
        ModuleSource::Bundled => bundled_addon(name, lib_dir),
    }?;
    serves(&root, target).then_some(root)
}

/// The toolchain's own copy of the add-on `name`: `addons/<name>/` under the
/// `--lib-dir`, the directory holding `lumenc`, then `LUMEN_LIB_DIR`, and
/// last the `std/addons/` directory of the source tree this `lumenc` was
/// built from, when that tree is still on disk.
pub fn bundled_addon(name: &str, lib_dir: Option<&Path>) -> Option<PathBuf> {
    crate::package::cli::search_dirs(lib_dir, true)
        .into_iter()
        .map(|root| root.join(BUNDLED_ADDON_DIR))
        .chain(source_tree_addons())
        .map(|root| root.join(name))
        .find(|root| is_addon(root))
}

/// Where the first-party add-ons live in the source tree: `std/addons/`
/// under the workspace this crate was compiled in. A release build's tree is
/// gone by the time anyone runs it, so there this finds nothing and the
/// toolchain's own `addons/` directory is what answers; a checkout's build
/// finds the add-ons it was built beside, edits included.
fn source_tree_addons() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join("std").join(BUNDLED_ADDON_DIR))
        .find(|dir| dir.is_dir())
}

/// The import root an add-on's candela sugar is compiled from, when it
/// carries some: the package is a candela package too, entered the way any
/// is, through its `candela.toml` or `src/main.cdl`.
fn candela_root(package: &AddonPackage) -> Option<(String, PathBuf)> {
    let is_package =
        package.dir.join("candela.toml").is_file() || package.dir.join("src/main.cdl").is_file();
    is_package.then(|| (package.addon.name.clone(), package.dir.clone()))
}

/// Every add-on and import root the app at `dir` declares for any target,
/// which is what `lumenc check` compiles against: a check is for no one
/// target, so a call written for either passes.
///
/// # Errors
///
/// As [`target_deps`].
pub fn every_target(dir: &Path) -> Result<CompileDeps, String> {
    let mut deps = CompileDeps::new(Target::Desktop);
    for target in Target::ALL {
        let found = target_deps(dir, target, None)?;
        for root in found.compile.import_roots {
            if !deps.import_roots.contains(&root) {
                deps.import_roots.push(root);
            }
        }
        for addon in found.compile.addons {
            if !deps.addons.iter().any(|a| a.name == addon.name) {
                deps.addons.push(addon);
            }
        }
    }
    Ok(deps)
}

/// An add-on's files, as a site ships them: the whole package under one
/// directory named for its contents, so a module's relative imports and the
/// files it fetches keep working and a changed add-on is a new URL.
#[cfg(feature = "web")]
pub mod site {
    use std::path::Path;

    use base64::Engine as _;
    use lumen_modules::addon::AddonPackage;
    use lumen_web::{CheckedFile, WebAddon};
    use sha2::{Digest, Sha384};

    /// Where every add-on's directory goes, inside the site.
    pub const SITE_DIR: &str = "addons";

    /// One file to write into the site.
    pub struct SiteFile {
        /// Where it goes, relative to the site root.
        pub path: String,
        /// What goes there.
        pub bytes: Vec<u8>,
    }

    /// The files `package` ships and the way the documents load it.
    ///
    /// # Errors
    ///
    /// A file the descriptor names could not be read.
    pub fn ship(package: &AddonPackage) -> Result<(WebAddon, Vec<SiteFile>), String> {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut take =
            |relative: &str| -> Result<(), String> { collect(&package.dir, relative, &mut files) };
        take(&package.module)?;
        for style in &package.styles {
            take(style)?;
        }
        if let Some(head) = &package.head {
            take(head)?;
        }
        for file in &package.files {
            take(file)?;
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files.dedup_by(|a, b| a.0 == b.0);

        // One name for the whole directory, taken from every file in it.
        let mut digest = Vec::new();
        for (path, bytes) in &files {
            digest.extend_from_slice(path.as_bytes());
            digest.push(0);
            digest.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            digest.extend_from_slice(bytes);
        }
        let root = lumen_web::content_name(&format!("{SITE_DIR}/{}", package.addon.name), &digest);

        let checked = |relative: &str| -> CheckedFile {
            let bytes = files
                .iter()
                .find(|(path, _)| path == relative)
                .map(|(_, bytes)| bytes.as_slice())
                .unwrap_or_default();
            CheckedFile {
                path: format!("{root}/{relative}"),
                integrity: integrity(bytes),
            }
        };
        let addon = WebAddon {
            name: package.addon.name.clone(),
            module: checked(&package.module),
            styles: package.styles.iter().map(|s| checked(s)).collect(),
            head: package.head.as_deref().map(checked),
            config: config_json(&package.config)?,
        };
        let files = files
            .into_iter()
            .map(|(path, bytes)| SiteFile {
                path: format!("{root}/{path}"),
                bytes,
            })
            .collect();
        Ok((addon, files))
    }

    /// The `config` table an app gave an add-on, as the JSON text a page
    /// hands its module, or `None` for an empty one.
    ///
    /// # Errors
    ///
    /// The table holds a value JSON has no spelling for.
    fn config_json(config: &toml::Table) -> Result<Option<String>, String> {
        if config.is_empty() {
            return Ok(None);
        }
        serde_json::to_string(config)
            .map(Some)
            .map_err(|e| format!("the add-on's config table: {e}"))
    }

    /// The Subresource Integrity value for `bytes`: SHA-384, base64.
    pub fn integrity(bytes: &[u8]) -> String {
        let hash = Sha384::digest(bytes);
        format!(
            "sha384-{}",
            base64::engine::general_purpose::STANDARD.encode(hash)
        )
    }

    /// Read `relative` under `dir`, every file under it when it is a
    /// directory, into `files` with `/`-separated paths.
    fn collect(
        dir: &Path,
        relative: &str,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let path = dir.join(relative);
        if path.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?
                .filter_map(Result::ok)
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let name = entry.file_name().to_string_lossy().into_owned();
                collect(dir, &format!("{relative}/{name}"), files)?;
            }
            return Ok(());
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        files.push((relative.to_owned(), bytes));
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_config_table_travels_as_json_text() {
            assert_eq!(config_json(&toml::Table::new()).unwrap(), None);
            let table: toml::Table =
                toml::from_str("allow = [\"https://cdn.example/\"]\ncap = 3\n").unwrap();
            assert_eq!(
                config_json(&table).unwrap().as_deref(),
                Some(r#"{"allow":["https://cdn.example/"],"cap":3}"#)
            );
        }

        #[test]
        fn integrity_is_the_sha384_the_browser_checks() {
            // `printf abc | openssl dgst -sha384 -binary | base64`
            assert_eq!(
                integrity(b"abc"),
                "sha384-ywB1P0WjXou1oD1pmsZQBycsMqsO3tFjGotgWkP/W+2AhgcroefMI1i67KE0yCWn"
            );
        }
    }
}
