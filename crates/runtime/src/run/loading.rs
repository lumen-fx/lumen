use super::*;

use crate::config::ScriptEngine;

/// Everything [`load_ir`] produces: the parsed [`lumen_ir::layout_ir::LayoutIR`] plus
/// the full set of files the hot-reload watcher must poll (markup, CSS,
/// external `.rhai` scripts, `<include>`d `.lmn` files, and `@import`ed
/// `.css` files) with their current mtimes captured alongside.
pub(crate) struct LoadResult {
    pub(crate) ir: lumen_ir::layout_ir::LayoutIR,
    pub(crate) html_mtime: Option<SystemTime>,
    pub(crate) css_mtime: Option<SystemTime>,
    /// Resolved absolute paths of external `.rhai` scripts.
    pub(crate) script_paths: Vec<PathBuf>,
    pub(crate) script_mtimes: Vec<Option<SystemTime>>,
    /// Normalized paths of every `<include>`d `.lmn` file (transitive).
    pub(crate) include_paths: Vec<PathBuf>,
    pub(crate) include_mtimes: Vec<Option<SystemTime>>,
    /// Normalized paths of every `@import`ed `.css` file (transitive).
    pub(crate) css_import_paths: Vec<PathBuf>,
    pub(crate) css_import_mtimes: Vec<Option<SystemTime>>,
    /// The app's program split by engine, as recorded by the AOT compiler.
    /// Empty on the from-source path, which reads the split off the script
    /// files themselves.
    pub(crate) scripts: GroupedScripts,
    /// The page set of a compiled multi-page app. `None` on the from-source
    /// path, which discovers the pages from the directory, and for any
    /// single-page app.
    pub(crate) pages: Option<lumen_ir::artifact::CompiledPages>,
    /// Every fragment the app declares, for the runtime to instantiate by
    /// key.
    pub(crate) fragments: lumen_ir::fragment::FragmentTable,
}

/// Produce a [`LoadResult`] for [`build_app`] from whichever source the
/// caller configured: a precompiled AOT artifact ([`RunOptions::artifact`])
/// or, when the `runtime-parse` feature is on, `main.lmn` + `main.css`
/// parsed from source. A parser-free build with no artifact returns
/// [`RunError::ParserDisabled`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_inputs(
    opts: &RunOptions,
    parser: Option<&dyn SourceParser>,
    html_path: &Path,
    css_path: &Path,
    dir: &Path,
    asset_roots: &[PathBuf],
    skin_override: Option<&str>,
    plan: Option<&crate::pages::PagePlan>,
) -> Result<LoadResult, RunError> {
    if let Some(bytes) = &opts.artifact_bytes {
        return load_ir_from_artifact_bytes(bytes, dir);
    }
    if let Some(path) = &opts.artifact {
        return load_ir_from_artifact(path, dir);
    }
    #[cfg(feature = "runtime-parse")]
    {
        let parser = parser.ok_or(RunError::ParserDisabled)?;
        load_ir(
            parser,
            html_path,
            css_path,
            dir,
            asset_roots,
            skin_override,
            &lumen_ir::css::MediaContext::default(),
            SourceOverrides {
                markup: opts.markup.as_deref(),
                css: opts.css.as_deref(),
                plan,
            },
        )
    }
    #[cfg(not(feature = "runtime-parse"))]
    {
        let _ = (
            parser,
            html_path,
            css_path,
            dir,
            asset_roots,
            skin_override,
            plan,
        );
        Err(RunError::ParserDisabled)
    }
}

/// Deserialize a precompiled AOT artifact into a [`LoadResult`]. All
/// resolution (`<include>` / `@import` splicing, the CSS cascade, script
/// concatenation) happened at build time, so this path never touches the
/// parser or the filesystem beyond reading the artifact itself. Hot-reload
/// watch fields come back empty - a compiled artifact has no source files to
/// watch.
fn load_ir_from_artifact(path: &Path, dir: &Path) -> Result<LoadResult, RunError> {
    let compiled = lumen_ir::artifact::read(path).map_err(|e| RunError::Artifact(e.to_string()))?;
    Ok(load_result_from_compiled(compiled, dir))
}

