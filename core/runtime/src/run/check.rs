use super::*;
use crate::app_layout::AppLayout;

/// Summary returned by [`check_app`].
#[derive(Debug, Clone, Copy)]
pub struct CheckReport {
    /// Number of elements parsed (including root).
    pub element_count: usize,
    /// Whether the markup contained a non-empty `<script>` block.
    pub has_script: bool,
}

/// What a compile takes from outside the app directory: the target it builds
/// for, and what `lumenc` resolved for that target before it started.
///
/// The runtime resolves nothing itself, so a caller with no registry and no
/// add-on in reach passes [`CompileDeps::new`] and the compile reads the app
/// alone.
#[derive(Debug, Clone)]
pub struct CompileDeps {
    /// The target the program is compiled for: it decides which
    /// `[target.<name>.dependencies]` table applies, and the names a script's
    /// compile-time conditions are true for
    /// ([`lumen_modules::Target::cfg_flags`]).
    pub target: lumen_modules::Target,
    /// Script libraries the app imports, as the name a script imports under
    /// and the directory holding its sources.
    pub import_roots: Vec<(String, PathBuf)>,
    /// What the web halves of the modules the app depends on declare. A
    /// module's descriptor is its script surface on every target, so their
    /// functions are declared to the compile and their elements to the
    /// parser whatever the target, and the compiled app carries them. The
    /// functions are declared here and bound, at run time, to whatever the
    /// loaded module registers.
    pub addons: Vec<lumen_ir::addon::Addon>,
    /// What the modules with no web half register for scripts, read by
    /// installing each the way a run does ([`module_surface`]). Empty for a
    /// web build, which cannot take such a module.
    pub modules: ModuleSurface,
}

impl CompileDeps {
    /// A compile for `target` with nothing resolved from outside the app.
    pub fn new(target: lumen_modules::Target) -> Self {
        Self {
            target,
            import_roots: Vec::new(),
            addons: Vec::new(),
            modules: ModuleSurface::default(),
        }
    }
}

/// What a set of modules registers for scripts: the functions a program
/// calls them by, and the language sources compiled ahead of it.
///
/// Each function keeps its name, namespace and signature, and its body is
/// replaced by one that answers the zero of its declared return type (`0`,
/// `false`, the empty string, an empty list or map, null). A compile declares
/// the functions and a check runs the script's `main` once, so a call there
/// answers without doing the module's work.
#[derive(Debug, Clone, Default)]
pub struct ModuleSurface {
    /// The functions, in registration order.
    pub fns: Vec<lumen_script::ScriptFn>,
    /// The language sources, in registration order.
    pub preludes: Vec<lumen_script::ScriptPrelude>,
}

/// What the modules in `deps` register for scripts, read the way a run reads
/// it: each is installed into a scratch app, in headless mode, by the loader a
/// run uses, and its registrations are taken back out. A module compiled into
/// this binary answers from there, and any other is opened from disk, so the
/// surface is the one the app's run binds to.
///
/// `dir` is the app directory and `resolved` what the compiler resolved for
/// the `version` sources. A module that does not load prints the loader's
/// banner and registers nothing, so a call into it fails the compile that
/// follows.
#[cfg(feature = "modules")]
pub fn module_surface(
    dir: &Path,
    deps: &lumen_modules::DependenciesCfg,
    resolved: &lumen_modules::ResolvedModules,
) -> ModuleSurface {
    if deps.0.is_empty() {
        return ModuleSurface::default();
    }
    let app_id = crate::config::LumenToml::load_or_default(dir)
        .ok()
        .and_then(|cfg| cfg.app.id)
        .unwrap_or_else(|| lumen_capability::derive_app_id(dir));
    let mut app = App::new();
    app.world
        .insert_resource(lumen_core::app::RunMode { headless: true });
    let env = crate::modules::InitEnv {
        app_dir: dir.to_path_buf(),
        app_id,
        headless: true,
        hot_reload: false,
    };
    crate::modules::load_modules(&mut app, dir, deps, resolved, &env);
    let Some(registry) = app.world.get_resource::<lumen_script::ScriptFnRegistry>() else {
        return ModuleSurface::default();
    };
    ModuleSurface {
        fns: registry.fns().iter().map(answering_zero).collect(),
        preludes: registry.preludes().to_vec(),
    }
}

/// `f` with a body that answers the zero of its declared return type.
#[cfg(feature = "modules")]
fn answering_zero(f: &lumen_script::ScriptFn) -> lumen_script::ScriptFn {
    let zero = zero_of(&f.sig.ret);
    let mut declared = f.clone();
    declared.body = std::sync::Arc::new(move |_| Ok(zero.clone()));
    declared
}

