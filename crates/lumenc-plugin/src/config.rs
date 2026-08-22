//! The `[[plugins]]` schema in `lumen.toml`.
//!
//! An array of tables, so declaration order is the run order:
//!
//! ```toml
//! [[plugins]]
//! name = "markdown"                 # must match what the cdylib reports
//! version = "1.2"                   # registry source, resolved via the cache
//! config = { flavor = "gfm" }       # optional, handed to the plugin verbatim
//!
//! [[plugins]]
//! name = "local-dev"
//! path = "plugins/local-dev"        # local cdylib, relative to the app dir
//! ```
//!
//! Exactly one source per entry: `version` (resolved against the plugin
//! cache and pinned in `lumen.lock`, see [`crate::resolve`]) or `path` (a
//! built cdylib for local development, never locked). `git` and `registry`
//! are reserved for the package registry and rejected with an error naming
//! the reason, so the future shape is not precluded and a present-day typo
//! does not read as an unknown field.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where one plugin's cdylib comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginSource {
    /// A built cdylib on disk, relative to the app directory unless
    /// absolute. Without an extension, the platform spellings are probed
    /// (`lib<p>.so`, `lib<p>.dylib`, `<p>.dll`).
    Path(String),
    /// A version requirement (cargo semantics: `"1.2"` means `^1.2`),
    /// resolved against the per-user plugin cache and pinned in
    /// `lumen.lock`.
    Version(String),
}

/// One `[[plugins]]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCfg {
    /// Plugin name; the load handshake requires the cdylib to report the
    /// same one.
    pub name: String,
    /// Where the cdylib comes from.
    pub source: PluginSource,
    /// The plugin's own configuration, passed through untouched.
    pub config: toml::Table,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginCfg {
    name: String,
    path: Option<String>,
    version: Option<String>,
    git: Option<String>,
    rev: Option<String>,
    registry: Option<String>,
    #[serde(default)]
    config: toml::Table,
}

impl<'de> Deserialize<'de> for PluginCfg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPluginCfg::deserialize(deserializer)?;
        if raw.name.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "plugins: `name` must not be empty",
            ));
        }
        let reserved = [
            ("git", &raw.git),
            ("rev", &raw.rev),
            ("registry", &raw.registry),
        ];
        if let Some((key, _)) = reserved.iter().find(|(_, v)| v.is_some()) {
            return Err(serde::de::Error::custom(format!(
                "plugin '{}': `{key}` sources are not supported yet; use `version` (registry cache) or `path` (a built cdylib)",
                raw.name
            )));
        }
        let source = match (raw.path, raw.version) {
            (Some(path), None) => PluginSource::Path(path),
            (None, Some(version)) => PluginSource::Version(version),
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(format!(
                    "plugin '{}': `path` and `version` are two sources; declare exactly one",
                    raw.name
                )));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(format!(
                    "plugin '{}': a source is required - `version` (registry cache) or `path` (a built cdylib)",
                    raw.name
                )));
            }
        };
        Ok(PluginCfg {
            name: raw.name,
            source,
            config: raw.config,
        })
    }
}

impl PluginCfg {
    /// Read the `[[plugins]]` array out of an already-parsed `lumen.toml`
    /// document. A missing array is an empty set; a malformed entry is an
    /// error naming it.
    pub fn from_document(doc: &toml::Table) -> Result<Vec<PluginCfg>, String> {
        let Some(value) = doc.get("plugins") else {
            return Ok(Vec::new());
        };
        value
            .clone()
            .try_into::<Vec<PluginCfg>>()
            .map_err(|e| format!("lumen.toml [[plugins]]: {e}"))
    }

    /// Read the `[[plugins]]` array from re-serialized raw entries (the shape
    /// `lumen-runtime`'s config carries them in).
    pub fn from_values(values: &[toml::Value]) -> Result<Vec<PluginCfg>, String> {
        values
            .iter()
            .map(|v| {
                v.clone()
                    .try_into::<PluginCfg>()
                    .map_err(|e| format!("lumen.toml [[plugins]]: {e}"))
            })
            .collect()
    }
}

