//! Which runtime module runs which script language.
//!
//! A script host is a runtime module like any other. What makes it a host is
//! a descriptor beside it, `lumen-language.toml`, which names the language it
//! runs, the file extensions that language owns, and the form it runs a
//! program in:
//!
//! ```toml
//! [language]
//! name = "candela"
//! extensions = ["cdl"]
//! form = "bytecode"   # or "source"
//! default = true      # the language an inline `<script>` block is read as
//! ```
//!
//! One language can have two providers: one that runs source, with a compiler
//! and hot reload, for the edit loop, and one that runs bytecode compiled
//! ahead of time, for a shipped app. A language with a single source-form
//! provider runs from source everywhere.
//!
//! The descriptors are found where bundled modules are: `modules/<name>/`
//! under the directories a toolchain ships them in, and the crate under
//! `std/` of a source checkout. Nothing here names a language.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{DepCfg, DependenciesCfg, ModuleSource};

/// The descriptor's file name, in a module's directory.
pub const DESCRIPTOR: &str = "lumen-language.toml";

/// The directory a toolchain keeps its bundled modules' files in, beside its
/// binaries.
pub const BUNDLED_MODULE_DIR: &str = "modules";

/// The form a provider runs a program in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    /// Bytecode compiled ahead of time; the provider carries no compiler.
    Bytecode,
    /// Source text, compiled when the program loads.
    Source,
}

/// One module's claim to run a language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageProvider {
    /// The module's declared name, the one the loader opens it by.
    pub module: String,
    /// The language it runs.
    pub language: String,
    /// The file extensions the language owns, without the dot.
    pub extensions: Vec<String>,
    /// The form it runs a program in.
    pub form: Form,
    /// Whether the language is the one an inline `<script>` block is read as.
    pub default: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    language: LanguageSection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LanguageSection {
    name: String,
    #[serde(default)]
    extensions: Vec<String>,
    form: Form,
    #[serde(default)]
    default: bool,
}

/// Read the descriptor `text` of the module `module`.
///
/// # Errors
///
/// The text is not a descriptor, or names a language with no name.
pub fn parse_descriptor(module: &str, text: &str) -> Result<LanguageProvider, String> {
    let descriptor: Descriptor =
        toml::from_str(text).map_err(|e| format!("module '{module}': {DESCRIPTOR}: {e}"))?;
    let section = descriptor.language;
    if section.name.trim().is_empty() {
        return Err(format!(
            "module '{module}': {DESCRIPTOR} names a language with an empty name"
        ));
    }
    Ok(LanguageProvider {
        module: module.to_string(),
        language: section.name,
        extensions: section
            .extensions
            .into_iter()
            .map(|e| e.trim_start_matches('.').to_string())
            .collect(),
        form: section.form,
        default: section.default,
    })
}

/// Every language provider in reach, and the questions an app assembly asks
/// of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageTable {
    providers: Vec<LanguageProvider>,
}

