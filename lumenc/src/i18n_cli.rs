//! `lumenc i18n extract <app_dir>` - translation-string extractor.
//!
//! Walks `<app_dir>` for `.lmn` + `.rhai` files, finds every
//! translation call site, and writes a `locale/<base_lang>.ftl` file
//! with placeholder values matching the keys.
//!
//! Two call shapes are recognized:
//!
//! - Rhai / Rust macro: `t!("key", ...)` / `tr!("key", ...)` /
//!   `lumen.tr("key", ...)` - string literal as the first argument.
//! - Markup attribute: `<text translatable="key">...</text>` or
//!   `<label translatable="key">...</label>`.
//!
//! The extractor is **idempotent**: existing entries in the target
//! `.ftl` file are preserved verbatim (so translators can edit them
//! without fear of being overwritten). Only newly-discovered keys
//! get appended at the end of the file, each with a placeholder
//! value matching the key (translators replace this).
//!
//! Output layout:
//!
//! ```text
//! <app_dir>/locale/<base_lang>.ftl
//! ```
//!
//! `<base_lang>` defaults to `en-US`; override with `--lang <tag>`.
//!
//! This is an intentionally simple substring/regex-style scanner - it
//! catches the common shapes and emits a stub `.ftl`. A full
//! AST-walking pass over the `.rhai` and `.lmn` parsers can come
//! later; the W5.7 plan calls this out as the stub-and-iterate path.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Entry point for `lumenc i18n ...`.
pub fn cmd_i18n(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(sub) = args.next() else {
        eprintln!("lumenc i18n: missing subcommand (expected `extract`)");
        return ExitCode::from(2);
    };
    match sub.as_str() {
        "extract" => cmd_extract(args),
        other => {
            eprintln!("lumenc i18n: unknown subcommand `{other}` (expected `extract`)");
            ExitCode::from(2)
        }
    }
}

fn cmd_extract(args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir: Option<String> = None;
    let mut lang = String::from("en-US");
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => {
                let Some(v) = args.next() else {
                    eprintln!("lumenc i18n extract: --lang needs a BCP-47 tag");
                    return ExitCode::from(2);
                };
                lang = v;
            }
            s if s.starts_with("--lang=") => {
                lang = s["--lang=".len()..].to_string();
            }
            _ if dir.is_none() => dir = Some(a),
            other => {
                eprintln!("lumenc i18n extract: unexpected arg '{other}'");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("lumenc i18n extract: missing <app_dir>");
        return ExitCode::from(2);
    };
    let app = PathBuf::from(&dir);
    if !app.is_dir() {
        eprintln!("lumenc i18n extract: {dir} is not a directory");
        return ExitCode::from(2);
    }

    let mut keys = BTreeSet::new();
    if let Err(e) = scan_dir(&app, &mut keys) {
        eprintln!("lumenc i18n extract: {e}");
        return ExitCode::FAILURE;
    }

    let locale_dir = app.join("locale");
    if let Err(e) = fs::create_dir_all(&locale_dir) {
        eprintln!("lumenc i18n extract: create {}: {e}", locale_dir.display());
        return ExitCode::FAILURE;
    }
    let target = locale_dir.join(format!("{lang}.ftl"));
    let merged = match merge_into_ftl(&target, &keys) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("lumenc i18n extract: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = write_atomic(&target, &merged.contents) {
        eprintln!("lumenc i18n extract: write {}: {e}", target.display());
        return ExitCode::FAILURE;
    }
    println!(
        "lumenc i18n extract: scanned {dir} -> {} ({} keys total, {} new)",
        target.display(),
        keys.len(),
        merged.added,
    );
    ExitCode::SUCCESS
}

/// Recursively walk `dir` and feed every `.lmn` / `.rhai` file
/// through [`extract_keys_into`].
pub fn scan_dir(dir: &Path, keys: &mut BTreeSet<String>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            // Skip vendored / build directories.
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), "target" | "node_modules" | ".git" | "locale") {
                continue;
            }
            scan_dir(&path, keys)?;
            continue;
        }
        if file_type.is_file()
            && let Some(ext) = path.extension()
        {
            let ext = ext.to_string_lossy();
            if matches!(ext.as_ref(), "lmn" | "rhai") {
                let body = fs::read_to_string(&path)?;
                extract_keys_into(&body, keys);
            }
        }
    }
    Ok(())
}

