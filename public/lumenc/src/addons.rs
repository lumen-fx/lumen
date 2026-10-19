//! The web halves of the modules an app depends on, found for the target a
//! build is for.
//!
//! Every `[dependencies]` entry is a module, and every module is found at a
//! root directory the same way whatever uses it: a `path` source names the
//! directory, a `version` source is the package `lpm` unpacked, and a
//! `bundled` one is the toolchain's copy, `modules/<name>/` beside `lumenc`
//! (or, in a checkout, the crate under `std/` whose package is `<name>`).
//! A module's web half is the `web/` directory under its root, holding a
//! `lumen-addon.toml`.
//!
//! The descriptor is where a module declares its script surface, and both of
//! its halves offer that surface, so every compile reads it: the functions it
//! declares are what the app's scripts compile against, for every target. A
//! module without a web half has no descriptor, so a compile for any target
//! but the web installs it the way a run does and declares what it registers
//! ([`compile_deps`]).
//!
//! Only a web build takes the rest of a web half, and `lumenc web` refuses a
//! module that has none. Every other build hands the table to the loader,
//! which opens each module's library when the app runs.
//!
//! What comes back is what the rest of a build needs from a web half: the
//! description the compiled app carries, the files a site ships, and, for one
//! that carries candela sugar, the import root its script is compiled from.

use std::path::{Path, PathBuf};

use lumen_modules::addon::{AddonPackage, read_web_half, web_half};
use lumen_modules::{DepCfg, DependenciesCfg, ModuleSource, ResolvedModules, Target};
use lumen_runtime::CompileDeps;
use lumen_runtime::app_kind::{self, AppKind};

use crate::package::lpm::Resolved;

/// The directory the toolchain keeps its bundled modules' roots in, inside
/// each place it keeps its files.
pub const BUNDLED_MODULE_DIR: &str = "modules";

/// What a build for one target took from outside the app directory.
#[derive(Debug, Clone)]
pub struct TargetDeps {
    /// What the compile reads: the target, the import roots, and the
    /// descriptors of the modules' web halves.
    pub compile: CompileDeps,
    /// The web halves, with the files a site ships for each. Empty for every
    /// target but the web: the others read only the descriptors.
    pub packages: Vec<AddonPackage>,
    /// Everything `lpm` resolved for the target.
    pub resolved: Resolved,
    /// The target's dependencies that have no web half, in table order: what
    /// a web build cannot take, and what a compile for any other target reads
    /// by installing each module.
    pub without_web_half: DependenciesCfg,
}

/// Resolve what the app at `dir` depends on for a build for `target`: its
/// registry packages and its modules' web halves.
///
/// `lib_dir` is the `--lib-dir` a build was given, which is searched for
/// bundled modules before the toolchain's own directories.
///
/// # Errors
///
/// `lumen.toml` does not parse, a registry requirement does not resolve, or a
/// web half's descriptor is refused.
pub fn target_deps(
    dir: &Path,
    target: Target,
    lib_dir: Option<&Path>,
) -> Result<TargetDeps, String> {
    target_deps_reading(dir, target, lib_dir, false)
}

/// [`target_deps`], with the modules a Rust app links through its
/// `Cargo.toml` taken as dependencies too when `linked_modules` is set (see
/// [`linked_modules`]).
fn target_deps_reading(
    dir: &Path,
    target: Target,
    lib_dir: Option<&Path>,
    linked_modules: bool,
) -> Result<TargetDeps, String> {
    let cfg =
        lumen_runtime::LumenToml::load_or_default(dir).map_err(|e| format!("lumen.toml: {e}"))?;
    let resolved = crate::registry_packages(dir, target)?;
    let mut deps = cfg.dependencies_for(target);
    if linked_modules && app_kind::resolve(dir, cfg.app.kind) == AppKind::Rust {
        let linked = self::linked_modules(dir, &deps, lib_dir);
        deps.0.extend(linked);
    }
    let halves = web_halves(dir, &deps, &resolved, lib_dir)?;
    let without_web_half = DependenciesCfg(
        deps.0
            .into_iter()
            .filter(|dep| !halves.iter().any(|p| p.addon.name == dep.name))
            .collect(),
    );
    let addons = halves.iter().map(|p| p.addon.clone()).collect();
    let packages = if target == Target::Web {
        halves
    } else {
        Vec::new()
    };
    let mut import_roots = resolved.candela_roots.clone();
    import_roots.extend(packages.iter().filter_map(candela_root));
    let compile = CompileDeps {
        target,
        import_roots,
        addons,
        ..CompileDeps::new(target)
    };
    Ok(TargetDeps {
        compile,
        packages,
        resolved,
        without_web_half,
    })
}

