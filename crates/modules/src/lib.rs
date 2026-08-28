//! Runtime dependencies: the `[dependencies]` schema in `lumen.toml`, and
//! the loader that brings each declared library into the app at startup.
//!
//! One table declares two kinds, told apart at load by the symbols the file
//! exports (see [`loader`](crate::load_modules)):
//!
//! - An **engine-locked runtime module** is an ordinary
//!   [`lumen_core::app::Plugin`] built as a shared library that links the
//!   engine dynamically (`lumen-dylib`). The loader opens it, verifies it
//!   against the running engine, and calls its install entry, which
//!   registers real systems, components, and resources into the same ECS
//!   worlds the app runs on.
//! - A **portable plugin** is a C-ABI cdylib (`lumen-plugin`) exchanging
//!   serialized bytes: script functions, language preludes, and events. It
//!   needs no dynamic engine and loads into static hosts too.
//!
//! A module the binary was built with takes neither path. It has no file to
//! open, so its constructor put its install entry on the registry before
//! `main`, and the loader answers the declared name from there before it
//! looks for anything on disk. That arm works on every platform, needs no
//! shared engine, and skips both hazards below: the module is this build.
//!
//! Two hazards shape the engine-locked arm, and both are checked before any
//! Rust symbol is touched:
//!
//! - **Build skew.** Nothing detects a layout-changed rebuild naturally: the
//!   dynamic linker resolves happily and `TypeId` equality still passes while
//!   field reads are shifted. The only defense is the C-ABI probe
//!   (`lumen_module_probe_<name>`, spelled from the declared name), compared
//!   against the engine's own `lumen_dylib::BUILD_ID` for exact equality.
//! - **A second engine instance.** If the host process compiled the engine in
//!   (a plain `cargo` binary, a static bundle), dlopening a module maps
//!   `liblumen_engine` a second time; the probe strings would match while
//!   worlds and statics differ. The loader asks the running process for the
//!   engine's exported `lumen_engine_build_id` symbol first and refuses every
//!   module when it is absent.
//!
//! The failure policy is banner-and-continue: any load failure prints an
//! unmissable stderr banner naming the module, the reason, and every probed
//! path, and the app keeps booting without that module. One refusal speaks
//! quietly instead: a name that this build neither compiles in nor can open
//! beside a shared engine gets a single stderr line, because that is a
//! property of how the binary was put together, not a defect in the app. A
//! loaded library is never unloaded: the schedules hold
//! function pointers into it for as long as the app lives.
//!
//! ```toml
//! [dependencies]
//! lumen-audio = { bundled = true }
//! markdown-widgets = "1.2"
//! shape-tools = { path = "modules/shape-tools", config = { units = "mm" } }
//! ```
//!
//! The table is unordered, so load order is the sorted key order; declaring
//! order in the file carries no meaning.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

pub mod link_kit;

#[cfg(feature = "loader")]
mod loader;
#[cfg(feature = "loader")]
pub use loader::{
    InitEnv, LoadedKind, LoadedModule, LoadedModules, ModuleFailure, PortablePlugins, load_modules,
};

/// The prefix of the C-ABI probe an engine-locked module exports; the module's
/// declared name completes it. The probe returns the module's NUL-terminated
/// `BUILD_ID`, read before any Rust symbol is touched.
pub const PROBE_PREFIX: &str = "lumen_module_probe_";
/// The prefix of the Rust-ABI install entry, called only after the probe
/// matched exactly.
pub const INSTALL_PREFIX: &str = "lumen_module_install_";
/// The prefix of the registration entry every module exports, opened or
/// linked. Naming it on a link line is what pulls a module out of an archive
/// that nothing else references.
pub const REGISTER_PREFIX: &str = "lumen_module_register_";

/// One module's entry symbol: the prefix, then the declared name with every
/// character a symbol cannot carry replaced by `_`.
///
/// `lumen-module-macros` spells the same names at the module's compile time,
/// from the name its author passed to `lumen_module!`. The two spellings are
/// the contract between a module and the loader: a module declared under a
/// name it was not built with reads as a library exporting nothing. They live
/// here rather than beside the loader because a link kit spells them with no
/// loader in the build at all.
pub fn entry_symbol(prefix: &str, name: &str) -> String {
    let mut symbol = String::with_capacity(prefix.len() + name.len());
    symbol.push_str(prefix);
    symbol.extend(
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }),
    );
    symbol
}

