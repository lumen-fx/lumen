use super::*;

/// Summary returned by [`check_app`].
#[derive(Debug, Clone, Copy)]
pub struct CheckReport {
    /// Number of elements parsed (including root).
    pub element_count: usize,
    /// Whether the markup contained a non-empty `<script>` block.
    pub has_script: bool,
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
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    compile_app_with_skin(dir, parser, None)
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
    skin: Option<&str>,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    // The same discovery the run path does, so compiling sees the app the way
    // running it does: the entry file it would open, and every sibling page.
    let plan = crate::pages::discover(dir, &cfg);
    let html_path = plan.entry_file.clone();
    let css_path = dir.join("main.css");
    let asset_roots = cfg.resolved_asset_roots(dir);
    let skin_override = skin.map(str::to_string).or_else(|| cfg.skin.name.clone());
    let loaded = load_ir(
        parser,
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
            bytecode: compiled_bytecode(engine, &source, &uri)?,
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
    Ok(lumen_ir::artifact::CompiledApp {
        ir,
        script_source,
        scripts,
        pages,
        // Every fragment the app declares, whether or not this build
        // instantiates it: the artifact carries the declarations, not just
        // their expansions.
        fragments: loaded.fragments,
    })
}

/// The compiled bytecode image for one engine's program, or `None` for an
/// engine that has no ahead-of-time form.
///
/// candela is the one that does: its `.cdlb` image is what `candela-vm` runs
/// where the compiler is absent. A build without the candela host trimmed in
/// cannot produce one, and writes the source alone.
#[cfg(feature = "runtime-parse")]
fn compiled_bytecode(
    engine: crate::config::ScriptEngine,
    source: &str,
    uri: &str,
) -> Result<Option<Vec<u8>>, RunError> {
    #[cfg(feature = "host-candela")]
    if engine == crate::config::ScriptEngine::Candela {
        return lumen_script_candela::compile_bytecode(source, uri)
            .map(Some)
            .map_err(|e| RunError::Script(e.to_string()));
    }
    #[cfg(not(feature = "host-candela"))]
    let _ = (engine, source, uri);
    Ok(None)
}

/// Parse `<dir>/main.lmn` + optional `<dir>/main.css` and validate them
/// without spawning a window. Used by CI / pre-commit hooks.
///
/// Parse-time `LayoutIR.lint_findings` (an unknown attribute, a
/// boolean attribute with an off-list value, bare `{name}`
/// interpolation) are printed to stderr but never fail the build -
/// `check` validates AST shape, not style. Run
/// `lumenc lint --signals <dir>` for the full lint stream.
///
/// Requires the source parser (`runtime-parse` feature).
#[cfg(feature = "runtime-parse")]
pub fn check_app(dir: &Path, parser: &dyn SourceParser) -> Result<CheckReport, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    let roots = cfg.resolved_asset_roots(dir);
    // File-based pages: validate the whole assembled multi-page tree (entry +
    // grafted sibling pages + global templates), not just the entry file in
    // isolation - otherwise an entry that `<use>`s a shared `layout` template
    // would falsely fail `check`.
    let plan = crate::pages::discover(dir, &cfg);
    let entry_path = plan.entry_file.clone();
    let LoadResult { ir, .. } = load_ir(
        parser,
        &entry_path,
        &dir.join("main.css"),
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
            #[cfg(feature = "host-candela")]
            crate::config::ScriptEngine::Candela => {
                CandelaHost::new()
                    .compile_check(&source, &uri)
                    .map_err(|e| RunError::Script(e.to_string()))?;
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
