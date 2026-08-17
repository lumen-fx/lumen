//! `lumenc web <app_dir>` - emit an app as a static site.
//!
//! The app is compiled exactly the way `lumenc build` compiles it, and the
//! result is written out as HTML: one document per page, with the markup
//! already in it. The stylesheet and the assets are written beside the pages,
//! and `[web] render` decides whether the compiled app and the browser runtime
//! join them.
//!
//! What a site is made of is [`lumen_web`]'s to decide; this reads the app,
//! hands the emitter a [`SiteSpec`], and puts the files it gets back on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lumen_core::nav::{PATH_SIGNAL, SEGMENT_SIGNAL, resolve_path};
use lumen_core::signals::ArrayItem;
use lumen_html::contract::{
    DEFAULT_ARTIFACT_FILE, NavigationMode, ScriptFormat, ScriptRef, Seed, SeedValue,
};
use lumen_i18n::{I18n, LanguageIdentifier, SharedI18n, translated_or_authored};
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Element, LayoutIR, relativize_asset_paths};
use lumen_runtime::config::{
    LumenToml, WebCssMode, WebHost, WebNavigation, WebPrerender, WebRender, WebSeedValue,
};
use lumen_runtime::run::locale_dir;
use lumen_web::urls::is_external;
use lumen_web::{
    AssetRef, CssMode, HostRewrite, LocaleSpec, PageSpec, SignalEnv, SiteSpec, WebSpec,
};

use crate::web_serve::Server;

/// Where a site is written when `lumen.toml` and `--out` both stay quiet.
const DEFAULT_OUT_DIR: &str = "dist/web";

/// Directory inside the site that the app's own files are copied into.
const ASSET_DIR: &str = "assets";

/// File the compiled candela program is written as.
const BYTECODE_FILE: &str = "app.cdlb";

/// Port `--serve` listens on when none is named.
const DEFAULT_PORT: u16 = 8787;

const WEB_USAGE: &str = "lumenc web - emit an app as a static site

USAGE:
    lumenc web <app_dir> [--out DIR] [--base PATH] [--locale TAG]...
                         [--render static|csr] [--prerender seeds|none]
                         [--no-hooks] [--lib-dir DIR] [--strict]
                         [--serve [--port N]]

Compiles the app and writes one HTML document per page, the stylesheet and
the app's assets. The documents carry the markup already rendered, so a page
reads without scripting. Under --render csr the compiled app and the browser
runtime are written beside them, and the pages load them.

    --out DIR         Where the site is written (default: lumen.toml
                      [web] out_dir, else dist/web).
    --base PATH       URL prefix the site is served under, such as /docs
                      (default: [web] base_path, else /).
    --locale TAG      Emit a document tree for this locale. Repeat for
                      more; the first is served from the site root
                      (default: [web] locales, else [app] locale).
    --render MODE     Whether the pages carry the browser runtime: csr
                      loads it, static is files alone (default: [web]
                      render). Both write the whole markup tree.
    --prerender MODE  Where the state the pages are rendered with comes
                      from: seeds (lumen.toml [web.seed] and the markup)
                      or none (default: [web] prerender).
    --no-hooks        Skip the app's prebuild [[hooks]].
    --lib-dir DIR     Directory holding lumen-web.wasm and lumen-web.js,
                      instead of the ones shipped with lumenc.
    --strict          Fail the build on any warning it prints.
    --serve           Serve the site after emitting it, and print the URL.
    --port N          Port to serve on (default: 8787; 0 picks a free one).";

