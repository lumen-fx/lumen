//! `lumenc web <app_dir>` - emit an app as a static site.
//!
//! The app is compiled exactly the way `lumenc build` compiles it, and the
//! result is written out as HTML: one document per page, with the markup
//! already in it. The stylesheet and the assets are written beside the pages,
//! and `[web] render` says where a document comes from: a build writes it, or
//! a render produces it for the request that asks.
//!
//! What a site is made of is [`lumen_web`]'s to decide; this reads the app,
//! hands the emitter a [`SiteSpec`], and puts the files it gets back on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lumen_core::nav::{PATH_SIGNAL, SEGMENT_SIGNAL, resolve_path};
use lumen_core::signals::ArrayItem;
use lumen_core::{say_line, warn_line};
use lumen_html::contract::{
    DEFAULT_ARTIFACT_FILE, NavigationMode, ScriptFormat, ScriptRef, Seed, SeedValue,
};
use lumen_i18n::{I18n, LanguageIdentifier, SharedI18n, translated_or_authored};
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Element, LayoutIR, relativize_asset_paths};
use lumen_prerender::{self as prerender, Budget, Prerendered, Settled};
use lumen_runtime::config::{
    LumenToml, WebCssMode, WebHost, WebNavigation, WebPrerender, WebRender, WebSeedValue,
};
use lumen_runtime::run::locale_dir;
use lumen_ssr::{FetchPolicy, RenderOptions, SsrSite};
use lumen_web::urls::is_external;
use lumen_web::{
    AssetRef, CssMode, HostRewrite, LocaleSpec, PageSpec, SignalEnv, SiteSpec, State, WebSpec,
};

use crate::web_serve::{LOOPBACK, Server};
use crate::web_ssr::RenderHandler;

/// Where a site is written when `lumen.toml` and `--out` both stay quiet.
const DEFAULT_OUT_DIR: &str = "dist/web";

/// Directory inside the site that the app's own files are copied into.
const ASSET_DIR: &str = "assets";

/// File the compiled candela program is written as.
const BYTECODE_FILE: &str = "app.cdlb";

/// Port `--serve` listens on when none is named.
const DEFAULT_PORT: u16 = 8787;

const WEB_USAGE: &str = "lumenc web - emit an app as a site

USAGE:
    lumenc web <app_dir> [--out DIR] [--base PATH] [--locale TAG]...
                         [--render static|csr|ssr] [--prerender seeds|run|none]
                         [--runtime|--no-runtime]
                         [--no-hooks] [--lib-dir DIR] [--strict]
                         [--serve] [--port N] [--host ADDR]
                         [--allow-host NAME]...

Compiles the app and writes the stylesheet, the app's assets and, unless the
pages are rendered per request, one HTML document per page. A document carries
the markup already rendered, so a page reads without scripting. When the pages
carry the browser runtime, the compiled app and the runtime are written too and
a page loads them.

    --out DIR         Where the site is written (default: lumen.toml
                      [web] out_dir, else dist/web).
    --base PATH       URL prefix the site is served under, such as /docs
                      (default: [web] base_path, else /).
    --locale TAG      Emit a document tree for this locale. Repeat for
                      more; the first is served from the site root
                      (default: [web] locales, else [app] locale).
    --render MODE     Where a page's document comes from: static writes it
                      with nothing to run it, csr writes it and the runtime
                      adopts it, ssr produces it for the request that asks
                      (default: [web] render). Every mode writes the whole
                      markup tree.
    --runtime         Put the browser runtime in the documents.
    --no-runtime      Leave it out, so a page reads and its links work and
                      nothing runs. Only --render ssr leaves this open;
                      --render static already means --no-runtime and
                      --render csr already means --runtime, so contradicting
                      either is refused (default: [web] runtime).
    --prerender MODE  Where the state the pages are rendered with comes
                      from: seeds (lumen.toml [web.seed] and the markup),
                      run (the app runs here and the state it settles into
                      is written in) or none (default: [web] prerender).
    --no-hooks        Skip the app's prebuild [[hooks]].
    --lib-dir DIR     Directory holding lumen-web.wasm and lumen-web.js,
                      instead of the ones shipped with lumenc.
    --strict          Fail the build on any warning it prints.
    --serve           Serve the site after emitting it, and print the URL.
                      Under --render ssr every page comes from a render.
    --port N          Port to serve on (default: 8787; 0 picks a free one).
    --host ADDR       Address to listen on (default: 127.0.0.1). Any other
                      address makes the site reachable from other machines.
    --allow-host NAME Let a render ask this host for data. Repeat for more;
                      a render reaches nothing that is not named.";