/// The zero of a declared type.
#[cfg(feature = "modules")]
fn zero_of(ty: &lumen_script::ScriptTy) -> lumen_script::ScriptValue {
    use lumen_script::{ScriptTy, ScriptValue};
    match ty {
        ScriptTy::Bool => ScriptValue::Bool(false),
        ScriptTy::Int => ScriptValue::I64(0),
        ScriptTy::Float => ScriptValue::F64(0.0),
        ScriptTy::Str => ScriptValue::Str(String::new()),
        ScriptTy::Array(_) => ScriptValue::Array(Vec::new()),
        ScriptTy::Map(_) => ScriptValue::Map(Default::default()),
        ScriptTy::Struct(shape) => shape.default_value(),
        ScriptTy::Any | ScriptTy::Unit | ScriptTy::Dynamic => ScriptValue::Unit,
    }
}

/// The functions a compile declares: the add-ons' functions, bound to the
/// body every target without a page binds, and the modules' functions as
/// [`ModuleSurface`] holds them. A compiler declares them; nothing a run
/// registers comes from here, since a run binds the module's own.
#[cfg(feature = "runtime-parse")]
fn declared_fns(deps: &CompileDeps) -> Result<Vec<lumen_script::ScriptFn>, RunError> {
    let mut fns = addon_stubs(&deps.addons)?;
    fns.extend(deps.modules.fns.iter().cloned());
    Ok(fns)
}

/// The add-ons' functions alone, bound to the body every target without a
/// page binds.
#[cfg(feature = "runtime-parse")]
fn addon_stubs(addons: &[lumen_ir::addon::Addon]) -> Result<Vec<lumen_script::ScriptFn>, RunError> {
    let mut fns = Vec::new();
    for addon in addons {
        fns.extend(lumen_script::addon::browser_only_fns(addon, None).map_err(RunError::Script)?);
    }
    Ok(fns)
}

/// Ahead-of-time compile an app directory into a
/// [`lumen_ir::artifact::CompiledApp`]: parse markup + CSS once, run the full
/// cascade, resolve asset / include / import paths, and bake the combined
/// script source. The returned artifact is what `lumenc build` writes to
/// disk and what a parser-free runtime loads via [`RunOptions::artifact`].
///
/// This is the AOT counterpart to [`load_ir`]: same front-end work, on the
/// same discovered page set, but the result is serialized instead of spawned.
/// A multi-page app compiles whole - every page assembled into the one gated
/// tree the run path builds, plus the page set the routing needs. Requires the
/// source parser (`runtime-parse` feature).
///
/// What a script compiler warned about is added to `warnings`, one line each,
/// for the caller to print where it prints its own warnings.
#[cfg(feature = "runtime-parse")]
pub fn compile_app(
    dir: &Path,
    parser: &dyn SourceParser,
    plugins: &dyn crate::compiler_plugins::CompilerPlugins,
    deps: &CompileDeps,
    warnings: &mut Vec<String>,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    compile_app_with_skin(dir, parser, plugins, None, deps, warnings)
}