/// Entry: `lumenc web <app_dir> [flags]`.
pub fn cmd_web(args: impl Iterator<Item = String>) -> ExitCode {
    let options = match parse_args(args) {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("lumenc web: {message}\n\n{WEB_USAGE}");
            return ExitCode::from(2);
        }
    };
    match build(&options) {
        Ok(report) => {
            for warning in &report.warnings {
                eprintln!("lumenc web: warning: {warning}");
            }
            println!(
                "lumenc web: {} page{} -> {}",
                report.pages,
                if report.pages == 1 { "" } else { "s" },
                report.out.display()
            );
            if options.strict && !report.warnings.is_empty() {
                eprintln!("lumenc web: --strict: {} warning(s)", report.warnings.len());
                return ExitCode::FAILURE;
            }
            if options.serve {
                return serve(&report.out, &report.base, options.port);
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("lumenc web: {message}");
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
    prerender: Option<WebPrerender>,
    no_hooks: bool,
    lib_dir: Option<PathBuf>,
    strict: bool,
    serve: bool,
    port: u16,
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
        prerender: None,
        no_hooks: false,
        lib_dir: None,
        strict: false,
        serve: false,
        port: DEFAULT_PORT,
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
                println!("{WEB_USAGE}");
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
                    other => {
                        return Err(format!(
                            "unknown --render mode `{other}` (expected static or csr)"
                        ));
                    }
                });
            }
            "--prerender" => {
                let mode = value("--prerender")?;
                options.prerender = Some(match mode.as_str() {
                    "seeds" => WebPrerender::Seeds,
                    "none" => WebPrerender::None,
                    other => {
                        return Err(format!(
                            "unknown --prerender mode `{other}` (expected seeds or none)"
                        ));
                    }
                });
            }
            "--no-hooks" => options.no_hooks = true,
            "--lib-dir" => options.lib_dir = Some(PathBuf::from(value("--lib-dir")?)),
            "--strict" => options.strict = true,
            "--serve" => options.serve = true,
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
    warnings: Vec<String>,
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

    // Assets travel with the site, so every `<image src>` is rewritten from
    // the path it has on this machine to the path it will have on the
    // server, and the files are copied there.
    let assets = collect_assets(&mut compiled.ir.root, dir, &mut warnings);

    let plan = lumen_runtime::pages::discover(dir, &cfg);
    let keys: Vec<String> = plan.pages.iter().map(|page| page.key.clone()).collect();
    let keys = if keys.is_empty() {
        vec![plan.entry_key.clone()]
    } else {
        keys
    };
    let entry = plan.entry_key.clone();
    check_links(&compiled.ir.root, &keys, &entry, &mut warnings);

    // A static site was asked for files alone, so nothing is looked for and
    // there is nothing to warn about. Under `csr` a missing runtime is still
    // only a missing file: the pages are emitted the way a static site's are,
    // and the build says which one it could not find.
    let runtime = match render {
        WebRender::Static => {
            if options.lib_dir.is_some() {
                warnings.push(
                    "--lib-dir names a runtime for pages to load, and a static site loads none; \
                     pass --render csr to use it"
                        .to_string(),
                );
            }
            None
        }
        WebRender::Csr => {
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
        }
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
    // The compiled app the browser loads is the one with the site's asset
    // paths in it, so a node the runtime creates points where the emitted
    // markup points. A static site loads neither, and they are the two
    // largest files a site would otherwise carry for nobody.
    if web.runtime {
        crate::artifact::write(&out.join(DEFAULT_ARTIFACT_FILE), &compiled)
            .map_err(|e| format!("write {}: {e}", out.join(DEFAULT_ARTIFACT_FILE).display()))?;
        write_bytecode(&compiled, &out)?;
    }

    let seed = seed_values(&cfg, &compiled.ir.root, prerender);
    let mut pages_written = 0;
    for (index, locale) in locales.iter().enumerate() {
        let mut spec = SiteSpec {
            pages: Vec::new(),
            web: web.clone(),
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
        let ir = translated_ir(&compiled.ir, dir, locale, &mut warnings)?;
        for key in &keys {
            spec.pages.push(page_spec(key, &ir, &cfg, &seed, prerender));
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
        copy_file(&runtime.wasm, &out.join(web.wasm.as_str()))?;
        copy_file(&runtime.js, &out.join(web.js.as_str()))?;
    }
    if matches!(cfg.web.host, WebHost::Static) {
        note_deep_paths(&compiled, &keys, &entry);
    }

    Ok(Report {
        out,
        base,
        pages: pages_written,
        warnings,
    })
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

/// One page, rendered with the state it arrives in.
fn page_spec(
    key: &str,
    ir: &LayoutIR,
    cfg: &LumenToml,
    seed: &BTreeMap<String, WebSeedValue>,
    prerender: WebPrerender,
) -> PageSpec {
    let page_cfg = cfg.web.pages.get(key);
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
        ir: ir.clone(),
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
fn check_exports(compiled: &CompiledApp, warnings: &mut Vec<String>) {
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
        for name in called_by_name(&script.source) {
            if !exports.contains(&name) {
                warnings.push(format!(
                    "`{name}` is called by name and the compiled program does not export it, so \
                     nothing happens when it is called; annotate every parameter it takes, as in \
                     `fn {name}(id: any)`"
                ));
            }
        }
    }
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
    println!(
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
fn serve(out: &Path, base: &str, port: u16) -> ExitCode {
    let server = match Server::bind(out, base, port) {
        Ok(server) => server,
        Err(message) => {
            eprintln!("lumenc web: {message}");
            return ExitCode::FAILURE;
        }
    };
    println!("lumenc web: serving {} at {}", out.display(), server.url());
    println!("lumenc web: press Ctrl-C to stop");
    server.run();
    ExitCode::SUCCESS
}
