//! Rhai script intelligence: diagnostics, completion, hover, and
//! signature help for `.rhai` buffers.
//!
//! Diagnostics come from compiling the buffer with a real
//! [`lumen_script_rhai::RhaiHost`] engine - the same engine the runtime
//! uses, with every Lumen builtin registered - so calls to `signal`,
//! `derive`, `on`, timers, etc. never surface as "unknown function"
//! errors. Optimization is disabled before compiling so constant-folding
//! can never execute a builtin (e.g. `read_file`/`write_file`) as a side
//! effect of analysis; only genuine syntax/parse errors are reported.
//!
//! Completion, hover, and signature help are driven by
//! [`lumen_script_rhai::builtins::BUILTINS`], the shared signature table.

use lumen_script_rhai::RhaiHost;
use lumen_script_rhai::builtins::{self, BuiltinFn};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, Documentation,
    InsertTextFormat, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position,
    Range, SignatureHelp, SignatureInformation,
};

/// Compile `src` with a builtin-registered Rhai engine and return any
/// parse error as a single diagnostic. Returns an empty vec on success.
pub fn compute_rhai_diagnostics(src: &str) -> Vec<Diagnostic> {
    let mut host = RhaiHost::new();
    let engine = host.engine_mut();
    // Never run the optimizer during analysis: with builtins registered
    // it could evaluate constant-argument calls (`read_file("x")`) and
    // touch the filesystem. We only want parse errors.
    engine.set_optimization_level(rhai::OptimizationLevel::None);
    match engine.compile(src) {
        Ok(_) => Vec::new(),
        Err(err) => vec![diagnostic_from_rhai(src, &err)],
    }
}

/// Convert a `rhai::ParseError` into an LSP diagnostic, mapping rhai's
/// 1-based line/column position into an LSP range.
fn diagnostic_from_rhai(src: &str, err: &rhai::ParseError) -> Diagnostic {
    let pos = err.position();
    let range = rhai_position_to_range(src, pos);
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("lumen-lsp".into()),
        message: format!("Rhai: {}", err.err_type()),
        ..Default::default()
    }
}

/// Map a `rhai::Position` (1-based line + column, or "none" at EOF) onto
/// an LSP range. When the column is known we highlight a single
/// character; otherwise we fall back to the end of the source.
fn rhai_position_to_range(src: &str, pos: rhai::Position) -> Range {
    match (pos.line(), pos.position()) {
        (Some(line), Some(col)) => {
            let start = Position {
                line: line.saturating_sub(1) as u32,
                character: col.saturating_sub(1) as u32,
            };
            let end = Position {
                line: start.line,
                character: start.character + 1,
            };
            Range { start, end }
        }
        _ => {
            let end = crate::diagnostics::byte_to_position(src, src.len());
            Range { start: end, end }
        }
    }
}

/// Whether the byte at `cursor` sits inside a Rhai identifier we would
/// complete against the builtins list. Returns the partial word typed so
/// far (which may be empty).
fn ident_prefix(src: &str, cursor: usize) -> String {
    let cursor = cursor.min(src.len());
    let bytes = src.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    src[start..cursor].to_string()
}

/// Completion items for a `.rhai` buffer at `cursor`. Offers the Lumen
/// builtins whose names start with the identifier prefix under the
/// cursor. Empty prefix returns the full set.
pub fn completions(src: &str, cursor: usize) -> Vec<CompletionItem> {
    let prefix = ident_prefix(src, cursor);
    builtins::BUILTINS
        .iter()
        .filter(|b| b.name.starts_with(prefix.as_str()))
        .map(builtin_completion_item)
        .collect()
}

fn builtin_completion_item(b: &BuiltinFn) -> CompletionItem {
    CompletionItem {
        label: b.name.to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some(b.signature()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: b.hover_markdown(),
        })),
        insert_text: Some(b.snippet()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    }
}

/// Hover markdown for the builtin whose name is under `cursor`, if any.
pub fn hover(src: &str, cursor: usize) -> Option<String> {
    let name = ident_at(src, cursor)?;
    builtins::lookup(&name).map(|b| b.hover_markdown())
}