/// Append every translation key found in `src` to `out`.
///
/// Recognizes three forms:
///
/// - `t!(i18n, "key", ...)` / `tr!(i18n, "key", ...)` - Rust macro;
///   first arg is the I18n resource binding (an expression),
///   second arg is the key literal.
/// - `lumen.tr("key", ...)` / `lumen.t("key", ...)` - Rhai builtin;
///   first arg is the key literal.
/// - `translatable="key"` - markup attribute.
///
/// The scanner is regex-free (no extra dep): it looks for the prefix
/// substring, advances past whitespace, then either reads a string
/// literal directly (Rhai / markup forms) or skips one argument and
/// then reads the literal (macro form).
pub fn extract_keys_into(src: &str, out: &mut BTreeSet<String>) {
    // Macro forms - the key is the second arg.
    for prefix in ["t!(", "tr!("] {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let start = idx + pos + prefix.len();
            if let Some(key) = read_string_arg_after_skip(&src[start..]) {
                out.insert(key);
            }
            idx = start;
        }
    }
    // Rhai builtins - the key is the first arg.
    for prefix in ["lumen.tr(", "lumen.t("] {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let start = idx + pos + prefix.len();
            if let Some(key) = read_string_arg(&src[start..]) {
                out.insert(key);
            }
            idx = start;
        }
    }
    // Markup attribute - `translatable="key"`.
    let attr = "translatable=";
    let mut idx = 0;
    while let Some(pos) = src[idx..].find(attr) {
        let start = idx + pos + attr.len();
        if let Some(key) = read_string_arg(&src[start..]) {
            out.insert(key);
        }
        idx = start;
    }
}

/// Skip the first argument (a simple ident / path expression), then
/// read the next string literal. Used for the `t!(i18n, "key", ...)`
/// shape. "Simple" here means no parens / brackets / braces in the
/// first arg; that catches the common case where the first arg is a
/// `Res<I18n>` binding name.
fn read_string_arg_after_skip(s: &str) -> Option<String> {
    let s = s.trim_start();
    // Walk past chars until we hit the next `,` (top-level).
    let mut depth = 0i32;
    let mut chars = s.char_indices();
    for (i, c) in chars.by_ref() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                return read_string_arg(&s[i + 1..]);
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    None
}

/// Read a leading string literal from `s` (skipping whitespace).
/// Accepts either `"..."` or `'...'`. Returns `None` if `s` does not
/// open with a string literal. Naive - does not honor escape
/// sequences (`\"` is treated as a closing quote). That's fine for
/// translation keys which are conventionally simple ASCII slugs.
fn read_string_arg(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut chars = s.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &s[quote.len_utf8()..];
    let end = rest.find(quote)?;
    let key = &rest[..end];
    // Reject obvious garbage (empty / contains a newline or quote).
    if key.is_empty() || key.contains('\n') {
        return None;
    }
    Some(key.to_string())
}

struct MergedFtl {
    contents: String,
    added: usize,
}