/// The platform spellings of a library called `name`, in probe order.
pub(crate) fn library_spellings(name: &str) -> [String; 3] {
    [
        format!("lib{name}.so"),
        format!("lib{name}.dylib"),
        format!("{name}.dll"),
    ]
}

/// Resolve a `[[plugins]] path` to the file to load. Relative paths anchor at
/// the app directory. A path with an extension is used verbatim; without one,
/// the platform library spellings are probed in order. Returns the first
/// existing candidate, or `Err` listing every probed path.
pub fn resolve_plugin_path(app_dir: &Path, path: &str) -> Result<PathBuf, Vec<PathBuf>> {
    let base = {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            app_dir.join(p)
        }
    };
    if base.extension().is_some() {
        return if base.is_file() {
            Ok(base)
        } else {
            Err(vec![base])
        };
    }
    let stem = base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = base.parent().unwrap_or(Path::new("."));
    let candidates = library_spellings(&stem).map(|f| dir.join(f));
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(candidates.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(doc: &str) -> Result<Vec<PluginCfg>, String> {
        let table: toml::Table = toml::from_str(doc).unwrap();
        PluginCfg::from_document(&table)
    }

    #[test]
    fn declaration_order_is_preserved() {
        let cfgs = parse(
            "[[plugins]]\nname = \"a\"\npath = \"a\"\n\
             [[plugins]]\nname = \"b\"\npath = \"b\"\n\
             [[plugins]]\nname = \"c\"\npath = \"c\"\n",
        )
        .unwrap();
        let names: Vec<&str> = cfgs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn missing_source_is_an_error() {
        let err = parse("[[plugins]]\nname = \"x\"\n").unwrap_err();
        assert!(err.contains("plugin 'x'"), "{err}");
        assert!(err.contains("a source is required"), "{err}");
    }

    #[test]
    fn two_sources_are_an_error() {
        let err = parse("[[plugins]]\nname = \"x\"\npath = \"x\"\nversion = \"1\"\n").unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
    }

    #[test]
    fn version_source_parses() {
        let cfgs = parse("[[plugins]]\nname = \"x\"\nversion = \"1.2\"\n").unwrap();
        assert_eq!(cfgs[0].source, PluginSource::Version("1.2".to_string()));
    }

    #[test]
    fn reserved_sources_are_rejected_with_the_reason() {
        for key in ["git", "rev", "registry"] {
            let err = parse(&format!("[[plugins]]\nname = \"x\"\n{key} = \"1\"\n")).unwrap_err();
            assert!(err.contains("not supported yet"), "{key}: {err}");
        }
    }

    #[test]
    fn config_table_passes_through() {
        let cfgs =
            parse("[[plugins]]\nname = \"x\"\npath = \"x\"\nconfig = { flavor = \"gfm\" }\n")
                .unwrap();
        assert_eq!(
            cfgs[0].config.get("flavor").and_then(|v| v.as_str()),
            Some("gfm")
        );
    }

    #[test]
    fn no_plugins_array_is_empty() {
        assert!(parse("[app]\nname = \"x\"\n").unwrap().is_empty());
    }

    #[test]
    fn extensionless_path_probes_platform_spellings() {
        let dir = std::env::temp_dir().join("lumenc-plugin-probe-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = resolve_plugin_path(&dir, "plugins/demo").unwrap_err();
        let names: Vec<String> = err
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["libdemo.so", "libdemo.dylib", "demo.dll"]);

        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        let hit = dir.join("plugins").join("libdemo.so");
        std::fs::write(&hit, b"").unwrap();
        assert_eq!(resolve_plugin_path(&dir, "plugins/demo").unwrap(), hit);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
