//! In-process source -> LMNA compile for the link-not-embed launcher.
//!
//! [`compile_dir_to_lmna`] turns an app directory (`main.lmn` + optional
//! `main.css` + inline / external `<script>`) into precompiled
//! [`lumen_ir::artifact`] bytes using ONLY the compiler front-end
//! (`parser_html` / `parser_css` / `resolve`), the shared CSS cascade
//! (`lumen_ir::css`), and the artifact codec -- with NO dependency on
//! `lumen-runtime`. That is what lets the `dlopen-run` launcher compile source
//! without static-linking the fat runtime: it produces the bytes here and hands
//! them across the C-ABI (`lumen_app_new_from_lmna`) to a dlopen'd liblumen.
//!
//! This mirrors the essential steps of `lumen_runtime::run::load_ir` +
//! `compile_app` (the `dev-run` AOT path), minus file-based multi-page
//! assembly and the embedded `skin=` user-agent stylesheet -- the same scope
//! `lumenc build` covers today (single entry page). See
//! `docs/design/link-not-embed.md` for the alpha-limitation rationale.
//!
//! Gated on `runtime-parse`: it wraps the parser front-end, which is itself
//! gated there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lumen_ir::layout_ir::Element;

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
    /// Inline `var()` substitution failed.
    #[error("inline var(): {0}")]
    Var(String),
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

    // `:root { --foo }` globals, extracted ahead of markup parse so inline
    // `var(--foo)` attr substrings resolve before typed parsing.
    let root_vars = match css.as_deref() {
        Some(src) => extract_root_vars(src),
        None => HashMap::new(),
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
    let spliced = if root_vars.is_empty() {
        spliced
    } else {
        lumen_ir::css_vars::resolve(&spliced, &root_vars, "").map_err(CompileError::Var)?
    };

    // Includes are already spliced away, so the string-only parser suffices.
    let mut ir = crate::parse_html(&spliced).map_err(|e| CompileError::ParseHtml(e.to_string()))?;
    ir.included_files = include_paths;

    // Rewrite `<image src>` relative paths absolute against the app dir.
    resolve_asset_paths(&mut ir.root, dir);

    // Single combined cascade pass over the author sheet.
    if let Some(css_src) = &css {
        let sheet = crate::parse_css(css_src).map_err(|e| CompileError::ParseCss(e.to_string()))?;
        if !sheet.rules.is_empty() {
            let warnings = lumen_ir::css::apply_css_with_media(
                &mut ir,
                &sheet,
                &lumen_ir::css::MediaContext::default(),
            )
            .map_err(|e| CompileError::ApplyCss(e.to_string()))?;
            for w in &warnings {
                eprintln!("{w}");
            }
        }
        ir.combined_stylesheet = Some(sheet);
    }

    // Bake inline + external `<script>` into one string; strip both from the IR
    // so the parser-free runtime reconstructs the exact script-host input.
    let script_source = combined_script_source(&ir, dir)?;
    ir.script_source = String::new();
    ir.external_scripts.clear();

    Ok(lumen_ir::artifact::CompiledApp { ir, script_source })
}

/// Read the `[app] entry` key from `lumen.toml`, falling back to `main.lmn`.
/// A missing or malformed config is not fatal here -- it just yields the
/// default; the compile step surfaces a genuinely missing entry file as a
/// [`CompileError::Read`].
fn entry_name(dir: &Path) -> String {
    let toml_path = dir.join("lumen.toml");
    let Ok(text) = std::fs::read_to_string(&toml_path) else {
        return "main.lmn".to_string();
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return "main.lmn".to_string();
    };
    value
        .get("app")
        .and_then(|a| a.get("entry"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "main.lmn".to_string())
}

/// Concatenate the inline `<script>` body with every external `.rhai` file
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

/// Extract `:root { --name: value; ... }` declarations from a CSS source.
/// Only the `:root` selector is honoured (parity with the runtime loader).
fn extract_root_vars(css: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(sheet) = crate::parse_css(css) else {
        return out;
    };
    for rule in &sheet.rules {
        if rule.selector.tag.as_deref() != Some("root") || !rule.selector.classes.is_empty() {
            continue;
        }
        for decl in &rule.declarations {
            if let Some(name) = decl.name.strip_prefix("--") {
                out.insert(name.to_string(), decl.value.clone());
            }
        }
    }
    out
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
    /// cascaded tree -- the whole link-not-embed compile path, no runtime.
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
