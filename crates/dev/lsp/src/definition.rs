//! `textDocument/definition` handler that jumps from a template use site to its `<template name="...">` declaration.
//!
//! - Same-file lookup only.
//! - Locates the tag-name token under the cursor via [`crate::hover::target_at`], scans `src` for `<template name="<tag>"`, and returns the position of the `name="..."` value.

use crate::hover::{HoverTarget, target_at};
use tower_lsp::lsp_types::{Position, Range};

/// Result of a goto-definition lookup. Carries the byte range of the
/// matching `<template name="...">` declaration (inside `src`); the
/// caller converts to LSP ranges via `byte_range_to_lsp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionHit {
    /// Byte offset of the start of the matched template `name=` value.
    pub start: usize,
    /// Byte offset of the end (exclusive).
    pub end: usize,
}

/// Find the `<template name="X">` definition for the tag under
/// `cursor`, if any. Returns `None` when the cursor isn't on a
/// recognised tag name or no template by that name is declared in
/// `src`.
pub fn find_definition(src: &str, cursor: usize) -> Option<DefinitionHit> {
    let target = target_at(src, cursor)?;
    let tag = match target {
        HoverTarget::Tag(name) => name,
        HoverTarget::Attr(_) => return None,
    };
    locate_template(src, &tag)
}

/// Linear scan for `<template name="<tag>"`. Both use-site spellings,
/// `<X />` and `<use template="X" />`, name the same declaration, so a hit
/// on it serves both.
pub fn locate_template(src: &str, tag: &str) -> Option<DefinitionHit> {
    let needle = format!("name=\"{tag}\"");
    let mut search_from = 0usize;
    while let Some(rel) = src[search_from..].find(&needle) {
        let at = search_from + rel;
        // Confirm we're inside a `<template ...>` opening tag by walking
        // back to the most recent `<` and checking the tag name.
        let preceding = &src[..at];
        if let Some(open_at) = preceding.rfind('<') {
            let header = &src[open_at..at];
            if header.starts_with("<template")
                && header
                    .as_bytes()
                    .get(9)
                    .is_some_and(|b| *b == b' ' || *b == b'\t' || *b == b'\n')
            {
                let value_start = at + "name=\"".len();
                let value_end = value_start + tag.len();
                return Some(DefinitionHit {
                    start: value_start,
                    end: value_end,
                });
            }
        }
        search_from = at + needle.len();
    }
    None
}

/// Convert a byte range inside `src` to an LSP [`Range`] in UTF-16
/// units (LSP's wire format).
pub fn byte_range_to_lsp(src: &str, start: usize, end: usize) -> Range {
    Range {
        start: byte_offset_to_position(src, start),
        end: byte_offset_to_position(src, end),
    }
}

fn byte_offset_to_position(src: &str, offset: usize) -> Position {
    let mut line: u32 = 0;
    let mut col_utf16: u32 = 0;
    let mut byte: usize = 0;
    for c in src.chars() {
        if byte >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col_utf16 = 0;
        } else {
            col_utf16 += c.len_utf16() as u32;
        }
        byte += c.len_utf8();
    }
    Position {
        line,
        character: col_utf16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_template_definition_in_same_file() {
        let src = r#"<root>
  <template name="card">
    <column class="card"><slot/></column>
  </template>
  <card id="x" />
</root>"#;
        // Cursor sits on the `<card` tag at the use site.
        let cursor = src.find("<card id").unwrap() + 1;
        let hit = find_definition(src, cursor).expect("definition");
        let value = &src[hit.start..hit.end];
        assert_eq!(value, "card");
    }

    #[test]
    fn returns_none_when_no_template() {
        let src = r##"<root><tile bg="#000"/></root>"##;
        let cursor = src.find("<tile").unwrap() + 1;
        assert!(find_definition(src, cursor).is_none());
    }

    #[test]
    fn returns_none_on_attribute_cursor() {
        let src = r##"<root><template name="x"><tile/></template><x bg="#000"/></root>"##;
        // Cursor inside the `bg` attribute name, not on the tag.
        let cursor = src.find("bg=").unwrap() + 1;
        assert!(find_definition(src, cursor).is_none());
    }
}
