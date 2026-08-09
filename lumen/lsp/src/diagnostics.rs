//! Convert `lumenc::ParseError` values into LSP `Diagnostic`s.
//!
//! `lumenc::ParseError` carries either a byte offset (UnknownTag) or
//! just human-readable context (BadAttribute, Xml). We do a best-effort
//! source scan to recover a useful highlight range; if all else fails we
//! point at line 1, column 1 so the diagnostic is still discoverable.

use lumenc::ParseError;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// Translate a `ParseError` into a single LSP `Diagnostic` against the
/// given source text.
pub fn diagnostic_from_error(src: &str, err: &ParseError) -> Diagnostic {
    let (range, message) = match err {
        ParseError::Xml(msg) => (range_for_xml(src, msg), format!("XML parse error: {msg}")),
        ParseError::UnknownTag(name, offset) => (
            range_for_offset_token(src, *offset, name),
            format!("Unknown tag `<{name}>`. See LSP completion list for the full set."),
        ),
        ParseError::BadAttribute {
            name,
            value,
            tag,
            reason,
        } => (
            range_for_attribute(src, tag, name, value),
            format!("Invalid attribute `{name}=\"{value}\"` on `<{tag}>`: {reason}"),
        ),
        ParseError::Include(msg) => (
            // Include/import errors carry their own position text; point at
            // the top of the file as a discoverable fallback.
            Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            msg.clone(),
        ),
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("lumen-lsp".into()),
        message,
        ..Default::default()
    }
}

/// roxmltree XML error messages typically end with `at <row>:<col>`.
/// Parse that out when present.
fn range_for_xml(src: &str, msg: &str) -> Range {
    if let Some((row, col)) = parse_row_col_suffix(msg) {
        let line = row.saturating_sub(1) as u32;
        let character = col.saturating_sub(1) as u32;
        let pos = Position { line, character };
        // Highlight a single character, which is what most XML errors
        // refer to.
        return Range {
            start: pos,
            end: Position {
                line,
                character: character + 1,
            },
        };
    }
    range_full_first_line(src)
}

fn parse_row_col_suffix(msg: &str) -> Option<(usize, usize)> {
    // Look for the last occurrence of "<digits>:<digits>" in the message
    // - roxmltree formats positions as "at 3:5".
    let bytes = msg.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        if bytes[i - 1] == b':'
            && i < bytes.len()
            && bytes[i].is_ascii_digit()
            && i >= 2
            && bytes[i - 2].is_ascii_digit()
        {
            // Walk back to find the start of the row digits.
            let mut start = i - 2;
            while start > 0 && bytes[start - 1].is_ascii_digit() {
                start -= 1;
            }
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let row: usize = msg.get(start..i - 1)?.parse().ok()?;
            let col: usize = msg.get(i..end)?.parse().ok()?;
            return Some((row, col));
        }
        i -= 1;
    }
    None
}

fn range_full_first_line(src: &str) -> Range {
    let first_line_len = src.lines().next().map(|l| l.len()).unwrap_or(0) as u32;
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: first_line_len,
        },
    }
}

/// For `UnknownTag`, `roxmltree` gives the node's byte-offset (start of
/// the element). The tag name follows the leading `<`.
fn range_for_offset_token(src: &str, byte_offset: usize, token: &str) -> Range {
    // The byte offset roxmltree hands us is the position of the `<`
    // (start of the element). Search forward from there for the token,
    // bounded so we don't drift into unrelated text.
    let window_end = (byte_offset + token.len() + 8).min(src.len());
    let window = &src.get(byte_offset..window_end).unwrap_or("");
    if let Some(rel) = window.find(token) {
        let abs = byte_offset + rel;
        return byte_range(src, abs, abs + token.len());
    }
    byte_range(src, byte_offset, byte_offset + 1)
}

/// For `BadAttribute`, scan the source for the first occurrence of
/// `attr="value"` (best effort - there may be more than one but pointing
/// at any of them is better than line 1).
fn range_for_attribute(src: &str, _tag: &str, attr: &str, value: &str) -> Range {
    let needle = format!("{attr}=\"{value}\"");
    if let Some(idx) = src.find(&needle) {
        return byte_range(src, idx, idx + needle.len());
    }
    // Fall back to just the attribute name.
    if let Some(idx) = src.find(attr) {
        return byte_range(src, idx, idx + attr.len());
    }
    range_full_first_line(src)
}

/// Convert a byte-range into an LSP line/character `Range`.
pub fn byte_range(src: &str, start: usize, end: usize) -> Range {
    Range {
        start: byte_to_position(src, start),
        end: byte_to_position(src, end),
    }
}

/// Convert a byte offset within `src` into an LSP `Position`.
pub fn byte_to_position(src: &str, byte: usize) -> Position {
    let byte = byte.min(src.len());
    let mut line = 0u32;
    let mut last_line_start = 0usize;
    for (i, b) in src.as_bytes().iter().enumerate().take(byte) {
        if *b == b'\n' {
            line += 1;
            last_line_start = i + 1;
        }
    }
    // Character is UTF-16 code units per LSP spec. For ASCII source -
    // which Lumen markup is in practice - UTF-8 byte count and UTF-16
    // code-unit count coincide. Handle the common-case correctly for
    // ASCII and degrade gracefully for non-ASCII by counting chars.
    let line_bytes = &src.as_bytes()[last_line_start..byte];
    let character = if line_bytes.is_ascii() {
        line_bytes.len() as u32
    } else {
        // Decode the UTF-8 slice and count UTF-16 code units.
        std::str::from_utf8(line_bytes)
            .map(|s| s.chars().map(|c| c.len_utf16() as u32).sum())
            .unwrap_or(line_bytes.len() as u32)
    };
    Position { line, character }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tag_points_at_tag_name() {
        let src = "<root>\n  <nope/>\n</root>\n";
        let err = lumenc::parse_html(src).unwrap_err();
        let diag = diagnostic_from_error(src, &err);
        // Highlight should sit on line 1 (zero-based), inside the `<nope/>`.
        assert_eq!(diag.range.start.line, 1);
        assert!(diag.message.contains("nope"));
    }

    #[test]
    fn bad_attribute_points_at_attribute() {
        let src = "<root>\n  <tile bg=\"not-hex\"/>\n</root>\n";
        let err = lumenc::parse_html(src).unwrap_err();
        let diag = diagnostic_from_error(src, &err);
        assert_eq!(diag.range.start.line, 1);
        // Match span starts on the `bg=` token.
        let line_text = src.lines().nth(1).unwrap();
        let col = diag.range.start.character as usize;
        assert!(line_text[col..].starts_with("bg=\"not-hex\""));
    }

    #[test]
    fn byte_to_position_handles_ascii() {
        let src = "ab\ncd\nef";
        let p = byte_to_position(src, 4);
        assert_eq!(p.line, 1);
        assert_eq!(p.character, 1);
    }

    #[test]
    fn malformed_xml_produces_diagnostic() {
        let src = "<root><tile></root>\n";
        let err = lumenc::parse_html(src).unwrap_err();
        let diag = diagnostic_from_error(src, &err);
        assert!(diag.message.starts_with("XML parse error"));
    }
}
