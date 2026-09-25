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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use lumen_core::nav::{PATH_SIGNAL, SEGMENT_SIGNAL, resolve_path};
use lumen_core::signals::ArrayItem;
use lumen_core::{say_line, warn_line};
use lumen_html::contract::{
    DEFAULT_ARTIFACT_FILE, DEFAULT_CSS_FILE, DEFAULT_JS_FILE, DEFAULT_WASM_FILE, ForeignElement,
    NavigationMode, ScriptFormat, ScriptRef, Seed, SeedValue,
};
use lumen_i18n::{Catalogues, I18nPlugin, LanguageIdentifier};
use lumen_ir::artifact::{CompiledApp, CompiledI18n};
use lumen_ir::layout_ir::{BgSpec, Element, LayoutIR, relativize_asset_paths};
use lumen_modules::Target;
use lumen_prerender::{self as prerender, Budget, Language, Prerendered, Settled};
use lumen_runtime::app_layout::src_dir;
use lumen_runtime::config::{
    LumenToml, WebCssMode, WebHost, WebNavigation, WebPrerender, WebRender, WebSeedValue,
};
use lumen_runtime::pages::PagePlan;
use lumen_runtime::run::locale_dir;
use lumen_web::urls::is_external;
use lumen_web::{
    AssetRef, CssMode, HostRewrite, LocaleSpec, LocaleTree, PageHead, PageSpec, RowFills,
    SERVER_SPEC_FILE, ServerPolicy, ServerSpec, SignalEnv, SiteLocales, SiteSpec, WebSpec,
    intrinsic_size,
};