/// [`compile_app`] with the skin named outright instead of read from
/// `lumen.toml`.
///
/// One caller needs that: a site is built once and served to every OS, so it
/// cannot let `[skin] name = "auto"` resolve against whichever machine ran
/// the build. Everything else compiles an app for the machine it will run
/// on and calls [`compile_app`].
#[cfg(feature = "runtime-parse")]
pub fn compile_app_with_skin(
    dir: &Path,
    parser: &dyn SourceParser,
    plugins: &dyn crate::compiler_plugins::CompilerPlugins,
    skin: Option<&str>,
    deps: &CompileDeps,
    warnings: &mut Vec<String>,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    register_declared_tags(&cfg, &[deps.target], &deps.addons);
    let stubs = declared_fns(deps)?;
    let layout = AppLayout::resolve(dir, &cfg)?;
    // The same discovery the run path does, so compiling sees the app the way
    // running it does: the entry file it would open, and every sibling page.
    let plan = crate::pages::discover(&layout.src_dir, &cfg);
    let html_path = plan.entry_file.clone();
    let css_path = layout.css_path;
    let asset_roots = cfg.resolved_asset_roots(dir);
    let skin_override = skin.map(str::to_string).or_else(|| cfg.skin.name.clone());
    let loaded = load_ir(
        parser,
        plugins,
        &html_path,
        &css_path,
        dir,
        &asset_roots,
        skin_override.as_deref(),
        &lumen_ir::css::MediaContext::default(),
        SourceOverrides {
            plan: Some(&plan),
            ..SourceOverrides::default()
        },
    )?;
    // Concatenate inline + external `<script>` sources once, then strip both
    // from the IR: the artifact carries the combined string in its own field
    // so the parser-free runtime never re-reads `.rhai` files.
    let script_source = combined_script_source(&loaded.ir, dir)?;
    // Which engine runs which part of the program is decided here, at compile
    // time, from the script files' own extensions. The runtime cannot
    // rediscover it later: a shipped app carries no `.lua` / `.rhai` files for
    // the directory scan to read, and the flattened source above has no
    // language boundary left in it.
    let uri = html_path.display().to_string();
    let mut scripts = Vec::new();
    for (engine, source) in grouped_script_sources(&loaded.ir, dir, &cfg)? {
        scripts.push(lumen_ir::artifact::CompiledScript {
            engine: engine.name().to_string(),
            // An engine with an ahead-of-time form compiles here, so the
            // artifact carries the program a compiler-free runtime can run.
            // The others have none, and are run from the source beside it.
            bytecode: compiled_bytecode(
                engine,
                &source,
                &uri,
                &layout.lib_dir,
                deps,
                &stubs,
                warnings,
            )?,
            source,
        });
    }
    // Routing data for a multi-page app. The pages themselves are already in
    // the tree, each behind its gate; this is the part the runtime would
    // otherwise rediscover by listing `.lmn` files.
    let pages = plan.multipage.then(|| lumen_ir::artifact::CompiledPages {
        entry: plan.entry_key.clone(),
        keys: plan.keys(),
    });
    let mut ir = loaded.ir;
    ir.script_source = String::new();
    ir.external_scripts.clear();
    // Every catalogue travels in the artifact, so an app compiled here reads
    // in its languages with no `locale/` directory beside it. A loose file
    // still wins where there is one. Each is parsed here, so a broken one
    // fails the build rather than the app.
    let fallback: Vec<String> = cfg
        .app
        .fallback_locale
        .iter()
        .map(ToString::to_string)
        .collect();
    let catalogues = lumen_i18n::read_catalogues(&super::locale_dir(dir), |p| std::fs::read(p))
        .and_then(|catalogues| {
            lumen_i18n::Catalogues::parse(&catalogues, &fallback).map(|_| catalogues)
        })
        .map_err(|e| RunError::I18n(e.to_string()))?;
    let i18n = lumen_ir::artifact::CompiledI18n {
        catalogues,
        fallback,
    };
    Ok(lumen_ir::artifact::CompiledApp {
        ir,
        script_source,
        scripts,
        pages,
        // Every fragment the app declares, whether or not this build
        // instantiates it: the artifact carries the declarations, not just
        // their expansions.
        fragments: loaded.fragments,
        i18n,
        addons: deps.addons.clone(),
    })
}

/// The compiled bytecode image for one engine's program, or `None` for an
/// engine that has no ahead-of-time form.
///
/// candela is the one that does: its `.cdlb` image is what `candela-vm` runs
/// where the compiler is absent. A build without the candela host trimmed in
/// cannot produce one, and writes the source alone.
///
/// `fns` are declared to the compile the way a live run declares a module's
/// functions, so the image names them and a runtime that binds the same
/// names can load it. Their bodies are never called here.
///
/// What the compiler warned about is added to `warnings`, one line each.
#[cfg(feature = "runtime-parse")]
fn compiled_bytecode(
    engine: crate::config::ScriptEngine,
    source: &str,
    uri: &str,
    lib_dir: &Path,
    deps: &CompileDeps,
    fns: &[lumen_script::ScriptFn],
    warnings: &mut Vec<String>,
) -> Result<Option<Vec<u8>>, RunError> {
    #[cfg(feature = "host-candela")]
    if engine == crate::config::ScriptEngine::Candela {
        let host = candela_compiler(lib_dir, deps, fns)?;
        let (image, raised) = host
            .compile_bytecode(source, uri)
            .map_err(|e| RunError::Script(e.to_string()))?;
        warnings.extend(raised);
        return Ok(Some(image));
    }
    #[cfg(not(feature = "host-candela"))]
    let _ = (engine, source, uri, lib_dir, deps, fns, warnings);
    Ok(None)
}