/// `version`-source resolutions handed to the loader by the compiler, keyed
/// by module name.
///
/// The runtime never resolves a version itself - no semver, no cache, no
/// lock in its graph. `lumenc` resolves each `version` requirement through
/// the shared plugin cache and `lumen.lock` before the app builds, and hands
/// the outcome in here: `Ok` is the library file to open, `Err` is the
/// reason resolution failed, which the loader banners in place of its own
/// probe. A module absent from the map falls back to the loader's on-disk
/// probe (the `modules/` directories a build step stages into).
#[derive(Debug, Clone, Default)]
pub struct ResolvedModules(pub BTreeMap<String, Result<PathBuf, String>>);

// The candidate file names of a library, shared with every other loader that
// probes by name. The one copy lives beside the `[[plugins]]` path resolution
// in `lumen-plugin-abi`; the loader's on-disk probe and `lumenc package` both
// read this re-export.
pub use lumen_plugin_abi::config::library_spellings;

/// The parsed `[dependencies]` table: one [`DepCfg`] per declared module,
/// sorted by name, which is the load order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DependenciesCfg(pub Vec<DepCfg>);

/// One `[dependencies]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct DepCfg {
    /// Module name - the table key.
    pub name: String,
    /// Where the module's library comes from.
    pub source: ModuleSource,
    /// The module's own configuration, handed to it verbatim at install.
    pub config: toml::Table,
}

/// Where one module's library comes from. A bare string value is shorthand
/// for [`ModuleSource::Version`].
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleSource {
    /// Ships with the toolchain, beside the running engine.
    Bundled,
    /// A version requirement, resolved by lumenc; the runtime never resolves
    /// or fetches one, it only loads a module already on disk.
    Version(String),
    /// A built library on disk, relative to the app directory unless
    /// absolute. Without an extension, the platform spellings are probed
    /// (`lib<m>.so`, `lib<m>.dylib`, `<m>.dll`, plus underscored variants
    /// for a hyphenated name).
    Path(String),
}

impl<'de> Deserialize<'de> for DependenciesCfg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // A BTreeMap so iteration - and therefore load order - is the sorted
        // key order, the only stable order an unordered TOML table has.
        let raw: BTreeMap<String, toml::Value> = BTreeMap::deserialize(deserializer)?;
        let mut deps = Vec::with_capacity(raw.len());
        for (name, value) in raw {
            deps.push(DepCfg::from_entry(name, value).map_err(serde::de::Error::custom)?);
        }
        Ok(DependenciesCfg(deps))
    }
}

