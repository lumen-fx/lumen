use super::*;

/// Summary returned by [`check_app`].
#[derive(Debug, Clone, Copy)]
pub struct CheckReport {
    /// Number of elements parsed (including root).
    pub element_count: usize,
    /// Whether the markup contained a non-empty `<script>` block.
    pub has_script: bool,
}

/// Ahead-of-time compile `<dir>/main.lmn` + optional `main.css` into a
/// [`lumen_ir::artifact::CompiledApp`]: parse markup + CSS once, run the full
/// cascade, resolve asset / include / import paths, and bake the combined
/// script source. The returned artifact is what `lumenc build` writes to
/// disk and what a parser-free runtime loads via [`RunOptions::artifact`].
///
/// This is the AOT counterpart to [`load_ir`]: same front-end work, but the
/// result is serialized instead of spawned. Requires the source parser
/// (`runtime-parse` feature).
#[cfg(feature = "runtime-parse")]
pub fn compile_app(
    dir: &Path,
    parser: &dyn SourceParser,
) -> Result<lumen_ir::artifact::CompiledApp, RunError> {
    let cfg = crate::config::LumenToml::load_or_default(dir).map_err(RunError::Config)?;
    let entry = cfg.app.entry.as_deref().unwrap_or("main.lmn");
    let html_path = dir.join(entry);
    let css_path = dir.join("main.css");
    let asset_roots = cfg.resolved_asset_roots(dir);
    let skin_override = cfg.skin.name.clone();
    let loaded = load_ir(
        parser,
        &html_path,
        &css_path,
        dir,
        &asset_roots,
        skin_override.as_deref(),
        &lumen_ir::css::MediaContext::default(),
        // AOT multi-page packaging is a follow-up: `lumenc build` bakes the
        // single entry page today (SourceOverrides::default -> plan: None).
        // See the `pages` module docs for the fold-in.
        SourceOverrides::default(),
    )?;
    // Concatenate inline + external `<script>` sources once, then strip both
    // from the IR: the artifact carries the combined string in its own field
    // so the parser-free runtime never re-reads `.rhai` files.
    let script_source = combined_script_source(&loaded.ir, dir)?;
    let mut ir = loaded.ir;
    ir.script_source = String::new();
    ir.external_scripts.clear();
    Ok(lumen_ir::artifact::CompiledApp { ir, script_source })
}

/// Parse `<dir>/main.lmn` + optional `<dir>/main.css` and validate them
/// without spawning a window. Used by CI / pre-commit hooks.
///
/// Parse-time `LayoutIR.lint_findings` (info-level stylistic nudges
/// like the bare-`{name}` interpolation deprecation) are printed to
/// stderr but never fail the build - `check` validates AST shape, not
/// style. Run `lumenc lint --signals <dir>` for the full lint stream.
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
    // Surface parse-time lint findings on stderr at info level. We
    // keep this advisory - `check` validates structure, the dedicated
    // `lumenc lint --signals` command is the gate.
    for f in &ir.lint_findings {
        eprintln!(
            "info  {file}:{line}:{col}  [{kind}] {msg}",
            file = entry_path.display(),
            line = f.line,
            col = f.col,
            kind = <&'static str>::from(f.kind),
            msg = f.message,
        );
        if let Some(s) = &f.suggest {
            eprintln!("       hint: replace with `{s}`");
        }
    }
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
    // current build trimmed out falls back to the always-present Rhai host.
    let uri = entry_path.display().to_string();
    for (engine, source) in grouped_script_sources(&ir, dir, &cfg)? {
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
            _ => {
                RhaiHost::new()
                    .compile_check(&source)
                    .map_err(|e| RunError::Script(e.to_string()))?;
            }
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
#[cfg(feature = "runtime-parse")]
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