/// Deserialize a precompiled AOT artifact from in-memory bytes into a
/// [`LoadResult`]: the byte-slice counterpart of [`load_ir_from_artifact`].
/// The link-not-embed launcher path: the compiler produces LMNA bytes
/// in-process and hands them across the C-ABI, so the runtime never touches a
/// file for the artifact itself.
fn load_ir_from_artifact_bytes(bytes: &[u8], dir: &Path) -> Result<LoadResult, RunError> {
    let compiled =
        lumen_ir::artifact::read_bytes(bytes).map_err(|e| RunError::Artifact(e.to_string()))?;
    Ok(load_result_from_compiled(compiled, dir))
}

/// Shared tail of the two artifact-load entry points: fold a decoded
/// [`lumen_ir::artifact::CompiledApp`] into a [`LoadResult`] with empty
/// hot-reload watch fields (a compiled artifact has no source files to watch).
fn load_result_from_compiled(compiled: lumen_ir::artifact::CompiledApp, dir: &Path) -> LoadResult {
    let mut ir = compiled.ir;
    // The build step bakes the combined (inline + external) script source
    // into the artifact and clears `external_scripts`, so the parser-free
    // runtime reconstructs the exact script-host input with no disk read.
    ir.script_source = compiled.script_source;
    ir.external_scripts.clear();
    // An artifact built for a fixed directory carries absolute asset paths and
    // is unaffected here; one built to travel (`lumenc package`) carries paths
    // relative to the app, and `dir` is where that app now lives.
    resolve_asset_paths(&mut ir.root, dir, &[]);
    let scripts = compiled
        .scripts
        .into_iter()
        .map(|s| (ScriptEngine::from_name(&s.engine), s.source))
        .collect();
    let pages = compiled.pages;
    LoadResult {
        ir,
        fragments: compiled.fragments,
        html_mtime: None,
        css_mtime: None,
        script_paths: Vec::new(),
        script_mtimes: Vec::new(),
        include_paths: Vec::new(),
        include_mtimes: Vec::new(),
        css_import_paths: Vec::new(),
        css_import_mtimes: Vec::new(),
        scripts,
        pages,
    }
}

/// In-memory source overrides threaded from [`RunOptions::markup`] /
/// [`RunOptions::css`] into [`load_ir`]. `None` fields fall back to the
/// on-disk lookup; the default value reads everything from disk.
#[cfg(feature = "runtime-parse")]
#[derive(Clone, Copy, Default)]
pub(crate) struct SourceOverrides<'a> {
    pub(crate) markup: Option<&'a str>,
    pub(crate) css: Option<&'a str>,
    /// File-based-pages plan. When `Some(_).multipage`, the loader grafts
    /// every sibling `.lmn` page under `<if>` gates and parses each against
    /// the app-wide fragment table. `None` / single-page keeps the legacy
    /// path.
    pub(crate) plan: Option<&'a crate::pages::PagePlan>,
}