/// A candela compiler set up the way every compile path sets it up: the
/// app's library directory, the packages it imports, the compile-time flags
/// of the target it compiles for, `fns` declared, and the candela sources the
/// modules register staged ahead of the program.
#[cfg(all(feature = "runtime-parse", feature = "host-candela"))]
fn candela_compiler(
    lib_dir: &Path,
    deps: &CompileDeps,
    fns: &[lumen_script::ScriptFn],
) -> Result<CandelaHost, RunError> {
    let mut host = CandelaHost::new();
    host.set_library_dir(lib_dir);
    for (name, dir) in &deps.import_roots {
        host.add_import_root(name.clone(), dir.clone());
    }
    host.set_cfg_flags(deps.target.cfg_flags());
    let lang = host.lang();
    for f in fns.iter().filter(|f| f.visible_to(lang)) {
        host.register_script_fn(f)
            .map_err(|e| RunError::Script(e.to_string()))?;
    }
    for prelude in deps.modules.preludes.iter().filter(|p| p.lang == lang) {
        host.add_prelude(&prelude.ns, &prelude.source);
    }
    Ok(host)
}

/// The names a compiled program can be called by.
///
/// `None` for a program with no ahead-of-time form, and for a build with no
/// host that can read one back: neither knows what the program exports, and
/// neither can say anything is missing from it.
///
/// A build tool asks this to tell a function the app calls by name from one
/// it will only appear to call: candela exports a function only when every
/// parameter it takes is annotated, and a shipped runtime carries no compiler
/// to fall back on.
///
/// `addons` are the browser add-ons the app was compiled against; their
/// functions are declared in the program, so reading it back binds them too.
#[cfg(feature = "runtime-parse")]
#[must_use]
pub fn script_exports(
    script: &lumen_ir::artifact::CompiledScript,
    addons: &[lumen_ir::addon::Addon],
) -> Option<Result<Vec<String>, String>> {
    let bytecode = script.bytecode.as_deref()?;
    #[cfg(feature = "host-candela")]
    {
        let fns = match addon_stubs(addons) {
            Ok(fns) => fns,
            Err(e) => return Some(Err(e.to_string())),
        };
        Some(lumen_script_candela::image_exports(bytecode, &fns).map_err(|e| e.to_string()))
    }
    #[cfg(not(feature = "host-candela"))]
    {
        let _ = (bytecode, addons);
        None
    }
}

/// Parse `<dir>/src/main.lmn` + optional `<dir>/src/main.css` and validate
/// them without spawning a window. Used by CI / pre-commit hooks.
///
/// Parse-time `LayoutIR.lint_findings` (an unknown attribute, a
/// boolean attribute with an off-list value, bare `{name}`
/// interpolation) are printed to stderr but never fail the build -
/// `check` validates AST shape, not style. Run
/// `lumenc lint --signals <dir>` for the full lint stream.
///
/// `check` is for no one target. `targets` holds what was resolved for each
/// target the app can be built for: the markup is checked once, accepting the
/// elements of every target's add-ons, and the scripts are compiled once per
/// target, each against that target's add-ons, script libraries and
/// `@cfg(...)` flags, so code written for one target is checked the way that
/// target's build compiles it.
///
/// Requires the source parser (`runtime-parse` feature).
#[cfg(feature = "runtime-parse")]
pub fn check_app(
    dir: &Path,
    parser: &dyn SourceParser,
    plugins: &dyn crate::compiler_plugins::CompilerPlugins,
    targets: &[CompileDeps],
) -> Result<CheckReport, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    let mut every_addon: Vec<lumen_ir::addon::Addon> = Vec::new();
    for addon in targets.iter().flat_map(|deps| &deps.addons) {
        if !every_addon.iter().any(|a| a.name == addon.name) {
            every_addon.push(addon.clone());
        }
    }
    let every_target: Vec<lumen_modules::Target> = targets.iter().map(|deps| deps.target).collect();
    register_declared_tags(&cfg, &every_target, &every_addon);
    let layout = AppLayout::resolve(dir, &cfg)?;
    let roots = cfg.resolved_asset_roots(dir);
    // File-based pages: validate the whole assembled multi-page tree (entry +
    // grafted sibling pages + global templates), not just the entry file in
    // isolation - otherwise an entry that `<use>`s a shared `layout` template
    // would falsely fail `check`.
    let plan = crate::pages::discover(&layout.src_dir, &cfg);
    let entry_path = plan.entry_file.clone();
    let LoadResult { ir, .. } = load_ir(
        parser,
        plugins,
        &entry_path,
        &layout.css_path,
        dir,
        &roots,
        cfg.skin.name.as_deref(),
        &lumen_ir::css::MediaContext::default(),
        SourceOverrides {
            plan: Some(&plan),
            ..SourceOverrides::default()
        },
    )?;
    // Parse-time lint findings already went to stderr from `load_ir`,
    // which every compile path shares.
    let has_script = !ir.script_source.trim().is_empty() || !ir.external_scripts.is_empty();
    // RC6: compile the app's scripts with the same engine settings
    // `lumenc run` uses. Compile-only - the top level is never evaluated, so
    // `check` stays side-effect free. A script that would die at load (parse
    // error, expression-depth overflow, ...) fails the check instead of
    // false-passing while `run` shows a window whose every handler is dead.
    //
    // Check each language's program with its own compiler, on the same
    // grouping `build_app` runs: the Rhai checker false-fails on the other
    // languages' syntax (a candela `host "lumen" { ... }` block is not valid
    // Rhai), so a mixed app checked as one blob could never pass. A host the
    // current build trimmed out falls back to a compiled one, the same way the
    // run path folds it (`remap_trimmed_hosts`).
    let uri = entry_path.display().to_string();
    let grouped = super::app_build::remap_trimmed_hosts(grouped_script_sources(&ir, dir, &cfg)?)?;
    for (engine, source) in grouped {
        match engine {
            // The one host with compile-time conditions, so the one checked
            // per target.
            #[cfg(feature = "host-candela")]
            crate::config::ScriptEngine::Candela => {
                check_candela_per_target(&source, &uri, &layout.lib_dir, targets)?;
            }
            #[cfg(feature = "host-lua")]
            crate::config::ScriptEngine::Lua => {
                LuaHost::new()
                    .compile_check(&source, &uri)
                    .map_err(|e| RunError::Script(e.to_string()))?;
            }
            #[cfg(feature = "host-rhai")]
            crate::config::ScriptEngine::Rhai => {
                RhaiHost::new()
                    .compile_check(&source)
                    .map_err(|e| RunError::Script(e.to_string()))?;
            }
            #[cfg(not(all(
                feature = "host-rhai",
                feature = "host-lua",
                feature = "host-candela"
            )))]
            _ => unreachable!("a trimmed script host is remapped before this match"),
        }
    }
    Ok(CheckReport {
        element_count: count_elements(&ir.root),
        has_script,
    })
}

