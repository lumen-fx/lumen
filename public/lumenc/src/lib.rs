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

// The linkage anchor for the `dynamic-engine` shape: naming the engine dylib
// is what puts `liblumen_engine` in the binary's link graph, and with
// `-C prefer-dynamic` the engine crates above resolve into it instead of
// compiling in. See the feature note in Cargo.toml.
#[cfg(all(feature = "dynamic-engine", not(windows)))]
use lumen_engine as _;

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
/// `lumenc add` / `remove` / `fetch` / `update` - the app's registry
/// dependencies from the command line. Gated with the registry client it
/// drives and the `lumen.toml` reader it edits.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod deps_cli;
/// Markup formatter - requires `roxmltree`, gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod formatter;
/// Fragment instantiation, gated with the parser stack that produces the
/// use sites it resolves.
#[cfg(feature = "runtime-parse")]
pub mod fragments;
/// `lumenc i18n extract` - scan an app's sources for translatable keys and
/// write its catalogue. Gated with `dev-run`: the source language it defaults
/// to is `[app] fallback_locale`, which it reads through the runtime's
/// `lumen.toml`, and a thin build has no runtime to read it with.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod i18n_cli;
/// `lumenc package --static` - link one executable out of the per-target link
/// kit a release publishes, with the app's declared runtime modules compiled
/// in. Gated with `package_cli`, whose folder assembly it is one arm of.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod link_kit;
/// `lumenc link-kit emit` - write the per-target link kit a release ships,
/// out of a recorded link and the files that link read. A release step rather
/// than a command anyone runs by hand, so it is absent from `lumenc --help`.
/// Gated with `package_cli`, whose target table names the release assets.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod link_kit_cli;
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
/// `lpm`, the registry client. A `version` source in `[dependencies]` or
/// `[[plugins]]` names a registry package, and this is what asks `lpm` to
/// resolve, download, and lock it. Gated with the shape that compiles an app
/// from source and can fetch: a compiler that only loads a prebuilt artifact
/// resolves nothing.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod lpm;
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
/// The compiler side of the injected compiler-plugin boundary: builds an
/// app's `[[plugins]]` chain over the `lumenc-plugin` loader.
pub mod plugin_host;
/// Which published release this toolchain draws its files from. Every download
/// location and cache directory is keyed by the answer.
pub mod release;
/// `<include>` / `@import` resolution - parser-side only.
#[cfg(feature = "runtime-parse")]
pub mod resolve;
pub mod scaffold;
/// The compiler's implementation of the runtime's injected parser boundary.
/// Needs the source parser (`runtime-parse`) AND the runtime's `SourceParser`
/// trait (`dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod source_parser;
/// The daily "a newer release exists" notice an installed toolchain prints.
pub mod update_check;
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
/// `lumenc web --render ssr --serve` - the server's pages come from a render
/// of the app for the request that asked, through [`lumen_ssr`].
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
#[cfg(feature = "dev-run")]
pub use lumen_runtime::{
    app_kind, app_layout, config, pages, profile, run, run_headless, skins, spawn, window_state,
};

// The IR data model - LayoutIR, the CSS AST + Cascade-5 application, the
// shared value parsers, the `var()` resolver, and the AOT compiled-app
// artifact - lives in `lumen-ir`. Re-export those modules under their
// historical names so every `lumenc::{artifact,layout_ir,values,css_vars}::...`
// path (internal `crate::...` refs and external consumers alike) resolves
// unchanged after the extraction.
pub use lumen_ir::{artifact, css_vars, fragment, layout_ir, translate, values};

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

/// Inject the compiler-side resolutions into `opts`, beside the parser
/// above: everything the app names in the registry, and the app's
/// `[[plugins]]` chain (when the caller hasn't supplied one).
///
/// A malformed plugin declaration, a failing plugin load, or a registry
/// requirement that does not resolve aborts the run here, before any window
/// exists. The precompiled-artifact paths skip the chain - the artifact
/// already carries the transformed tree - but not the resolutions: a compiled
/// app still loads its runtime modules.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn with_default_compiler_plugins(mut opts: RunOptions) -> Result<RunOptions, RunError> {
    let resolved = registry_packages(&opts.dir).map_err(RunError::Plugin)?;
    opts.resolved_modules = lumen_runtime::modules::ResolvedModules(
        resolved
            .modules
            .iter()
            .map(|(name, file)| (name.clone(), Ok(file.clone())))
            .collect(),
    );
    if opts.compiler_plugins.is_none() && opts.artifact.is_none() && opts.artifact_bytes.is_none() {
        let chain = plugin_host::compiler_plugins_for(&opts.dir, false, &resolved.compiler_plugins)
            .map_err(RunError::Plugin)?;
        opts = opts.with_compiler_plugins(chain);
    }
    Ok(opts)
}