#[cfg(feature = "runtime-parse")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_ir(
    parser: &dyn SourceParser,
    html_path: &Path,
    css_path: &Path,
    dir: &Path,
    asset_roots: &[PathBuf],
    skin_override: Option<&str>,
    media: &lumen_ir::css::MediaContext,
    sources: SourceOverrides<'_>,
) -> Result<LoadResult, RunError> {
    let plan = sources.plan;
    let html = match sources.markup {
        Some(src) => src.to_string(),
        None => std::fs::read_to_string(html_path)
            .map_err(|e| RunError::Read(html_path.to_path_buf(), e))?,
    };
    let css_raw = match sources.css {
        Some(src) => Some(src.to_string()),
        None => read_optional(css_path)?,
    };
    // Resolve `@import "..."` directives: splice imported sheets ahead of
    // this file's own rules (imported-first, so the importing file wins at
    // equal specificity). Kept out of `parse_css` so that entry stays a
    // pure string->Stylesheet used by tests and the LSP.
    let mut css_import_paths: Vec<PathBuf> = Vec::new();
    let css = match &css_raw {
        Some(src) => Some(
            parser
                .resolve_css_imports(src, css_path, &mut css_import_paths)
                .map_err(RunError::ParseCss)?,
        ),
        None => None,
    };
    let css_import_mtimes: Vec<Option<SystemTime>> =
        css_import_paths.iter().map(|p| mtime(p)).collect();
    // Built-in Palette light/dark theme (`lumen_core::palette::Palette`) as
    // a synthetic `:root` sheet - Lumen's own named-color layer, sitting
    // beneath even the UA baseline so a skin or the app's own CSS can
    // still redeclare any of its names and win. Parsed once and reused
    // below for the live combined stylesheet too. See `Palette::root_vars`
    // for the full precedence story and current limits (a load-time bake,
    // not a live re-resolve).
    let palette_sheet = parser
        .parse_css(&lumen_ir::css::palette_root_css())
        .map_err(RunError::ParseCss)?;
    // Inline-attribute var() resolution. `:root { --foo }` globals are
    // merged from the Palette theme, the UA baseline, the configured skin
    // (if any), and the app's own stylesheet - in that order, so a later
    // layer overrides an earlier one for the same name, matching the real
    // cascade built below (Palette, then UA, then skin, then author
    // last-wins). This runs ahead of HTML parse, then `var(--foo)`
    // substrings in markup attr values are textually substituted before
    // typed parsing runs. Selector-scoped overrides (`.dark { --bg: ... }`)
    // still flow through `apply_css` as normal.
    //
    // Only `skin_override` (the `lumen.toml [skin] name` default) is known
    // at this point - markup hasn't been parsed yet, so an explicit
    // `<root skin="...">` override *in this file* (which always wins once
    // known, see below) is invisible to this merge. An inline attribute
    // that references a token only defined by such a markup-only override
    // still won't resolve here. Closing that gap needs either a two-pass
    // parse (parse once to discover `ir.skin`, substitute, parse again) or
    // a lightweight pre-parse scan for `<root skin="...">` outside the real
    // XML parser; both are bigger changes than this fix covers, so this
    // merges the documented, common path (`lumen.toml [skin] name`) only.
    let root_vars = {
        let mut vars = palette_sheet.root_vars();
        vars.extend(extract_root_vars(parser, crate::skins::UA));
        if let Some(name) = skin_override.filter(|s| !s.is_empty())
            && let Some(skin_src) = crate::skins::lookup(name)
        {
            vars.extend(extract_root_vars(parser, skin_src));
        }
        if let Some(src) = css.as_deref() {
            vars.extend(extract_root_vars(parser, src));
        }
        vars
    };
    // Resolve `<include src="..."/>` directives against the real
    // filesystem FIRST (relative paths resolve against the entry file's
    // directory), so inline `var()` substitution below applies uniformly
    // to the main file *and* every included fragment.
    let mut include_paths: Vec<PathBuf> = Vec::new();
    let html = parser
        .resolve_includes(&html, html_path, &mut include_paths)
        .map_err(RunError::ParseHtml)?;
    let mut include_mtimes: Vec<Option<SystemTime>> =
        include_paths.iter().map(|p| mtime(p)).collect();
    // Inline var() substitution runs on the fully-spliced markup. An
    // unresolvable call degrades to empty text plus a printed warning
    // instead of aborting the whole load - matching how the stylesheet
    // path (`apply_css_with_media` below) drops one bad declaration and
    // keeps applying the rest of the cascade rather than failing outright.
    // Runs even when no `:root` block defined anything: with an empty
    // variable set every call is unresolvable, and dropping it is the
    // point. Skipping the pass would leave the literal `var(...)` text in
    // the attribute for the parser to choke on.
    let html = {
        let resolution = lumen_ir::css_vars::resolve_lenient(&html, &root_vars);
        for msg in &resolution.warnings {
            eprintln!("css warning: inline var() - {msg}");
        }
        resolution.output
    };
    // File-based pages: collect the app-wide fragment table so the entry
    // page's own `<use template="...">` references (e.g. a shared `layout`)
    // resolve during THIS parse too - not just for the sibling pages grafted
    // in `pages::assemble` below. Collected per load, so editing a fragment
    // file and hot-reloading picks the new body up.
    let mut fragments = match plan {
        Some(plan) if sources.markup.is_none() && plan.multipage => {
            crate::pages::collect_fragments(plan, parser).map_err(RunError::ParseHtml)?
        }
        _ => lumen_ir::fragment::FragmentTable::new(),
    };
    // The `lmn!` blocks the app's candela scripts write, read before the tree.
    // Markup names a candela component by writing the function as a tag, so
    // the blocks are in the table this parse instantiates against. Compiled
    // here so a run from source instantiates the same fragments a built
    // artifact carries, and so a malformed block fails the load rather than
    // the window.
    let scripted = script_fragments(&html, html_path, dir, plan, parser)?;
    fragments
        .merge(scripted.clone())
        .map_err(|e| RunError::Script(e.to_string()))?;
    // Everything the app declares, whether or not this parse instantiates it:
    // the runtime instantiates by key, so the table it gets holds every
    // `<template>` in the directory, not only the ones the entry page reached.
    let mut declared = match plan {
        Some(plan) if sources.markup.is_none() => {
            crate::pages::collect_fragments(plan, parser).map_err(RunError::ParseHtml)?
        }
        _ => parser
            .collect_fragments(&html, html_path)
            .map_err(RunError::ParseHtml)?,
    };
    declared
        .merge(scripted)
        .map_err(|e| RunError::Script(e.to_string()))?;
    // Expand the use sites the fragment bodies hold against each other, so
    // every body the runtime instantiates is the whole subtree it stands for.
    // An `lmn!` block writing a `<template>` the markup declares is what needs
    // it: the block is read before the markup, so the two meet here. Ahead of
    // the tree because this table is the one that knows which file each
    // declaration came from, so a contested name is reported against both.
    let declared = parser
        .link_fragments(declared)
        .map_err(RunError::ParseHtml)?;
    // Includes are already spliced away, so the string-only parser suffices.
    let mut ir = parser
        .parse_html(&html, &fragments)
        .map_err(RunError::ParseHtml)?;
    // Carry the resolved include list on the IR for parity with
    // `external_scripts` (used by hot reload + inspectable by consumers).
    ir.included_files = include_paths.clone();
    // File-based pages: graft every sibling `.lmn` page under a synthetic
    // `<if signal="route.path" eq="<key>">` gate (reusing the `<if>`
    // reconciler), parse each against the app-wide fragment table, and merge
    // every page's scripts - all BEFORE asset resolution + the CSS cascade so the
    // assembled tree flows through the rest of the pipeline uniformly. Only
    // for a real multi-page disk app; in-memory (`markup`) sources and
    // single-file apps keep the untouched legacy path.
    if sources.markup.is_none()
        && let Some(plan) = plan
        && plan.multipage
    {
        let extra = crate::pages::assemble(&mut ir, plan, &fragments, parser)
            .map_err(RunError::ParseHtml)?;
        // Watch + mtime-track every page file so editing any page hot-reloads.
        for p in extra {
            let m = mtime(&p);
            include_paths.push(p.clone());
            include_mtimes.push(m);
            ir.included_files.push(p);
        }
    }
    // Resolve every `<image src="..." />` path against the app dir so
    // relative paths work regardless of cwd at run time. Authors write
    // `src="apps/weather/icons/sun.png"`-style paths relative to the
    // .lmn file's directory; the runtime never re-resolves against cwd.
    resolve_asset_paths(&mut ir.root, dir, asset_roots);
    // lumen.toml `[skin] name` provides a default only - explicit
    // `<root skin="...">` in markup always wins.
    if ir.skin.is_none()
        && let Some(s) = skin_override
        && !s.is_empty()
    {
        ir.skin = Some(s.to_string());
    }
    // The built-in Palette theme, the always-on UA baseline (`skins::UA`),
    // the opt-in named skin (if any, via `<root skin="...">` / lumen.toml),
    // and the user's `main.css` are concatenated into one combined
    // stylesheet with renumbered source order, then applied in a single
    // cascade pass. One pass matters: applying the sheets sequentially made
    // a later pass's inline-origin snapshot treat an earlier sheet's values
    // as inline-set, restoring them OVER author CSS - the author could
    // never override a skin `bg`. With one combined sheet the standard
    // cascade rules do the work.
    //
    // Origin precedence: Palette, UA, and skin rules are all tagged
    // `Origin::UserAgent`; author rules are `Origin::Author`. The cascade
    // sorts on origin FIRST (CSS Cascade section 6.1), so any author rule
    // beats any Palette, UA, or skin rule for normal declarations
    // regardless of specificity - e.g. author `.editor` wins over skin
    // `textarea:hover`. Palette, UA, and skin share that one origin tier,
    // so their relative order comes from the `source_order` bump below
    // instead: Palette rules are numbered first, then UA, then skin, so an
    // equal-specificity later rule always sorts after and wins the tie.
    // Palette's only rules are `:root` custom properties (see
    // `palette_root_css`), and nothing in UA or an unskinned app's own CSS
    // references any of its names today, so this tier ordering is
    // currently only observable once something actually writes
    // `var(--accent-color)` or the like. (The reconciler also re-applies
    // this combined sheet to runtime-substituted `<for>` template
    // elements.)
    let mut combined_rules = Vec::new();
    for mut rule in palette_sheet.rules {
        rule.origin = lumen_ir::css::Origin::UserAgent;
        combined_rules.push(rule);
    }
    let palette_rule_count = combined_rules.len();
    {
        let sheet = parser
            .parse_css(crate::skins::UA)
            .map_err(RunError::ParseCss)?;
        for mut rule in sheet.rules {
            rule.origin = lumen_ir::css::Origin::UserAgent;
            rule.source_order += palette_rule_count;
            combined_rules.push(rule);
        }
    }
    let ua_rule_count = combined_rules.len();
    if let Some(name) = ir.skin.clone() {
        let skin_src = crate::skins::lookup(&name).ok_or_else(|| {
            RunError::ParseCss(format!(
                "unknown skin '{name}' - supported: \"auto\", \"default\", \"macos\", \"windows\", \"linux\""
            ))
        })?;
        let sheet = parser.parse_css(skin_src).map_err(RunError::ParseCss)?;
        for mut rule in sheet.rules {
            // Built-in skin ships as the user-agent origin so author CSS
            // always overrides it (per the cascade sort above); the
            // source-order bump keeps it ordered after the UA baseline
            // within that shared origin.
            rule.origin = lumen_ir::css::Origin::UserAgent;
            rule.source_order += ua_rule_count;
            combined_rules.push(rule);
        }
    }
    let skin_rule_count = combined_rules.len();
    if let Some(css_src) = &css {
        let sheet = parser.parse_css(css_src).map_err(RunError::ParseCss)?;
        for mut rule in sheet.rules {
            // Author origin is the parser default; the source-order bump
            // keeps author rules ordered after UA + skin rules within the
            // combined sheet (matters only when origins ever tie).
            rule.source_order += skin_rule_count;
            combined_rules.push(rule);
        }
    }
    let combined = lumen_ir::css::Stylesheet {
        rules: combined_rules,
    };
    if !combined.rules.is_empty() {
        let warnings = lumen_ir::css::apply_css_with_media(&mut ir, &combined, media)
            .map_err(|e| RunError::ApplyCss(e.to_string()))?;
        for w in &warnings {
            eprintln!("{w}");
        }
    }
    ir.combined_stylesheet = Some(combined);
    // Parse-time lint findings, beside the CSS warnings and on the same
    // terms: advisory, printed once per load, never fatal. Every path
    // that loads markup from source comes through here, so `check` and
    // `run` report the same set. `lumenc lint --signals` is the gate
    // that can fail a build on them.
    for f in &ir.lint_findings {
        eprintln!("{}", f.render(html_path));
    }
    let script_paths: Vec<PathBuf> = ir.external_scripts.iter().map(|p| dir.join(p)).collect();
    let script_mtimes: Vec<Option<SystemTime>> = script_paths.iter().map(|p| mtime(p)).collect();
    Ok(LoadResult {
        ir,
        html_mtime: mtime(html_path),
        css_mtime: mtime(css_path),
        script_paths,
        script_mtimes,
        include_paths,
        include_mtimes,
        css_import_paths,
        css_import_mtimes,
        // The script files are right there on disk, so the engine split comes
        // from their extensions rather than from a recorded one, and the page
        // set from the directory listing.
        scripts: Vec::new(),
        pages: None,
        fragments: declared,
    })
}

