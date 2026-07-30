//! Completion logic.
//!
//! We do a lightweight lex of the source up to the cursor to figure out
//! whether we are in:
//!
//! - **Tag position**: just after `<` (e.g. `<co|`).
//! - **Attribute name position**: inside a tag, after whitespace,
//!   before `=`.
//! - **Attribute value position**: inside the quoted value of a known
//!   attribute we can suggest values for.
//!
//! Anything else returns an empty list - better silent than wrong.

use crate::docs::{ATTRS, TAGS, attr_value_completions};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, InsertTextFormat, MarkupContent, MarkupKind,
};

/// Cursor-position classification used by completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// Not somewhere we can usefully complete.
    None,
    /// Cursor is in tag-name position. `prefix` is the partial tag
    /// already typed since the `<`.
    TagName {
        /// Partial tag name typed since the `<`.
        prefix: String,
    },
    /// Cursor is in attribute-name position inside `<tag ...>`.
    AttrName {
        /// Partial attribute name typed since the preceding whitespace.
        prefix: String,
    },
    /// Cursor is inside a quoted attribute value.
    AttrValue {
        /// Owning attribute name.
        attr: String,
        /// Partial value typed since the opening `"`.
        prefix: String,
    },
}

/// Classify the cursor at byte offset `cursor` in `src`.
pub fn classify(src: &str, cursor: usize) -> Context {
    let cursor = cursor.min(src.len());
    let before = &src[..cursor];
    let bytes = before.as_bytes();

    // Are we inside a quoted attribute value? Walk backwards to find an
    // unmatched `"`.
    if let Some(quote_pos) = find_open_quote(bytes) {
        // The `=` should be just before the quote (allowing whitespace).
        if let Some(attr_name) = attr_name_before_equals(before, quote_pos) {
            let prefix = before[quote_pos + 1..].to_string();
            return Context::AttrValue {
                attr: attr_name,
                prefix,
            };
        }
        // In an unattributed quoted string - give up.
        return Context::None;
    }

    // Are we inside an open tag (after `<tag` but before the closing
    // `>` or `/>`)?
    if let Some(lt_pos) = find_open_tag(bytes) {
        // Walk forward from `lt_pos + 1` to extract the tag name.
        let after_lt = &before[lt_pos + 1..];
        // Bail if this is `</` (closing tag - no completions today).
        if after_lt.starts_with('/') {
            return Context::None;
        }
        // Bail on `<!` (comments / declarations).
        if after_lt.starts_with('!') || after_lt.starts_with('?') {
            return Context::None;
        }
        let tag_end = after_lt
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(after_lt.len());
        let in_tag_name = !after_lt.as_bytes()[..tag_end]
            .iter()
            .any(|b| matches!(*b, b' ' | b'\n' | b'\t' | b'\r' | b'>' | b'/'));
        if in_tag_name && tag_end == after_lt.len() {
            // Cursor sits inside the tag name.
            return Context::TagName {
                prefix: after_lt.to_string(),
            };
        }
        // Past the tag name -> attribute name.
        // Extract the partial attribute name (everything since the last
        // whitespace).
        let partial = after_lt
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        // Reject if the partial contains `=` or `"` - that's value land,
        // already handled above.
        if partial.contains('=') || partial.contains('"') {
            return Context::None;
        }
        return Context::AttrName {
            prefix: partial.to_string(),
        };
    }

    Context::None
}

/// Walk backwards through `bytes` looking for an unmatched opening `"`.
/// Returns its index if we are currently inside a quoted string.
fn find_open_quote(bytes: &[u8]) -> Option<usize> {
    // Count `"` since the last `>` or `<` - if odd, we're inside a string.
    let mut last_quote: Option<usize> = None;
    let mut count = 0usize;
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'"' => {
                count += 1;
                if last_quote.is_none() {
                    last_quote = Some(i);
                }
            }
            b'>' | b'<' => break,
            _ => {}
        }
    }
    if count % 2 == 1 { last_quote } else { None }
}

/// Given a source slice and the position of an opening `"`, return the
/// attribute name immediately preceding the `=` (skipping whitespace).
fn attr_name_before_equals(src: &str, quote_pos: usize) -> Option<String> {
    let bytes = src.as_bytes();
    let mut i = quote_pos;
    // Skip whitespace before `"`.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'=' {
        return None;
    }
    i -= 1;
    // Skip whitespace before `=`.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Collect the attribute name backwards.
    let end = i;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    if i == end {
        return None;
    }
    Some(src[i..end].to_string())
}