impl DepCfg {
    /// Parse one `name = <value>` entry of the `[dependencies]` table.
    fn from_entry(name: String, value: toml::Value) -> Result<DepCfg, String> {
        if name.trim().is_empty() {
            return Err("dependencies: a module name must not be empty".to_string());
        }
        let table = match value {
            toml::Value::String(version) => {
                return Ok(DepCfg {
                    name,
                    source: ModuleSource::Version(version),
                    config: toml::Table::new(),
                });
            }
            toml::Value::Table(t) => t,
            other => {
                return Err(format!(
                    "dependency '{name}': expected a version string or a table, got {other}"
                ));
            }
        };

        let mut bundled: Option<bool> = None;
        let mut version: Option<String> = None;
        let mut path: Option<String> = None;
        let mut config = toml::Table::new();
        for (key, v) in table {
            match key.as_str() {
                "bundled" => match v.as_bool() {
                    Some(b) => bundled = Some(b),
                    None => {
                        return Err(format!("dependency '{name}': `bundled` must be a boolean"));
                    }
                },
                "version" => match v.as_str() {
                    Some(s) => version = Some(s.to_string()),
                    None => {
                        return Err(format!("dependency '{name}': `version` must be a string"));
                    }
                },
                "path" => match v.as_str() {
                    Some(s) => path = Some(s.to_string()),
                    None => return Err(format!("dependency '{name}': `path` must be a string")),
                },
                "config" => match v {
                    toml::Value::Table(t) => config = t,
                    _ => return Err(format!("dependency '{name}': `config` must be a table")),
                },
                "git" | "rev" | "registry" => {
                    return Err(format!(
                        "dependency '{name}': `{key}` sources are not supported yet; use \
                         `bundled`, `version`, or `path`"
                    ));
                }
                "permissions" => {
                    return Err(format!(
                        "dependency '{name}': capability declarations are not supported yet; a \
                         module is native code running in the app's process, the same trust \
                         model as [[hooks]]"
                    ));
                }
                other => {
                    return Err(format!(
                        "unknown key `{other}` in dependency '{name}': this key needs a newer \
                         toolchain"
                    ));
                }
            }
        }

        if bundled == Some(false) {
            return Err(format!(
                "dependency '{name}': `bundled = false` declares nothing; drop the key or set \
                 it to true"
            ));
        }
        let declared: Vec<&str> = [
            bundled.map(|_| "bundled"),
            version.as_ref().map(|_| "version"),
            path.as_ref().map(|_| "path"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let source = match (bundled, version, path) {
            (Some(true), None, None) => ModuleSource::Bundled,
            (None, Some(v), None) => ModuleSource::Version(v),
            (None, None, Some(p)) => ModuleSource::Path(p),
            (None, None, None) => {
                return Err(format!(
                    "dependency '{name}': a source is required - `bundled`, `version`, or `path`"
                ));
            }
            _ => {
                return Err(format!(
                    "dependency '{name}': {} are {} sources; declare exactly one",
                    declared.join(" and "),
                    declared.len()
                ));
            }
        };
        Ok(DepCfg {
            name,
            source,
            config,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(doc: &str) -> Result<DependenciesCfg, String> {
        toml::from_str::<DependenciesCfg>(doc).map_err(|e| e.to_string())
    }

    #[test]
    fn a_bare_string_is_a_version() {
        let deps = parse("md = \"1.2\"\n").unwrap();
        assert_eq!(deps.0.len(), 1);
        assert_eq!(deps.0[0].name, "md");
        assert_eq!(deps.0[0].source, ModuleSource::Version("1.2".into()));
        assert!(deps.0[0].config.is_empty());
    }

    #[test]
    fn bundled_true_parses() {
        let deps = parse("fs = { bundled = true }\n").unwrap();
        assert_eq!(deps.0[0].source, ModuleSource::Bundled);
    }

    #[test]
    fn bundled_false_is_refused() {
        let err = parse("fs = { bundled = false }\n").unwrap_err();
        assert!(err.contains("declares nothing"), "{err}");
    }

    #[test]
    fn path_with_config_parses() {
        let deps =
            parse("shape = { path = \"modules/shape\", config = { units = \"mm\" } }\n").unwrap();
        assert_eq!(deps.0[0].source, ModuleSource::Path("modules/shape".into()));
        assert_eq!(
            deps.0[0].config.get("units").and_then(|v| v.as_str()),
            Some("mm")
        );
    }

    #[test]
    fn permissions_are_refused_with_the_trust_model() {
        let err = parse("x = { path = \"m\", permissions = [\"fs\"] }\n").unwrap_err();
        assert!(err.contains("capability declarations"), "{err}");
        assert!(err.contains("[[hooks]]"), "{err}");
    }

    #[test]
    fn reserved_sources_are_refused_with_the_reason() {
        for key in ["git", "rev", "registry"] {
            let err = parse(&format!("x = {{ {key} = \"v\" }}\n")).unwrap_err();
            assert!(err.contains("not supported yet"), "{key}: {err}");
        }
    }

    #[test]
    fn an_unknown_key_names_the_toolchain() {
        let err = parse("y = { path = \"m\", checksum = \"abc\" }\n").unwrap_err();
        assert!(
            err.contains("unknown key `checksum` in dependency 'y'"),
            "{err}"
        );
        assert!(err.contains("needs a newer toolchain"), "{err}");
    }

    #[test]
    fn two_sources_are_refused() {
        let err = parse("x = { path = \"m\", version = \"1\" }\n").unwrap_err();
        assert!(err.contains("declare exactly one"), "{err}");
    }

    #[test]
    fn no_source_is_refused() {
        let err = parse("x = { config = { a = 1 } }\n").unwrap_err();
        assert!(err.contains("a source is required"), "{err}");
    }

    #[test]
    fn load_order_is_sorted_by_name() {
        let deps = parse("zeta = \"1\"\nalpha = \"1\"\nmid = \"1\"\n").unwrap();
        let names: Vec<&str> = deps.0.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["alpha", "mid", "zeta"]);
    }
}