use super::serve::{self, Serve};

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
                         [--no-hooks] [--lib-dir DIR] [--strict] [--offline]
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
    --locale TAG      Emit the site in this locale. Repeat for more; the
                      first is served from the site root and the rest from
                      /<tag>/. Under --render ssr no documents are written
                      and a render answers in the locale the request asks
                      for (default: [web] locales, else [app] locale).
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
                      instead of the ones shipped with lumenc. A bundled
                      module's web half is looked for under
                      modules/<name>/web/ here first.
    --strict          Fail the build on any warning it prints.
    --offline         Resolve the app's registry packages from what is
                      already downloaded, and never reach the network.
    --serve           Serve the site after emitting it with lumen-server
                      --dev, found beside lumenc, else at $LUMEN_SERVER,
                      else on PATH. Under --render ssr every page comes
                      from a render.
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
                return serve::run(&Serve {
                    site: &report.out,
                    base: &report.base,
                    per_request: report.per_request,
                    host: options.host.as_deref(),
                    port: options.port,
                    allow_hosts: &options.allow_hosts,
                });
            }
            // A rendered site is the files a render needs and no documents, so
            // there is nothing here for a file server to hand out. The spec
            // file names everything else, the compiled app included.
            if report.per_request {
                say_line!(
                    "lumenc web: pass --serve to render the pages here, run `lumen-server {}` \
                     to serve them, or point a server built on lumen-ssr at this directory: \
                     {SERVER_SPEC_FILE} names the files it renders from, the compiled app {} \
                     among them",
                    report.out.display(),
                    report.artifact
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
    /// `--host`, as written: lumen-server reads it, and says what is wrong
    /// with it.
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
    /// The compiled app the site was written with, relative to its root. A
    /// server renders from this one.
    artifact: String,
    /// Whether the pages are produced for the request that asks for them, so
    /// the directory holds what a render needs rather than the documents.
    per_request: bool,
    warnings: Vec<String>,
}

/// A file the site carries under a name taken from its own contents.
///
/// The build holds the bytes anyway, so it is the build that names the file
/// and puts the name in the [`WebSpec`]; the emitter writes whatever the
/// spec says.
struct NamedFile {
    /// Where it goes, relative to the site root.
    path: String,
    /// What goes there.
    bytes: Vec<u8>,
}

impl NamedFile {
    /// `bytes`, to be written as `name` with their hash in it.
    fn new(name: &str, bytes: Vec<u8>) -> Self {
        Self {
            path: lumen_web::content_name(name, &bytes),
            bytes,
        }
    }

    /// The same, for a file that is already on disk somewhere else.
    fn read(source: &Path, name: &str) -> Result<Self, String> {
        let bytes = std::fs::read(source).map_err(|e| format!("read {}: {e}", source.display()))?;
        Ok(Self::new(name, bytes))
    }
}

/// The prebuilt browser runtime a site carries.
struct WebRuntime {
    wasm: NamedFile,
    js: NamedFile,
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
    // A page takes each module's web half, the files it loads beside the
    // runtime; a browser cannot open the library a desktop build loads. A
    // candela package is script source, so it compiles into the app like the
    // app's own scripts and travels wherever the app does, the web included.
    let deps = crate::addons::target_deps(dir, Target::Web, options.lib_dir.as_deref())?;
    let desktop_only = cfg
        .dependencies_for(Target::Web)
        .0
        .into_iter()
        .find(|dep| !deps.packages.iter().any(|p| p.addon.name == dep.name));
    if let Some(dep) = desktop_only {
        return Err(format!(
            "this app declares '{}' as a dependency of its web build, and the module has no \
             web half (a web/lumen-addon.toml under its root): its library is loaded by \
             opening it, which a browser cannot do. Declare it under \
             [target.desktop.dependencies] to keep it out of the site, or ship the app as a \
             desktop package.",
            dep.name
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
    let mut compiled = crate::compile_app_with(dir, Some(&skin), &deps)
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
             page; pass --render ssr --serve to render them here, or list the host in lumen.toml \
             [web.ssr] allow_hosts, which a build rendered per request writes down for its \
             server"
                .to_string(),
        );
    }
    // The policy is written into the file a server renders from, and only a
    // build rendered per request writes one.
    if !per_request && cfg.raw.get("web").and_then(|web| web.get("ssr")).is_some() {
        warnings.push(
            "[web.ssr] says what a render may reach, and these pages are not rendered per \
             request; it applies once the site is built with render `ssr`"
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

    let plan = lumen_runtime::pages::discover(&src_dir(dir), &cfg);

    let locales = locales(options, &cfg);
    // Taken once and used twice: the build resolves `translatable` into every
    // tree it writes, and the same bytes travel with the site so the browser
    // reads what it builds after the page opens in the same language. The
    // compile has already parsed every catalogue and the chain, so a broken
    // one failed the build before it got here.
    let fallback = compiled.i18n.fallback.clone();
    let catalogues = site_catalogues(&compiled.i18n, &locales, &fallback_chain(&cfg));
    let parsed =
        Catalogues::parse(&catalogues, &fallback).map_err(|e| format!("locale catalogues: {e}"))?;

    // A component that has to run is resolved here, before anything else reads
    // the tree. Its body is markup like any other once it is in: the asset
    // rewrite below reaches an `<image>` inside it, the link check sees its
    // links, and the artifact the browser loads carries it, so the runtime
    // adopts the body the page already shows instead of building it again.
    //
    // The boot starts from the declared state, which is the state a page with
    // no run behind it is written with, so what a component renders for a row
    // is what that page shows. The seed is read again below, once the bodies
    // are in the tree, because a body can declare a signal default of its own.
    //
    // It runs in the root tree's locale, so a component that calls `t()`
    // renders the text the site root is written in.
    let declared = declared_seed(&seed_values(&cfg, &compiled.ir.root, prerender));
    let (seeded_fills, browser_filled) = crate::web::component_fill::fill(
        &mut compiled,
        &plan.entry_key,
        language(&locales[0], &parsed),
        &declared,
        &mut warnings,
    );

    // Assets travel with the site, so every `<image src>` and every `url()`
    // in a carried at-rule is rewritten from the path it has on this machine
    // to the path it will have on the server, and the files are copied there.
    let assets = collect_assets(&mut compiled.ir, dir, &mut warnings);

    // The sitemap says when each page last changed, and it has to say it the
    // same way for the same sources, so the dates come off the files rather
    // than off the clock this build ran on.
    let stamps = page_stamps(dir, &plan, &assets);

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
        match crate::package::cli::locate_web_runtime(options.lib_dir.as_deref()) {
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

    // Where the runtime came from is kept before the pair is read, because
    // the license text sits beside it (one level up in an installed
    // toolchain) and has to travel with the wasm that carries the engine.
    let runtime_dir = runtime
        .as_ref()
        .and_then(|files| files.wasm.parent().map(Path::to_path_buf));

    // The runtime pair is read rather than copied straight across, because a
    // file is named here after what is in it and the name has to be in the
    // spec before a document can point at it.
    let runtime = match &runtime {
        Some(files) => Some(WebRuntime {
            wasm: NamedFile::read(&files.wasm, DEFAULT_WASM_FILE)?,
            js: NamedFile::read(&files.js, DEFAULT_JS_FILE)?,
        }),
        None => None,
    };

    // Every add-on travels with the site, whether or not the documents run
    // anything: its stylesheets style its elements' fallback content too. The
    // documents load its module only when they carry the runtime.
    let mut addons = Vec::with_capacity(deps.packages.len());
    let mut addon_files = Vec::new();
    for package in &deps.packages {
        let (addon, files) = crate::addons::site::ship(package)?;
        addons.push(addon);
        addon_files.extend(files);
    }
    let foreign = deps
        .packages
        .iter()
        .flat_map(|package| &package.addon.elements)
        .map(|element| {
            (
                element.tag.clone(),
                ForeignElement {
                    html: element.html.clone(),
                    void: element.void,
                },
            )
        })
        .collect();

    let scripts = script_refs(&compiled, &mut warnings);
    check_exports(&compiled, &browser_filled, &mut warnings);
    let css_mode = match cfg.web.css {
        WebCssMode::Sheet => CssMode::Sheet,
        WebCssMode::Computed => CssMode::Computed,
    };

    // A style written on an element becomes a class and a rule, and the class
    // goes into the tree before the artifact is written: a row the browser
    // builds later is spawned from this tree, so it arrives already wearing
    // the class the stylesheet declares. In `computed` mode the cascade is
    // already resolved onto each element, so there is nothing to lift.
    let markup = match css_mode {
        CssMode::Computed => lumen_web::MarkupSheet::default(),
        CssMode::Sheet => lumen_web::lift_markup_styles(&mut compiled.ir.root),
    };

    // The catalogues travel in the artifact only where no catalogue file
    // travels beside it: a page that loads the runtime reads the files the
    // manifest names, so a copy inside the artifact would be downloaded for
    // nobody, and a server reads the same files through the spec. The
    // fallback chain stays, because nothing else carries it to the browser.
    if runtime.is_some() {
        compiled.i18n.catalogues.clear();
    }
    // The compiled app carries the site's asset paths, so a node built from it
    // points where the emitted markup points. The browser runtime loads it,
    // and so does the server that renders the pages, so a rendered site keeps
    // it whether or not its documents run anything.
    let artifact = if runtime.is_some() || per_request {
        let bytes = crate::artifact::serialize(&compiled)
            .map_err(|e| format!("serialize the compiled app: {e}"))?;
        Some(NamedFile::new(DEFAULT_ARTIFACT_FILE, bytes))
    } else {
        None
    };
    // A catalogue is only ever read by the browser runtime, so a site whose
    // documents carry none ships without them: their text was resolved into
    // the documents while they were written.
    let catalogue_files: Vec<(String, NamedFile)> = if runtime.is_some() {
        catalogues
            .iter()
            .map(|(tag, source)| {
                let name = format!("locale/{tag}.ftl");
                (
                    tag.clone(),
                    NamedFile::new(&name, source.clone().into_bytes()),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    // The stylesheet is built twice, here to name it and again in the
    // emitter to write it. The two agree because both read the same tree
    // through the same function.
    let sheet = lumen_web::styles_css(compiled.ir.combined_stylesheet.as_ref(), &markup, css_mode);

    let sitemap_on = cfg.web.sitemap.unwrap_or(true);
    let web = WebSpec {
        base_path: base.clone(),
        url: cfg.web.url.clone(),
        canonical: cfg.web.canonical.clone(),
        entry: entry.clone(),
        title: title(&cfg, dir),
        description: cfg.web.description.clone(),
        og_image: cfg.web.og_image.clone(),
        // Every file a build writes carries the hash of its own contents in
        // its name, so a redeploy writes names nothing has cached and a
        // visitor holding the last build's files fetches this one's.
        artifact: match &artifact {
            Some(file) => file.path.clone(),
            None => DEFAULT_ARTIFACT_FILE.to_string(),
        },
        css: lumen_web::content_name(DEFAULT_CSS_FILE, sheet.as_bytes()),
        css_mode,
        wasm: match &runtime {
            Some(runtime) => runtime.wasm.path.clone(),
            None => DEFAULT_WASM_FILE.to_string(),
        },
        js: match &runtime {
            Some(runtime) => runtime.js.path.clone(),
            None => DEFAULT_JS_FILE.to_string(),
        },
        catalogues: catalogue_files
            .iter()
            .map(|(tag, file)| (tag.clone(), file.path.clone()))
            .collect(),
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
        // when the site has one unless the app says not to.
        sitemap: sitemap_on,
        // The file's job is to name the sitemap, so it follows it; asking
        // for it outright writes the allow-all file either way.
        robots: cfg
            .web
            .robots
            .unwrap_or(sitemap_on && cfg.web.url.is_some()),
        runtime: runtime.is_some(),
        scripts,
        addons,
        foreign,
        ..WebSpec::default()
    };

    std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
    if let Some(artifact) = &artifact {
        write_file(&out.join(&artifact.path), &artifact.bytes)?;
    }
    for (_, file) in &catalogue_files {
        write_file(&out.join(&file.path), &file.bytes)?;
    }
    for file in &addon_files {
        write_file(&out.join(&file.path), &file.bytes)?;
    }
    // The compiled program beside it is the browser's copy: a render runs the
    // one inside the artifact. It is one of the two largest files a site would
    // otherwise carry for nobody.
    if web.runtime {
        write_bytecode(&compiled, &out)?;
    }

    let seed = seed_values(&cfg, &compiled.ir.root, prerender);
    // The app is run once per page and locale: what a script writes through
    // `t()` is in the language it ran in, and so is a row it builds.
    let settled = match prerender {
        WebPrerender::Run => run_pages(
            &compiled,
            &Runs {
                keys: &keys,
                entry: &entry,
                locales: &locales,
                catalogues: &parsed,
            },
            &seed,
            options.strict,
            &mut warnings,
        ),
        WebPrerender::Seeds | WebPrerender::None => BTreeMap::new(),
    };
    // What each page says about itself is the same in every language.
    let heads: Vec<PageHead> = keys.iter().map(|key| page_head(key, &cfg)).collect();
    let shared = SiteSpec {
        pages: Vec::new(),
        web: WebSpec {
            // A render answers for every locale, including the trees under a
            // locale prefix, so writing documents for them would put two
            // answers behind one address.
            per_request,
            ..web.clone()
        },
        locale: LocaleSpec::default(),
        assets: assets.clone(),
        markup: markup.clone(),
    };
    // One tree per locale, shared by every page of it: which page a document
    // shows is a signal inside the tree, not a tree of its own. Building the
    // trees is the emitter's, because a server holding the build's files
    // builds the same ones.
    let trees = lumen_web::locale_trees(
        SiteLocales {
            ir: &compiled.ir,
            keys: &keys,
            heads: &heads,
            locales: &locales,
            catalogues: &parsed,
            shared: &shared,
        },
        &mut warnings,
    );
    let mut pages_written = 0;
    for (index, LocaleTree { mut spec, i18n }) in trees.into_iter().enumerate() {
        let locale = spec.locale.locale.clone();
        for page in &mut spec.pages {
            let run = settled.get(&(locale.clone(), page.key.clone()));
            // A row's component body is markup the tree never held, so it is
            // translated on its own; without that a locale tree reads in its
            // language everywhere except inside its lists. A run read its
            // bodies already translated, in the language it ran in.
            let mut fills = match prerender {
                WebPrerender::Run => run.map(|run| run.fills.clone()).unwrap_or_default(),
                WebPrerender::Seeds => seeded_fills.clone(),
                WebPrerender::None => RowFills::default(),
            };
            if prerender != WebPrerender::Run
                && let Some(i18n) = &i18n
            {
                for body in fills.bodies_mut() {
                    lumen_web::translate_element(body, i18n);
                }
            }
            settle_page(page, &seed, prerender, run, fills);
            // What a page shows is the app's answer; when it last changed is
            // the sources', so it is put on here rather than rendered.
            page.modified = stamps.get(&page.key).copied();
        }
        let site = lumen_web::emit(&spec).map_err(|e| e.to_string())?;
        for file in &site.files {
            write_file(&out.join(&file.path), file.contents.as_bytes())?;
        }
        if index == 0 {
            pages_written = spec.pages.len();
            warnings.extend(site.warnings);
        }
    }

    for asset in &assets {
        copy_file(&asset.source, &out.join(&asset.path))?;
    }
    if let Some(runtime) = &runtime {
        write_file(&out.join(&runtime.wasm.path), &runtime.wasm.bytes)?;
        write_file(&out.join(&runtime.js.path), &runtime.js.bytes)?;
    }
    if let Some(dir) = &runtime_dir {
        crate::package::cli::stage_license_files(std::slice::from_ref(dir), &out)?;
    }
    // Which paths a file server has no file for is the build's to say; a
    // render answers every path with the page it names.
    if matches!(cfg.web.host, WebHost::Static) && !per_request {
        note_deep_paths(&compiled, &keys, &entry);
    }

    let artifact_path = web.artifact.clone();
    // A render starts from the compiled app rather than from the documents on
    // disk: the state a page is written with is what the app settles into for
    // the request asking, which is the whole difference between a rendered
    // page and a built one. What the build knew beyond the app goes into the
    // spec file, so a server of anyone's holds the site this build holds.
    if per_request {
        let spec = ServerSpec {
            locales: locales.clone(),
            fallback,
            pages: heads,
            seed: declared_seed(&seed),
            policy: ServerPolicy {
                allow_hosts: cfg.web.ssr.allow_hosts.clone(),
                max_requests: cfg.web.ssr.max_requests,
                headers: cfg.web.ssr.headers.clone(),
            },
            ..ServerSpec::new(web).with_images(&assets)
        };
        write_file(&out.join(SERVER_SPEC_FILE), spec.to_json().as_bytes())?;
    }

    Ok(Report {
        out,
        base,
        pages: pages_written,
        artifact: artifact_path,
        per_request,
        warnings,
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

/// What a build runs the app for: every page, in every locale.
struct Runs<'a> {
    keys: &'a [String],
    entry: &'a str,
    /// The locales the site is emitted in, the root tree's first.
    locales: &'a [String],
    catalogues: &'a Catalogues,
}

/// The language a run in `locale` is in.
///
/// A locale that is no language tag is said once, where its tree is written,
/// and a run in it reads in the text the author wrote.
fn language<'a>(locale: &'a str, catalogues: &'a Catalogues) -> Language<'a> {
    if locale.parse::<LanguageIdentifier>().is_ok() {
        Language { locale, catalogues }
    } else {
        Language::untranslated(locale)
    }
}

/// Run the app once for each page in each locale and keep the state each one
/// settles into, keyed by locale and page.
///
/// A page written from a run has to come out the same on every machine and on
/// every build, so the entry page of the root tree is always built twice and
/// compared, and `--strict` compares every page in every locale.
fn run_pages(
    compiled: &CompiledApp,
    runs: &Runs<'_>,
    seed: &BTreeMap<String, WebSeedValue>,
    strict: bool,
    warnings: &mut Vec<String>,
) -> BTreeMap<(String, String), Prerendered> {
    let declared = declared_seed(seed);
    let mut settled = BTreeMap::new();
    for (index, locale) in runs.locales.iter().enumerate() {
        let language = language(locale, runs.catalogues);
        // A page is named with its locale only when the site has more than
        // one; a one-language build names the page alone.
        let name = if runs.locales.len() > 1 {
            format!("{locale}/")
        } else {
            String::new()
        };
        for key in runs.keys {
            let run = prerender::page(compiled, key, language, &declared, Budget::default());
            report_run(&format!("{name}{key}"), &run, warnings);
            if (index == 0 && key == runs.entry) || strict {
                let again = prerender::page(compiled, key, language, &declared, Budget::default());
                if again.state != run.state {
                    warnings.push(format!(
                        "page `{name}{key}` settled differently the second time it was run, so \
                         what it holds depends on something outside the app"
                    ));
                }
            }
            settled.insert((locale.clone(), key.clone()), run);
        }
    }
    settled
}

/// What one run leaves a build to say.
fn report_run(key: &str, run: &Prerendered, warnings: &mut Vec<String>) {
    if let Some(error) = &run.language_error {
        warnings.push(format!(
            "page `{key}` ran untranslated, because it could not start in its locale: {error}"
        ));
    }
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
    for name in &run.browser_only {
        warnings.push(format!(
            "page `{key}` called `{name}`, which runs only in a browser; the call raised during \
             the build, so the page is written with what the app had before it, and the browser \
             makes the call on arrival"
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

/// What `key` says about itself in its `<head>`, from `[web.pages.<key>]`.
fn page_head(key: &str, cfg: &LumenToml) -> PageHead {
    let page_cfg = cfg.web.pages.get(key);
    PageHead {
        key: key.to_string(),
        title: page_cfg.and_then(|page| page.title.clone()),
        description: page_cfg.and_then(|page| page.description.clone()),
        index: page_cfg.and_then(|page| page.index).unwrap_or(true),
    }
}

/// Put the state `page` is written with onto it.
fn settle_page(
    page: &mut PageSpec,
    seed: &BTreeMap<String, WebSeedValue>,
    prerender: WebPrerender,
    settled: Option<&Prerendered>,
    fills: RowFills,
) {
    page.fills = fills;
    // A run started from the declared values and holds the page's route, so
    // what it settled into is the whole state this page is written with.
    if let Some(run) = settled {
        page.signals = run.state.signals.clone();
        page.seed = run.state.seed.clone();
        page.nodes = run.state.nodes.clone();
        return;
    }
    let key = page.key.as_str();
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
    page.signals = signals;
    page.seed = page_seed;
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
/// signal's value; [`settle_page`] puts them in the page's arrays instead.
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

/// The locales a key missing from a page's own catalogue falls through to:
/// what the app named in `[app] fallback_locale`, else what a run of the
/// same app falls through to, so a written page and a desktop run resolve a
/// miss the same way.
fn fallback_chain(cfg: &LumenToml) -> Vec<LanguageIdentifier> {
    match &cfg.app.fallback_locale {
        Some(fallback) => vec![fallback.clone()],
        None => I18nPlugin::default().fallback_chain,
    }
}

/// The catalogues of the compiled app a site reads, as a tag and its source.
///
/// Only the locales the site is emitted in are kept, plus the ones every other
/// falls back to: a catalogue for a locale the site has no tree for has no
/// reader on either side. A locale with no catalogue keeps nothing, which is
/// what leaves its pages reading in the source language.
fn site_catalogues(
    compiled: &CompiledI18n,
    locales: &[String],
    fallback: &[LanguageIdentifier],
) -> Vec<(String, String)> {
    let fallback: Vec<String> = fallback.iter().map(ToString::to_string).collect();
    compiled
        .catalogues
        .iter()
        .filter(|(tag, _)| locales.contains(tag) || fallback.contains(tag))
        .cloned()
        .collect()
}

/// When each page last changed, keyed by page key.
///
/// A page is dated by its own `.lmn` and by everything the whole app is built
/// from: `lumen.toml`, the shared markup, scripts, styles, assets and the
/// translation catalogues. Other pages' `.lmn` files are left out, because
/// counting them would give every page one identical date, which tells a
/// crawler nothing. A page nothing readable stands behind gets no entry and
/// is listed with no date.
fn page_stamps(dir: &Path, plan: &PagePlan, assets: &[AssetRef]) -> BTreeMap<String, SystemTime> {
    let pages: BTreeSet<PathBuf> = plan.pages.iter().map(|page| page.path.clone()).collect();
    let mut shared: Option<SystemTime> = None;
    let count = |path: &Path, into: &mut Option<SystemTime>| {
        let Ok(at) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
            return;
        };
        if into.is_none_or(|held| at > held) {
            *into = Some(at);
        }
    };
    count(&dir.join("lumen.toml"), &mut shared);
    for path in walk(&src_dir(dir)) {
        if !pages.contains(&path) {
            count(&path, &mut shared);
        }
    }
    for asset in assets {
        count(&asset.source, &mut shared);
    }
    for path in walk(&locale_dir(dir)) {
        count(&path, &mut shared);
    }

    let mut stamps = BTreeMap::new();
    for page in &plan.pages {
        let mut stamp = shared;
        count(&page.path, &mut stamp);
        if let Some(stamp) = stamp {
            stamps.insert(page.key.clone(), stamp);
        }
    }
    stamps
}

/// Every file under `dir`, however deep. A directory that cannot be read
/// contributes nothing.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// Move every asset the app points at into the site, and rewrite what points
/// at it to the path it lands on.
///
/// The markup points at one with an `<image src>` or a `bg="url(...)"`, and
/// the stylesheet points at one with a `url()` in a declaration, such as a
/// `bg` image or a custom property a theme swaps one through, or inside an
/// at-rule it carried, such as the font file a `@font-face` names. All of
/// them are resolved against the app directory and share the same set of
/// placed files, so two references to one file ship one copy.
fn collect_assets(ir: &mut LayoutIR, dir: &Path, warnings: &mut Vec<String>) -> Vec<AssetRef> {
    let mut outside: Vec<String> = Vec::new();
    relativize_asset_paths(&mut ir.root, dir, &mut outside);
    let mut assets: Vec<AssetRef> = Vec::new();
    let mut placed: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    rewrite_assets(&mut ir.root, dir, &mut assets, &mut placed, &mut taken);
    if let Some(sheet) = ir.combined_stylesheet.as_mut() {
        let mut place = |css: &str| {
            lumen_web::rewrite_css_urls(css, |url| {
                if is_external(url) {
                    return None;
                }
                Some(place_asset(url, dir, &mut assets, &mut placed, &mut taken))
            })
        };
        for rule in &mut sheet.rules {
            for declaration in &mut rule.declarations {
                declaration.value = place(&declaration.value);
            }
        }
        for at_rule in &mut sheet.at_rules {
            at_rule.body = place(&at_rule.body);
        }
    }
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
        element.attrs.src = Some(place_asset(&src, dir, assets, placed, taken));
    }
    // A `bg` attribute is held twice: parsed, for the build's own cascade, and
    // as written, for the rule the web target lifts off the element.
    if let Some(BgSpec::Image(path)) = element.attrs.bg.as_mut()
        && !is_external(path)
    {
        *path = place_asset(path, dir, assets, placed, taken);
    }
    for (_, value) in &mut element.attrs.markup_styles {
        *value = lumen_web::rewrite_css_urls(value, |url| {
            if is_external(url) {
                return None;
            }
            Some(place_asset(url, dir, assets, placed, taken))
        });
    }
    for child in &mut element.children {
        rewrite_assets(child, dir, assets, placed, taken);
    }
}

/// The path inside the site one file the app names lands on, recording it
/// for the copy that follows.
fn place_asset(
    src: &str,
    dir: &Path,
    assets: &mut Vec<AssetRef>,
    placed: &mut BTreeMap<PathBuf, String>,
    taken: &mut BTreeSet<String>,
) -> String {
    let source = if Path::new(src).is_absolute() {
        PathBuf::from(src)
    } else {
        dir.join(src)
    };
    if let Some(path) = placed.get(&source) {
        return path.clone();
    }
    let bytes = std::fs::read(&source).ok();
    let path = site_path(src, bytes.as_deref());
    // A name carries the hash of what is in the file, so two sources that
    // reach the same name hold the same bytes: one file under one name,
    // copied once, pointed at by both. The same read answers how big an image
    // is, which is what the page says so that it holds the image's place
    // before the bytes arrive.
    if taken.insert(path.clone()) {
        let asset = AssetRef::new(source.clone(), path.clone());
        let size = bytes
            .as_deref()
            .and_then(|bytes| intrinsic_size(bytes, src));
        assets.push(match size {
            Some(size) => asset.with_size(size),
            None => asset,
        });
    }
    placed.insert(source, path.clone());
    path
}

/// Where one asset lands inside the site. A file from inside the app keeps
/// the shape of its path and one from outside keeps its name alone, and
/// either way the name carries the hash of the file, so a redeploy of a
/// changed image is a URL nothing has a copy of.
///
/// A file that could not be read keeps its plain name; the copy that follows
/// is what reports it missing.
fn site_path(src: &str, bytes: Option<&[u8]>) -> String {
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
    let path = format!("{ASSET_DIR}/{candidate}");
    match bytes {
        Some(bytes) => lumen_web::content_name(&path, bytes),
        None => path,
    }
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
            Some(bytecode) => refs.push(ScriptRef {
                engine: script.engine.clone(),
                path: lumen_web::content_name(BYTECODE_FILE, bytecode),
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
/// box is what a reader would not notice. `browser_filled` names the markers
/// the fill pass left standing on purpose, which are the ones nothing called
/// and so the ones this must not speak for.
fn check_exports(
    compiled: &CompiledApp,
    browser_filled: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) {
    let mut exported: BTreeSet<String> = BTreeSet::new();
    let mut read_any = false;
    for script in &compiled.scripts {
        let Some(read_back) = lumen_runtime::run::script_exports(script, &compiled.addons) else {
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
        for (name, params) in called_by_name(&script.source) {
            if !exports.contains(&name) {
                warnings.push(format!(
                    "`{name}` is called by name and the compiled program does not export it, so \
                     nothing happens when it is called{}",
                    annotation_advice(&name, &params)
                ));
            }
        }
        exported.extend(exports);
        read_any = true;
    }

    // A component belongs to the app, not to whichever script the loop above
    // was on, so it is judged once against everything the app exports. With no
    // export list read there is nothing to judge it against.
    if !read_any {
        return;
    }
    // Every name still here is one the build ran and could not fill, so each
    // gets the reason it could not be. A component the build did fill is its
    // body by now and names nothing, and one the build left standing on
    // purpose was never called to judge.
    for name in components_called(compiled).difference(browser_filled) {
        if exported.contains(name) {
            warnings.push(format!(
                "`{name}` returned no markup when the build called it, so the page carries an \
                 empty box where its body belongs; a component returns one `lmn!` block"
            ));
        } else {
            let params = compiled
                .fragments
                .component(name)
                .map(|component| component.params.clone())
                .unwrap_or_default();
            warnings.push(format!(
                "the markup writes `<{name}/>`, and the compiled program does not export \
                 `{name}`, so the page carries an empty box where its body belongs{}",
                annotation_advice(name, &params)
            ));
        }
    }
}

/// The half of a warning that says how to make the program export `name`,
/// written out of the parameters the function declares.
///
/// candela exports a function only when every parameter it takes is
/// annotated, so the advice is the function's own parameter list with an
/// annotation on each one, which is text an author can paste. A function that
/// takes nothing is exported as written, so a missing name there is not an
/// annotation problem and the sentence ends where it is.
fn annotation_advice(name: &str, params: &[String]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let signature: Vec<String> = params.iter().map(|param| format!("{param}: any")).collect();
    format!(
        "; annotate every parameter it takes, as in `fn {name}({})`",
        signature.join(", ")
    )
}

/// Every component name the tree names, which the runtime fills by calling the
/// function of that name.
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

/// Every function `source` defines that something calls by name, with the
/// parameters it takes: a handler bound by name, a derivation body, or one of
/// the `on_` names Lumen calls when the thing they stand for happens.
fn called_by_name(source: &str) -> BTreeMap<String, Vec<String>> {
    let quoted: BTreeSet<&str> = source
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|text| is_identifier(text))
        .collect();
    defined_functions(source)
        .into_iter()
        .filter(|(name, _)| name.starts_with("on_") || quoted.contains(name.as_str()))
        .collect()
}

/// The functions `source` declares, each with the names of its parameters.
fn defined_functions(source: &str) -> BTreeMap<String, Vec<String>> {
    let mut functions = BTreeMap::new();
    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("fn ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('(') else {
            continue;
        };
        let name = name.trim();
        if is_identifier(name) {
            functions.insert(name.to_string(), parameter_names(rest));
        }
    }
    functions
}

/// The parameter names in a `fn` line, read off the text after its opening
/// parenthesis. An annotation a parameter already carries is left off, so a
/// list that is already annotated comes back the way it went in.
fn parameter_names(rest: &str) -> Vec<String> {
    rest.split(')')
        .next()
        .unwrap_or_default()
        .split(',')
        .map(|param| param.split(':').next().unwrap_or_default().trim())
        .filter(|name| is_identifier(name))
        .map(str::to_string)
        .collect()
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
            write_file(
                &out.join(lumen_web::content_name(BYTECODE_FILE, bytecode)),
                bytecode,
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The fix a warning prints is the function's own parameter list, not a
    /// stand-in: pasting it is the whole remedy.
    #[test]
    fn the_suggested_signature_names_the_parameters_the_function_takes() {
        let params = vec!["name".to_string(), "count".to_string()];
        assert_eq!(
            annotation_advice("Greet", &params),
            "; annotate every parameter it takes, as in `fn Greet(name: any, count: any)`"
        );
    }

    /// candela exports a function that takes nothing, so a missing name there
    /// is not an annotation problem and the sentence stops.
    #[test]
    fn a_function_that_takes_nothing_is_told_to_annotate_nothing() {
        assert!(annotation_advice("on_start", &[]).is_empty());
    }

    #[test]
    fn a_parameter_list_is_read_with_and_without_its_annotations() {
        let functions = defined_functions("fn Greet(name, count) {}\nfn Row(label: any) {}\n");
        assert_eq!(functions["Greet"], vec!["name", "count"]);
        assert_eq!(functions["Row"], vec!["label"]);
    }

    #[test]
    fn a_function_that_takes_nothing_has_an_empty_parameter_list() {
        let functions = defined_functions("fn main() {}\n");
        assert!(functions["main"].is_empty());
    }
}
