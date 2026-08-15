//! In-process source -> LMNA compile for the link-not-embed launcher.
//!
//! [`compile_dir_to_lmna`] turns an app directory (`main.lmn` + optional
//! `main.css` + inline / external `<script>`) into precompiled
//! [`lumen_ir::artifact`] bytes using only the compiler front-end
//! (`parser_html` / `parser_css` / `resolve`), the shared CSS cascade
//! (`lumen_ir::css`), and the artifact codec, with no dependency on
//! `lumen-runtime`. That is what lets the `dlopen-run` launcher compile source
//! without static-linking the fat runtime: it produces the bytes here and hands
//! them across the C-ABI (`lumen_app_new_from_lmna`) to a dlopen'd liblumen.
//!
//! This mirrors the essential steps of `lumen_runtime::run::load_ir` +
//! `compile_app` (the `dev-run` AOT path), minus file-based multi-page
//! assembly and the opt-in, by-name `skin=` user-agent stylesheet: the
//! same scope `lumenc build` covers today (single entry page). See
//! `docs/design/link-not-embed.md` for the alpha-limitation rationale.
//!
//! The always-on `ua.css` baseline is not part of that skipped scope: it
//! is not a skin (nothing selects it by name, and it can't be opted
//! out of), so an app compiled through this path still needs it folded
//! into the cascade or its controls render with no sizing floor at all.
//! [`UA_CSS`] folds it in at the same precedence `load_ir` uses.
//!
//! Gated on `runtime-parse`: it wraps the parser front-end, which is itself
//! gated there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lumen_ir::layout_ir::Element;

/// The always-on user-agent baseline (button / input / toggle / switch /
/// slider / progress / checkbox / radio sizing floors, root / title-bar
/// fill); see the header comment in the file itself for the full
/// precedence story. `lumen_runtime::skins::UA` embeds the same file for
/// the runtime-parse dev-run path and `lumenc build`; this path cannot
/// depend on `lumen-runtime` (see the module doc comment above), so it
/// includes the identical bytes directly instead of sharing the constant.
/// There is exactly one `ua.css` file on disk, with two `include_str!`
/// sites reading it.
const UA_CSS: &str = include_str!("../../runtime/src/skins/ua.css");

/// Errors raised while compiling a source directory to LMNA bytes.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// A required input file was missing or unreadable.
    #[error("read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    /// Markup failed to parse.
    #[error("parse markup: {0}")]
    ParseHtml(String),
    /// CSS failed to parse.
    #[error("parse css: {0}")]
    ParseCss(String),
    /// The CSS cascade failed to apply.
    #[error("apply css: {0}")]
    ApplyCss(String),
    /// The artifact failed to encode.
    #[error("artifact: {0}")]
    Artifact(String),
}

/// Compile `<dir>/main.lmn` (+ optional `main.css`, `<include>`, `@import`,
/// inline `:root` `var()`, and inline / external `<script>`) into precompiled
/// LMNA artifact bytes. No filesystem is touched beyond the source files; the
/// result is what [`lumen_ir::artifact::serialize`] would write to a `.lmna`.
///
/// The `[app] entry` key in `lumen.toml` (if present) selects the markup entry
/// file; otherwise `main.lmn` is used. Asset (`<image src>`) paths are rewritten
/// absolute against `dir` so they survive a cwd shift at run time.
pub fn compile_dir_to_lmna(dir: &Path) -> Result<Vec<u8>, CompileError> {
    let compiled = compile_dir(dir)?;
    lumen_ir::artifact::serialize(&compiled).map_err(|e| CompileError::Artifact(e.to_string()))
}

