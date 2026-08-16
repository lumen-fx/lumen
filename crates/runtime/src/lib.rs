//! `lumen-runtime` - the "lumen.so core": the winit/ECS run loop, the default
//! plugin stack, script-command machinery, hot reload, file-based pages, and
//! the AOT-artifact / from-source app loaders.
//!
//! Carved out of `lumenc` so the SDKs (`lumenui`, the C-ABI `lumen`, and
//! everything downstream) depend on the runtime without pulling the compiler
//! front-end. The markup / CSS parser stays in `lumenc`; the runtime reaches
//! it only through the injected [`SourceParser`] hook (see
//! [`source_parser`]), so `lumen-runtime` links no parser and no `lumenc`.
//!
//! The IR data model - [`LayoutIR`](lumen_ir::layout_ir::LayoutIR), the CSS
//! AST + Cascade-5 application, the shared value parsers, the `var()`
//! resolver, and the AOT [`artifact`](lumen_ir::artifact) container - lives in
//! `lumen-ir` and is used here directly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// SDK app-kind detection + external (Rust/C++/Python) build/run dispatch.
pub mod app_kind;
/// Per-app `lumen.toml` configuration model.
pub mod config;
#[cfg(feature = "devtools")]
pub mod devtools_mount;
/// The app's compiled fragments and what an instance of one carries.
pub use lumen_scene::fragments;
/// `[[hooks]]` runner - executes an app's declared `prebuild` / `prerun`
/// build/setup commands. See [`config::HookCfg`] for the schema.
pub mod hooks;
/// File-based pages - multi-`.lmn` discovery, `<if>`-reconciler page mount,
/// and the navigation resolver reachable from every embedding surface.
pub mod pages;
/// `--profile chrome|stderr|tracy` profiler install (feature-gated).
pub mod profile;
/// The winit/ECS run loop, `RunOptions`/`RunError`, `build_app`, hot reload,
/// and the script-command machinery.
pub mod run;
/// Rendered offscreen headless mode (`lumenc run --headless`).
pub mod run_headless;
/// Embedded user-agent skin stylesheets.
pub mod skins;
/// The injected markup/CSS parser boundary. See [`SourceParser`].
pub mod source_parser;
/// IR -> ECS spawner and the `<for>` / `<if>` reconcilers.
pub use lumen_scene::spawn;
/// Windowed geometry persistence (`[window] remember_state`).
pub mod window_state;

pub use config::{ConfigError, LumenToml};
#[cfg(feature = "host-rhai")]
pub use run::run_with;
pub use run::{
    AppHook, CheckReport, RunError, RunOptions, WindowSetup, build_headless_app, run_app,
    run_app_headless,
};
#[cfg(feature = "runtime-parse")]
pub use run::{check_app, compile_app, compile_app_with_skin};
pub use run_headless::{HeadlessOptions, run_app_headless_rendered};
pub use source_parser::SourceParser;