/// [`target_deps`] for a compile. For every target but the web, each module
/// without a web half is installed the way a run installs it, and what it
/// registers for scripts is declared to the compile
/// ([`lumen_runtime::module_surface`]). A web build cannot take such a module,
/// so there is nothing to read for one.
///
/// # Errors
///
/// As [`target_deps`].
pub fn compile_deps(
    dir: &Path,
    target: Target,
    lib_dir: Option<&Path>,
) -> Result<TargetDeps, String> {
    compile_deps_reading(dir, target, lib_dir, false)
}

/// [`compile_deps`] for the UI of an SDK app, compiled on its own for the
/// app's executable to carry (`lumenc build --ui`).
///
/// Such an app brings elements the markup app has no way to declare: the
/// modules a Rust app compiles in through its `Cargo.toml` (see
/// [`linked_modules`]), and the widgets it defines itself with
/// `#[derive(Widget)]`, which register their tags when the app starts rather
/// than when its UI compiles. Both are taken from the app's own files, so the
/// compile accepts the elements the running app answers for.
///
/// # Errors
///
/// As [`target_deps`].
pub fn sdk_ui_deps(dir: &Path, lib_dir: Option<&Path>) -> Result<TargetDeps, String> {
    for tag in app_widget_tags(dir) {
        lumen_widget::register_widget_tag_owned(&tag);
    }
    compile_deps_reading(dir, Target::Desktop, lib_dir, true)
}

/// The tags the app's own Rust widgets claim: the `tag = "..."` of every
/// `#[widget(...)]` attribute in a `.rs` file under the app's `src/`, which
/// is where the `Widget` derive reads it from.
fn app_widget_tags(dir: &Path) -> Vec<String> {
    let mut tags = Vec::new();
    let mut pending = vec![dir.join("src")];
    while let Some(at) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                tags.extend(widget_tags_in(&text));
            }
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

/// The `tag = "..."` values of the `#[widget(...)]` attributes in `text`.
fn widget_tags_in(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("#[widget(") {
        rest = &rest[at + "#[widget(".len()..];
        let attr = rest.split(")]").next().unwrap_or("");
        let Some(key) = attr.find("tag") else {
            continue;
        };
        let after = attr[key + "tag".len()..].trim_start();
        let Some(value) = after.strip_prefix('=').map(str::trim_start) else {
            continue;
        };
        if let Some(quoted) = value.strip_prefix('"')
            && let Some(end) = quoted.find('"')
        {
            tags.push(quoted[..end].to_string());
        }
    }
    tags
}

fn compile_deps_reading(
    dir: &Path,
    target: Target,
    lib_dir: Option<&Path>,
    linked_modules: bool,
) -> Result<TargetDeps, String> {
    let mut found = target_deps_reading(dir, target, lib_dir, linked_modules)?;
    if target != Target::Web {
        found.compile.modules = lumen_runtime::module_surface(
            dir,
            &found.without_web_half,
            &resolved_modules(&found.resolved),
        );
    }
    Ok(found)
}

/// What `lpm` resolved for the `version` sources, in the form the module
/// loader reads.
pub fn resolved_modules(resolved: &Resolved) -> ResolvedModules {
    ResolvedModules(
        resolved
            .modules
            .iter()
            .map(|(name, file)| (name.clone(), Ok(file.clone())))
            .collect(),
    )
}

