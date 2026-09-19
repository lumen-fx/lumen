//! Multi-file resolution for markup `<include>` and CSS `@import`.
//!
//! Both directives splice another file's source into the current one at
//! parse/load time. The primitives here are string-in / string-out so the
//! pure entry points ([`crate::parse_html`], [`crate::parse_css`]) stay
//! file-system-free - tests and the LSP keep passing raw strings, while
//! the runtime ([`crate::run`]) supplies a real [`FileLoader`].
//!
//! # Semantics
//!
//! - **Includes** ([`resolve_includes`]) run *before* template expansion,
//!   so `<template>` blocks defined in an included file register globally
//!   and are usable from any file. The included file's top-level elements
//!   splice in place of the `<include src="..."/>` tag.
//! - **CSS imports** ([`resolve_css_imports`]) are spliced *before* the
//!   importing file's own rules (imported-first), so at equal specificity
//!   the importing file wins the cascade. Only top-of-file `@import` is
//!   allowed; anything later is an error.
//! - Both do lexical cycle detection keyed on a normalized path and name
//!   the full chain on failure. Nested directives are followed
//!   recursively; each file resolves its relative paths against its own
//!   directory.

use crate::layout_ir::ParseError;
use std::path::{Component, Path, PathBuf};

/// Abstracts reading a file's UTF-8 contents so string-only callers can
/// pass a mock (or `None`) while the runtime passes [`FsLoader`].
pub trait FileLoader {
    /// Load the file at `path`, returning its UTF-8 contents.
    fn load(&self, path: &Path) -> std::io::Result<String>;
}

/// [`FileLoader`] backed by the real filesystem.
pub struct FsLoader;

