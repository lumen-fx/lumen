//! `lumenc` library. Parses Lumen markup into a [`LayoutIR`] tree and spawns it into an ECS world via [`run_app`].
//!
//! Supported tags (all are styled containers; the tag selects defaults - for example `<column>` defaults `flex="column"`):
//!
//! | Tag        | Notes                                                 |
//! |------------|-------------------------------------------------------|
//! | `<root>`   | Top-level; defaults to width=100% height=100%.        |
//! | `<column>` | flex=column.                                          |
//! | `<row>`    | flex=row.                                             |
//! | `<scroll>` | Adds a `Scroll` component (plus `ScrollOffset` etc.). |
//! | `<tile>`   | Styled clickable box; defaults BackgroundColor.       |
//! | `<label>`  | Carries TextContent.                                  |
//! | `<div>`    | Generic container, no defaults.                       |
//!
//! Supported attributes:
//!
//! | Attribute      | Type / values                                              |
//! |----------------|------------------------------------------------------------|
//! | `width`        | `auto`, `<n>px`, `<n>%`                                    |
//! | `height`       | same                                                       |
//! | `flex`         | `row`, `column`                                            |
//! | `bg`           | `#rrggbb` or `#rrggbbaa`                                   |
//! | `radius`       | pixels                                                     |
//! | `padding`      | `<n>` or `<l> <r> <t> <b>` (all px)                        |
//! | `margin`       | same                                                       |
//! | `text`         | text content                                               |
//! | `text-color`   | `#rrggbb` or `#rrggbbaa`                                   |
//! | `scroll`       | `y`, `x`, `both` (auto-implied on `<scroll>`)              |
//! | `sensitivity`  | `f32` (forwarded to `Scroll::sensitivity`)                 |
//! | `inertia`      | `f32` (forwarded to `Scroll::inertia`)                     |
//! | `tab-index`    | `i32`                                                      |

// `deny` (not `forbid`) so the single audited dlopen shim in `loader`, the
// link-not-embed launcher's only unsafe, can opt in via `#[allow]`. Every
// other module stays unsafe-free and trips the deny.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// `lumenc build` - parse an app once and emit an AOT [`artifact`].
/// Requires the source parser (`runtime-parse`) AND the runtime (`dev-run`):
/// it drives `compile_app` + `app_kind`, both of which live in `lumen-runtime`.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod build_cli;
/// `lumenc bundle` - pack an app dir into a `.lpak` archive. Uses lumen-assets
/// (which pulls vello), so it is gated behind the default-on `bundle` feature.
#[cfg(feature = "bundle")]
pub mod bundle_cli;
/// In-process source -> LMNA compile for the link-not-embed launcher. Uses only
/// the parser front-end + CSS cascade + artifact codec (no `lumen-runtime`), so
/// a `dlopen-run` launcher compiles source without static-linking the runtime.
/// Gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod compile;
/// Markup formatter - requires `roxmltree`, gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod formatter;
pub mod i18n_cli;
/// Static signal lint - walks the source parser (`runtime-parse`) and reads
/// `lumen.toml` config (`lumen-runtime`, `dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod lint_signals_cli;
/// dlopen loader for the link-not-embed launcher: discover + open the shared
/// liblumen, verify its ABI, and drive a prebuilt LMNA app across the C-ABI.
/// The crate's only `unsafe`: dynamic symbol resolution and FFI calls, audited
/// against the C-ABI contract in `lumen/ffi`.
#[cfg(feature = "dlopen-run")]
#[allow(unsafe_code)]
pub mod loader;
/// MCP CLI handlers - read `lumen.toml` config (`dev-run`) and defer the
/// `--signals` lint to [`lint_signals_cli`] (`runtime-parse`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod mcp_cli;
pub mod parser_css;
/// Markup (`.lmn`) parser - the `roxmltree`-backed front-end, dropped from
/// parser-free runtime builds via the `runtime-parse` feature.
#[cfg(feature = "runtime-parse")]
pub mod parser_html;
/// `<include>` / `@import` resolution - parser-side only.
#[cfg(feature = "runtime-parse")]
pub mod resolve;
pub mod scaffold;
/// The compiler's implementation of the runtime's injected parser boundary.
/// Needs the source parser (`runtime-parse`) AND the runtime's `SourceParser`
/// trait (`dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod source_parser;

// The runtime core - the winit/ECS run loop, `RunOptions`/`RunError`,
// `build_app`, hot reload, the default plugin stack, file-based pages,
// `lumen.toml` config, SDK app-kind dispatch, the offscreen-headless path, the
// IR spawner, window-geometry persistence, embedded skins, and the profiler
// install - was carved out into `lumen-runtime`. Re-export those modules under
// their historical names so every `lumenc::{run,spawn,pages,config,...}::...`
// path (internal `crate::...` refs and external consumers alike) keeps resolving
// after the extraction.
#[cfg(all(feature = "devtools", feature = "dev-run"))]
pub use lumen_runtime::devtools_mount;
#[cfg(feature = "dev-run")]
pub use lumen_runtime::{
    app_kind, config, pages, profile, run, run_headless, skins, spawn, window_state,
};

