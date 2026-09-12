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
use std::time::SystemTime;

use lumen_core::nav::{PATH_SIGNAL, SEGMENT_SIGNAL, resolve_path};
use lumen_core::signals::ArrayItem;
use lumen_core::{say_line, warn_line};
use lumen_html::contract::{
    DEFAULT_ARTIFACT_FILE, DEFAULT_CSS_FILE, DEFAULT_JS_FILE, DEFAULT_WASM_FILE, NavigationMode,
    ScriptFormat, ScriptRef, Seed, SeedValue,
};
use lumen_i18n::{I18n, I18nPlugin, LanguageIdentifier, SharedI18n};
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Element, LayoutIR, relativize_asset_paths};
use lumen_prerender::{self as prerender, Budget, Prerendered, Settled};
use lumen_runtime::app_layout::src_dir;
use lumen_runtime::config::{
    LumenToml, WebCssMode, WebHost, WebNavigation, WebPrerender, WebRender, WebSeedValue,
};
use lumen_runtime::pages::PagePlan;
use lumen_runtime::run::locale_dir;
use lumen_ssr::{FetchPolicy, RenderOptions, SsrSite};
use lumen_web::urls::is_external;
use lumen_web::{
    AssetRef, CssMode, HostRewrite, LocaleSpec, PageSpec, RowFills, SignalEnv, SiteSpec, WebSpec,
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
            // there is nothing here for a file server to hand out. The
            // compiled app is named, because a name carries the hash of the
            // file and there is no document here to read it out of.
            if report.per_request {
                say_line!(
                    "lumenc web: pass --serve to render the pages here, or point a server built \
                     on lumen-ssr at this directory and render from {}",
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
    /// The app a server renders per request, when one was asked for. It holds
    /// one tree per locale, so a rendered page reads in the language the
    /// request asks for.
    site: Option<SsrSite>,
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
    // Runtime modules are native shared libraries the engine dlopens, and a
    // browser has no dynamic loader to hand one to.
    if let Some(dep) = cfg.dependencies.0.first() {
        return Err(format!(
            "this app declares [dependencies] ('{}'), and runtime modules do not exist on the \
             web: a module is a native library the engine loads, which a browser cannot do. \
             Drop the declaration or ship the app as a desktop package.",
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

    let plan = lumen_runtime::pages::discover(&src_dir(dir), &cfg);

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
    let declared = declared_seed(&seed_values(&cfg, &compiled.ir.root, prerender));
    let (seeded_fills, browser_filled) =
        crate::component_fill::fill(&mut compiled, &plan.entry_key, &declared, &mut warnings);

    // Assets travel with the site, so every `<image src>` is rewritten from
    // the path it has on this machine to the path it will have on the
    // server, and the files are copied there.
    let assets = collect_assets(&mut compiled.ir.root, dir, &mut warnings);

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

    let scripts = script_refs(&compiled, &mut warnings);
    check_exports(&compiled, &browser_filled, &mut warnings);
    let locales = locales(options, &cfg);
    // Read once and used twice: the build resolves `translatable` into every
    // tree it writes, and the same bytes travel with the site so the browser
    // reads what it builds after the page opens in the same language.
    let fallback = fallback_chain(&cfg);
    let catalogues = read_catalogues(dir, &locales, &fallback)?;
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
        ..WebSpec::default()
    };

    std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
    if let Some(artifact) = &artifact {
        write_file(&out.join(&artifact.path), &artifact.bytes)?;
    }
    for (_, file) in &catalogue_files {
        write_file(&out.join(&file.path), &file.bytes)?;
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
    let mut served: Vec<SiteSpec> = Vec::new();
    for (index, locale) in locales.iter().enumerate() {
        let mut spec = SiteSpec {
            pages: Vec::new(),
            web: WebSpec {
                // A render answers for every locale, including the trees under
                // a locale prefix, so writing documents for them would put two
                // answers behind one address.
                per_request,
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
        let catalogue = locale_catalogue(&catalogues, locale, &fallback, &mut warnings)?;
        // Resolving the text is the emitter's, because a server holding a tree
        // per locale builds one the same way.
        let ir = Arc::new(match &catalogue {
            Some(i18n) => lumen_web::translate_ir(&compiled.ir, i18n),
            None => compiled.ir.clone(),
        });
        for key in &keys {
            // A row's component body is markup the tree never held, so it is
            // translated on its own; without that a locale tree reads in its
            // language everywhere except inside its lists.
            let mut fills = match prerender {
                WebPrerender::Run => settled
                    .get(key)
                    .map(|run| run.fills.clone())
                    .unwrap_or_default(),
                WebPrerender::Seeds => seeded_fills.clone(),
                WebPrerender::None => RowFills::default(),
            };
            if let Some(i18n) = &catalogue {
                for body in fills.bodies_mut() {
                    lumen_web::translate_element(body, i18n);
                }
            }
            let mut page = page_spec(key, &ir, &cfg, &seed, prerender, settled.get(key), fills);
            // What a page shows is the app's answer; when it last changed is
            // the sources', so it is put on here rather than rendered.
            page.modified = stamps.get(key).copied();
            spec.pages.push(page);
        }
        let site = lumen_web::emit(&spec).map_err(|e| e.to_string())?;
        for file in &site.files {
            write_file(&out.join(&file.path), file.contents.as_bytes())?;
        }
        if index == 0 {
            pages_written = spec.pages.len();
            warnings.extend(site.warnings);
        }
        // A render answers in whichever of these the request asks for, so it
        // is handed every one of them.
        if per_request && options.serve {
            served.push(spec);
        }
    }

    for asset in &assets {
        copy_file(&asset.source, &out.join(&asset.path))?;
    }
    if let Some(runtime) = &runtime {
        write_file(&out.join(&runtime.wasm.path), &runtime.wasm.bytes)?;
        write_file(&out.join(&runtime.js.path), &runtime.js.bytes)?;
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
    // page and a built one. The files beside the documents are still the
    // build's, and the server sends them from the directory.
    let site = if served.is_empty() {
        None
    } else {
        let mut site = SsrSite::new(compiled, web).map_err(|e| e.to_string())?;
        for tree in served {
            site = site.with_locale(tree).map_err(|e| e.to_string())?;
        }
        Some(site.with_seed(declared_seed(&seed)))
    };

    Ok(Report {
        out,
        base,
        pages: pages_written,
        artifact: artifact_path,
        per_request,
        warnings,
        site,
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
) -> BTreeMap<String, Prerendered> {
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
        settled.insert(key.clone(), run);
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
    settled: Option<&Prerendered>,
    fills: RowFills,
) -> PageSpec {
    let page_cfg = cfg.web.pages.get(key);
    // A run started from the declared values and holds the page's route, so
    // what it settled into is the whole state this page is written with.
    if let Some(run) = settled {
        return PageSpec {
            key: key.to_string(),
            ir: Arc::clone(ir),
            title: page_cfg.and_then(|page| page.title.clone()),
            description: page_cfg.and_then(|page| page.description.clone()),
            index: page_cfg.and_then(|page| page.index).unwrap_or(true),
            signals: run.state.signals.clone(),
            seed: run.state.seed.clone(),
            nodes: run.state.nodes.clone(),
            fills,
            modified: None,
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
        index: page_cfg.and_then(|page| page.index).unwrap_or(true),
        signals,
        seed: page_seed,
        nodes: BTreeMap::new(),
        fills,
        modified: None,
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

/// The catalogue `locale`'s documents are written through, which is what makes
/// a page readable in that language with nothing running: every `translatable`
/// element's text is resolved through it, and so is a row's component body.
///
/// `None` for a locale that is not a language tag: the pages are still
/// emitted, in the text the author wrote.
fn locale_catalogue(
    catalogues: &[(String, String)],
    locale: &str,
    fallback: &[LanguageIdentifier],
    warnings: &mut Vec<String>,
) -> Result<Option<SharedI18n>, String> {
    let lang = match locale.parse::<LanguageIdentifier>() {
        Ok(lang) => lang,
        Err(e) => {
            warnings.push(format!("locale `{locale}` is not a valid BCP-47 tag: {e}"));
            return Ok(None);
        }
    };
    // A page is written through the chain a desktop run resolves through,
    // down to dropping a fallback the page is already being written in.
    let fallback: Vec<LanguageIdentifier> = fallback
        .iter()
        .filter(|other| **other != lang)
        .cloned()
        .collect();
    let mut i18n = I18n::new(lang, fallback);
    for (tag, source) in catalogues {
        let tag = tag
            .parse::<LanguageIdentifier>()
            .map_err(|e| format!("locale `{tag}` is not a valid BCP-47 tag: {e}"))?;
        i18n.load_ftl(tag, source)
            .map_err(|e| format!("locale catalogues: {e}"))?;
    }
    Ok(Some(SharedI18n::new(i18n)))
}

/// The app's Fluent catalogues, as a tag and its source, one entry per
/// locale that has a file.
///
/// Only the locales the site is emitted in are read, plus the one every other
/// falls back to (`[app] fallback_locale`): a catalogue for a locale the site
/// has no tree for has no reader on either side. A locale with no file loads
/// nothing, which is what leaves its pages reading in the source language.
fn read_catalogues(
    dir: &Path,
    locales: &[String],
    fallback: &[LanguageIdentifier],
) -> Result<Vec<(String, String)>, String> {
    let mut tags: Vec<String> = locales.to_vec();
    for lang in fallback {
        let tag = lang.to_string();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    let mut out = Vec::new();
    for tag in &tags {
        // A build reads the author's loose files; no asset chain exists yet.
        let path = locale_dir(dir).join(format!("{tag}.ftl"));
        match std::fs::read_to_string(&path) {
            Ok(source) => out.push((tag.clone(), source)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        }
    }
    Ok(out)
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
            let bytes = std::fs::read(&source).ok();
            let path = site_path(&src, bytes.as_deref());
            // A name carries the hash of what is in the file, so two sources
            // that reach the same name hold the same bytes: one file under
            // one name, copied once, pointed at by both.
            if taken.insert(path.clone()) {
                assets.push(AssetRef::new(source.clone(), path.clone()));
            }
            placed.insert(source.clone(), path.clone());
            path
        });
        element.attrs.src = Some(path);
    }
    for child in &mut element.children {
        rewrite_assets(child, dir, assets, placed, taken);
    }
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
        let handler = match RenderHandler::start(site, render) {
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