fn compile_dir(dir: &Path) -> Result<lumen_ir::artifact::CompiledApp, CompileError> {
    let entry = entry_name(dir);
    let html_path = dir.join(&entry);
    let css_path = dir.join("main.css");

    let html = std::fs::read_to_string(&html_path)
        .map_err(|e| CompileError::Read(html_path.clone(), e))?;
    let css_raw = read_optional(&css_path)?;

    // Resolve `@import "..."` (imported-first) before parsing the sheet.
    let css = match &css_raw {
        Some(src) => {
            let mut imports: Vec<PathBuf> = Vec::new();
            Some(
                crate::resolve::resolve_css_imports(
                    src,
                    &css_path,
                    &crate::resolve::FsLoader,
                    &mut imports,
                )
                .map_err(|e| CompileError::ParseCss(e.to_string()))?,
            )
        }
        None => None,
    };

    // Built-in Palette light/dark theme, parsed once and reused below for
    // the live combined stylesheet too - see
    // `lumen_ir::css::palette_root_css`. Always-on and not a named skin
    // (nothing selects it, nothing opts out of it), so - like `UA_CSS` -
    // it stays in scope on this no-`skin=` path.
    let palette_sheet = crate::parse_css(&lumen_ir::css::palette_root_css())
        .map_err(|e| CompileError::ParseCss(e.to_string()))?;

    // `:root { --foo }` globals, merged from the Palette theme, the UA
    // baseline, and the app's own stylesheet (this path has no `skin=`
    // support - module doc comment above - so there is no skin layer to
    // merge in), extracted ahead of markup parse so inline `var(--foo)`
    // attr substrings resolve before typed parsing.
    let root_vars = {
        let mut vars = palette_sheet.root_vars();
        vars.extend(extract_root_vars(UA_CSS));
        if let Some(src) = css.as_deref() {
            vars.extend(extract_root_vars(src));
        }
        vars
    };

    // Splice `<include src=...>` against the real filesystem, THEN apply inline
    // var() substitution over the fully-spliced markup (parity with the runtime
    // load order so included fragments get var() too).
    let mut include_paths: Vec<PathBuf> = Vec::new();
    let spliced = crate::resolve::resolve_includes(
        &html,
        &html_path,
        Some(&crate::resolve::FsLoader),
        &mut include_paths,
    )
    .map_err(|e| CompileError::ParseHtml(e.to_string()))?;
    // An unresolvable var() degrades to empty text plus a printed warning
    // instead of failing the compile - matching how the stylesheet cascade
    // below drops one bad declaration and keeps going (see `load_ir`'s
    // twin comment in `lumen-runtime` for the full rationale).
    // Runs even with no `:root` block: an empty variable set makes every
    // call unresolvable, and dropping it is exactly what should happen.
    let spliced = {
        let resolution = lumen_ir::css_vars::resolve_lenient(&spliced, &root_vars);
        for msg in &resolution.warnings {
            eprintln!("css warning: inline var() - {msg}");
        }
        resolution.output
    };

    // Includes are already spliced away, so the string-only parser suffices.
    let mut ir = crate::parse_html(&spliced).map_err(|e| CompileError::ParseHtml(e.to_string()))?;
    ir.included_files = include_paths;

    // Rewrite `<image src>` relative paths absolute against the app dir.
    resolve_asset_paths(&mut ir.root, dir);

    // The Palette theme, then the always-on UA baseline, then the author
    // sheet (if any), combined into one stylesheet with renumbered source
    // order and applied in a single cascade pass; the same scheme
    // `load_ir` uses (one pass so an author rule can actually override a
    // UA one; see that function's comment for why a second pass breaks
    // it). This path has no `skin=` support (module doc comment above), so
    // there is no skin-named sheet to insert between Palette+UA and
    // author.
    //
    // Origin precedence: Palette and `UA_CSS` rules are both tagged
    // `Origin::UserAgent`; author rules default to `Origin::Author`. The
    // cascade sorts on origin first, so any author rule beats any Palette
    // or UA rule for a normal declaration regardless of specificity. The
    // source-order bump keeps Palette ordered before UA within that shared
    // origin tier.
    let mut combined_rules = Vec::new();
    for mut rule in palette_sheet.rules {
        rule.origin = lumen_ir::css::Origin::UserAgent;
        combined_rules.push(rule);
    }
    let palette_rule_count = combined_rules.len();
    {
        let sheet = crate::parse_css(UA_CSS).map_err(|e| CompileError::ParseCss(e.to_string()))?;
        for mut rule in sheet.rules {
            rule.origin = lumen_ir::css::Origin::UserAgent;
            rule.source_order += palette_rule_count;
            combined_rules.push(rule);
        }
    }
    let ua_rule_count = combined_rules.len();
    if let Some(css_src) = &css {
        let sheet = crate::parse_css(css_src).map_err(|e| CompileError::ParseCss(e.to_string()))?;
        for mut rule in sheet.rules {
            rule.source_order += ua_rule_count;
            combined_rules.push(rule);
        }
    }
    let combined = lumen_ir::css::Stylesheet {
        rules: combined_rules,
    };
    // `combined` always carries at least the UA rules, so this is never
    // actually empty; the guard is defensive parity with `load_ir`.
    if !combined.rules.is_empty() {
        let warnings = lumen_ir::css::apply_css_with_media(
            &mut ir,
            &combined,
            &lumen_ir::css::MediaContext::default(),
        )
        .map_err(|e| CompileError::ApplyCss(e.to_string()))?;
        for w in &warnings {
            eprintln!("{w}");
        }
    }
    ir.combined_stylesheet = Some(combined);
    // Parse-time lint findings, same terms as `load_ir`: advisory, on
    // stderr, never fatal. Both compile paths print them so an app
    // compiled through the launcher reports what `lumenc check` does.
    for f in &ir.lint_findings {
        eprintln!("{}", f.render(&html_path));
    }

    // Bake inline + external `<script>` into one string; strip both from the IR
    // so the parser-free runtime reconstructs the exact script-host input.
    let script_source = combined_script_source(&ir, dir)?;
    let scripts = grouped_script_sources(&ir, dir)?;
    ir.script_source = String::new();
    ir.external_scripts.clear();

    Ok(lumen_ir::artifact::CompiledApp {
        ir,
        script_source,
        scripts,
        // This path covers the single entry page (see the module comment);
        // `lumenc build` and `lumenc package` are what compile a page set.
        pages: None,
    })
}

