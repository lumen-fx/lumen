//! The compiler side of the injected compiler-plugin boundary.
//!
//! `lumen-scene` declares the [`CompilerPlugins`] trait the pipeline calls
//! (the same inversion the parser uses); this module implements it over the
//! `lumenc-plugin` loader and builds the chain from an app's `[[plugins]]`
//! declarations. The runtime never links the loader; every path that
//! compiles from source gets its chain from here.

use std::path::Path;
#[cfg(feature = "dev-run")]
use std::sync::Arc;

#[cfg(feature = "dev-run")]
use lumen_runtime::compiler_plugins::{CompilerPlugins, NoCompilerPlugins};
#[cfg(feature = "dev-run")]
use lumenc_plugin::{PluginSet, SourceKind};

use lumenc_plugin::PluginCfg;

#[cfg(feature = "dev-run")]
use lumen_ir::layout_ir::LayoutIR;

/// A loaded plugin chain behind the scene-facing trait.
#[cfg(feature = "dev-run")]
pub struct InstalledCompilerPlugins(pub PluginSet);

#[cfg(feature = "dev-run")]
impl CompilerPlugins for InstalledCompilerPlugins {
    fn transform_markup(&self, src: String, entry: &Path) -> Result<String, String> {
        self.0
            .transform_source(SourceKind::Markup, src, entry, entry)
            .map_err(|e| e.to_string())
    }

    fn transform_css(&self, src: String, entry: &Path, file: &Path) -> Result<String, String> {
        self.0
            .transform_source(SourceKind::Css, src, entry, file)
            .map_err(|e| e.to_string())
    }

    fn transform_ir(&self, ir: &mut LayoutIR, entry: &Path) -> Result<(), String> {
        self.0.transform_ir(ir, entry).map_err(|e| e.to_string())
    }

    fn finish(&self, ir: &LayoutIR, entry: &Path) -> Result<Vec<String>, String> {
        let findings = self.0.finish(ir, entry).map_err(|e| e.to_string())?;
        Ok(PluginSet::render_findings(&findings, entry))
    }
}

/// Build the plugin chain an app directory declares. Reads `[[plugins]]`
/// straight from `lumen.toml`, resolves each source (probing `path`s,
/// resolving `version`s through the cache and `lumen.lock`), dlopens the
/// libraries, and verifies each handshake. An app with no declarations gets
/// the empty chain.
///
/// `check_only` marks the chain for `lumenc check`: hooks still run, emit
/// outputs are discarded.
#[cfg(feature = "dev-run")]
pub fn compiler_plugins_for(
    dir: &Path,
    check_only: bool,
) -> Result<Arc<dyn CompilerPlugins>, String> {
    let cfgs = read_plugin_cfgs(dir)?;
    if cfgs.is_empty() {
        return Ok(Arc::new(NoCompilerPlugins));
    }
    let set = PluginSet::load(dir, &cfgs)
        .map_err(|e| e.to_string())?
        .check_only(check_only);
    Ok(Arc::new(InstalledCompilerPlugins(set)))
}

/// The `[[plugins]]` declarations of `<dir>/lumen.toml`. A missing or
/// unreadable file is an empty set (matching `LumenToml::load_or_default`'s
/// leniency); a present array with a malformed entry is an error naming it.
pub fn read_plugin_cfgs(dir: &Path) -> Result<Vec<PluginCfg>, String> {
    let Ok(text) = std::fs::read_to_string(dir.join("lumen.toml")) else {
        return Ok(Vec::new());
    };
    let Ok(doc) = toml::from_str::<toml::Table>(&text) else {
        return Ok(Vec::new());
    };
    PluginCfg::from_document(&doc)
}
