//! `lumenc` library. Parses Lumen markup into a [`LayoutIR`] tree and spawns it into an ECS world via [`run_app`].
//!
//! Every tag is a styled container and the tag selects the defaults: `<column>` sets `flex="column"`, `<scroll>`
//! attaches the scroll components, `<root>` fills the viewport, and so on. Attributes cover sizing, spacing, paint,
//! typography, scrolling, interaction, and binding.
//!
//! The accepted tags live in `KNOWN_TAGS` in [`parser_html`], with the per-tag attribute handling beside it; the
//! reader-facing lists are the "Tags and attributes" and "CSS" reference pages in `docs/docs/reference/`.

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
/// Filling a component that has to run while the site is built, so its body is
/// in the page a crawler reads. Needs what `web_cli` needs.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod component_fill;
/// Markup formatter - requires `roxmltree`, gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod formatter;
/// Fragment instantiation, gated with the parser stack that produces the
/// use sites it resolves.
#[cfg(feature = "runtime-parse")]
pub mod fragments;
pub mod i18n_cli;
/// Static signal lint - walks the source parser (`runtime-parse`) and reads
/// `lumen.toml` config (`lumen-runtime`, `dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod lint_signals_cli;
/// Ahead-of-time extraction of `lmn!` markup blocks from candela scripts, so
/// a shipped app carries the fragments they name and parses no markup at run
/// time. Gated with the parser stack it compiles bodies through.
#[cfg(feature = "runtime-parse")]
pub mod lmn;
/// dlopen loader for the link-not-embed launcher: discover + open the shared
/// liblumen, verify its ABI, and drive a prebuilt LMNA app across the C-ABI.
/// The crate's only `unsafe`: dynamic symbol resolution and FFI calls, audited
/// against the C-ABI contract in the root `lumen` crate.
#[cfg(feature = "dlopen-run")]
#[allow(unsafe_code)]
pub mod loader;
/// MCP CLI handlers - read `lumen.toml` config (`dev-run`) and defer the
/// `--signals` lint to [`lint_signals_cli`] (`runtime-parse`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod mcp_cli;
/// `lumenc package` - assemble a shippable app folder from the launcher stub,
/// the app's compiled artifact, the shared runtime library, and the app's own
/// files. Gated with the compile path it uses (`runtime-parse` + `dev-run`)
/// and with `package`, which carries the release-channel fetch `--target`
/// needs.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod package_cli;
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
/// `lumenc web` - emit an app as a static site. Compiles the app the way
/// `build` does, so it needs the same parser (`runtime-parse`) and runtime
/// (`dev-run`), plus the emitter behind the default-on `web` feature.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod web_cli;
/// The loopback HTTP server behind `lumenc web --serve`. A browser needs a
/// real origin and real content types to load a site; this is that, for one
/// directory on one machine.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod web_serve;
/// `lumenc web --ssr` - the server's pages come from a render of the app for
/// the request that asked, through [`lumen_ssr`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod web_ssr;

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
pub use lumen_ir::{artifact, css_vars, fragment, layout_ir, values};

pub use artifact::{ArtifactError, CompiledApp};
#[cfg(feature = "dev-run")]
pub use config::{ConfigError, LumenToml};
pub use layout_ir::{
    Edges, Element, LayoutIR, LengthSpec, LintFinding, LintKind, LintSeverity, ParseError,
};
#[cfg(feature = "dev-run")]
pub use lumen_runtime::{
    AppHook, CheckReport, HeadlessOptions, RunError, RunOptions, SourceParser, WindowSetup,
};
pub use parser_css::{CssWarning, Stylesheet, apply_css, parse_css};
#[cfg(feature = "runtime-parse")]
pub use parser_html::{
    ParsedMarkup, collect_fragments, collect_script_refs, parse_html, parse_html_with_loader,
    parse_markup,
};
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
) -> Result<(lumen_core::app::App, WindowSetup), RunError> {
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

/// True for the `--help` / `-h` spellings every subcommand answers with its
/// own usage block.
///
/// Deliberately not `help`: a bare word is a positional argument to several
/// subcommands (`lumenc new help`, `lumenc type help`), and reading it as a
/// flag would shadow them. The top-level `lumenc help` still prints the full
/// usage.
pub fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
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

/// AOT-compile an app from source with the skin named outright, which is what
/// `lumenc web` builds a site with. See [`lumen_runtime::compile_app_with_skin`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app_with_skin(
    dir: &std::path::Path,
    skin: Option<&str>,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    lumen_runtime::compile_app_with_skin(dir, &source_parser::LumencParser, skin)
}