impl FileLoader for FsLoader {
    fn load(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// Lexically normalize a path: fold `.` away and pop `..` where a real
/// prior component exists. Unlike [`std::fs::canonicalize`] this never
/// touches the filesystem, so it works with mock loaders and with paths
/// whose targets may not exist yet. It is a *cycle-detection key*, not a
/// canonical filesystem identity - that's all callers need it to be.
pub fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Compute a `line:col` (1-based) label for a byte offset in `src`.
fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, c) in src.char_indices() {
        if i >= byte {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Find the byte offset (relative to `s`, which must start at `<`) of the
/// `>` that closes this tag, skipping any `>` inside a quoted attribute value
/// (`<include src="x>y.lmn"/>`). Returns `None` if the tag is unterminated.
fn find_tag_gt(s: &str) -> Option<usize> {
    let mut in_quote: Option<u8> = None;
    for (i, &c) in s.as_bytes().iter().enumerate() {
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => in_quote = Some(c),
                b'>' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

/// Extract the value of `attr` from a raw tag string (`<include src="x"/>`).
fn tag_attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    // Look for `attr` followed by `=` and a double-quoted value.
    let bytes = tag.as_bytes();
    let mut search_from = 0usize;
    loop {
        let rel = tag[search_from..].find(attr)?;
        let idx = search_from + rel;
        // Require a word boundary *before* the attr name so `src` does not
        // match inside `data-src` / `nsrc` and pick the wrong include target.
        // (A trailing boundary is enforced implicitly by the `=` requirement,
        // so `srcset` never matches `src`.)
        let leading_ok = idx == 0 || {
            let p = bytes[idx - 1];
            !(p.is_ascii_alphanumeric() || p == b'_' || p == b'-')
        };
        if leading_ok {
            let after = &tag[idx + attr.len()..];
            let after_trim = after.trim_start();
            if let Some(v) = after_trim.strip_prefix('=') {
                let v = v.trim_start();
                if let Some(v) = v.strip_prefix('"') {
                    if let Some(end) = v.find('"') {
                        return Some(&v[..end]);
                    }
                }
            }
        }
        // `attr` matched inside a longer word or lacked `=` - keep scanning.
        search_from = idx + attr.len();
    }
}

/// Resolve every `<include src="..."/>` in `src`, splicing the referenced
/// file's contents in place. Recurses into nested includes.
///
/// - `self_path` names the file `src` came from - used to resolve relative
///   include paths (against its parent directory), to seed cycle detection,
///   and in error positions. Pass an empty path for anonymous strings.
/// - `loader` reads referenced files. When `None`, includes are dropped
///   (spliced away to empty) so string-only callers such as the LSP don't
///   choke on markup that references files they can't see.
/// - Every resolved file path is pushed to `out_files` (normalized) so the
///   hot-reload watcher can poll them.
pub fn resolve_includes(
    src: &str,
    self_path: &Path,
    loader: Option<&dyn FileLoader>,
    out_files: &mut Vec<PathBuf>,
) -> Result<String, ParseError> {
    let mut chain = vec![normalize_path(self_path)];
    resolve_includes_inner(src, self_path, loader, out_files, &mut chain)
}

fn resolve_includes_inner(
    src: &str,
    self_path: &Path,
    loader: Option<&dyn FileLoader>,
    out_files: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<String, ParseError> {
    let base_dir = self_path.parent().unwrap_or_else(|| Path::new(""));
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while let Some(rel) = src[i..].find("<include") {
        let start = i + rel;
        // Confirm this is the `<include` *tag*, not a longer name like
        // `<include-foo>` - the next char must be whitespace, `/`, or `>`.
        let after_name = src[start + "<include".len()..]
            .chars()
            .next()
            .unwrap_or('>');
        if !(after_name.is_whitespace() || after_name == '/' || after_name == '>') {
            out.push_str(&src[i..start + "<include".len()]);
            i = start + "<include".len();
            continue;
        }
        let gt = find_tag_gt(&src[start..])
            .ok_or_else(|| ParseError::Include("unterminated <include> tag".into()))?
            + start;
        let tag = &src[start..=gt];
        let self_closing = tag[..tag.len() - 1].trim_end().ends_with('/');
        // Consume through a matching `</include>` for the paired form.
        let consumed_end = if self_closing {
            gt + 1
        } else {
            match src[gt + 1..].find("</include>") {
                Some(rel_close) => gt + 1 + rel_close + "</include>".len(),
                None => {
                    return Err(ParseError::Include(
                        "unterminated <include>...</include>".into(),
                    ));
                }
            }
        };

        out.push_str(&src[i..start]);

        let src_attr = tag_attr(tag, "src").ok_or_else(|| {
            let (l, c) = line_col(src, start);
            ParseError::Include(format!(
                "<include> missing src=\"...\" at {}:{l}:{c}",
                display_path(self_path)
            ))
        })?;

        match loader {
            None => {
                // No loader (string-only / LSP path): drop the include.
            }
            Some(loader) => {
                let target = normalize_path(&base_dir.join(src_attr));
                if chain.contains(&target) {
                    let mut names: Vec<String> = chain.iter().map(|p| display_path(p)).collect();
                    names.push(display_path(&target));
                    return Err(ParseError::Include(format!(
                        "include cycle detected: {}",
                        names.join(" -> ")
                    )));
                }
                let body = loader.load(&target).map_err(|e| {
                    let (l, c) = line_col(src, start);
                    ParseError::Include(format!(
                        "include \"{src_attr}\" not found (from {}:{l}:{c}): {e}",
                        display_path(self_path)
                    ))
                })?;
                out_files.push(target.clone());
                chain.push(target.clone());
                let resolved =
                    resolve_includes_inner(&body, &target, Some(loader), out_files, chain)?;
                chain.pop();
                out.push_str(&resolved);
            }
        }

        i = consumed_end;
    }
    out.push_str(&src[i..]);
    Ok(out)
}

/// Resolve leading `@import "..."`s in a CSS source, splicing each imported
/// sheet's contents (recursively) *ahead* of the importing file's own rules.
///
/// Position rule (relaxed from the CSS spec, which allows `@charset`/
/// `@layer` before `@import`): `@import` must appear at the very top of the
/// file, before any rule. An `@import` after other content is an error.
///
/// Returns the concatenated source (imported files first, this file's own
/// body last) ready to hand to the pure [`crate::parse_css`]. Every
/// imported file path is pushed to `out_files` (normalized).
pub fn resolve_css_imports(
    src: &str,
    self_path: &Path,
    loader: &dyn FileLoader,
    out_files: &mut Vec<PathBuf>,
) -> Result<String, ParseError> {
    let mut chain = vec![normalize_path(self_path)];
    resolve_css_imports_inner(src, self_path, loader, out_files, &mut chain)
}

fn resolve_css_imports_inner(
    src: &str,
    self_path: &Path,
    loader: &dyn FileLoader,
    out_files: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<String, ParseError> {
    let base_dir = self_path.parent().unwrap_or_else(|| Path::new(""));
    let mut imported = String::new();
    let mut rest = src;

    loop {
        let trimmed = skip_ws_and_comments(rest);
        if let Some(after_at) = trimmed.strip_prefix("@import") {
            // Expect a quoted path, optionally with a `url(...)` wrapper we
            // do not support - keep it simple: `@import "path.css";`.
            let after = after_at.trim_start();
            let (path, after_quote) = parse_import_target(after).ok_or_else(|| {
                ParseError::Include(format!(
                    "malformed @import in {} (expected @import \"path.css\";)",
                    display_path(self_path)
                ))
            })?;
            // Advance `rest` past the terminating `;`. Search *after* the
            // closing quote so a `;` inside the quoted path (`@import
            // "a;b.css";`) doesn't prematurely terminate the statement.
            let semi = after_quote.find(';').ok_or_else(|| {
                ParseError::Include(format!(
                    "@import missing ';' in {}",
                    display_path(self_path)
                ))
            })?;
            rest = &after_quote[semi + 1..];

            let target = normalize_path(&base_dir.join(&path));
            if chain.contains(&target) {
                let mut names: Vec<String> = chain.iter().map(|p| display_path(p)).collect();
                names.push(display_path(&target));
                return Err(ParseError::Include(format!(
                    "@import cycle detected: {}",
                    names.join(" -> ")
                )));
            }
            let body = loader.load(&target).map_err(|e| {
                ParseError::Include(format!(
                    "@import \"{path}\" not found (from {}): {e}",
                    display_path(self_path)
                ))
            })?;
            out_files.push(target.clone());
            chain.push(target.clone());
            let resolved = resolve_css_imports_inner(&body, &target, loader, out_files, chain)?;
            chain.pop();
            imported.push_str(&resolved);
            if !imported.ends_with('\n') {
                imported.push('\n');
            }
            continue;
        }
        break;
    }

    // Any `@import` appearing after real content is illegal. Detect a
    // stray `@import` token in the remaining body (outside comments/strings
    // is approximated by a simple substring check, which is adequate since
    // `@import` is not otherwise valid mid-sheet).
    if contains_stray_import(rest) {
        return Err(ParseError::Include(format!(
            "@import must appear at the top of {} (before any rule)",
            display_path(self_path)
        )));
    }

    let mut combined = imported;
    combined.push_str(rest);
    Ok(combined)
}

/// Parse `"path.css"` (or `'path.css'`) at the start of `s`, returning the
/// unquoted path and the slice immediately following the closing quote (so
/// the caller can locate the terminating `;` without being fooled by a `;`
/// inside the quotes).
fn parse_import_target(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let (quote, rest) = match s.chars().next()? {
        '"' => ('"', &s[1..]),
        '\'' => ('\'', &s[1..]),
        _ => return None,
    };
    let end = rest.find(quote)?;
    Some((rest[..end].to_string(), &rest[end + 1..]))
}

/// Skip leading whitespace and `/* ... */` comments.
fn skip_ws_and_comments(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if let Some(after) = t.strip_prefix("/*") {
            if let Some(end) = after.find("*/") {
                s = &after[end + 2..];
                continue;
            }
        }
        return t;
    }
}

/// Best-effort detection of an `@import` sitting after real content. Ignores
/// occurrences inside `/* comments */` and string literals so a declaration
/// like `content: "@import"` doesn't trip a false positive.
fn contains_stray_import(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match in_string {
            Some(q) => {
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == q {
                    in_string = None;
                }
                i += 1;
            }
            None => {
                if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                if c == b'"' || c == b'\'' {
                    in_string = Some(c);
                    i += 1;
                    continue;
                }
                // `@` (0x40) is always a char boundary in valid UTF-8, so the
                // slice below never splits a multibyte scalar.
                if c == b'@' && s[i..].starts_with("@import") {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// Render a path for error messages, falling back to a lossy string.
fn display_path(p: &Path) -> String {
    if p.as_os_str().is_empty() {
        "<string>".to_string()
    } else {
        p.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory loader keyed by normalized path string.
    struct MockLoader(HashMap<String, String>);

    impl FileLoader for MockLoader {
        fn load(&self, path: &Path) -> std::io::Result<String> {
            let key = normalize_path(path).display().to_string();
            self.0.get(&key).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such mock file")
            })
        }
    }

    fn mock(files: &[(&str, &str)]) -> MockLoader {
        MockLoader(
            files
                .iter()
                .map(|(k, v)| {
                    (
                        normalize_path(Path::new(k)).display().to_string(),
                        v.to_string(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn include_splices_in_place() {
        let loader = mock(&[("app/parts/header.lmn", "<tile class=\"hdr\"/>")]);
        let mut files = Vec::new();
        let out = resolve_includes(
            "<root><include src=\"parts/header.lmn\"/><label/></root>",
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap();
        assert_eq!(out, "<root><tile class=\"hdr\"/><label/></root>");
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("parts/header.lmn"));
    }

    #[test]
    fn include_template_registers() {
        let loader = mock(&[(
            "app/lib.lmn",
            "<template name=\"Card\"><tile class=\"card\"/></template>",
        )]);
        let mut files = Vec::new();
        let out = resolve_includes(
            "<root><include src=\"lib.lmn\"/><Card/></root>",
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap();
        assert!(out.contains("<template name=\"Card\">"));
        assert!(out.contains("<Card/>"));
    }

    #[test]
    fn include_nested() {
        let loader = mock(&[
            ("app/a.lmn", "<a><include src=\"b.lmn\"/></a>"),
            ("app/b.lmn", "<b/>"),
        ]);
        let mut files = Vec::new();
        let out = resolve_includes(
            "<root><include src=\"a.lmn\"/></root>",
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap();
        assert_eq!(out, "<root><a><b/></a></root>");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn include_cycle_detected() {
        let loader = mock(&[
            ("app/a.lmn", "<a><include src=\"b.lmn\"/></a>"),
            ("app/b.lmn", "<b><include src=\"a.lmn\"/></b>"),
        ]);
        let mut files = Vec::new();
        let err = resolve_includes(
            "<root><include src=\"a.lmn\"/></root>",
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cycle"), "got: {msg}");
        assert!(msg.contains("a.lmn") && msg.contains("b.lmn"), "got: {msg}");
    }

    #[test]
    fn include_missing_file_has_position() {
        let loader = mock(&[]);
        let mut files = Vec::new();
        let err = resolve_includes(
            "<root>\n  <include src=\"nope.lmn\"/>\n</root>",
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope.lmn"), "got: {msg}");
        assert!(msg.contains("2:3"), "expected include-site pos, got: {msg}");
    }

    #[test]
    fn include_dropped_without_loader() {
        let mut files = Vec::new();
        let out = resolve_includes(
            "<root><include src=\"x.lmn\"/><label/></root>",
            Path::new(""),
            None,
            &mut files,
        )
        .unwrap();
        assert_eq!(out, "<root><label/></root>");
        assert!(files.is_empty());
    }

    #[test]
    fn css_import_order_imported_first() {
        let loader = mock(&[("app/base.css", ".x { color: red; }")]);
        let mut files = Vec::new();
        let out = resolve_css_imports(
            "@import \"base.css\";\n.x { color: blue; }",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .unwrap();
        // imported rule appears before the importing file's own rule.
        let imported_at = out.find("color: red").unwrap();
        let own_at = out.find("color: blue").unwrap();
        assert!(imported_at < own_at, "imported should come first: {out}");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn css_import_cycle_detected() {
        let loader = mock(&[
            ("app/a.css", "@import \"b.css\";\n.a{}"),
            ("app/b.css", "@import \"a.css\";\n.b{}"),
        ]);
        let mut files = Vec::new();
        let err = resolve_css_imports(
            "@import \"a.css\";",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .unwrap_err();
        assert!(err.to_string().contains("cycle"), "got: {err}");
    }

    #[test]
    fn css_import_after_rule_errors() {
        let loader = mock(&[("app/base.css", ".x{}")]);
        let mut files = Vec::new();
        let err = resolve_css_imports(
            ".y {}\n@import \"base.css\";",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .unwrap_err();
        assert!(err.to_string().contains("top"), "got: {err}");
    }

    #[test]
    fn css_import_leading_comment_ok() {
        let loader = mock(&[("app/base.css", ".x{}")]);
        let mut files = Vec::new();
        let out = resolve_css_imports(
            "/* header */\n@import \"base.css\";\n.y{}",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .unwrap();
        assert!(out.contains(".x{}"));
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn tag_attr_requires_word_boundary() {
        // `src` must not match inside `data-src` - the include should pick
        // the real `src`, not the decoy.
        assert_eq!(
            tag_attr(r#"<include data-src="decoy.lmn" src="real.lmn"/>"#, "src"),
            Some("real.lmn")
        );
        // And not inside `nsrc`.
        assert_eq!(
            tag_attr(r#"<include nsrc="decoy.lmn" src="real.lmn"/>"#, "src"),
            Some("real.lmn")
        );
        // A trailing-boundary decoy (`srcset`) is skipped via the `=` rule.
        assert_eq!(
            tag_attr(r#"<include srcset="decoy" src="real.lmn"/>"#, "src"),
            Some("real.lmn")
        );
    }

    #[test]
    fn include_data_src_decoy_resolves_real_target() {
        let loader = mock(&[("app/real.lmn", "<tile class=\"real\"/>")]);
        let mut files = Vec::new();
        let out = resolve_includes(
            r#"<root><include data-src="decoy.lmn" src="real.lmn"/></root>"#,
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap();
        assert_eq!(out, "<root><tile class=\"real\"/></root>");
        assert!(files[0].ends_with("real.lmn"));
    }

    #[test]
    fn include_gt_in_quoted_src_not_truncated() {
        // A `>` inside the quoted `src` must not terminate the tag early.
        let loader = mock(&[("app/a>b.lmn", "<tile class=\"weird\"/>")]);
        let mut files = Vec::new();
        let out = resolve_includes(
            r#"<root><include src="a>b.lmn"/></root>"#,
            Path::new("app/main.lmn"),
            Some(&loader),
            &mut files,
        )
        .unwrap();
        assert_eq!(out, "<root><tile class=\"weird\"/></root>");
    }

    #[test]
    fn css_import_path_with_semicolon_in_quotes() {
        let loader = mock(&[("app/a;b.css", ".weird{}")]);
        let mut files = Vec::new();
        let out = resolve_css_imports(
            "@import \"a;b.css\";\n.y{}",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .unwrap();
        assert!(out.contains(".weird{}"));
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn stray_import_in_string_is_not_flagged() {
        // `@import` inside a declaration string must not trip the
        // "@import after content" guard.
        let loader = mock(&[]);
        let mut files = Vec::new();
        let out = resolve_css_imports(
            ".x { content: \"@import nope\"; }",
            Path::new("app/main.css"),
            &loader,
            &mut files,
        )
        .expect("string @import must not be flagged as stray");
        assert!(out.contains("content"));
    }
}