/// Read existing `<target>` (if present), parse out its keys, and
/// append every key in `discovered` that isn't already covered. The
/// returned `contents` is the merged FTL text.
fn merge_into_ftl(target: &Path, discovered: &BTreeSet<String>) -> std::io::Result<MergedFtl> {
    let existing = match fs::read_to_string(target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let existing_keys = parse_existing_keys(&existing);
    let mut buf = existing.clone();
    let mut added = 0;
    let mut new_keys: Vec<&String> = discovered
        .iter()
        .filter(|k| !existing_keys.contains(*k))
        .collect();
    new_keys.sort();
    if !new_keys.is_empty() {
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
        if !buf.is_empty() {
            buf.push('\n');
        } else {
            buf.push_str(
                "# Auto-generated by `lumenc i18n extract`. Existing entries are preserved\n\
                 # on re-run; new keys are appended. Translators edit the placeholder\n\
                 # values below.\n\n",
            );
        }
        for k in new_keys {
            // Placeholder value matches the key itself so untranslated
            // entries still render something sensible in the UI.
            buf.push_str(&format!("# TODO: translate\n{k} = {k}\n\n"));
            added += 1;
        }
    }
    Ok(MergedFtl {
        contents: buf,
        added,
    })
}

/// Tiny FTL key scanner - picks out `key = ...` lines from existing
/// content. Strips comment lines and indented attribute lines.
fn parse_existing_keys(ftl: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for line in ftl.lines() {
        let trimmed = line.trim_start();
        // Comments and continuation lines are skipped.
        if trimmed.starts_with('#') || trimmed != line {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim();
            if is_valid_ftl_key(key) {
                keys.insert(key.to_string());
            }
        }
    }
    keys
}

/// FTL keys are kebab-case ASCII identifiers per spec. Reject
/// anything else so we don't accidentally treat `[selector] = ...`
/// inside an FTL selector as a top-level key.
fn is_valid_ftl_key(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Write `bytes` to `target` via a temp-file + rename so a crash
/// mid-write doesn't leave the user's `.ftl` truncated.
fn write_atomic(target: &Path, bytes: &str) -> std::io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_t_macro() {
        let src = r#"
            fn build() {
                let a = t!(i, "greet");
                let b = t!(i, "good-bye", name = "Alice");
            }
        "#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("greet"));
        assert!(keys.contains("good-bye"));
    }

    #[test]
    fn extract_rhai_lumen_tr() {
        let src = r#"
            let s = lumen.tr("hello");
            let t = lumen.tr("count", #{ n: 3 });
        "#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("hello"));
        assert!(keys.contains("count"));
    }

    #[test]
    fn extract_markup_translatable() {
        let src = r#"<label translatable="app-title">Hello</label>"#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("app-title"));
    }

    #[test]
    fn read_string_arg_handles_single_quote() {
        assert_eq!(read_string_arg("'foo', other").as_deref(), Some("foo"));
    }

    #[test]
    fn read_string_arg_rejects_non_string() {
        assert_eq!(read_string_arg("42, other"), None);
    }

    #[test]
    fn merge_preserves_existing_and_appends_new() {
        let dir = tempdir();
        let target = dir.join("en-US.ftl");
        fs::write(&target, "greet = Hello!\n# notes\n").unwrap();
        let mut keys = BTreeSet::new();
        keys.insert("greet".to_string());
        keys.insert("brand-new".to_string());
        let merged = merge_into_ftl(&target, &keys).unwrap();
        assert!(merged.contents.contains("greet = Hello!"));
        assert!(merged.contents.contains("brand-new = brand-new"));
        assert_eq!(merged.added, 1);
    }

    #[test]
    fn merge_creates_when_missing() {
        let dir = tempdir();
        let target = dir.join("en-US.ftl");
        let mut keys = BTreeSet::new();
        keys.insert("greet".to_string());
        let merged = merge_into_ftl(&target, &keys).unwrap();
        assert!(merged.contents.contains("greet = greet"));
        assert_eq!(merged.added, 1);
    }

    #[test]
    fn parse_existing_keys_skips_comments_and_selectors() {
        let ftl = "# top comment\n\
                   greet = Hello!\n\
                   items = { $count ->\n\
                       [one] one item\n\
                      *[other] many items\n\
                   }\n";
        let keys = parse_existing_keys(ftl);
        assert!(keys.contains("greet"));
        assert!(keys.contains("items"));
        // Selector lines are indented, so they're not top-level keys.
        assert!(!keys.contains("[one]"));
    }

    #[test]
    fn full_extract_roundtrip() {
        let dir = tempdir();
        let lmn = dir.join("main.lmn");
        fs::write(
            &lmn,
            "<root><label translatable=\"app-title\">Hi</label></root>",
        )
        .unwrap();
        let rhai = dir.join("main.rhai");
        fs::write(&rhai, "let s = lumen.tr(\"greet\");\n").unwrap();
        let mut keys = BTreeSet::new();
        scan_dir(&dir, &mut keys).unwrap();
        assert!(keys.contains("app-title"));
        assert!(keys.contains("greet"));
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "lumen-i18n-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