/// The web half of each module in `deps` that has one, read in table order,
/// each carrying the `config` table its entry gave it. A module without one
/// is passed over; saying that a web build cannot take it is `lumenc web`'s
/// business.
///
/// # Errors
///
/// A web half's descriptor is refused.
pub fn web_halves(
    dir: &Path,
    deps: &DependenciesCfg,
    resolved: &Resolved,
    lib_dir: Option<&Path>,
) -> Result<Vec<AddonPackage>, String> {
    let mut packages = Vec::new();
    for dep in &deps.0 {
        let Some(root) = module_root(dir, &dep.name, &dep.source, resolved, lib_dir) else {
            continue;
        };
        if web_half(&root).is_none() {
            continue;
        }
        let mut package = read_web_half(&dep.name, &root)?;
        package.config = dep.config.clone();
        packages.push(package);
    }
    Ok(packages)
}

/// The root directory of the module `name`, declared with `source`, when it
/// is on disk.
pub fn module_root(
    dir: &Path,
    name: &str,
    source: &ModuleSource,
    resolved: &Resolved,
    lib_dir: Option<&Path>,
) -> Option<PathBuf> {
    match source {
        ModuleSource::Path(path) => Some(dir.join(path)),
        ModuleSource::Version(_) => resolved.roots.get(name).cloned(),
        ModuleSource::Bundled => bundled_module(name, lib_dir),
    }
}

/// The toolchain's own root of the bundled module `name`: `modules/<name>/`
/// under the `--lib-dir`, the directory holding `lumenc`, then
/// `LUMEN_LIB_DIR`, and last the crate under `std/` of the source tree this
/// `lumenc` was built from whose package is `name`, when that tree is still
/// on disk.
pub fn bundled_module(name: &str, lib_dir: Option<&Path>) -> Option<PathBuf> {
    crate::package::cli::search_dirs(lib_dir, true)
        .into_iter()
        .map(|root| root.join(BUNDLED_MODULE_DIR).join(name))
        .find(|root| root.is_dir())
        .or_else(|| source_tree_module(name))
}

/// The crate under `std/` whose package is `name`, in the workspace this
/// crate was compiled in. A release build's tree is gone by the time anyone
/// runs it, so there this finds nothing and the toolchain's own `modules/`
/// directory is what answers; a checkout's build finds the modules it was
/// built beside, edits included.
fn source_tree_module(name: &str) -> Option<PathBuf> {
    let std_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join("std"))
        .find(|dir| dir.is_dir())?;
    let mut crates: Vec<PathBuf> = std::fs::read_dir(&std_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    crates.sort();
    crates
        .into_iter()
        .find(|root| package_name(root).as_deref() == Some(name))
}

/// The package name in the `Cargo.toml` at `root`.
fn package_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let manifest: toml::Table = toml::from_str(&text).ok()?;
    manifest
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// The bundled modules a Rust app links through its `Cargo.toml`, as the
/// `[dependencies]` entries a markup app would write for them, leaving out any
/// `declared` already names.
///
/// A Rust app cannot declare a runtime module in `lumen.toml`: it compiles the
/// module in, by depending on its crate, and installs the plugin itself. The
/// crate is the module's own (`lumen-<name>`) or the published wrapper that
/// re-exports it (`lumenui-<name>`). A dependency counts when the toolchain
/// has a bundled module of that name, so the elements the module answers for
/// are the ones its descriptor declares, as for any app that depends on it.
fn linked_modules(dir: &Path, declared: &DependenciesCfg, lib_dir: Option<&Path>) -> Vec<DepCfg> {
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(manifest) = toml::from_str::<toml::Table>(&text) else {
        return Vec::new();
    };
    let mut tables: Vec<&toml::Table> = Vec::new();
    tables.extend(manifest.get("dependencies").and_then(toml::Value::as_table));
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(
            targets
                .values()
                .filter_map(|t| t.get("dependencies").and_then(toml::Value::as_table)),
        );
    }
    let mut linked: Vec<DepCfg> = Vec::new();
    for (key, value) in tables.into_iter().flatten() {
        // A renamed dependency names its crate in `package`.
        let package = value
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(key);
        let module = match package.strip_prefix("lumenui-") {
            Some(name) => format!("lumen-{name}"),
            None if package.starts_with("lumen-") => package.to_string(),
            None => continue,
        };
        let known = declared
            .0
            .iter()
            .chain(&linked)
            .any(|dep| dep.name == module);
        if known || bundled_module(&module, lib_dir).is_none() {
            continue;
        }
        linked.push(DepCfg {
            name: module,
            source: ModuleSource::Bundled,
            config: toml::Table::new(),
            tags: Vec::new(),
        });
    }
    linked
}