/// Walk back to find the `<` of the open tag we're currently inside, if
/// any. Returns `None` if a `>` is seen first (we're outside any tag).
fn find_open_tag(bytes: &[u8]) -> Option<usize> {
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'>' => return None,
            b'<' => return Some(i),
            _ => {}
        }
    }
    None
}

/// Build the list of completion items for a classified context.
pub fn items_for(ctx: &Context) -> Vec<CompletionItem> {
    match ctx {
        Context::None => Vec::new(),
        Context::TagName { prefix } => TAGS
            .iter()
            .filter(|t| t.starts_with(prefix.as_str()))
            .map(|t| tag_item(t))
            .collect(),
        Context::AttrName { prefix } => ATTRS
            .iter()
            .filter(|a| a.starts_with(prefix.as_str()))
            .map(|a| attr_item(a))
            .collect(),
        Context::AttrValue { attr, prefix } => attr_value_completions(attr)
            .iter()
            .filter(|v| v.starts_with(prefix.as_str()))
            .map(|v| value_item(v))
            .collect(),
    }
}

fn tag_item(tag: &str) -> CompletionItem {
    CompletionItem {
        label: tag.to_string(),
        kind: Some(CompletionItemKind::CLASS),
        documentation: crate::docs::tag_doc(tag).map(|md| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md.to_string(),
            })
        }),
        insert_text: Some(tag.to_string()),
        insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
        ..Default::default()
    }
}

fn attr_item(attr: &str) -> CompletionItem {
    CompletionItem {
        label: attr.to_string(),
        kind: Some(CompletionItemKind::PROPERTY),
        documentation: crate::docs::attr_doc(attr).map(|md| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md.to_string(),
            })
        }),
        insert_text: Some(format!("{attr}=\"$1\"")),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    }
}

fn value_item(value: &str) -> CompletionItem {
    CompletionItem {
        label: value.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        insert_text: Some(value.to_string()),
        insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at(src: &str) -> Context {
        let cursor = src.find('|').expect("test must contain | cursor marker");
        let cleaned: String = src.chars().filter(|c| *c != '|').collect();
        classify(&cleaned, cursor)
    }

    #[test]
    fn tag_position() {
        let c = ctx_at("<root>\n  <co|");
        match c {
            Context::TagName { prefix } => assert_eq!(prefix, "co"),
            other => panic!("expected TagName, got {other:?}"),
        }
    }

    #[test]
    fn attr_name_position() {
        let c = ctx_at("<root>\n  <tile wi|");
        match c {
            Context::AttrName { prefix } => assert_eq!(prefix, "wi"),
            other => panic!("expected AttrName, got {other:?}"),
        }
    }

    #[test]
    fn attr_name_empty_prefix() {
        let c = ctx_at("<root>\n  <tile |");
        match c {
            Context::AttrName { prefix } => assert_eq!(prefix, ""),
            other => panic!("expected AttrName, got {other:?}"),
        }
    }

    #[test]
    fn attr_value_position_flex() {
        let c = ctx_at("<root flex=\"|");
        match c {
            Context::AttrValue { attr, prefix } => {
                assert_eq!(attr, "flex");
                assert_eq!(prefix, "");
            }
            other => panic!("expected AttrValue, got {other:?}"),
        }
    }

    #[test]
    fn attr_value_position_with_prefix() {
        let c = ctx_at("<root flex=\"ro|");
        match c {
            Context::AttrValue { attr, prefix } => {
                assert_eq!(attr, "flex");
                assert_eq!(prefix, "ro");
            }
            other => panic!("expected AttrValue, got {other:?}"),
        }
    }

    #[test]
    fn outside_any_tag_yields_none() {
        let c = ctx_at("<root>\n  hello | world\n</root>\n");
        assert_eq!(c, Context::None);
    }

    #[test]
    fn tag_items_filter_by_prefix() {
        let items = items_for(&Context::TagName {
            prefix: "co".into(),
        });
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["column"]);
    }

    #[test]
    fn attr_value_items_for_scroll() {
        let items = items_for(&Context::AttrValue {
            attr: "scroll".into(),
            prefix: "".into(),
        });
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["y", "x", "both"]);
    }
}
