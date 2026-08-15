//! Text-to-value parsers behind the `parse_json` and `parse_markdown`
//! builtins.
//!
//! Both hand back a candela [`Value`] rather than a string, so a
//! script indexes the result directly. They are registered variadically: a
//! fixed host-fn signature must name one concrete return type, and neither
//! result has one (JSON is a map, an array, or a scalar; a markdown block mixes
//! string and int fields).
//!
//! Map keys built here cross the boundary through candela's `marshal_value`,
//! which interns them into the string pool, so `block.get("level")` resolves
//! whatever the key's length. candela's own `json_parse` skips that interning,
//! which is why `parse_json` parses host-side instead of delegating.

use std::collections::BTreeMap;

use candela_vm::Value;

/// Parse `src` as JSON into a candela value. Objects become maps, arrays become
/// arrays, and scalars keep their JSON type. Malformed input yields null.
#[must_use]
pub fn json(src: &str) -> Value {
    match serde_json::from_str::<serde_json::Value>(src) {
        Ok(v) => json_value(&v),
        Err(_) => Value::Null,
    }
}

/// Recursive [`serde_json::Value`] -> [`Value`] projection. A JSON
/// number lands as an int when it is integral and as a float otherwise, so
/// `as_int` works on `5` and `as_float` on `5.5`.
fn json_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map_or_else(|| Value::Float(n.as_f64().unwrap_or_default()), Value::Int),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(json_value).collect()),
        serde_json::Value::Object(fields) => Value::Map(
            fields
                .iter()
                .map(|(k, val)| (k.clone(), json_value(val)))
                .collect(),
        ),
    }
}

/// The block kinds `parse_markdown` recognises.
#[derive(Clone, Copy)]
enum BlockKind {
    Heading(u8),
    Paragraph,
    CodeBlock,
    Item,
}

impl BlockKind {
    /// The `kind` field value a block of this shape carries.
    const fn tag(self) -> &'static str {
        match self {
            Self::Heading(_) => "h",
            Self::Paragraph => "p",
            Self::CodeBlock => "code",
            Self::Item => "li",
        }
    }

    /// The `level` field value: the heading depth, `0` for everything else.
    const fn level(self) -> i64 {
        match self {
            Self::Heading(level) => level as i64,
            _ => 0,
        }
    }
}

/// Build one block record: `{ id, kind, level, text, lang }`.
fn block(counter: &mut usize, kind: &str, level: i64, text: String, lang: String) -> Value {
    let id = format!("blk-{counter}");
    *counter += 1;
    Value::Map(BTreeMap::from([
        ("id".to_owned(), Value::String(id)),
        ("kind".to_owned(), Value::String(kind.to_owned())),
        ("level".to_owned(), Value::Int(level)),
        ("text".to_owned(), Value::String(text)),
        ("lang".to_owned(), Value::String(lang)),
    ]))
}

/// Parse `src` as markdown into a list of block records, one per heading,
/// paragraph, code block, list item, and horizontal rule, in document order.
///
/// Lumen labels render plain text, so inline emphasis keeps its markdown
/// delimiters in the block text rather than being dropped. Links flatten to
/// their text.
#[must_use]
pub fn markdown(src: &str) -> Value {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let mut out: Vec<Value> = Vec::new();
    let mut counter: usize = 0;
    let mut cur_kind: Option<BlockKind> = None;
    let mut cur_text = String::new();
    let mut cur_lang = String::new();

    for ev in Parser::new(src) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                cur_kind = Some(BlockKind::Heading(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                }));
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::Paragraph) => {
                cur_kind = Some(BlockKind::Paragraph);
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                cur_kind = Some(BlockKind::CodeBlock);
                cur_text.clear();
                cur_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
            }
            Event::Start(Tag::Item) => {
                cur_kind = Some(BlockKind::Item);
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::Emphasis) | Event::End(TagEnd::Emphasis) => cur_text.push('*'),
            Event::Start(Tag::Strong) | Event::End(TagEnd::Strong) => cur_text.push_str("**"),
            Event::Start(Tag::Strikethrough) | Event::End(TagEnd::Strikethrough) => {
                cur_text.push('~');
            }
            Event::End(
                TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::CodeBlock | TagEnd::Item,
            ) => {
                if let Some(kind) = cur_kind.take() {
                    out.push(block(
                        &mut counter,
                        kind.tag(),
                        kind.level(),
                        std::mem::take(&mut cur_text),
                        std::mem::take(&mut cur_lang),
                    ));
                }
            }
            Event::Text(t) => cur_text.push_str(&t),
            Event::Code(t) => {
                cur_text.push('`');
                cur_text.push_str(&t);
                cur_text.push('`');
            }
            Event::SoftBreak => cur_text.push(' '),
            Event::HardBreak => cur_text.push('\n'),
            Event::Rule => {
                out.push(block(&mut counter, "hr", 0, String::new(), String::new()));
            }
            _ => {}
        }
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        v.as_map().and_then(|m| m.get(key))
    }

    #[test]
    fn json_keeps_scalar_types() {
        let v = json(r#"{"n": 5, "f": 2.5, "b": true, "s": "x", "z": null}"#);
        assert_eq!(field(&v, "n"), Some(&Value::Int(5)));
        assert_eq!(field(&v, "f"), Some(&Value::Float(2.5)));
        assert_eq!(field(&v, "b"), Some(&Value::Bool(true)));
        assert_eq!(field(&v, "s"), Some(&Value::String("x".to_owned())));
        assert_eq!(field(&v, "z"), Some(&Value::Null));
    }

    #[test]
    fn json_malformed_is_null() {
        assert_eq!(json("{"), Value::Null);
    }

    #[test]
    fn markdown_blocks_carry_kind_level_and_lang() {
        let v = markdown("# Title\n\nBody text\n\n```rust\nfn f() {}\n```\n\n---\n");
        let blocks = v.as_array().expect("array of blocks");
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            field(&blocks[0], "kind"),
            Some(&Value::String("h".to_owned()))
        );
        assert_eq!(field(&blocks[0], "level"), Some(&Value::Int(1)));
        assert_eq!(
            field(&blocks[1], "kind"),
            Some(&Value::String("p".to_owned()))
        );
        assert_eq!(
            field(&blocks[2], "lang"),
            Some(&Value::String("rust".to_owned()))
        );
        assert_eq!(
            field(&blocks[3], "kind"),
            Some(&Value::String("hr".to_owned()))
        );
        assert_eq!(
            field(&blocks[0], "id"),
            Some(&Value::String("blk-0".to_owned()))
        );
    }
}
