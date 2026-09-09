//! `lumenc i18n extract <app_dir>` - translation-string extractor.
//!
//! Walks `<app_dir>` for markup (`.lmn`) and script (`.rhai`, `.lua`,
//! `.cdl`) files, finds every translation call site, and writes a
//! `locale/<base_lang>.ftl` file with placeholder values matching the
//! keys.
//!
//! Three call shapes are recognized:
//!
//! - Script builtin: `t("key")` / `tr("key")`, and candela's namespaced
//!   `lumen::t("key")` - string literal as the first argument.
//! - Rust macro: `t!(i18n, "key", ...)` / `tr!(i18n, "key", ...)` -
//!   string literal as the second argument.
//! - Markup attribute: `translatable="key"`, on any element that shows a
//!   string.
//!
//! A marked element's other strings hang off the same message as Fluent
//! attributes, so a `placeholder` or an `alt` the markup authors is
//! collected as `key.placeholder` / `key.alt` alongside the key itself.
//! What the extractor writes is what resolves: a string the markup does not
//! write gets no entry, because nothing would read one.
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
//! The scanner works on text, not on a parsed AST: it finds the call
//! prefix, checks it starts a name rather than ending one, and reads the
//! string literal that follows. A key built at runtime rather than
//! written as a literal is invisible to it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::translate::attribute_key;

/// Usage block for `lumenc i18n --help` and `lumenc i18n extract --help`.
const I18N_USAGE: &str = "lumenc i18n - translation catalogue tooling

USAGE:
    lumenc i18n extract <app_dir> [--lang en-US]

Scans the app's .lmn, .rhai, .lua and .cdl files for t(\"key\", ...) /
tr(\"key\", ...) / lumen::t(\"key\", ...) / t!(i18n, \"key\", ...) /
translatable=\"key\" and writes or merges <app_dir>/locale/<lang>.ftl.
Idempotent: existing entries are preserved, new keys are appended with
placeholder values.

    --lang TAG        BCP-47 tag naming the catalogue to write
                      (default en-US).";

/// Entry point for `lumenc i18n ...`.
pub fn cmd_i18n(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(sub) = args.next() else {
        eprintln!("lumenc i18n: missing subcommand (expected `extract`)");
        return ExitCode::from(2);
    };
    match sub.as_str() {
        h if crate::is_help_flag(h) => {
            println!("{I18N_USAGE}");
            ExitCode::SUCCESS
        }
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
            h if crate::is_help_flag(h) => {
                println!("{I18N_USAGE}");
                return ExitCode::SUCCESS;
            }
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

/// Recursively walk `dir` and feed every markup / script file
/// (`.lmn`, `.rhai`, `.lua`, `.cdl`) through [`extract_keys_into`].
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
            if matches!(ext.as_ref(), "lmn" | "rhai" | "lua" | "cdl") {
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
/// - `t("key")` / `tr("key")` / `lumen::t("key")` - script builtin;
///   first arg is the key literal.
/// - `t!(i18n, "key", ...)` / `tr!(i18n, "key", ...)` - Rust macro;
///   first arg is the I18n resource binding (an expression),
///   second arg is the key literal.
/// - `translatable="key"` - markup attribute. The enclosing tag decides
///   which further keys come with it: a `placeholder` or an `alt` it
///   authors is collected as `key.placeholder` / `key.alt`, and the bare
///   key is collected for the element's own text unless one of those is
///   the only string the element shows.
///
/// The scanner is regex-free (no extra dep): it looks for the prefix
/// substring, advances past whitespace, then either reads a string
/// literal directly (builtin / markup forms) or skips one argument and
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
    // Script builtins - the key is the first arg. One scan covers every
    // host's spelling: bare `t("key")` (rhai / lua) and the namespaced
    // `lumen::t("key")` (candela) differ only in what precedes the call.
    for prefix in ["t(", "tr("] {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let at = idx + pos;
            let start = at + prefix.len();
            if is_call_boundary(src, at)
                && let Some(key) = read_string_arg(&src[start..])
            {
                out.insert(key);
            }
            idx = start;
        }
    }
    // Markup attribute - `translatable="key"`.
    let attr = "translatable=";
    let mut idx = 0;
    while let Some(pos) = src[idx..].find(attr) {
        let at = idx + pos;
        let start = at + attr.len();
        if let Some(key) = read_string_arg(&src[start..]) {
            let tag = enclosing_tag(src, at).unwrap_or("");
            let mut names_another = false;
            for name in ["placeholder", "alt"] {
                if tag_has_attribute(tag, name) {
                    out.insert(attribute_key(&key, name));
                    names_another = true;
                }
            }
            // Same rule the resolver follows: the key stands in for the
            // element's text unless the element's only translated string is
            // one of the attributes above, in which case there is no text
            // for a message value to become.
            if !names_another || tag_has_attribute(tag, "text") {
                out.insert(key);
            }
        }
        idx = start;
    }
}

/// The markup of the tag whose attribute list holds byte `at`.
///
/// Back to the `<` that opens it and forward to the `>` that closes it,
/// skipping quoted values so a `>` inside one does not end the tag early.
/// The scan stays text-based, so it keeps working on a file that does not
/// stand alone as a document.
fn enclosing_tag(src: &str, at: usize) -> Option<&str> {
    let open = src[..at].rfind('<')?;
    let mut quote: Option<char> = None;
    for (i, c) in src[open + 1..].char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            // The next tag opens before this one closed: not a tag.
            None if c == '<' => return None,
            None if c == '>' => return Some(&src[open..open + i + 2]),
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
    }
    None
}

/// Whether `tag` (one `<...>` slice) writes a `name="..."` attribute.
///
/// The walk skips quoted values, so a name written inside another
/// attribute's text is not one, and the name has to start an attribute
/// rather than end a longer one, so `alt` does not match `data-alt`.
fn tag_has_attribute(tag: &str, name: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut at_boundary = false;
    for (i, c) in tag.char_indices() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                at_boundary = false;
            }
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => at_boundary = true,
            None => {
                if at_boundary
                    && tag[i..].starts_with(name)
                    && tag[i + name.len()..].starts_with('=')
                {
                    return true;
                }
                at_boundary = false;
            }
        }
    }
    false
}