/// The import root a web half's candela sugar is compiled from, when it
/// carries some: the `web/` directory is a candela package too, entered the
/// way any is, through its `candela.toml` or `src/main.cdl`.
fn candela_root(package: &AddonPackage) -> Option<(String, PathBuf)> {
    let is_package =
        package.dir.join("candela.toml").is_file() || package.dir.join("src/main.cdl").is_file();
    is_package.then(|| (package.addon.name.clone(), package.dir.clone()))
}

/// What the app at `dir` compiles against for each target it can be built
/// for, which is what `lumenc check` compiles against: a check is for no one
/// target, so it checks the app the way each target's build compiles it.
///
/// A web build refuses an app whose web dependencies include a module with no
/// web half, so for such an app the web is left out and the check is the
/// desktop's alone.
///
/// A Rust app's modules come from its `Cargo.toml` as well as `lumen.toml`
/// (see [`linked_modules`]), so the elements a module it compiles in answers
/// for check the way they run.
///
/// # Errors
///
/// As [`target_deps`].
pub fn every_target(dir: &Path) -> Result<Vec<CompileDeps>, String> {
    let mut targets = Vec::new();
    for target in Target::ALL {
        let found = compile_deps_reading(dir, target, None, true)?;
        if target == Target::Web && !found.without_web_half.0.is_empty() {
            continue;
        }
        targets.push(found.compile);
    }
    Ok(targets)
}

/// A web half's files, as a site ships them: the whole `web/` directory under
/// one directory named for its contents, so a module's relative imports and the
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag of every `#[widget(...)]` attribute, wherever `tag` sits in
    /// it; an attribute without one, or a field-level `#[widget(prop)]`,
    /// names none.
    #[test]
    fn widget_tags_are_read_off_the_derive_attributes() {
        let source = r#"
            #[derive(Component, Widget)]
            #[widget(tag = "smoke-gauge")]
            struct Gauge {
                #[widget(prop)]
                label: String,
            }
            #[derive(Widget)]
            #[widget(name = "x", tag="color-chip")]
            struct Chip;
        "#;
        assert_eq!(widget_tags_in(source), ["smoke-gauge", "color-chip"]);
    }

    /// The scan covers the whole of `src/`, nested modules included.
    #[test]
    fn an_apps_widget_tags_come_from_every_file_under_src() {
        let dir = std::env::temp_dir().join(format!("lumenc-widget-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src").join("panels")).unwrap();
        std::fs::write(
            dir.join("src").join("main.rs"),
            "#[widget(tag = \"b-tag\")] struct B;",
        )
        .unwrap();
        std::fs::write(
            dir.join("src").join("panels").join("a.rs"),
            "#[widget(tag = \"a-tag\")] struct A;",
        )
        .unwrap();
        std::fs::write(
            dir.join("src").join("main.lmn"),
            "#[widget(tag = \"not-rust\")]",
        )
        .unwrap();
        assert_eq!(app_widget_tags(&dir), ["a-tag", "b-tag"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