/// The fragments the app's candela scripts declare through `lmn!`.
///
/// The inline `<script>` block and every `.cdl` file the app's markup names
/// are scanned; a script in another language writes no `lmn!` block. A
/// multi-page app scans every one of its `.lmn` files, the same set the
/// `<template>` table is collected from, so a component declared beside one
/// page is usable from another.
///
/// Reading the files here rather than taking them from the caller keeps the
/// ahead-of-time compile, the from-source run, and each hot reload on one code
/// path.
#[cfg(feature = "runtime-parse")]
fn script_fragments(
    html: &str,
    html_path: &Path,
    dir: &Path,
    plan: Option<&crate::pages::PagePlan>,
    parser: &dyn SourceParser,
) -> Result<lumen_ir::fragment::FragmentTable, RunError> {
    let mut markup: Vec<(String, PathBuf)> = vec![(html.to_string(), html_path.to_path_buf())];
    if let Some(plan) = plan {
        for path in &plan.fragment_files {
            if path == html_path {
                continue;
            }
            let src = std::fs::read_to_string(path).map_err(|e| RunError::Read(path.clone(), e))?;
            markup.push((src, path.clone()));
        }
    }

    let mut table = lumen_ir::fragment::FragmentTable::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let fold = |table: &mut lumen_ir::fragment::FragmentTable,
                src: &str,
                uri: &str|
     -> Result<(), RunError> {
        let declared = parser
            .script_fragments(src, uri)
            .map_err(RunError::Script)?;
        table
            .merge(declared)
            .map_err(|e| RunError::Script(e.to_string()))
    };
    for (src, path) in &markup {
        let refs = parser.script_refs(src, path).map_err(RunError::ParseHtml)?;
        if !refs.inline.trim().is_empty() {
            fold(&mut table, &refs.inline, &path.display().to_string())?;
        }
        for rel in &refs.external {
            if ScriptEngine::from_path(Path::new(rel)) != Some(ScriptEngine::Candela) {
                continue;
            }
            let script = dir.join(rel);
            if seen.contains(&script) {
                continue;
            }
            let body =
                std::fs::read_to_string(&script).map_err(|e| RunError::Read(script.clone(), e))?;
            fold(&mut table, &body, &script.display().to_string())?;
            seen.push(script);
        }
    }
    Ok(table)
}