/// The engine name a script file's extension selects. Mirror of
/// `lumen_runtime::config::ScriptEngine::from_extension`, which this path
/// cannot reach (see the module doc comment).
fn engine_for(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("cdl") => Some("candela"),
        Some("lua") => Some("lua"),
        Some("rhai") => Some("rhai"),
        _ => None,
    }
}

/// Split the app's script by the engine that runs each part, so an app that
/// mixes languages keeps one host per language after compilation. Each
/// `<script src>` file joins its extension's engine; the inline block joins
/// the app's one external language when there is exactly one, and candela
/// otherwise. Mirror of `lumen_runtime::run::loading::grouped_script_sources`
/// without the `[script] engine` override, which the runtime applies itself.
fn grouped_script_sources(
    ir: &lumen_ir::layout_ir::LayoutIR,
    dir: &Path,
) -> Result<Vec<lumen_ir::artifact::CompiledScript>, CompileError> {
    let externals: Vec<(&'static str, &String)> = ir
        .external_scripts
        .iter()
        .map(|rel| (engine_for(Path::new(rel)).unwrap_or("candela"), rel))
        .collect();
    let mut engines: Vec<&'static str> = Vec::new();
    for (engine, _) in &externals {
        if !engines.contains(engine) {
            engines.push(engine);
        }
    }
    let inline_engine = match engines.as_slice() {
        [only] => *only,
        _ => "candela",
    };

    let mut out: Vec<lumen_ir::artifact::CompiledScript> = Vec::new();
    let mut push = |engine: &str, body: &str| {
        if body.trim().is_empty() {
            return;
        }
        match out.iter_mut().find(|s| s.engine == engine) {
            Some(entry) => {
                entry.source.push('\n');
                entry.source.push_str(body);
            }
            None => out.push(lumen_ir::artifact::CompiledScript {
                engine: engine.to_string(),
                source: body.to_string(),
                // This path has no script host to compile with (see the module
                // doc comment); the runtime runs it from source.
                bytecode: None,
            }),
        }
    };
    push(inline_engine, &ir.script_source);
    for (engine, rel) in &externals {
        let path = dir.join(rel);
        let body =
            std::fs::read_to_string(&path).map_err(|e| CompileError::Read(path.clone(), e))?;
        push(engine, &body);
    }
    Ok(out)
}

/// Read the `[app] entry` key from `lumen.toml`, falling back to `main.lmn`.
/// A missing or malformed config is not fatal here; it just yields the
/// default; the compile step surfaces a genuinely missing entry file as a
/// [`CompileError::Read`].
fn entry_name(dir: &Path) -> String {
    let toml_path = dir.join("lumen.toml");
    let Ok(text) = std::fs::read_to_string(&toml_path) else {
        return "main.lmn".to_string();
    };
    // `toml::from_str`, not `text.parse()`: the `FromStr` impl parses a single
    // TOML value, so a whole `lumen.toml` document only round-trips through the
    // deserializer. Getting this wrong is silent here - the error arm returns
    // the default entry rather than reporting anything.
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return "main.lmn".to_string();
    };
    value
        .get("app")
        .and_then(|a| a.get("entry"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "main.lmn".to_string())
}

/// Concatenate the inline `<script>` body with every external script file
/// referenced via `<script src="...">`, separated by newlines. Mirror of
/// `lumen_runtime::run::combined_script_source`.
fn combined_script_source(
    ir: &lumen_ir::layout_ir::LayoutIR,
    dir: &Path,
) -> Result<String, CompileError> {
    let mut combined = ir.script_source.clone();
    for rel in &ir.external_scripts {
        let path = dir.join(rel);
        let body =
            std::fs::read_to_string(&path).map_err(|e| CompileError::Read(path.clone(), e))?;
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&body);
    }
    Ok(combined)
}

