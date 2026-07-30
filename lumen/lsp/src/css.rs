//! CSS diagnostics for `.css` buffers.
//!
//! Two layers, matching `lumenc`'s own pipeline:
//!
//! 1. **Parse errors** - [`lumenc::parse_css`] failing surfaces as an
//!    error diagnostic.
//! 2. **Apply-time warnings** - a successfully parsed stylesheet is run
//!    against a scratch [`LayoutIR`] via [`lumenc::apply_css`], which
//!    performs per-declaration recovery and returns [`lumenc::CssWarning`]
//!    entries for unknown properties / unparseable values. Each becomes a
//!    warning diagnostic pointed at the offending declaration.
//!
//! The scratch IR is parsed from the sibling markup when available so
//! selector matching is realistic; otherwise an empty `<root>` is used
//! (warnings that don't depend on matching - unknown property names,
//! malformed values - still surface, because `apply_css` reports them
//! against the root's own declarations).

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::diagnostics::{byte_to_position, diagnostic_from_error};

/// Compute diagnostics for a CSS buffer. `markup_src`, when supplied, is
/// the sibling `.lmn` source used to build a realistic scratch tree.
pub fn compute_css_diagnostics(css_src: &str, markup_src: Option<&str>) -> Vec<Diagnostic> {
    let sheet = match lumenc::parse_css(css_src) {
        Ok(s) => s,
        Err(e) => return vec![diagnostic_from_error(css_src, &e)],
    };

    let mut ir = match markup_src.and_then(|m| lumenc::parse_html(m).ok()) {
        Some(ir) => ir,
        None => match lumenc::parse_html("<root></root>") {
            Ok(ir) => ir,
            // Should never happen; degrade to no apply-time warnings.
            Err(_) => return Vec::new(),
        },
    };

    match lumenc::apply_css(&mut ir, &sheet) {
        Ok(warnings) => warnings
            .iter()
            .map(|w| warning_to_diagnostic(css_src, w))
            .collect(),
        // A hard apply error (rare - recovery is per-declaration) maps to
        // a single error diagnostic.
        Err(e) => vec![diagnostic_from_error(css_src, &e)],
    }
}

fn warning_to_diagnostic(css_src: &str, w: &lumenc::CssWarning) -> Diagnostic {
    Diagnostic {
        range: declaration_range(css_src, &w.property),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("lumen-lsp".into()),
        message: format!("CSS `{}` in `{}`: {}", w.property, w.selector, w.message),
        ..Default::default()
    }
}

/// Best-effort range for the declaration named `property`: the first
/// `property` token immediately followed (after optional whitespace) by
/// `:`. Falls back to the top of the file when not found.
fn declaration_range(css_src: &str, property: &str) -> Range {
    let mut from = 0usize;
    while let Some(rel) = css_src[from..].find(property) {
        let at = from + rel;
        let after = at + property.len();
        // Preceding byte must not be part of a longer identifier.
        let prev_ok = at == 0
            || !css_src.as_bytes()[at - 1].is_ascii_alphanumeric()
                && css_src.as_bytes()[at - 1] != b'-';
        // Next non-space byte must be `:`.
        let tail = css_src[after..].trim_start();
        if prev_ok && tail.starts_with(':') {
            return Range {
                start: byte_to_position(css_src, at),
                end: byte_to_position(css_src, after),
            };
        }
        from = after;
    }
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_css_has_no_diagnostics() {
        let css = ".card { background: #ff0000; }";
        let diags = compute_css_diagnostics(css, None);
        assert!(diags.is_empty(), "unexpected: {diags:?}");
    }

    #[test]
    fn unknown_property_warns() {
        // `notaproperty` is not a recognised property; apply_css should
        // report a per-declaration warning against the matched element.
        let markup = r#"<root><tile class="card"/></root>"#;
        let css = ".card { notaproperty: 10px; }";
        let diags = compute_css_diagnostics(css, Some(markup));
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(diags[0].message.contains("notaproperty"));
        // Range should sit on the property token, not line 0 col 0.
        let start = diags[0].range.start;
        let line = css.lines().nth(start.line as usize).unwrap();
        assert!(line[start.character as usize..].starts_with("notaproperty"));
    }

    #[test]
    fn parse_error_surfaces() {
        // Unterminated block - parser should reject.
        let css = ".card { background: ";
        let diags = compute_css_diagnostics(css, None);
        assert!(!diags.is_empty());
    }
}