/// Compile-check a candela program once for each of `targets`.
///
/// Where every target rejects the program the same way, the error is the
/// compiler's own, as it is for an app with no target-specific code. Where
/// they disagree, the error names the target whose build it breaks.
#[cfg(all(feature = "runtime-parse", feature = "host-candela"))]
fn check_candela_per_target(
    source: &str,
    uri: &str,
    lib_dir: &Path,
    targets: &[CompileDeps],
) -> Result<(), RunError> {
    let mut failures = Vec::new();
    for deps in targets {
        let stubs = declared_fns(deps)?;
        let checked = candela_compiler(lib_dir, deps, &stubs)?.compile_check(source, uri);
        if let Err(e) = checked {
            failures.push((deps.target, e.to_string()));
        }
    }
    let Some((target, first)) = failures.first() else {
        return Ok(());
    };
    let agree = failures.len() == targets.len() && failures.iter().all(|(_, e)| e == first);
    Err(RunError::Script(if agree {
        first.clone()
    } else {
        format!("{first} (in the {target} build)")
    }))
}

#[cfg(feature = "runtime-parse")]
fn count_elements(el: &Element) -> usize {
    1 + el.children.iter().map(count_elements).sum::<usize>()
}

/// Walk the IR and rewrite every `src` attribute on tags that load
/// assets (`<image>`) to be absolute, joining the path against the
/// app directory. Author-written relative paths then survive
/// arbitrary cwd shifts at run time.
///
/// Runs on the from-source load and on the artifact load alike, which is what
/// lets a packaged app carry paths relative to itself and still find its
/// files from whichever directory it was started in.
pub(crate) fn resolve_asset_paths(el: &mut Element, dir: &Path, extra_roots: &[PathBuf]) {
    if el.tag == "image"
        && let Some(src) = &el.attrs.src
    {
        let p = Path::new(src);
        if p.is_relative() {
            // Prefer the app dir first; fall back to extra `asset_roots`
            // from lumen.toml in declared order. We only swap to an extra
            // root if a file actually exists there - keeps the default
            // path stable when no overrides are configured.
            let primary = dir.join(p);
            let resolved = if primary.exists() {
                primary
            } else {
                extra_roots
                    .iter()
                    .map(|r| r.join(p))
                    .find(|cand| cand.exists())
                    .unwrap_or(primary)
            };
            if let Some(s) = resolved.to_str() {
                el.attrs.src = Some(s.to_string());
            }
        }
    }
    for child in &mut el.children {
        resolve_asset_paths(child, dir, extra_roots);
    }
}