/// Parse a CSS source and extract its `:root { --name: value; ... }`
/// declarations via [`lumen_ir::css::Stylesheet::root_vars`] (parity with
/// the runtime loader, `lumen_runtime::run::loading::extract_root_vars`). A
/// source that fails to parse contributes no vars rather than failing the
/// caller here - the same source is parsed again (and any real syntax error
/// surfaced there) as part of building the combined cascade stylesheet.
fn extract_root_vars(css: &str) -> HashMap<String, String> {
    crate::parse_css(css)
        .map(|sheet| sheet.root_vars())
        .unwrap_or_default()
}

/// Rewrite every relative `<image src>` absolute against `dir`. Mirror of
/// `lumen_runtime::run::resolve_asset_paths` (without the extra asset-roots,
/// which come from `lumen.toml` config the launcher path does not read).
fn resolve_asset_paths(el: &mut Element, dir: &Path) {
    if el.tag == "image"
        && let Some(src) = &el.attrs.src
    {
        let p = Path::new(src);
        if p.is_relative() {
            let resolved = dir.join(p);
            if let Some(s) = resolved.to_str() {
                el.attrs.src = Some(s.to_string());
            }
        }
    }
    for child in &mut el.children {
        resolve_asset_paths(child, dir);
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, CompileError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CompileError::Read(path.to_path_buf(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with markup + CSS + inline script compiles to valid LMNA
    /// bytes that decode back into an artifact carrying the baked script and a
    /// cascaded tree: the whole link-not-embed compile path, no runtime.
    #[test]
    fn compiles_dir_to_valid_lmna() {
        let tmp = std::env::temp_dir().join(format!("lumenc-compile-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        std::fs::write(
            tmp.join("main.lmn"),
            "<root><label id=\"a\" text=\"hi\"/><script>let x = 1;</script></root>",
        )
        .expect("write lmn");
        std::fs::write(tmp.join("main.css"), "#a { bg: #ff0000; }").expect("write css");

        let bytes = compile_dir_to_lmna(&tmp).expect("compile");
        let app = lumen_ir::artifact::read_bytes(&bytes).expect("decode");
        assert!(app.script_source.contains("let x = 1;"));
        assert_eq!(app.ir.root.tag, "root");
        assert!(app.ir.combined_stylesheet.is_some());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The always-on `ua.css` baseline reaches a control compiled through
    /// this path even when there is no `main.css` at all - the exact
    /// scenario that used to skip the cascade entirely on this path (no
    /// author sheet meant no `apply_css_with_media` call), so `<button>`
    /// came out with no `min-height` in the artifact. Once
    /// `apply_ua_style_defaults` stopped setting per-tag sizing directly
    /// on `Style` at spawn time and that sizing moved into `ua.css`
    /// instead, a compile path that never folds `ua.css` in produces an
    /// artifact with no sizing floor recorded anywhere - a control that
    /// would render with no tap-size minimum. This pins `UA_CSS` actually
    /// reaching `compile_dir`'s cascade.
    ///
    /// This only covers what the artifact carries (the resolved
    /// `Attributes`), not the rendered pixels - this crate has no
    /// dependency on `lumen-runtime`'s spawn / layout code to check
    /// further, and that is the whole point of this compile path.
    #[test]
    fn ua_css_reaches_dlopen_run_compile_path() {
        let tmp = std::env::temp_dir().join(format!("lumenc-compile-ua-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        std::fs::write(
            tmp.join("main.lmn"),
            "<root><button id=\"b\" text=\"Go\"/></root>",
        )
        .expect("write lmn");
        // Deliberately no main.css and no lumen.toml `[skin]` - the
        // skinless, cssless case where nothing but `ua.css` supplies any
        // sizing at all.

        let bytes = compile_dir_to_lmna(&tmp).expect("compile");
        let app = lumen_ir::artifact::read_bytes(&bytes).expect("decode");

        let button = app
            .ir
            .root
            .children
            .iter()
            .find(|e| e.tag == "button")
            .expect("button child present");
        assert_eq!(
            button.attrs.min_height,
            Some(lumen_ir::layout_ir::LengthSpec::Px(36.0)),
            "ua.css's `button {{ min-height: 36 }}` must reach this compile path"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A missing entry file surfaces as a read error, not a panic.
    #[test]
    fn missing_entry_is_error() {
        let tmp = std::env::temp_dir().join(format!("lumenc-compile-miss-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        assert!(matches!(
            compile_dir_to_lmna(&tmp),
            Err(CompileError::Read(_, _))
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