/// Concatenate the inline `<script>` body with every external script file
/// referenced via `<script src="...">`, separated by newlines.
///
/// One blob for one host. Used by the AOT paths (`lumenc build` bakes a single
/// source string into the artifact) and by the `[script] engine` override,
/// which puts the whole app on one engine by definition. The from-source run
/// path groups per language instead; see [`grouped_script_sources`].
pub(crate) fn combined_script_source(
    ir: &lumen_ir::layout_ir::LayoutIR,
    dir: &Path,
) -> Result<String, RunError> {
    let mut combined = ir.script_source.clone();
    for rel in &ir.external_scripts {
        let path = dir.join(rel);
        let body = std::fs::read_to_string(&path).map_err(|e| RunError::Read(path.clone(), e))?;
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&body);
    }
    Ok(combined)
}

/// The app's script source split by language: one entry per engine the app
/// needs, each holding that engine's whole program, in [`ScriptEngine::ALL`]
/// order. Empty when the app ships no script.
pub(crate) type GroupedScripts = Vec<(ScriptEngine, String)>;

/// Group the app's scripts by the engine that runs them.
///
/// Each `<script src="...">` file joins its extension's engine (`.cdl` ->
/// candela, `.lua` -> Lua, `.rhai` -> Rhai); within an engine the files
/// concatenate in source order. An extension no host claims is read as candela.
///
/// An inline `<script>` block carries no extension. It joins the app's one
/// external language when there is exactly one, and candela otherwise, so a
/// markup file whose script sits next to `main.rhai` keeps running under Rhai.
///
/// `[script] engine` overrides all of it: every script, inline and external,
/// joins the named engine as a single program.
pub(crate) fn grouped_script_sources(
    ir: &lumen_ir::layout_ir::LayoutIR,
    dir: &Path,
    cfg: &crate::config::LumenToml,
) -> Result<GroupedScripts, RunError> {
    if cfg.script.engine.is_some() {
        let combined = combined_script_source(ir, dir)?;
        if combined.trim().is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![(cfg.script.engine_kind(), combined)]);
    }

    // Which engines the external files name, in first-seen order.
    let externals: Vec<(ScriptEngine, &String)> = ir
        .external_scripts
        .iter()
        .map(|rel| {
            (
                ScriptEngine::from_path(Path::new(rel)).unwrap_or_default(),
                rel,
            )
        })
        .collect();
    let mut external_engines: Vec<ScriptEngine> = Vec::new();
    for (engine, _) in &externals {
        if !external_engines.contains(engine) {
            external_engines.push(*engine);
        }
    }
    let inline_engine = match external_engines.as_slice() {
        [only] => *only,
        // No `<script src>` names a language. That is an inline-only app, or a
        // precompiled artifact whose external sources were baked into one blob
        // at build time and stripped from the IR. Fall back to the directory
        // scan, which is what chose the host before grouping existed, so an
        // artifact keeps running under the host its source files name.
        [] => match crate::config::infer_script_hosts(dir, cfg).as_slice() {
            [only] => *only,
            _ => ScriptEngine::default(),
        },
        _ => ScriptEngine::default(),
    };

    let mut sources: Vec<(ScriptEngine, String)> = Vec::new();
    let mut push = |engine: ScriptEngine, body: &str| {
        if body.trim().is_empty() {
            return;
        }
        match sources.iter_mut().find(|(e, _)| *e == engine) {
            Some((_, acc)) => {
                acc.push('\n');
                acc.push_str(body);
            }
            None => sources.push((engine, body.to_string())),
        }
    };
    push(inline_engine, &ir.script_source);
    for (engine, rel) in &externals {
        let path = dir.join(rel);
        let body = std::fs::read_to_string(&path).map_err(|e| RunError::Read(path.clone(), e))?;
        push(*engine, &body);
    }
    sources.sort_by_key(|(engine, _)| *engine);
    Ok(sources)
}

#[cfg(feature = "runtime-parse")]
pub(crate) fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Parse a CSS source and extract its `:root { --name: value; ... }`
/// declarations via [`lumen_ir::css::Stylesheet::root_vars`]. A source that
/// fails to parse contributes no vars rather than failing the caller - the
/// same source is parsed again (and any real syntax error surfaced there)
/// as part of building the combined cascade stylesheet below.
#[cfg(feature = "runtime-parse")]
fn extract_root_vars(
    parser: &dyn SourceParser,
    css: &str,
) -> std::collections::HashMap<String, String> {
    parser
        .parse_css(css)
        .map(|sheet| sheet.root_vars())
        .unwrap_or_default()
}

#[cfg(feature = "runtime-parse")]
fn read_optional(path: &Path) -> Result<Option<String>, RunError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(RunError::Read(path.to_path_buf(), e)),
    }
}

pub(crate) fn derive_title(dir: &Path) -> String {
    dir.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Lumen".into())
}
