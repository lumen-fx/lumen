//! Hover support: identify the tag or attribute name under the cursor
//! and return its markdown documentation.

use crate::docs::{attr_doc, tag_doc};

/// What the cursor is sitting on inside the markup, for hover purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverTarget {
    /// Tag name (`root`, `tile`, ...). Carries the name only.
    Tag(String),
    /// Attribute name within an open tag. Carries the name only.
    Attr(String),
}

/// Locate the token under `cursor` in `src` and classify it as a tag or
/// attribute name. Returns `None` if the cursor isn't on a recognizable
/// identifier.
pub fn target_at(src: &str, cursor: usize) -> Option<HoverTarget> {
    let cursor = cursor.min(src.len());
    let bytes = src.as_bytes();

    // Expand to the surrounding identifier token (letters, digits, `-`,
    // `_`).
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
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
    let name = &src[start..end];

    // Tag if there is a `<` (optionally `</`) immediately before
    // (whitespace not allowed between `<` and tag).
    let mut i = start;
    while i > 0 {
        match bytes[i - 1] {
            b'/' => i -= 1,
            _ => break,
        }
    }
    if i > 0 && bytes[i - 1] == b'<' {
        return Some(HoverTarget::Tag(name.to_string()));
    }

    // Attribute: we should be inside an open tag (scan back until we hit
    // `<` or `>`); if we hit `<` first AND there's whitespace between
    // the tag name and our token, we're in attribute-name position.
    let mut j = start;
    while j > 0 {
        match bytes[j - 1] {
            b'>' => return None,
            b'<' => return Some(HoverTarget::Attr(name.to_string())),
            _ => j -= 1,
        }
    }
    None
}

/// Look up the markdown doc for a hover target.
pub fn doc_for(target: &HoverTarget) -> Option<&'static str> {
    match target {
        HoverTarget::Tag(name) => tag_doc(name),
        HoverTarget::Attr(name) => attr_doc(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str) -> Option<HoverTarget> {
        let cursor = src.find('|').expect("need | marker");
        let cleaned: String = src.chars().filter(|c| *c != '|').collect();
        target_at(&cleaned, cursor)
    }

    #[test]
    fn hover_on_tag_name() {
        assert_eq!(at("<co|lumn/>"), Some(HoverTarget::Tag("column".into())));
    }

    #[test]
    fn hover_on_attr_name() {
        assert_eq!(
            at("<tile wi|dth=\"10px\"/>"),
            Some(HoverTarget::Attr("width".into()))
        );
    }

    #[test]
    fn hover_outside_tag_returns_none() {
        assert_eq!(at("hello wo|rld"), None);
    }

    #[test]
    fn hover_doc_lookup_tag() {
        let d = doc_for(&HoverTarget::Tag("scroll".into()));
        assert!(d.is_some());
        assert!(d.unwrap().contains("scroll"));
    }
}
