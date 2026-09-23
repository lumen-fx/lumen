//! `lumenc` library. Parses Lumen markup into a [`LayoutIR`] tree and spawns it into an ECS world via [`run_app`].
//!
//! Every tag is a styled container and the tag selects the defaults: `<column>` sets `flex="column"`, `<scroll>`
//! attaches the scroll components, `<root>` fills the viewport, and so on. Attributes cover sizing, spacing, paint,
//! typography, scrolling, interaction, and binding.
//!
//! The accepted tags live in `KNOWN_TAGS` in [`parse::html`], with the per-tag attribute handling beside it; the
//! reader-facing lists are the "Tags and attributes" and "CSS" reference pages in `docs/src/reference/`.

// `deny` (not `forbid`) so the single audited dlopen shim in `link::loader`,
// the link-not-embed launcher's only unsafe, can opt in via `#[allow]`. Every
// other module stays unsafe-free and trips the deny.
#![deny(unsafe_code)]
#![warn(missing_docs)]

// The linkage anchor for the `dynamic-engine` shape: naming the engine dylib
// is what puts `liblumen_engine` in the binary's link graph, and with
// `-C prefer-dynamic` the engine crates above resolve into it instead of
// compiling in. See the feature note in Cargo.toml.
#[cfg(all(feature = "dynamic-engine", not(windows)))]
use lumen_engine as _;

/// Browser add-ons an app depends on, found for the target a build is for.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod addons;
/// CLI subcommand handlers: `build`, `bundle`, `add`/`remove`/`fetch`/`update`,
/// `i18n`, the MCP inspection commands, the static signal lint, and the
/// `lumenc new` scaffolder.
pub mod cli;
/// In-process source -> LMNA compile for the link-not-embed launcher. Uses only
/// the parser front-end + CSS cascade + artifact codec (no `lumen-runtime`), so
/// a `dlopen-run` launcher compiles source without static-linking the runtime.
/// Gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod compile;
/// Linking a per-target link kit into one executable (`lumenc package
/// --static`), writing the link kit a release ships, and the dlopen loader
/// the link-not-embed launcher drives across the C-ABI.
pub mod link;
/// Assembling a shippable app (`lumenc package`), the registry client
/// (`lpm`) that resolves what it names, and the release channel both draw
/// their files from.
pub mod package;
/// The markup + CSS front end: parse `.lmn` / `.css` from source, resolve
/// `<include>` / `@import`, fill fragments, and format markup back to text.
pub mod parse;
/// The compiler side of the injected compiler-plugin boundary: builds an
/// app's `[[plugins]]` chain over the `lumenc-plugin` loader.
pub mod plugin_host;
/// `lumenc web` - emit an app as a static site, serve it locally, and render
/// it per-request over SSR.
pub mod web;

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
pub use parse::css::{CssWarning, Stylesheet, apply_css, parse_css};
#[cfg(feature = "runtime-parse")]
pub use parse::html::{
    ParsedMarkup, collect_fragments, collect_script_refs, parse_html, parse_html_with_loader,
    parse_markup,
};
#[cfg(feature = "runtime-parse")]
pub use parse::resolve::{FileLoader, FsLoader};
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub use parse::source_parser::LumencParser;

/// The compiler's default markup/CSS front-end, boxed for injection into
/// [`RunOptions::parser`]. The SDKs and the C-ABI hand this to the runtime so a
/// from-source run can re-parse (`lumen-runtime` links no parser itself).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn default_parser() -> Box<dyn SourceParser> {
    Box::new(parse::source_parser::LumencParser)
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
    let deps = addons::target_deps(&opts.dir, lumen_modules::Target::Desktop, None)
        .map_err(RunError::Plugin)?;
    let resolved = &deps.resolved;
    opts.resolved_modules = lumen_runtime::modules::ResolvedModules(
        resolved
            .modules
            .iter()
            .map(|(name, file)| (name.clone(), Ok(file.clone())))
            .collect(),
    );
    opts.import_roots = deps.compile.import_roots.clone();
    opts.addons = deps.compile.addons.clone();
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

