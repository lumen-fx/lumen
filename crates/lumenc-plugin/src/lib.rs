//! SDK for lumenc compiler plugins.
//!
//! A compiler plugin is a Rust cdylib that lumenc loads while compiling an
//! app. It can rewrite the entry markup and CSS before parsing, transform the
//! parsed tree before the cascade, lint the cascaded tree, and emit extra
//! build outputs. Authors implement [`CompilerPlugin`] and export it with
//! [`lumenc_plugin!`]; no unsafe code is involved on the author side.
//!
//! The plugin and the compiler exchange bytes over a C ABI ([`abi`]), so a
//! plugin works with the prebuilt lumenc binary as long as both were built
//! from the same release tag (enforced by a version handshake at load).

pub mod abi;
pub mod codec;
mod config;
#[doc(hidden)]
pub mod export;
#[cfg(feature = "host")]
mod host;
#[cfg(feature = "host")]
pub mod resolve;
#[cfg(feature = "testing")]
pub mod testing;

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use config::{PluginCfg, PluginSource, resolve_plugin_path};
#[cfg(feature = "host")]
pub use host::{PluginError, PluginSet, SourceKind};
/// The IR the hooks operate on, re-exported so a plugin crate never declares
/// its own `lumen-ir` dependency (a second copy would not exist, but the
/// version pin lives here either way).
pub use lumen_ir;
pub use lumen_ir::layout_ir::LayoutIR;

/// What a compiler plugin can do. Every hook has a default no-op body;
/// implement the ones the plugin needs.
///
/// One instance serves the whole process and hooks take `&self`, so a plugin
/// holding mutable state brings its own lock (the `Send + Sync` bound is what
/// makes that explicit).
pub trait CompilerPlugin: Send + Sync + 'static {
    /// Rewrite the entry markup text, before `<include>` splicing. Return
    /// `Ok(None)` to leave it unchanged. Emitted `<include>` and `@import`
    /// directives are resolved afterwards as if hand-written.
    fn transform_markup(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error> {
        let _ = (src, ctx);
        Ok(None)
    }

    /// Rewrite the entry stylesheet text, before `@import` splicing. Return
    /// `Ok(None)` to leave it unchanged.
    fn transform_css(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error> {
        let _ = (src, ctx);
        Ok(None)
    }

    /// Transform the parsed tree. Runs after multi-page assembly and before
    /// asset resolution and the cascade, so injected elements get asset
    /// paths resolved and styles applied like hand-written ones.
    fn transform_ir(&self, ir: &mut LayoutIR, ctx: &Ctx) -> Result<(), Error> {
        let _ = (ir, ctx);
        Ok(())
    }

    /// Read-only pass over the cascaded tree. Findings print beside the
    /// built-in lint findings; they are advisory and never fail the build.
    fn lint(&self, ir: &LayoutIR, ctx: &Ctx) -> Result<Vec<Finding>, Error> {
        let _ = (ir, ctx);
        Ok(Vec::new())
    }

    /// Extra build products, written under `.lumen/generated/<plugin>/` in
    /// the app directory. Outputs are side products (manifests, reports,
    /// generated sources for the next compile), not inputs to this one.
    fn emit(&self, ir: &LayoutIR, ctx: &Ctx) -> Result<Vec<Output>, Error> {
        let _ = (ir, ctx);
        Ok(Vec::new())
    }
}

/// What a hook knows about the compile it runs inside.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ctx {
    /// The app directory being compiled.
    pub app_dir: PathBuf,
    /// The markup entry file.
    pub entry: PathBuf,
    /// The file this call is about; equals `entry` for the IR, lint, and
    /// emit hooks.
    pub file: PathBuf,
    /// True under `lumenc check`: hooks still run, but emit outputs are
    /// discarded instead of written.
    pub check_only: bool,
    /// This plugin's own `config` table from `lumen.toml`, re-serialized.
    /// Read it through [`Ctx::config`].
    config_toml: String,
}

impl Ctx {
    /// Build a context. Host-side; a plugin only ever reads one.
    pub fn new(
        app_dir: PathBuf,
        entry: PathBuf,
        file: PathBuf,
        check_only: bool,
        config_toml: String,
    ) -> Self {
        Ctx {
            app_dir,
            entry,
            file,
            check_only,
            config_toml,
        }
    }

    /// Deserialize the plugin's `config` table from `lumen.toml` into any
    /// serde type. An app that declares no table yields the type's view of
    /// an empty table.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, Error> {
        toml::from_str(&self.config_toml).map_err(Error::from)
    }
}

/// Severity of a plugin lint [`Finding`]. Matches the tiers of the built-in
/// lints; every tier is advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warn,
    Info,
    Hint,
}

impl Severity {
    /// The lowercase label the diagnostic line starts with.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warn => "warn",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// One diagnostic from a plugin's lint hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Short rule name; printed as `[<plugin>/<rule>]`.
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    /// File the finding anchors to; `None` anchors to the entry file.
    pub file: Option<PathBuf>,
    /// 1-based; 0 when unknown.
    pub line: usize,
    /// 1-based; 0 when unknown.
    pub col: usize,
    /// Machine-applicable replacement, when one exists.
    pub suggest: Option<String>,
}

/// One file the emit hook produces. `path` is relative and stays inside the
/// plugin's own directory under `.lumen/generated/`; absolute paths and `..`
/// are rejected by the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A hook failure. The compile fails with this message, prefixed with the
/// plugin's name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
}

// Blanket over Display so `?` works on any error in a hook body. This
// compiles only while `Error` itself implements neither Display nor
// std::error::Error; adding either collides with core's reflexive
// `From<T> for T`. If that trade ever needs reversing, replace this with
// explicit From impls for the common error types.
impl<E: std::fmt::Display> From<E> for Error {
    fn from(e: E) -> Self {
        Error {
            message: e.to_string(),
        }
    }
}
