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
    /// The web halves of the modules the app depends on, for a web build;
    /// every other target takes none. Their functions are declared to the
    /// compile, their elements to the parser, and the compiled app carries
    /// them.
    pub addons: Vec<lumen_ir::addon::Addon>,
}

impl CompileDeps {
    /// A compile for `target` with nothing resolved from outside the app.
    pub fn new(target: lumen_modules::Target) -> Self {
        Self {
            target,
            import_roots: Vec::new(),
            addons: Vec::new(),
        }
    }
}

/// The add-ons' functions, bound to the body every target without a page
/// binds, for handing to a compiler: it declares them and never calls them.
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
#[cfg(feature = "runtime-parse")]
pub fn compile_app(
    dir: &Path,
    parser: &dyn SourceParser,
    plugins: &dyn crate::compiler_plugins::CompilerPlugins,
    deps: &CompileDeps,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    compile_app_with_skin(dir, parser, plugins, None, deps)
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
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    register_declared_tags(&cfg, &[deps.target], &deps.addons);
    let stubs = addon_stubs(&deps.addons)?;
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
            bytecode: compiled_bytecode(engine, &source, &uri, &layout.lib_dir, deps, &stubs)?,
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
#[cfg(feature = "runtime-parse")]
fn compiled_bytecode(
    engine: crate::config::ScriptEngine,
    source: &str,
    uri: &str,
    lib_dir: &Path,
    deps: &CompileDeps,
    fns: &[lumen_script::ScriptFn],
) -> Result<Option<Vec<u8>>, RunError> {
    #[cfg(feature = "host-candela")]
    if engine == crate::config::ScriptEngine::Candela {
        let host = candela_compiler(lib_dir, deps, fns)?;
        return host
            .compile_bytecode(source, uri)
            .map(Some)
            .map_err(|e| RunError::Script(e.to_string()));
    }
    #[cfg(not(feature = "host-candela"))]
    let _ = (engine, source, uri, lib_dir, deps, fns);
    Ok(None)
}

/// A candela compiler set up the way every compile path sets it up: the
/// app's library directory, the packages it imports, the compile-time flags
/// of the target it compiles for, and `fns` declared.
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
    for f in fns {
        host.register_script_fn(f)
            .map_err(|e| RunError::Script(e.to_string()))?;
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
        let stubs = addon_stubs(&deps.addons)?;
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