/// Entry: `lumenc web <app_dir> [flags]`.
pub fn cmd_web(args: impl Iterator<Item = String>) -> ExitCode {
    let options = match parse_args(args) {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            warn_line!("lumenc web: {message}\n\n{WEB_USAGE}");
            return ExitCode::from(2);
        }
    };
    match build(&options) {
        Ok(report) => {
            for warning in &report.warnings {
                warn_line!("lumenc web: warning: {warning}");
            }
            let plural = if report.pages == 1 { "" } else { "s" };
            if report.per_request {
                say_line!(
                    "lumenc web: {} page{plural}, each rendered for the request that asks -> {}",
                    report.pages,
                    report.out.display()
                );
            } else {
                say_line!(
                    "lumenc web: {} page{plural} -> {}",
                    report.pages,
                    report.out.display()
                );
            }
            if options.strict && !report.warnings.is_empty() {
                warn_line!("lumenc web: --strict: {} warning(s)", report.warnings.len());
                return ExitCode::FAILURE;
            }
            if options.serve {
                return serve(report, &options);
            }
            // A rendered site is the files a render needs and no documents, so
            // there is nothing here for a file server to hand out.
            if report.per_request {
                say_line!(
                    "lumenc web: pass --serve to render the pages here, or point a server built \
                     on lumen-ssr at this directory"
                );
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            warn_line!("lumenc web: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What the command was asked to do.
struct Options {
    dir: PathBuf,
    out: Option<PathBuf>,
    base: Option<String>,
    locales: Vec<String>,
    render: Option<WebRender>,
    /// Whether the documents carry the browser runtime. `None` takes what
    /// `render` implies.
    runtime: Option<bool>,
    prerender: Option<WebPrerender>,
    no_hooks: bool,
    lib_dir: Option<PathBuf>,
    strict: bool,
    serve: bool,
    port: u16,
    host: Option<String>,
    allow_hosts: Vec<String>,
}

/// Parse the command line. `Ok(None)` means help was printed.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut dir: Option<PathBuf> = None;
    let mut options = Options {
        dir: PathBuf::new(),
        out: None,
        base: None,
        locales: Vec::new(),
        render: None,
        runtime: None,
        prerender: None,
        no_hooks: false,
        lib_dir: None,
        strict: false,
        serve: false,
        port: DEFAULT_PORT,
        host: None,
        allow_hosts: Vec::new(),
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            help if crate::is_help_flag(help) => {
                say_line!("{WEB_USAGE}");
                return Ok(None);
            }
            "--out" => options.out = Some(PathBuf::from(value("--out")?)),
            "--base" => options.base = Some(value("--base")?),
            "--locale" => options.locales.push(value("--locale")?),
            "--render" => {
                let mode = value("--render")?;
                options.render = Some(match mode.as_str() {
                    "static" => WebRender::Static,
                    "csr" => WebRender::Csr,
                    "ssr" => WebRender::Ssr,
                    other => {
                        return Err(format!(
                            "unknown --render mode `{other}` (expected static, csr or ssr)"
                        ));
                    }
                });
            }
            "--prerender" => {
                let mode = value("--prerender")?;
                options.prerender = Some(match mode.as_str() {
                    "seeds" => WebPrerender::Seeds,
                    "run" => WebPrerender::Run,
                    "none" => WebPrerender::None,
                    other => {
                        return Err(format!(
                            "unknown --prerender mode `{other}` (expected seeds, run or none)"
                        ));
                    }
                });
            }
            "--runtime" => options.runtime = Some(true),
            "--no-runtime" => options.runtime = Some(false),
            "--no-hooks" => options.no_hooks = true,
            "--lib-dir" => options.lib_dir = Some(PathBuf::from(value("--lib-dir")?)),
            "--strict" => options.strict = true,
            "--serve" => options.serve = true,
            "--host" => options.host = Some(value("--host")?),
            "--allow-host" => options.allow_hosts.push(value("--allow-host")?),
            "--port" => {
                let raw = value("--port")?;
                options.port = raw
                    .parse::<u16>()
                    .map_err(|_| format!("--port needs a port number, got `{raw}`"))?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag `{other}`"));
            }
            _ if dir.is_none() => dir = Some(PathBuf::from(arg)),
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    let Some(dir) = dir else {
        return Err("missing <app_dir>".to_string());
    };
    if !dir.is_dir() {
        return Err(format!("'{}' is not a directory", dir.display()));
    }
    options.dir = dir;
    Ok(Some(options))
}

/// What a finished build has to say for itself.
struct Report {
    out: PathBuf,
    base: String,
    pages: usize,
    /// Whether the pages are produced for the request that asks for them, so
    /// the directory holds what a render needs rather than the documents.
    per_request: bool,
    warnings: Vec<String>,
    /// The app a server renders per request, when one was asked for. It holds
    /// the tree the documents were emitted from, so a rendered page and a
    /// built one are the same page.
    site: Option<SsrSite>,
    /// The locales other than the one a render answers in, which is what a
    /// server hands to the documents on disk instead.
    other_locales: Vec<String>,
}

fn build(options: &Options) -> Result<Report, String> {
    // The app directory is made absolute before anything reads it, because
    // an asset's path is resolved against it and then relativized against it
    // again on the way into the site; the two only agree when the directory
    // is the same shape both times.
    let dir = options
        .dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", options.dir.display()))?;
    let dir = dir.as_path();
    let cfg = LumenToml::load_or_default(dir).map_err(|e| format!("lumen.toml: {e}"))?;
    let kind = crate::app_kind::resolve(dir, cfg.app.kind);
    if kind != crate::app_kind::AppKind::Markup {
        return Err(format!(
            "only a markup app is emitted as a site; this one is a {kind:?} app"
        ));
    }
    let mut warnings: Vec<String> = Vec::new();

    if !options.no_hooks {
        lumen_runtime::hooks::run_hooks(&cfg.hooks, lumen_runtime::hooks::HookWhen::Prebuild, dir)
            .map_err(|e| e.to_string())?;
    }

    // A site is served to every OS, so the skin cannot be the build
    // machine's. `[web] skin` names it; failing that the app's own skin,
    // unless that is `auto`, which is the machine's.
    let skin = skin_for(&cfg, &mut warnings);
    let mut compiled = crate::compile_app_with_skin(dir, Some(&skin))
        .map_err(|e| format!("compile {}: {e}", dir.display()))?;
    if compiled.ir.skin.as_deref() == Some("auto") {
        warnings.push(
            "the markup asks for skin=\"auto\", which is whichever OS built the site; name a \
             skin in the markup or in [web] skin"
                .to_string(),
        );
    }

    // Reported and written as the caller wrote it, not as the canonical
    // path: it is the one they will go looking in.
    let out = out_dir(options, &cfg, &options.dir);
    let base = options
        .base
        .clone()
        .or_else(|| cfg.web.base_path.clone())
        .unwrap_or_else(|| "/".to_string());
    let render = options.render.unwrap_or(cfg.web.render);
    let prerender = options.prerender.unwrap_or(cfg.web.prerender);
    let per_request = render == WebRender::Ssr;
    let carries_runtime = carries_runtime(render, options.runtime.or(cfg.web.runtime))?;
    // Both of these say what state a page is written with, and they name
    // different moments to read it at: a run here, and the app answering the
    // request. Taking either one would leave the other asked for and unused.
    if per_request && prerender == WebPrerender::Run {
        return Err(
            "render `ssr` produces each page for the request that asks, and prerender `run` \
             writes the state a run of the app settled into here; a page comes from one or the \
             other. Drop `run`, or render the pages at build time."
                .to_string(),
        );
    }
    if !options.allow_hosts.is_empty() && !(per_request && options.serve) {
        warnings.push(
            "--allow-host names a host a render may ask for data, and nothing here renders a \
             page; pass --render ssr --serve to render them here, or set the policy in the \
             server you build on lumen-ssr"
                .to_string(),
        );
    }
    if per_request && !matches!(cfg.web.host, WebHost::Static) {
        warnings.push(
            "[web] host writes the file that makes a file server send the shell for a deep path, \
             and a rendered site answers that path itself; no rewrite file is written"
                .to_string(),
        );
    }

    let plan = lumen_runtime::pages::discover(dir, &cfg);

    // A component that has to run is resolved here, before anything else reads
    // the tree. Its body is markup like any other once it is in: the asset
    // rewrite below reaches an `<image>` inside it, the link check sees its
    // links, and the artifact the browser loads carries it, so the runtime
    // adopts the body the page already shows instead of building it again.
    crate::component_fill::fill(&mut compiled, &plan.entry_key, &mut warnings);

    // Assets travel with the site, so every `<image src>` is rewritten from
    // the path it has on this machine to the path it will have on the
    // server, and the files are copied there.
    let assets = collect_assets(&mut compiled.ir.root, dir, &mut warnings);

    let keys: Vec<String> = plan.pages.iter().map(|page| page.key.clone()).collect();
    let keys = if keys.is_empty() {
        vec![plan.entry_key.clone()]
    } else {
        keys
    };
    let entry = plan.entry_key.clone();
    check_links(&compiled.ir.root, &keys, &entry, &mut warnings);

    // Pages that carry no runtime were asked for files alone, so nothing is
    // looked for and there is nothing to warn about. A page that carries one
    // is the same page whether a build wrote it or a render produced it, so
    // both reach for the same files; a missing one is still only a missing
    // file, and the pages are emitted the way a runtime-less site's are.
    let runtime = if carries_runtime {
        match crate::package_cli::locate_web_runtime(options.lib_dir.as_deref()) {
            Ok(files) => Some(files),
            Err(message) => {
                warnings.push(format!(
                    "{message} The site is emitted without it: the pages read and their links \
                     work, and nothing runs in the browser."
                ));
                None
            }
        }
    } else {
        if options.lib_dir.is_some() {
            warnings.push(
                "--lib-dir names a runtime for pages to load, and these pages carry none; drop \
                 --no-runtime, or pass --render csr, to use it"
                    .to_string(),
            );
        }
        None
    };

    let scripts = script_refs(&compiled, &mut warnings);
    check_exports(&compiled, &mut warnings);
    let locales = locales(options, &cfg);
    let web = WebSpec {
        base_path: base.clone(),
        url: cfg.web.url.clone(),
        canonical: cfg.web.canonical.clone(),
        entry: entry.clone(),
        title: title(&cfg, dir),
        description: cfg.web.description.clone(),
        og_image: cfg.web.og_image.clone(),
        css_mode: match cfg.web.css {
            WebCssMode::Sheet => CssMode::Sheet,
            WebCssMode::Computed => CssMode::Computed,
        },
        navigation: match cfg.web.navigation {
            WebNavigation::Soft => NavigationMode::Soft,
            WebNavigation::Hard => NavigationMode::Hard,
        },
        host: match cfg.web.host {
            WebHost::Static => HostRewrite::Static,
            WebHost::Netlify => HostRewrite::Netlify,
            WebHost::Vercel => HostRewrite::Vercel,
            WebHost::Apache => HostRewrite::Apache,
            WebHost::Nginx => HostRewrite::Nginx,
        },
        // A sitemap needs an absolute address to list, so one is written
        // when the site has a URL unless the app says not to.
        sitemap: cfg.web.sitemap.unwrap_or(true),
        runtime: runtime.is_some(),
        scripts,
        ..WebSpec::default()
    };

    // A style written on an element becomes a class and a rule, and the class
    // goes into the tree before the artifact is written: a row the browser
    // builds later is spawned from this tree, so it arrives already wearing
    // the class the stylesheet declares. In `computed` mode the cascade is
    // already resolved onto each element, so there is nothing to lift.
    let markup = match web.css_mode {
        CssMode::Computed => lumen_web::MarkupSheet::default(),
        CssMode::Sheet => lumen_web::lift_markup_styles(&mut compiled.ir.root),
    };

    std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
    // The compiled app carries the site's asset paths, so a node built from it
    // points where the emitted markup points. The browser runtime loads it,
    // and so does the server that renders the pages, so a rendered site keeps
    // it whether or not its documents run anything.
    if web.runtime || per_request {
        crate::artifact::write(&out.join(DEFAULT_ARTIFACT_FILE), &compiled)
            .map_err(|e| format!("write {}: {e}", out.join(DEFAULT_ARTIFACT_FILE).display()))?;
    }
    // The compiled program beside it is the browser's copy: a render runs the
    // one inside the artifact. It is one of the two largest files a site would
    // otherwise carry for nobody.
    if web.runtime {
        write_bytecode(&compiled, &out)?;
    }

    let seed = seed_values(&cfg, &compiled.ir.root, prerender);
    // The app is run once per page, not once per page per locale: a
    // translation is resolved in the tree the emitter walks, and a signal is
    // the same value in every language.
    let settled = match prerender {
        WebPrerender::Run => run_pages(
            &compiled,
            &keys,
            &entry,
            &seed,
            options.strict,
            &mut warnings,
        ),
        WebPrerender::Seeds | WebPrerender::None => BTreeMap::new(),
    };
    let mut pages_written = 0;
    let mut served: Option<SiteSpec> = None;
    for (index, locale) in locales.iter().enumerate() {
        let mut spec = SiteSpec {
            pages: Vec::new(),
            web: WebSpec {
                // A renderer holds one site and a site is in one language, so
                // the tree at the site root is the one a render answers for.
                // The others are answered by the documents beside it, which is
                // why they are still written.
                per_request: per_request && index == 0,
                ..web.clone()
            },
            locale: LocaleSpec {
                alternates: locales
                    .iter()
                    .filter(|other| *other != locale)
                    .cloned()
                    .collect(),
                default_locale: locales[0].clone(),
                ..LocaleSpec::new(locale.clone())
            },
            assets: assets.clone(),
            markup: markup.clone(),
        };
        // One tree per locale, shared by every page of it: which page a
        // document shows is a signal inside the tree, not a tree of its own.
        let ir = Arc::new(translated_ir(&compiled.ir, dir, locale, &mut warnings)?);
        for key in &keys {
            spec.pages.push(page_spec(
                key,
                &ir,
                &cfg,
                &seed,
                prerender,
                settled.get(key),
            ));
        }
        let site = lumen_web::emit(&spec).map_err(|e| e.to_string())?;
        for file in &site.files {
            write_file(&out.join(&file.path), file.contents.as_bytes())?;
        }
        if index == 0 {
            pages_written = spec.pages.len();
            warnings.extend(site.warnings);
            // A render answers in the locale served from the site root, which
            // is the tree the pages at the root were emitted from. Serving a
            // request in one of the other locales is the reverse proxy's to
            // decide, and it has the built documents for it.
            if per_request && options.serve {
                served = Some(spec);
            }
        }
    }

    for asset in &assets {
        copy_file(&asset.source, &out.join(&asset.path))?;
    }
    if let Some(runtime) = &runtime {
        copy_file(&runtime.wasm, &out.join(web.wasm.as_str()))?;
        copy_file(&runtime.js, &out.join(web.js.as_str()))?;
    }
    // Which paths a file server has no file for is the build's to say; a
    // render answers every path with the page it names.
    if matches!(cfg.web.host, WebHost::Static) && !per_request {
        note_deep_paths(&compiled, &keys, &entry);
    }

    // A render starts from the compiled app rather than from the documents on
    // disk: the state a page is written with is what the app settles into for
    // the request asking, which is the whole difference between a rendered
    // page and a built one. The files beside the documents are still the
    // build's, and the server sends them from the directory.
    let site = match served {
        Some(spec) => {
            let mut site = SsrSite::new(compiled, web).map_err(|e| e.to_string())?;
            *site.spec_mut() = spec;
            Some(site.with_seed(declared_seed(&seed)))
        }
        None => None,
    };

    Ok(Report {
        out,
        base,
        pages: pages_written,
        per_request,
        warnings,
        site,
        other_locales: locales.into_iter().skip(1).collect(),
    })
}

/// Whether the documents carry the browser runtime.
///
/// `render` and `runtime` are separate questions, and only one combination of
/// them is new: a document produced per request that carries no runtime, which
/// is a page rendered for the visitor asking with nothing to run afterwards.
/// The other two modes answer the runtime question themselves, so `wanted`
/// there is either what the mode already says or a contradiction, and a
/// contradiction is refused rather than quietly picked apart.
fn carries_runtime(render: WebRender, wanted: Option<bool>) -> Result<bool, String> {
    let implied = render != WebRender::Static;
    let Some(wanted) = wanted else {
        return Ok(implied);
    };
    match (render, wanted) {
        (WebRender::Static, true) => Err(
            "render `static` writes documents with nothing to run them, and runtime `true` asks \
             for the runtime in them. A page a build writes and the runtime takes over is render \
             `csr`."
                .to_string(),
        ),
        (WebRender::Csr, false) => Err(
            "render `csr` is a page the runtime adopts, and runtime `false` takes the runtime \
             away. A page a build writes with nothing to run it is render `static`."
                .to_string(),
        ),
        _ => Ok(wanted),
    }
}

/// The skin the site is styled with.
fn skin_for(cfg: &LumenToml, warnings: &mut Vec<String>) -> String {
    if let Some(skin) = cfg.web.skin.as_deref().filter(|s| !s.is_empty()) {
        return skin.to_string();
    }
    match cfg.skin.name.as_deref() {
        Some("auto") => {
            warnings.push(
                "[skin] name = \"auto\" picks a skin from the machine that builds; the site is \
                 emitted with the default skin. Name one in [web] skin to choose."
                    .to_string(),
            );
            "default".to_string()
        }
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "default".to_string(),
    }
}

/// Where the site is written.
fn out_dir(options: &Options, cfg: &LumenToml, dir: &Path) -> PathBuf {
    let configured = options
        .out
        .clone()
        .or_else(|| cfg.web.out_dir.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUT_DIR));
    if configured.is_absolute() || options.out.is_some() {
        configured
    } else {
        dir.join(configured)
    }
}

/// The title every page falls back to.
fn title(cfg: &LumenToml, dir: &Path) -> String {
    cfg.window
        .title
        .clone()
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Lumen".to_string())
}

/// The locales the site is emitted in, the first one at the site root.
fn locales(options: &Options, cfg: &LumenToml) -> Vec<String> {
    let mut locales: Vec<String> = if !options.locales.is_empty() {
        options.locales.clone()
    } else {
        cfg.web.locales.clone().unwrap_or_default()
    };
    let default = cfg
        .web
        .default_locale
        .clone()
        .or_else(|| cfg.app.locale.as_ref().map(|l| l.to_string()))
        .or_else(|| locales.first().cloned())
        .unwrap_or_else(|| "en-US".to_string());
    if !locales.iter().any(|locale| locale == &default) {
        locales.insert(0, default.clone());
    }
    // The default locale leads: it is the tree served from the site root.
    locales.sort_by_key(|locale| locale != &default);
    locales.dedup();
    locales
}

/// Run the app once for each page and keep the state each one settles into.
///
/// A page written from a run has to come out the same on every machine and on
/// every build, so the entry page is always built twice and compared, and
/// `--strict` compares every page.
fn run_pages(
    compiled: &CompiledApp,
    keys: &[String],
    entry: &str,
    seed: &BTreeMap<String, WebSeedValue>,
    strict: bool,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, State> {
    let declared = declared_seed(seed);
    let mut settled = BTreeMap::new();
    for key in keys {
        let run = prerender::page(compiled, key, &declared, Budget::default());
        report_run(key, &run, warnings);
        if key == entry || strict {
            let again = prerender::page(compiled, key, &declared, Budget::default());
            if again.state != run.state {
                warnings.push(format!(
                    "page `{key}` settled differently the second time it was run, so what it \
                     holds depends on something outside the app"
                ));
            }
        }
        settled.insert(key.clone(), run.state);
    }
    settled
}

/// What one run leaves a build to say.
fn report_run(key: &str, run: &Prerendered, warnings: &mut Vec<String>) {
    if let Settled::Capped(ticks) = run.settled {
        warnings.push(format!(
            "page `{key}` was still changing after {ticks} ticks, so it is written with the \
             state it had reached by then"
        ));
    }
    for url in &run.denied {
        warnings.push(format!(
            "page `{key}` asked for `{url}`, and a build answers the network itself so that \
             every machine writes the same page; the browser fetches it on arrival"
        ));
    }
    for skipped in &run.state.skipped {
        warnings.push(format!("page `{key}` is written without {skipped}"));
    }
    for engine in &run.unsupported_engines {
        warnings.push(format!(
            "page `{key}` carries a `{engine}` program, which this lumenc has no host for; what \
             it publishes is missing from the page"
        ));
    }
}

/// The declared state as a run reads it: what the app starts from before its
/// own scripts write anything.
fn declared_seed(seed: &BTreeMap<String, WebSeedValue>) -> Seed {
    let mut declared = Seed::new();
    for (name, value) in seed {
        match value {
            WebSeedValue::Rows(rows) => {
                declared.arrays.insert(name.clone(), rows.clone());
            }
            value => {
                declared
                    .globals
                    .insert(name.clone(), seed_value(value.clone()));
            }
        }
    }
    declared
}

/// One page, rendered with the state it arrives in.
fn page_spec(
    key: &str,
    ir: &Arc<LayoutIR>,
    cfg: &LumenToml,
    seed: &BTreeMap<String, WebSeedValue>,
    prerender: WebPrerender,
    settled: Option<&State>,
) -> PageSpec {
    let page_cfg = cfg.web.pages.get(key);
    // A run started from the declared values and holds the page's route, so
    // what it settled into is the whole state this page is written with.
    if let Some(state) = settled {
        return PageSpec {
            key: key.to_string(),
            ir: Arc::clone(ir),
            title: page_cfg.and_then(|page| page.title.clone()),
            description: page_cfg.and_then(|page| page.description.clone()),
            signals: state.signals.clone(),
            seed: state.seed.clone(),
        };
    }
    let mut signals = SignalEnv::new();
    let mut page_seed = Seed::new();
    // Which page a document is showing is not app state: it is what the
    // document is. The runtime starts on the same page for the same reason.
    signals = signals.with_global(PATH_SIGNAL, key);
    signals = signals.with_global(SEGMENT_SIGNAL, "");
    page_seed
        .globals
        .insert(PATH_SIGNAL.to_string(), SeedValue::Str(key.to_string()));
    page_seed
        .globals
        .insert(SEGMENT_SIGNAL.to_string(), SeedValue::Str(String::new()));
    if prerender != WebPrerender::None {
        for (name, value) in seed {
            if let WebSeedValue::Rows(rows) = value {
                signals = signals.with_array(name.clone(), rows.iter().map(row_item).collect());
                page_seed.arrays.insert(name.clone(), rows.clone());
                continue;
            }
            signals = signals.with_global(name.clone(), seed_text(value));
            page_seed
                .globals
                .insert(name.clone(), seed_value(value.clone()));
        }
    }
    PageSpec {
        key: key.to_string(),
        ir: Arc::clone(ir),
        title: page_cfg.and_then(|page| page.title.clone()),
        description: page_cfg.and_then(|page| page.description.clone()),
        signals,
        seed: page_seed,
    }
}

/// The signal values every page is rendered with: what `[web.seed]` names,
/// on top of the defaults the markup itself declares.
fn seed_values(
    cfg: &LumenToml,
    root: &Element,
    prerender: WebPrerender,
) -> BTreeMap<String, WebSeedValue> {
    let mut seed = BTreeMap::new();
    if prerender == WebPrerender::None {
        return seed;
    }
    collect_signal_seeds(root, &mut seed);
    // A value written in `lumen.toml` is the app author's answer, so it wins
    // over the default a widget declared.
    for (name, value) in &cfg.web.seed {
        seed.insert(name.clone(), value.clone());
    }
    seed
}

fn collect_signal_seeds(element: &Element, seed: &mut BTreeMap<String, WebSeedValue>) {
    if let Some((name, value)) = &element.attrs.signal_seed {
        seed.entry(name.clone())
            .or_insert_with(|| WebSeedValue::Str(value.clone()));
    }
    for child in &element.children {
        collect_signal_seeds(child, seed);
    }
}

/// A seed value as the markup reads it: signals hold text. Rows are not a
/// signal's value; [`page_spec`] puts them in the page's arrays instead.
fn seed_text(value: &WebSeedValue) -> String {
    match value {
        WebSeedValue::Str(text) => text.clone(),
        WebSeedValue::Int(number) => number.to_string(),
        WebSeedValue::Float(number) => number.to_string(),
        WebSeedValue::Bool(flag) => flag.to_string(),
        WebSeedValue::Rows(_) => String::new(),
    }
}

/// A seed value as the runtime reads it, with its type intact. Rows go
/// through [`Seed::arrays`], which keeps their shape.
fn seed_value(value: WebSeedValue) -> SeedValue {
    match value {
        WebSeedValue::Str(text) => SeedValue::Str(text),
        WebSeedValue::Int(number) => SeedValue::I64(number),
        WebSeedValue::Float(number) => SeedValue::F64(number),
        WebSeedValue::Bool(flag) => SeedValue::Bool(flag),
        WebSeedValue::Rows(_) => SeedValue::Str(String::new()),
    }
}

/// One `[web.seed]` row as the reconciler reads a row: a record of fields.
fn row_item(row: &BTreeMap<String, String>) -> ArrayItem {
    row.iter()
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect()
}

/// The app's tree with every `translatable` element's text resolved for
/// `locale`, which is what makes a page readable in that language with
/// nothing running.
fn translated_ir(
    ir: &LayoutIR,
    dir: &Path,
    locale: &str,
    warnings: &mut Vec<String>,
) -> Result<LayoutIR, String> {
    let mut out = ir.clone();
    let lang = match locale.parse::<LanguageIdentifier>() {
        Ok(lang) => lang,
        Err(e) => {
            warnings.push(format!("locale `{locale}` is not a valid BCP-47 tag: {e}"));
            return Ok(out);
        }
    };
    let fallback = "en-US"
        .parse::<LanguageIdentifier>()
        .map(|fallback| vec![fallback])
        .unwrap_or_default();
    let mut i18n = I18n::new(lang, fallback);
    i18n.load_dir(&locale_dir(dir))
        .map_err(|e| format!("locale catalogues: {e}"))?;
    // The same no-argument lookup markup gets at run time.
    translate(&mut out.root, &SharedI18n::new(i18n));
    Ok(out)
}

fn translate(element: &mut Element, i18n: &SharedI18n) {
    if let Some(key) = element.attrs.translatable.clone() {
        element.attrs.text = Some(translated_or_authored(
            i18n.try_t(&key),
            element.attrs.text.as_deref(),
            &key,
        ));
    }
    for child in &mut element.children {
        translate(child, i18n);
    }
}

/// Move every asset the markup points at into the site, and rewrite the
/// markup to point at where it lands.
fn collect_assets(root: &mut Element, dir: &Path, warnings: &mut Vec<String>) -> Vec<AssetRef> {
    let mut outside: Vec<String> = Vec::new();
    relativize_asset_paths(root, dir, &mut outside);
    let mut assets: Vec<AssetRef> = Vec::new();
    let mut placed: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    rewrite_assets(root, dir, &mut assets, &mut placed, &mut taken);
    for path in outside {
        warnings.push(format!(
            "`{path}` is outside the app directory; it is copied to the top of {ASSET_DIR}/"
        ));
    }
    assets
}

fn rewrite_assets(
    element: &mut Element,
    dir: &Path,
    assets: &mut Vec<AssetRef>,
    placed: &mut BTreeMap<PathBuf, String>,
    taken: &mut BTreeSet<String>,
) {
    if element.tag == "image"
        && let Some(src) = element.attrs.src.clone()
        && !is_external(&src)
    {
        let source = if Path::new(&src).is_absolute() {
            PathBuf::from(&src)
        } else {
            dir.join(&src)
        };
        let path = placed.get(&source).cloned().unwrap_or_else(|| {
            let path = site_path(&src, taken);
            placed.insert(source.clone(), path.clone());
            assets.push(AssetRef::new(source.clone(), path.clone()));
            path
        });
        element.attrs.src = Some(path);
    }
    for child in &mut element.children {
        rewrite_assets(child, dir, assets, placed, taken);
    }
}

/// Where one asset lands inside the site. A file from inside the app keeps
/// the shape of its path; one from outside keeps its name alone, and gets a
/// number if that name is already taken.
fn site_path(src: &str, taken: &mut BTreeSet<String>) -> String {
    let relative = Path::new(src);
    let candidate = if relative.is_absolute() {
        relative
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "asset".to_string())
    } else {
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    };
    let mut path = format!("{ASSET_DIR}/{candidate}");
    let mut n = 1;
    while taken.contains(&path) {
        path = format!("{ASSET_DIR}/{n}-{candidate}");
        n += 1;
    }
    taken.insert(path.clone());
    path
}

/// The scripts the browser runtime loads at boot.
///
/// candela is the one that runs there: it compiles to a bytecode image the
/// runtime carries a virtual machine for. A program in another language is
/// left out rather than pointed at, so the manifest never names something
/// nothing can run.
fn script_refs(compiled: &CompiledApp, warnings: &mut Vec<String>) -> Vec<ScriptRef> {
    let mut refs = Vec::new();
    for script in &compiled.scripts {
        match &script.bytecode {
            Some(_) => refs.push(ScriptRef {
                engine: script.engine.clone(),
                path: BYTECODE_FILE.to_string(),
                format: ScriptFormat::Cdlb,
            }),
            None => warnings.push(format!(
                "the app's {} script does not run on the web; the pages are emitted without it",
                script.engine
            )),
        }
    }
    refs
}

/// Warn about a function the app calls by name that its compiled program
/// cannot be called by.
///
/// candela exports a function only when every parameter it takes is
/// annotated. One written with a bare parameter still compiles and still
/// ships; it is simply never called, because the runtime asks the artifact for
/// it by name and the artifact has no such name. The desktop hides this: the
/// compiler is in the process there and answers from the source, so the same
/// app works on a desktop and shows a blank where the value should be in a
/// browser.
///
/// A component the build could not stand in for is the same failure with a
/// worse symptom: the page carries the box the call was to fill, and an empty
/// box is what a reader would not notice.
fn check_exports(compiled: &CompiledApp, warnings: &mut Vec<String>) {
    let components = components_called(compiled);
    for script in &compiled.scripts {
        let Some(read_back) = lumen_runtime::run::script_exports(script) else {
            continue;
        };
        let exports = match read_back {
            Ok(exports) => exports,
            // The browser loads the program the same way this reads it, so a
            // program that will not load here will not load there either.
            Err(error) => {
                warnings.push(format!(
                    "the compiled {} program does not load: {error}. The pages are emitted, but \
                     the app's script will not run in a browser",
                    script.engine
                ));
                continue;
            }
        };
        let declared = defined_functions(&script.source);
        for name in called_by_name(&script.source) {
            if !exports.contains(&name) {
                warnings.push(format!(
                    "`{name}` is called by name and the compiled program does not export it, so \
                     nothing happens when it is called; annotate every parameter it takes, as in \
                     `fn {name}(id: any)`"
                ));
            }
        }
        // Every name still here is one the build ran and could not fill, so
        // each gets the reason it could not be. A component the build did fill
        // is its body by now and names nothing.
        for name in components.iter().filter(|name| declared.contains(*name)) {
            if exports.contains(name) {
                warnings.push(format!(
                    "`{name}` returned no markup when the build called it, so the page carries an \
                     empty box where its body belongs; a component returns one `lmn!` block"
                ));
            } else {
                warnings.push(format!(
                    "the markup writes `<{name}/>`, and the compiled program does not export \
                     `{name}`, so the page carries an empty box where its body belongs; annotate \
                     every parameter it takes, as in `fn {name}(id: any)`"
                ));
            }
        }
    }
}

/// Every component the build could not stand in for, which the runtime fills
/// by calling the function of that name.
///
/// The fragment bodies are walked as well as the page tree: a body is a
/// subtree like any other and can name a component of its own, which reaches
/// the page the moment something instantiates it.
fn components_called(compiled: &CompiledApp) -> BTreeSet<String> {
    fn walk(el: &Element, out: &mut BTreeSet<String>) {
        if let Some(use_site) = &el.frag_use {
            out.insert(use_site.key.clone());
        }
        for child in &el.children {
            walk(child, out);
        }
    }
    let mut out = BTreeSet::new();
    walk(&compiled.ir.root, &mut out);
    for (_, fragment) in compiled.fragments.iter() {
        for el in &fragment.body {
            walk(el, &mut out);
        }
    }
    out
}

/// Every function `source` defines that something calls by name: a handler
/// bound by name, a derivation body, or one of the `on_` names Lumen calls
/// when the thing they stand for happens.
fn called_by_name(source: &str) -> BTreeSet<String> {
    let quoted: BTreeSet<&str> = source
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|text| is_identifier(text))
        .collect();
    defined_functions(source)
        .into_iter()
        .filter(|name| name.starts_with("on_") || quoted.contains(name.as_str()))
        .collect()
}

/// The names of the functions `source` declares.
fn defined_functions(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("fn ") else {
            continue;
        };
        let Some((name, _)) = rest.split_once('(') else {
            continue;
        };
        let name = name.trim();
        if is_identifier(name) {
            names.insert(name.to_string());
        }
    }
    names
}

/// True for a name a script could declare a function under.
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(|c: char| c.is_ascii_digit())
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Write the compiled candela program beside the pages, when the app has one.
fn write_bytecode(compiled: &CompiledApp, out: &Path) -> Result<(), String> {
    for script in &compiled.scripts {
        if let Some(bytecode) = &script.bytecode {
            write_file(&out.join(BYTECODE_FILE), bytecode)?;
        }
    }
    Ok(())
}

/// Warn about a link that names no page: it reaches the app, which answers
/// it the way the desktop does, but nothing was emitted for it.
fn check_links(root: &Element, keys: &[String], entry: &str, warnings: &mut Vec<String>) {
    let mut sorted: Vec<String> = keys.to_vec();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    walk_links(root, &sorted, entry, &mut seen, warnings);
}

fn walk_links(
    element: &Element,
    keys: &[String],
    entry: &str,
    seen: &mut BTreeSet<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(href) = &element.attrs.href
        && !is_external(href)
        && seen.insert(href.clone())
    {
        // A path deeper than a page resolves to that page with the rest
        // left over, which is the point of `route.segment`. A path that
        // starts somewhere else resolved to nothing.
        let (key, _) = resolve_path(href, keys, entry);
        let requested = href.trim_start_matches('/').trim_end_matches('/');
        if !requested.is_empty() && !requested.starts_with(key.as_str()) {
            warnings.push(format!(
                "`{href}` names no page; a visitor following it lands on the app shell"
            ));
        }
    }
    for child in &element.children {
        walk_links(child, keys, entry, seen, warnings);
    }
}

/// Say which pages read the part of a path that is not a page, because on a
/// plain file server those paths are served through the emitted shell.
fn note_deep_paths(compiled: &CompiledApp, keys: &[String], entry: &str) {
    let mut readers: Vec<String> = Vec::new();
    for key in keys {
        if page_reads_segment(&compiled.ir.root, key, keys, entry) {
            readers.push(key.clone());
        }
    }
    let scripts = compiled
        .scripts
        .iter()
        .any(|script| script.source.contains(SEGMENT_SIGNAL));
    if readers.is_empty() && !scripts {
        return;
    }
    let example = readers.first().map(String::as_str).unwrap_or(entry);
    let mut what = readers.join(", ");
    if scripts {
        if !what.is_empty() {
            what.push_str(" and ");
        }
        what.push_str("the app's scripts");
    }
    say_line!(
        "lumenc web: {what} read `{}`, so a path like /{example}/42 is answered by 404.html. Set \
         [web] host to have your host serve those paths with a 200 instead.",
        SEGMENT_SIGNAL,
    );
}

/// Whether one page's own subtree mentions the leftover-path signal.
fn page_reads_segment(root: &Element, key: &str, keys: &[String], entry: &str) -> bool {
    // A multi-page app is one tree of route gates; a single-page app is the
    // page itself.
    let gate = root.children.iter().find(|child| {
        child.tag == "if"
            && child.attrs.if_signal.as_deref() == Some(PATH_SIGNAL)
            && child.attrs.if_eq.as_deref() == Some(key)
    });
    let subtree = match gate {
        Some(gate) => gate,
        None if keys.len() == 1 || key == entry => root,
        None => return false,
    };
    mentions_segment(subtree)
}

fn mentions_segment(element: &Element) -> bool {
    let attrs = &element.attrs;
    let named = [
        attrs.bind.as_ref().map(|bind| bind.name.as_str()),
        attrs.if_signal.as_deref(),
        attrs.text.as_deref(),
    ];
    if named
        .into_iter()
        .flatten()
        .any(|value| value.contains(SEGMENT_SIGNAL))
    {
        return true;
    }
    element.children.iter().any(mentions_segment)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn copy_file(source: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::copy(source, target)
        .map(|_| ())
        .map_err(|e| format!("copy {} to {}: {e}", source.display(), target.display()))
}

/// Serve the emitted site until the process is stopped.
///
/// This is the development and self-hosting path: one directory, one machine,
/// and one process. A site that answers the public belongs behind a reverse
/// proxy, and an app that answers it from a render belongs in a server of your
/// own built on [`lumen_ssr`], which is the same renderer this installs.
fn serve(report: Report, options: &Options) -> ExitCode {
    let host = match host_address(options.host.as_deref()) {
        Ok(host) => host,
        Err(message) => {
            warn_line!("lumenc web: {message}");
            return ExitCode::FAILURE;
        }
    };
    if !host.is_loopback() {
        warn_line!(
            "lumenc web: warning: --host {host} makes the site reachable from other machines. \
             This server is for development and for a site you host yourself; put a reverse proxy \
             in front of it before anyone else uses it."
        );
    }
    let mut server = match Server::bind(&report.out, &report.base, host, options.port) {
        Ok(server) => server,
        Err(message) => {
            warn_line!("lumenc web: {message}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(site) = report.site {
        let mut fetch = FetchPolicy::default();
        for allowed in &options.allow_hosts {
            fetch = fetch.allow_host(allowed);
        }
        let render = RenderOptions {
            fetch,
            ..RenderOptions::default()
        };
        let handler = match RenderHandler::start(site, render, report.other_locales) {
            Ok(handler) => handler,
            Err(message) => {
                warn_line!("lumenc web: {message}");
                return ExitCode::FAILURE;
            }
        };
        server = server.with_handler(Arc::new(handler));
        // The number is the process's, not the machine's: a Lumen app reads
        // its state through buses that belong to the process, so two apps
        // ticking at once would read each other's writes.
        say_line!(
            "lumenc web: rendering every page for the request that asks, one render at a time"
        );
        if options.allow_hosts.is_empty() {
            say_line!(
                "lumenc web: a render reaches no host; pass --allow-host to let the app fetch its \
                 data while the page is rendered"
            );
        }
    }

    say_line!(
        "lumenc web: serving {} at {}",
        report.out.display(),
        server.url()
    );
    say_line!("lumenc web: press Ctrl-C to stop");
    server.run();
    ExitCode::SUCCESS
}

/// The address to listen on. Nothing named means the loopback address, which
/// is the machine this runs on and nobody else.
fn host_address(host: Option<&str>) -> Result<IpAddr, String> {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return Ok(LOOPBACK);
    };
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(LOOPBACK);
    }
    host.parse::<IpAddr>().map_err(|_| {
        format!(
            "--host takes an address this machine has, such as 127.0.0.1 or 0.0.0.0, got `{host}`"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_named_means_this_machine_and_nobody_else() {
        assert_eq!(host_address(None), Ok(LOOPBACK));
        assert_eq!(host_address(Some("")), Ok(LOOPBACK));
        assert_eq!(host_address(Some(" localhost ")), Ok(LOOPBACK));
        assert!(host_address(Some("127.0.0.1")).is_ok_and(|host| host.is_loopback()));
    }

    #[test]
    fn an_address_that_reaches_further_is_taken_as_written() {
        let any = host_address(Some("0.0.0.0")).expect("an address this machine can have");
        assert!(!any.is_loopback(), "the warning is on this being reachable");
        assert!(host_address(Some("::1")).is_ok_and(|host| host.is_loopback()));
    }

    #[test]
    fn something_that_is_not_an_address_is_named_back() {
        let error = host_address(Some("my-laptop")).expect_err("that is not an address");
        assert!(error.contains("my-laptop"), "{error}");
    }
}