/// Whether the name starting at byte `at` begins a call rather than
/// ending a longer identifier. `t(` must not match the tail of
/// `insert(` or `assert(`, but must match `lumen::t(` and `lumen.t(`,
/// so the test is on the preceding character: anything that cannot
/// continue an identifier starts a fresh name.
fn is_call_boundary(src: &str, at: usize) -> bool {
    src[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
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

/// One catalogue message as the scan found it.
#[derive(Default)]
struct Message {
    /// The element shows text of its own, so the message wants a value.
    value: bool,
    /// The Fluent attributes the element's other strings resolve through.
    attributes: BTreeSet<String>,
}

/// Group flat keys by the message they name, splitting each on the first
/// dot the same way [`attribute_key`] joined it.
fn group_by_message(discovered: &BTreeSet<String>) -> BTreeMap<String, Message> {
    let mut messages: BTreeMap<String, Message> = BTreeMap::new();
    for key in discovered {
        match key.split_once('.') {
            Some((name, attribute)) => {
                messages
                    .entry(name.to_string())
                    .or_default()
                    .attributes
                    .insert(attribute.to_string());
            }
            None => messages.entry(key.clone()).or_default().value = true,
        }
    }
    messages
}

/// Read existing `<target>` (if present), parse out its keys, and write in
/// every key in `discovered` that isn't already covered. The returned
/// `contents` is the merged FTL text.
///
/// A message the file lacks is appended whole. A message it already has
/// gains only the attribute lines it is missing, so a translator's own
/// wording is never rewritten.
fn merge_into_ftl(target: &Path, discovered: &BTreeSet<String>) -> std::io::Result<MergedFtl> {
    let existing = match fs::read_to_string(target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let existing_keys = parse_existing_keys(&existing);
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut edited = 0;
    let mut fresh = String::new();
    let mut appended = 0;
    for (name, message) in group_by_message(discovered) {
        if existing_keys.contains(&name) {
            edited += fill_in_message(&mut lines, &name, &message, &existing_keys);
        } else {
            fresh.push_str(&render_message(&name, &message));
            appended += usize::from(message.value) + message.attributes.len();
        }
    }
    // Nothing landed in the existing text, so it is handed back byte for
    // byte and a re-run over an unchanged app writes an identical file.
    let mut buf = if edited > 0 {
        let mut buf = lines.join("\n");
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf
    } else {
        existing
    };
    if !fresh.is_empty() {
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
        buf.push_str(&fresh);
    }
    Ok(MergedFtl {
        contents: buf,
        added: edited + appended,
    })
}

/// One whole message block, as it is appended to a catalogue.
///
/// Placeholder values match the key itself so untranslated entries still
/// render something sensible in the UI. A message whose element shows no
/// text of its own carries only its attributes, and gets no value.
fn render_message(name: &str, message: &Message) -> String {
    let mut out = String::from("# TODO: translate\n");
    if message.value {
        out.push_str(&format!("{name} = {name}\n"));
    } else {
        out.push_str(&format!("{name} =\n"));
    }
    for attribute in &message.attributes {
        out.push_str(&format!(
            "    .{attribute} = {}\n",
            attribute_key(name, attribute)
        ));
    }
    out.push('\n');
    out
}

/// Write the parts of `message` that the block for `name` in `lines` lacks,
/// returning how many lines that added.
///
/// New attributes go at the end of the block, which is where FTL wants them.
/// The one existing line this rewrites is a value line that is exactly
/// `name =`: the extractor wrote that itself for an element that showed no
/// text, so filling it in once text appears touches nothing a translator
/// typed.
fn fill_in_message(
    lines: &mut Vec<String>,
    name: &str,
    message: &Message,
    existing_keys: &BTreeSet<String>,
) -> usize {
    let Some(start) = lines
        .iter()
        .position(|line| message_name(line) == Some(name))
    else {
        return 0;
    };
    // A blank line, a comment or the next message ends the block; anything
    // else belongs to it, including the `}` closing a multi-line selector.
    let mut end = start;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() || line.starts_with('#') || message_name(line).is_some() {
            break;
        }
        end = i;
    }
    let new_lines: Vec<String> = message
        .attributes
        .iter()
        .map(|attribute| (attribute, attribute_key(name, attribute)))
        .filter(|(_, key)| !existing_keys.contains(key))
        .map(|(attribute, key)| format!("    .{attribute} = {key}"))
        .collect();
    let mut added = new_lines.len();
    lines.splice(end + 1..end + 1, new_lines);
    if message.value && lines[start].trim_end().ends_with('=') {
        lines[start] = format!("{name} = {name}");
        added += 1;
    }
    added
}

/// Tiny FTL key scanner - picks out every key an existing catalogue already
/// covers, message values and message attributes alike. An indented
/// `.name = ...` belongs to the message above it, so it is recorded under
/// the dotted key that resolves it.
fn parse_existing_keys(ftl: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut message: Option<String> = None;
    for line in ftl.lines() {
        let trimmed = line.trim_start();
        // A blank line ends the message block above it.
        if trimmed.is_empty() {
            message = None;
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed != line {
            if let Some(message) = &message
                && let Some(rest) = trimmed.strip_prefix('.')
                && let Some(eq) = rest.find('=')
            {
                let attribute = rest[..eq].trim();
                if is_valid_ftl_key(attribute) {
                    keys.insert(attribute_key(message, attribute));
                }
            }
            continue;
        }
        if let Some(key) = message_name(line) {
            keys.insert(key.to_string());
            message = Some(key.to_string());
        }
    }
    keys
}

/// The message a top-level `key = ...` line declares.
fn message_name(line: &str) -> Option<&str> {
    if line.starts_with('#') || line.trim_start() != line {
        return None;
    }
    let key = line[..line.find('=')?].trim();
    is_valid_ftl_key(key).then_some(key)
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
    fn extract_script_builtin_calls() {
        // Bare `t` / `tr` (rhai, lua) and candela's namespaced form.
        let src = r#"
            let a = t("hello");
            let b = tr("count");
            let c = lumen::t("cdl-key");
            let d = lumen::tr("cdl-alias");
        "#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        for k in ["hello", "count", "cdl-key", "cdl-alias"] {
            assert!(keys.contains(k), "missing {k}");
        }
    }

    #[test]
    fn extract_ignores_names_merely_ending_in_t() {
        let src = r#"
            list.insert("not-a-key");
            assert("also-not");
            print("nope");
        "#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.is_empty(), "picked up {keys:?}");
    }

    #[test]
    fn extract_markup_translatable() {
        let src = r#"<label translatable="app-title">Hello</label>"#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("app-title"));
    }

    #[test]
    fn a_marked_element_collects_the_attributes_it_authors() {
        let src = r#"
            <input placeholder="Search" translatable="search"/>
            <image src="logo.png" alt="The Lumen logo" translatable="logo"/>
            <button text="Save" translatable="save"/>
        "#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("search.placeholder"));
        assert!(keys.contains("logo.alt"));
        assert!(keys.contains("save"));
        // Neither element shows text of its own, so neither wants a value.
        assert!(!keys.contains("search"));
        assert!(!keys.contains("logo"));
        // Nothing is translated into existence.
        assert!(!keys.contains("save.placeholder"));
        assert!(!keys.contains("save.alt"));
    }

    #[test]
    fn an_element_with_both_a_placeholder_and_text_wants_both() {
        let src = r#"<input text="Q" placeholder="Search" translatable="search"/>"#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert!(keys.contains("search"));
        assert!(keys.contains("search.placeholder"));
    }

    #[test]
    fn an_attribute_name_inside_another_value_is_not_an_attribute() {
        let src = r#"<label text="type alt=x here" data-alt="y" translatable="hint"/>"#;
        let mut keys = BTreeSet::new();
        extract_keys_into(src, &mut keys);
        assert_eq!(keys, ["hint".to_string()].into_iter().collect());
    }

    #[test]
    fn merge_writes_a_message_that_carries_only_attributes() {
        let dir = tempdir();
        let target = dir.join("en-US.ftl");
        let mut keys = BTreeSet::new();
        keys.insert("search.placeholder".to_string());
        let merged = merge_into_ftl(&target, &keys).unwrap();
        assert!(merged.contents.contains("search =\n"));
        assert!(
            merged
                .contents
                .contains("    .placeholder = search.placeholder\n")
        );
        assert_eq!(merged.added, 1);
    }

    #[test]
    fn merge_adds_a_missing_attribute_to_a_message_the_file_has() {
        let dir = tempdir();
        let target = dir.join("de-DE.ftl");
        fs::write(&target, "search = Suche\n\nsave = Speichern\n").unwrap();
        let mut keys = BTreeSet::new();
        keys.insert("search".to_string());
        keys.insert("search.placeholder".to_string());
        keys.insert("save".to_string());
        let merged = merge_into_ftl(&target, &keys).unwrap();
        assert_eq!(
            merged.contents,
            "search = Suche\n    .placeholder = search.placeholder\n\nsave = Speichern\n"
        );
        assert_eq!(merged.added, 1);
    }

    #[test]
    fn merge_fills_in_a_value_line_it_left_empty_before() {
        let dir = tempdir();
        let target = dir.join("en-US.ftl");
        fs::write(&target, "search =\n    .placeholder = Search\n").unwrap();
        let mut keys = BTreeSet::new();
        keys.insert("search".to_string());
        keys.insert("search.placeholder".to_string());
        let merged = merge_into_ftl(&target, &keys).unwrap();
        assert_eq!(
            merged.contents,
            "search = search\n    .placeholder = Search\n"
        );
        assert_eq!(merged.added, 1);
    }

    #[test]
    fn a_second_run_over_an_unchanged_app_writes_the_same_bytes() {
        let dir = tempdir();
        fs::write(
            dir.join("main.lmn"),
            "<root>\n             <input placeholder=\"Search\" translatable=\"search\"/>\n\
             <image alt=\"A logo\" translatable=\"logo\"/>\n\
             <button text=\"Save\" translatable=\"save\"/>\n\
             </root>\n",
        )
        .unwrap();
        let mut keys = BTreeSet::new();
        scan_dir(&dir, &mut keys).unwrap();
        let target = dir.join("en-US.ftl");
        let first = merge_into_ftl(&target, &keys).unwrap();
        fs::write(&target, &first.contents).unwrap();
        let second = merge_into_ftl(&target, &keys).unwrap();
        assert_eq!(second.contents, first.contents);
        assert_eq!(second.added, 0);
    }

    #[test]
    fn parse_existing_keys_records_message_attributes() {
        let ftl = "search = Suche\n    .placeholder = Katalog durchsuchen\n\nsave = Speichern\n";
        let keys = parse_existing_keys(ftl);
        assert!(keys.contains("search"));
        assert!(keys.contains("search.placeholder"));
        assert!(keys.contains("save"));
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
        fs::write(&rhai, "let s = t(\"greet\");\n").unwrap();
        // Every script language the runtime hosts is scanned.
        fs::write(dir.join("logic.lua"), "local s = tr(\"lua-key\")\n").unwrap();
        fs::write(dir.join("app.cdl"), "let s = lumen::t(\"cdl-key\");\n").unwrap();
        let mut keys = BTreeSet::new();
        scan_dir(&dir, &mut keys).unwrap();
        for k in ["app-title", "greet", "lua-key", "cdl-key"] {
            assert!(keys.contains(k), "missing {k}");
        }
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