/// Every `version` source the app in `dir` declares for a build for any of
/// `targets`, with the table that declared it. `[dependencies]` entries (with
/// each target's own table laid over them) become runtime packages and
/// `[[plugins]]` entries compiler plugins; the registry says which platform
/// each one is for, and the table says what the app wants it for. A name two
/// targets declare is asked for once, as the first of them declares it.
///
/// A `lumen.toml` that does not parse is the error, here as everywhere: the
/// requirements cannot be read out of a file nobody can read, and `lumenc
/// fetch` has no later step to report it from.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn registry_requirements(
    dir: &std::path::Path,
    targets: &[lumen_modules::Target],
) -> Result<Vec<package::lpm::Requirement>, String> {
    use lumen_runtime::modules::ModuleSource;

    let cfg = LumenToml::load_or_default(dir).map_err(|e| format!("lumen.toml: {e}"))?;
    let mut reqs: Vec<package::lpm::Requirement> = Vec::new();
    let deps = targets
        .iter()
        .flat_map(|target| cfg.dependencies_for(*target).0);
    for dep in deps {
        if reqs.iter().any(|r| r.name == dep.name) {
            continue;
        }
        if let ModuleSource::Version(req) = &dep.source {
            reqs.push(package::lpm::Requirement {
                name: dep.name.clone(),
                req: req.clone(),
                table: package::lpm::Table::Dependencies,
            });
        }
    }
    for cfg in plugin_host::read_plugin_cfgs(dir)? {
        if let lumenc_plugin::PluginSource::Version(req) = &cfg.source {
            reqs.push(package::lpm::Requirement {
                name: cfg.name.clone(),
                req: req.clone(),
                table: package::lpm::Table::Plugins,
            });
        }
    }
    Ok(reqs)
}

/// Resolve everything the app in `dir` names in the registry for a build for
/// `target`, for this machine's platform. `lumenc package --target` asks for
/// another one.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn registry_packages(
    dir: &std::path::Path,
    target: lumen_modules::Target,
) -> Result<package::lpm::Resolved, String> {
    package::lpm::resolve(
        dir,
        package::lpm::host_target(),
        &registry_requirements(dir, &[target])?,
        package::lpm::Mode::of_invocation(),
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
///
/// A check is for no one target: the app's markup and calls are checked
/// against the add-ons and script libraries of every target it declares.
pub fn check_app(dir: &std::path::Path) -> Result<CheckReport, RunError> {
    let resolved =
        registry_packages(dir, lumen_modules::Target::Desktop).map_err(RunError::Plugin)?;
    let plugins = plugin_host::compiler_plugins_for(dir, true, &resolved.compiler_plugins)
        .map_err(RunError::Plugin)?;
    let deps = addons::every_target(dir).map_err(RunError::Plugin)?;
    lumen_runtime::check_app(dir, &parse::source_parser::LumencParser, &*plugins, &deps)
}

/// AOT-compile an app from source for the desktop (`lumenc build`), using the
/// compiler's default parser. See [`lumen_runtime::compile_app`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app(dir: &std::path::Path) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let deps =
        addons::target_deps(dir, lumen_modules::Target::Desktop, None).map_err(RunError::Plugin)?;
    compile_app_with(dir, None, &deps)
}

/// AOT-compile an app from source against what was resolved for its target,
/// with the skin named outright when `skin` is set, which is how `lumenc web`
/// builds a site. See [`lumen_runtime::compile_app_with_skin`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub fn compile_app_with(
    dir: &std::path::Path,
    skin: Option<&str>,
    deps: &addons::TargetDeps,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let plugins = plugin_host::compiler_plugins_for(dir, false, &deps.resolved.compiler_plugins)
        .map_err(RunError::Plugin)?;
    lumen_runtime::compile_app_with_skin(
        dir,
        &parse::source_parser::LumencParser,
        &*plugins,
        skin,
        &deps.compile,
    )
}