// The IR data model - LayoutIR, the CSS AST + Cascade-5 application, the
// shared value parsers, the `var()` resolver, and the AOT compiled-app
// artifact - lives in `lumen-ir`. Re-export those modules under their
// historical names so every `lumenc::{artifact,layout_ir,values,css_vars}::...`
// path (internal `crate::...` refs and external consumers alike) resolves
// unchanged after the extraction.
pub use lumen_ir::{artifact, css_vars, layout_ir, values};

pub use artifact::{ArtifactError, CompiledApp};
#[cfg(feature = "dev-run")]
pub use config::{ConfigError, LumenToml};
pub use layout_ir::{
    Edges, Element, LayoutIR, LengthSpec, LintFinding, LintKind, LintSeverity, ParseError,
};
#[cfg(feature = "dev-run")]
pub use lumen_runtime::{
    AppHook, CheckReport, HeadlessOptions, RunError, RunOptions, SourceParser,
};
pub use parser_css::{CssWarning, Stylesheet, apply_css, parse_css};
#[cfg(feature = "runtime-parse")]
pub use parser_html::{parse_html, parse_html_with_loader};
#[cfg(feature = "runtime-parse")]
pub use resolve::{FileLoader, FsLoader};
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub use source_parser::LumencParser;

/// The compiler's default markup/CSS front-end, boxed for injection into
/// [`RunOptions::parser`]. The SDKs and the C-ABI hand this to the runtime so a
/// from-source run can re-parse (`lumen-runtime` links no parser itself).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn default_parser() -> Box<dyn SourceParser> {
    Box::new(source_parser::LumencParser)
}

/// Inject the compiler's default [`SourceParser`] into `opts` when the caller
/// hasn't supplied one, so `lumenc`'s own CLI (`run` / `--headless`) parses
/// from source without every call site wiring the hook by hand.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
fn with_default_parser(mut opts: RunOptions) -> RunOptions {
    if opts.parser.is_none() {
        opts.parser = Some(default_parser());
    }
    opts
}
#[cfg(all(not(feature = "runtime-parse"), feature = "dev-run"))]
fn with_default_parser(opts: RunOptions) -> RunOptions {
    opts
}

/// Run a markup app, injecting the compiler's default parser. See
/// [`lumen_runtime::run_app`].
#[cfg(feature = "dev-run")]
pub fn run_app(opts: RunOptions) -> Result<(), RunError> {
    lumen_runtime::run_app(with_default_parser(opts))
}

/// Headless (window-free) run, injecting the compiler's default parser. See
/// [`lumen_runtime::run_app_headless`].
#[cfg(feature = "dev-run")]
pub fn run_app_headless(opts: RunOptions, ticks: u32) -> Result<(), RunError> {
    lumen_runtime::run_app_headless(with_default_parser(opts), ticks)
}

/// Build the app window-free, injecting the compiler's default parser. See
/// [`lumen_runtime::build_headless_app`].
#[cfg(feature = "dev-run")]
pub fn build_headless_app(
    opts: RunOptions,
) -> Result<(lumen_core::app::App, lumen_window_winit::WinitOptions), RunError> {
    lumen_runtime::build_headless_app(with_default_parser(opts))
}

/// Rendered offscreen headless run, injecting the compiler's default parser.
/// See [`lumen_runtime::run_app_headless_rendered`].
#[cfg(feature = "dev-run")]
pub fn run_app_headless_rendered(
    opts: RunOptions,
    headless: HeadlessOptions,
) -> Result<(), RunError> {
    lumen_runtime::run_app_headless_rendered(with_default_parser(opts), headless)
}

/// Minimal-boilerplate entry point: run `dir` with one native Rhai extension,
/// injecting the compiler's default parser. See [`lumen_runtime::run_with`].
#[cfg(feature = "dev-run")]
pub fn run_with<F>(dir: impl Into<std::path::PathBuf>, extend: F) -> Result<(), RunError>
where
    F: FnOnce(&mut rhai::Engine) + Send + 'static,
{
    run_app(RunOptions::new(dir).with_rhai_extension(extend))
}

/// Parse + validate an app from source (`lumenc check`), using the compiler's
/// default parser. See [`lumen_runtime::check_app`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn check_app(dir: &std::path::Path) -> Result<CheckReport, RunError> {
    lumen_runtime::check_app(dir, &source_parser::LumencParser)
}

/// AOT-compile an app from source (`lumenc build`), using the compiler's
/// default parser. See [`lumen_runtime::compile_app`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app(dir: &std::path::Path) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    lumen_runtime::compile_app(dir, &source_parser::LumencParser)
}