impl LanguageTable {
    /// A table over `providers`. A module listed twice keeps its first entry.
    ///
    /// # Errors
    ///
    /// Two different languages both claim to be the default, or two languages
    /// claim one extension.
    pub fn new(providers: Vec<LanguageProvider>) -> Result<Self, String> {
        let mut kept: Vec<LanguageProvider> = Vec::new();
        for provider in providers {
            if kept.iter().any(|p| p.module == provider.module) {
                continue;
            }
            kept.push(provider);
        }
        let mut defaults: Vec<&str> = kept
            .iter()
            .filter(|p| p.default)
            .map(|p| p.language.as_str())
            .collect();
        defaults.sort_unstable();
        defaults.dedup();
        if defaults.len() > 1 {
            return Err(format!(
                "script languages {} each claim to be the default; one language reads inline \
                 `<script>` blocks, so at most one module descriptor may say `default = true`",
                defaults.join(" and ")
            ));
        }
        let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
        for provider in &kept {
            for ext in &provider.extensions {
                match owners.insert(ext, &provider.language) {
                    Some(other) if other != provider.language => {
                        return Err(format!(
                            "script languages {other} and {} both claim the `.{ext}` extension",
                            provider.language
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(Self { providers: kept })
    }

    /// The descriptors in reach of this process: `modules/<name>/` under
    /// `lib_dir`, beside the running executable, beside the shared engine
    /// library, and under `LUMEN_LIB_DIR`; then the ones the modules linked
    /// into this binary registered; then the crates under `std/` of the source
    /// checkout this crate was built in, when that checkout is still on disk.
    /// The first descriptor found for a module is the one read.
    ///
    /// # Errors
    ///
    /// A descriptor does not parse, or the descriptors disagree (see
    /// [`Self::new`]).
    pub fn discover(lib_dir: Option<&Path>) -> Result<Self, String> {
        let mut providers = Vec::new();
        for root in search_roots(lib_dir) {
            let Ok(entries) = std::fs::read_dir(root.join(BUNDLED_MODULE_DIR)) else {
                continue;
            };
            let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort();
            for dir in dirs {
                let file = dir.join(DESCRIPTOR);
                let Ok(text) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let module = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                providers.push(parse_descriptor(&module, &text)?);
            }
        }
        #[cfg(feature = "loader")]
        for (module, text) in lumen_module_registry::languages() {
            providers.push(parse_descriptor(module, text)?);
        }
        for (module, text) in checkout_descriptors() {
            providers.push(parse_descriptor(&module, &text)?);
        }
        Self::new(providers)
    }

    /// Every provider, in the order they were found.
    pub fn providers(&self) -> &[LanguageProvider] {
        &self.providers
    }

    /// Whether no provider is in reach.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The language a script file belongs to, read off its extension.
    pub fn language_of(&self, path: &Path) -> Option<&str> {
        let ext = path.extension()?.to_str()?;
        self.providers
            .iter()
            .find(|p| p.extensions.iter().any(|e| e == ext))
            .map(|p| p.language.as_str())
    }

    /// The language an inline `<script>` block is read as.
    pub fn default_language(&self) -> Option<&str> {
        self.providers
            .iter()
            .find(|p| p.default)
            .map(|p| p.language.as_str())
    }

    /// Every extension a language owns.
    pub fn extensions(&self) -> Vec<&str> {
        let mut all: Vec<&str> = self
            .providers
            .iter()
            .flat_map(|p| p.extensions.iter().map(String::as_str))
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    /// The provider that runs `language` from source: the edit loop's, which
    /// can check, compile, and reload a program.
    pub fn source_provider(&self, language: &str) -> Option<&LanguageProvider> {
        self.provider(language, Form::Source)
    }

    /// The provider a shipped app runs `language` with: the bytecode one when
    /// the language has one, the source one otherwise.
    pub fn shipped_provider(&self, language: &str) -> Option<&LanguageProvider> {
        self.provider(language, Form::Bytecode)
            .or_else(|| self.provider(language, Form::Source))
    }

    fn provider(&self, language: &str, form: Form) -> Option<&LanguageProvider> {
        self.providers
            .iter()
            .find(|p| p.language == language && p.form == form)
    }
}

/// An app's scripts split by the language that runs them: one entry per
/// language, each holding that language's whole program, sorted by language
/// name. Empty when the app ships no script.
pub type GroupedScripts = Vec<(String, String)>;

/// Which language each part of an app's script belongs to.
///
/// `engine` is the app's `[script] engine` key, which puts every script, inline
/// and external, on one language. Otherwise each external file belongs to its
/// extension's language (an extension no language claims reads as the
/// default language), and the inline block joins the app's one external
/// language when there is exactly one, and the default language otherwise.
#[derive(Debug, Clone, Copy)]
pub struct ScriptGrouping<'a> {
    /// The descriptors in reach.
    pub table: &'a LanguageTable,
    /// The `[script] engine` override.
    pub engine: Option<&'a str>,
}

impl ScriptGrouping<'_> {
    /// The language an external script file belongs to.
    pub fn external_language(&self, rel: &str) -> String {
        if let Some(engine) = self.engine {
            return engine.to_string();
        }
        self.table
            .language_of(Path::new(rel))
            .or_else(|| self.table.default_language())
            .unwrap_or_default()
            .to_string()
    }

    /// The language the inline block belongs to, given the app's external
    /// script files.
    pub fn inline_language(&self, externals: &[&str]) -> String {
        if let Some(engine) = self.engine {
            return engine.to_string();
        }
        let mut languages: Vec<String> = externals
            .iter()
            .map(|rel| self.external_language(rel))
            .collect();
        languages.dedup();
        match languages.as_slice() {
            [only] => only.clone(),
            _ => self
                .table
                .default_language()
                .unwrap_or_default()
                .to_string(),
        }
    }

    /// The languages an app needs, from whether it has an inline block and
    /// the paths of its external script files: what a run loads hosts for
    /// before anything is parsed.
    pub fn languages(&self, has_inline: bool, externals: &[&str]) -> Vec<String> {
        let mut needed: Vec<String> = externals
            .iter()
            .map(|rel| self.external_language(rel))
            .collect();
        if has_inline {
            needed.push(self.inline_language(externals));
        }
        needed.sort();
        needed.dedup();
        needed
    }

    /// Split an app's script by language. `inline` is the inline block,
    /// `externals` each external file's path and text, in source order.
    pub fn group(&self, inline: &str, externals: &[(&str, String)]) -> GroupedScripts {
        let rels: Vec<&str> = externals.iter().map(|(rel, _)| *rel).collect();
        let mut sources: GroupedScripts = Vec::new();
        let mut push = |language: String, body: &str| {
            if body.trim().is_empty() {
                return;
            }
            match sources.iter_mut().find(|(l, _)| *l == language) {
                Some((_, acc)) => {
                    acc.push('\n');
                    acc.push_str(body);
                }
                None => sources.push((language, body.to_string())),
            }
        };
        push(self.inline_language(&rels), inline);
        for (rel, body) in externals {
            push(self.external_language(rel), body);
        }
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        sources
    }
}

/// `deps` with a bundled entry added for each module in `modules` it does not
/// already declare: the host modules an app needs, loaded the way a declared
/// module is.
pub fn with_implied(deps: &DependenciesCfg, modules: &[String]) -> DependenciesCfg {
    let mut all = deps.clone();
    for module in modules {
        if !all.0.iter().any(|d| d.name == *module) {
            all.0.push(DepCfg {
                name: module.clone(),
                source: ModuleSource::Bundled,
                config: toml::Table::new(),
                tags: Vec::new(),
            });
        }
    }
    all
}

/// The directories `modules/<name>/` is looked for under, in order.
fn search_roots(lib_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = lib_dir {
        roots.push(dir.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        roots.push(dir.to_path_buf());
    }
    #[cfg(feature = "loader")]
    if let Some(dir) = crate::engine_dir() {
        roots.push(dir);
    }
    if let Some(dir) = std::env::var_os("LUMEN_LIB_DIR") {
        roots.push(PathBuf::from(dir));
    }
    roots.dedup();
    roots
}

/// The descriptors under `std/` of the checkout this crate was compiled in,
/// keyed by package name. A release build's tree is gone by the time anyone
/// runs it, so there this finds nothing.
fn checkout_descriptors() -> Vec<(String, String)> {
    let Some(std_dir) = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join("std"))
        .find(|dir| dir.is_dir())
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&std_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| {
            let text = std::fs::read_to_string(dir.join(DESCRIPTOR)).ok()?;
            Some((package_name(&dir)?, text))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(
        module: &str,
        language: &str,
        ext: &str,
        form: Form,
        default: bool,
    ) -> LanguageProvider {
        LanguageProvider {
            module: module.into(),
            language: language.into(),
            extensions: vec![ext.into()],
            form,
            default,
        }
    }

    fn table() -> LanguageTable {
        LanguageTable::new(vec![
            provider("toy-vm", "toy", "toy", Form::Bytecode, true),
            provider("toy-dev", "toy", "toy", Form::Source, true),
            provider("old", "old", "old", Form::Source, false),
        ])
        .expect("one default language")
    }

    #[test]
    fn a_descriptor_reads_into_a_provider() {
        let p = parse_descriptor(
            "toy-dev",
            "[language]\nname = \"toy\"\nextensions = [\".toy\"]\nform = \"source\"\ndefault = true\n",
        )
        .expect("parses");
        assert_eq!(p, provider("toy-dev", "toy", "toy", Form::Source, true));
        assert!(parse_descriptor("x", "[language]\nname = \"\"\nform = \"source\"\n").is_err());
        assert!(parse_descriptor("x", "[language]\nname = \"a\"\nform = \"jit\"\n").is_err());
    }

    #[test]
    fn two_default_languages_are_refused() {
        let refusal = LanguageTable::new(vec![
            provider("a", "a", "a", Form::Source, true),
            provider("b", "b", "b", Form::Source, true),
        ])
        .expect_err("two defaults");
        assert!(refusal.contains("a and b"), "{refusal}");
    }

    #[test]
    fn two_languages_claiming_one_extension_are_refused() {
        assert!(
            LanguageTable::new(vec![
                provider("a", "a", "x", Form::Source, false),
                provider("b", "b", "x", Form::Source, false),
            ])
            .is_err()
        );
    }

    #[test]
    fn a_shipped_app_takes_the_bytecode_provider_when_there_is_one() {
        let t = table();
        assert_eq!(
            t.shipped_provider("toy").map(|p| p.module.as_str()),
            Some("toy-vm")
        );
        assert_eq!(
            t.source_provider("toy").map(|p| p.module.as_str()),
            Some("toy-dev")
        );
        assert_eq!(
            t.shipped_provider("old").map(|p| p.module.as_str()),
            Some("old")
        );
        assert!(t.shipped_provider("nope").is_none());
    }

    #[test]
    fn scripts_group_by_extension_and_inline_follows_the_one_external_language() {
        let t = table();
        let g = ScriptGrouping {
            table: &t,
            engine: None,
        };
        let grouped = g.group(
            "inline();",
            &[("a.old", "a();".into()), ("b.old", "b();".into())],
        );
        assert_eq!(
            grouped,
            vec![("old".into(), "inline();\na();\nb();".into())]
        );

        let mixed = g.group(
            "inline();",
            &[("a.old", "a();".into()), ("m.toy", "m();".into())],
        );
        assert_eq!(
            mixed,
            vec![
                ("old".into(), "a();".into()),
                ("toy".into(), "inline();\nm();".into())
            ]
        );
        assert_eq!(g.languages(true, &[]), vec!["toy".to_string()]);
        assert!(g.languages(false, &[]).is_empty());
    }

    #[test]
    fn the_engine_key_puts_every_script_on_one_language() {
        let t = table();
        let g = ScriptGrouping {
            table: &t,
            engine: Some("old"),
        };
        let grouped = g.group("i();", &[("m.toy", "m();".into())]);
        assert_eq!(grouped, vec![("old".into(), "i();\nm();".into())]);
    }

    #[test]
    fn implied_modules_join_the_declared_ones_once() {
        let declared = DependenciesCfg(vec![DepCfg {
            name: "toy-dev".into(),
            source: ModuleSource::Path("x".into()),
            config: toml::Table::new(),
            tags: Vec::new(),
        }]);
        let all = with_implied(&declared, &["toy-dev".into(), "old".into()]);
        assert_eq!(all.0.len(), 2);
        assert_eq!(all.0[0].source, ModuleSource::Path("x".into()));
        assert_eq!(all.0[1].source, ModuleSource::Bundled);
    }
}