/// Expand the identifier token surrounding `cursor` (letters, digits,
/// `_`). Returns `None` when the cursor is not on an identifier.
fn ident_at(src: &str, cursor: usize) -> Option<String> {
    let cursor = cursor.min(src.len());
    let bytes = src.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(src[start..end].to_string())
}

/// Signature help for an in-progress builtin call. Walks back from
/// `cursor` to find the innermost unclosed `name(` and, if `name` is a
/// builtin, returns its signature with the active parameter highlighted.
pub fn signature_help(src: &str, cursor: usize) -> Option<SignatureHelp> {
    let (name, active) = enclosing_call(src, cursor)?;
    let b = builtins::lookup(&name)?;
    let params: Vec<ParameterInformation> = b
        .params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(format!("{}: {}", p.name, p.ty)),
            documentation: None,
        })
        .collect();
    let sig = SignatureInformation {
        label: b.signature(),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: b.doc.to_string(),
        })),
        parameters: Some(params),
        active_parameter: Some(active.min(b.params.len().saturating_sub(1) as u32)),
    };
    Some(SignatureHelp {
        signatures: vec![sig],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// Find the call the cursor is inside: the function name of the nearest
/// unmatched `(` before `cursor`, plus the zero-based index of the
/// argument currently being typed (counting top-level commas). Skips
/// nested parens and string literals.
fn enclosing_call(src: &str, cursor: usize) -> Option<(String, u32)> {
    let cursor = cursor.min(src.len());
    let bytes = &src.as_bytes()[..cursor];
    let mut depth = 0i32;
    let mut commas = 0u32;
    let mut i = cursor;
    let mut in_string = false;
    // Walk backwards. We can't easily know string state from the right,
    // so approximate: count quotes; toggle on each unescaped `"`.
    while i > 0 {
        i -= 1;
        let c = bytes[i];
        if c == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match c {
            b')' | b']' | b'}' => depth += 1,
            b'(' => {
                if depth == 0 {
                    // Found the opening paren of the enclosing call.
                    let name_end = i;
                    let name = ident_before(src, name_end)?;
                    return Some((name, commas));
                }
                depth -= 1;
            }
            b'[' | b'{' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The identifier immediately preceding byte offset `at` (skipping
/// whitespace). Used to name the function owning a `(`.
fn ident_before(src: &str, at: usize) -> Option<String> {
    let bytes = src.as_bytes();
    let mut end = at;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = end;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(src[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_script_has_no_diagnostics() {
        let src = r#"
            fn on_click(id) {
                let c = signal("count", 0);
                c.set(c.get() + 1);
                set_timeout("tick", 500);
            }
        "#;
        assert!(compute_rhai_diagnostics(src).is_empty());
    }

    #[test]
    fn builtins_do_not_error() {
        // Every builtin called with plausible args must compile cleanly -
        // proves the analysis engine has them registered.
        let src = r#"
            fn demo() {
                derive("sum", ["a", "b"], |a, b| a + b);
                on("click", "save", "handle_save");
                notify("hi", "there");
                let x = parse_json("{}");
            }
        "#;
        let d = compute_rhai_diagnostics(src);
        assert!(
            d.is_empty(),
            "unexpected diagnostics: {:?}",
            d.iter().map(|x| &x.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn syntax_error_surfaces_with_range() {
        let src = "fn broken( {\n let x = ;\n}";
        let diags = compute_rhai_diagnostics(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(diags[0].message.starts_with("Rhai:"));
    }

    #[test]
    fn completion_filters_by_prefix() {
        let items = completions("set_t", 5);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"set_timeout"));
        assert!(labels.contains(&"set_text"));
        assert!(!labels.contains(&"notify"));
    }

    #[test]
    fn hover_on_builtin() {
        let src = "notify(\"a\", \"b\");";
        let h = hover(src, 2).unwrap();
        assert!(h.contains("notify"));
        assert!(h.contains("OS notification"));
    }

    #[test]
    fn signature_help_tracks_active_param() {
        let src = "set_timeout(\"tick\", ";
        let help = signature_help(src, src.len()).unwrap();
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(
            help.signatures[0].label,
            "set_timeout(name: string, ms: int) -> ()"
        );
    }

    #[test]
    fn signature_help_first_param() {
        let src = "notify(";
        let help = signature_help(src, src.len()).unwrap();
        assert_eq!(help.active_parameter, Some(0));
    }
}