/// A parser-free compiler compiles nothing: it runs a prebuilt artifact, so
/// there is no plugin chain to build and no registry requirement to read out
/// of a `lumen.toml` it does not parse. A `version` module reaches the
/// runtime's loader unresolved, which banners and keeps the app running.
#[cfg(all(not(feature = "runtime-parse"), feature = "dev-run"))]
pub fn with_default_compiler_plugins(opts: RunOptions) -> Result<RunOptions, RunError> {
    Ok(opts)
}

/// Every `version` source the app in `dir` declares, with the table that
/// declared it. `[dependencies]` entries become runtime packages and
/// `[[plugins]]` entries compiler plugins; the registry says which platform
/// each one is for, and the table says what the app wants it for.
///
/// A `lumen.toml` that does not parse yields no requirements: the run fails
/// in `build_app` with the real parse error, and a resolution failure here
/// would bury it.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn registry_requirements(dir: &std::path::Path) -> Result<Vec<lpm::Requirement>, String> {
    use lumen_runtime::modules::ModuleSource;

    let mut reqs = Vec::new();
    if let Ok(cfg) = LumenToml::load_or_default(dir) {
        for dep in &cfg.dependencies.0 {
            if let ModuleSource::Version(req) = &dep.source {
                reqs.push(lpm::Requirement {
                    name: dep.name.clone(),
                    req: req.clone(),
                    table: lpm::Table::Dependencies,
                });
            }
        }
    }
    for cfg in plugin_host::read_plugin_cfgs(dir)? {
        if let lumenc_plugin::PluginSource::Version(req) = &cfg.source {
            reqs.push(lpm::Requirement {
                name: cfg.name.clone(),
                req: req.clone(),
                table: lpm::Table::Plugins,
            });
        }
    }
    Ok(reqs)
}

/// Resolve everything the app in `dir` names in the registry, for this
/// machine's platform. `lumenc package --target` asks for another one.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn registry_packages(dir: &std::path::Path) -> Result<lpm::Resolved, String> {
    lpm::resolve(
        dir,
        lpm::host_target(),
        &registry_requirements(dir)?,
        lpm::Mode::of_invocation(),
    )
}

/// Run a markup app, injecting the compiler's default parser. See
/// [`lumen_runtime::run_app`].
#[cfg(feature = "dev-run")]
pub fn run_app(opts: RunOptions) -> Result<(), RunError> {
    lumen_runtime::run_app(with_default_compiler_plugins(with_default_parser(opts))?)
}

/// Headless (window-free) run, injecting the compiler's default parser. See
/// [`lumen_runtime::run_app_headless`].
#[cfg(feature = "dev-run")]
pub fn run_app_headless(opts: RunOptions, ticks: u32) -> Result<(), RunError> {
    lumen_runtime::run_app_headless(
        with_default_compiler_plugins(with_default_parser(opts))?,
        ticks,
    )
}

/// Build the app window-free, injecting the compiler's default parser. See
/// [`lumen_runtime::build_headless_app`].
#[cfg(feature = "dev-run")]
pub fn build_headless_app(
    opts: RunOptions,
) -> Result<(lumen_core::app::App, WindowSetup), RunError> {
    lumen_runtime::build_headless_app(with_default_compiler_plugins(with_default_parser(opts))?)
}

/// Rendered offscreen headless run, injecting the compiler's default parser.
/// See [`lumen_runtime::run_app_headless_rendered`].
#[cfg(feature = "dev-run")]
pub fn run_app_headless_rendered(
    opts: RunOptions,
    headless: HeadlessOptions,
) -> Result<(), RunError> {
    lumen_runtime::run_app_headless_rendered(
        with_default_compiler_plugins(with_default_parser(opts))?,
        headless,
    )
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
    let resolved = registry_packages(dir).map_err(RunError::Plugin)?;
    let plugins = plugin_host::compiler_plugins_for(dir, true, &resolved.compiler_plugins)
        .map_err(RunError::Plugin)?;
    lumen_runtime::check_app(dir, &source_parser::LumencParser, &*plugins)
}

/// AOT-compile an app from source (`lumenc build`), using the compiler's
/// default parser. See [`lumen_runtime::compile_app`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app(dir: &std::path::Path) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    compile_app_with_skin(dir, None)
}

/// AOT-compile an app from source with the skin named outright, which is what
/// `lumenc web` builds a site with. See [`lumen_runtime::compile_app_with_skin`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app_with_skin(
    dir: &std::path::Path,
    skin: Option<&str>,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let resolved = registry_packages(dir).map_err(RunError::Plugin)?;
    let plugins = plugin_host::compiler_plugins_for(dir, false, &resolved.compiler_plugins)
        .map_err(RunError::Plugin)?;
    lumen_runtime::compile_app_with_skin(dir, &source_parser::LumencParser, &*plugins, skin)
}
