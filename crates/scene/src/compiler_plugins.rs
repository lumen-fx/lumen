//! Injected compiler-plugin boundary.
//!
//! Compiler plugins are loaded and driven by the compiler (`lumenc`), but the
//! compile pipeline they hook into (`load_ir`, `compile_app`, hot reload)
//! lives in the runtime crate, which links no plugin loader for the same
//! reason it links no parser (see [`crate::source_parser`]): the dependency
//! would run the wrong way. The chain is injected instead:
//!
//! - The CLI, the Rust SDK, and the C-ABI dev paths hand a
//!   [`CompilerPlugins`] to `RunOptions`, built by `lumenc` from the app's
//!   `[[plugins]]` declarations.
//! - The dev source-load path and hot reload call it.
//! - The precompiled-artifact path already carries the transformed tree and
//!   ignores this hook.
//!
//! Errors are `String`s that already name the failing plugin; a hook error
//! fails the compile with it.

use bevy_ecs::prelude::Resource;
use lumen_ir::layout_ir::LayoutIR;
use std::path::Path;
use std::sync::Arc;

/// The loaded compiler-plugin chain of one app. Implemented in `lumenc` over
/// the `lumenc-plugin` loader; every hook runs the declared plugins in
/// `lumen.toml` order.
pub trait CompilerPlugins: Send + Sync {
    /// Rewrite the entry markup text, before `<include>` splicing.
    fn transform_markup(&self, src: String, entry: &Path) -> Result<String, String>;
    /// Rewrite the entry stylesheet text, before `@import` splicing.
    fn transform_css(&self, src: String, entry: &Path, file: &Path) -> Result<String, String>;
    /// Transform the assembled tree, before asset resolution and the cascade.
    fn transform_ir(&self, ir: &mut LayoutIR, entry: &Path) -> Result<(), String>;
    /// Lint the cascaded tree and write emit outputs. Returns rendered
    /// diagnostic lines for the caller to print beside the built-in lint
    /// findings.
    fn finish(&self, ir: &LayoutIR, entry: &Path) -> Result<Vec<String>, String>;
}

/// The empty chain: every hook is a no-op. What every path without a
/// `[[plugins]]` declaration runs.
pub struct NoCompilerPlugins;

impl CompilerPlugins for NoCompilerPlugins {
    fn transform_markup(&self, src: String, _entry: &Path) -> Result<String, String> {
        Ok(src)
    }
    fn transform_css(&self, src: String, _entry: &Path, _file: &Path) -> Result<String, String> {
        Ok(src)
    }
    fn transform_ir(&self, _ir: &mut LayoutIR, _entry: &Path) -> Result<(), String> {
        Ok(())
    }
    fn finish(&self, _ir: &LayoutIR, _entry: &Path) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// The injected chain, stashed as a resource beside
/// [`crate::source_parser::RuntimeParser`] so a hot reload reruns the same
/// hooks without reloading the plugin libraries.
#[derive(Resource, Clone)]
pub struct RuntimeCompilerPlugins(pub Arc<dyn CompilerPlugins>);
