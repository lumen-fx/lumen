//! Text-to-value parsers behind the `parse_json` and `parse_markdown`
//! builtins in [`crate::builtin_fns::builtin_script_fns`].
//!
//! Both hand back a [`ScriptValue`] rather than a string, so a script indexes
//! the result directly instead of parsing text itself; each host converts the
//! result into its own value type the same way it converts any other
//! builtin's return. `parse_json` and `parse_markdown` are each behind their
//! own Cargo feature (`json`, `markdown`) so a build that links no script
//! host carries neither `serde_json` nor `pulldown-cmark`.

#[cfg(any(feature = "json", feature = "markdown"))]
use crate::ScriptValue;

/// Parse `src` as JSON into a [`ScriptValue`]. An object becomes a
/// [`ScriptValue::Map`], an array a [`ScriptValue::Array`], and a scalar
/// keeps its JSON type. Malformed input yields [`ScriptValue::Unit`].
#[cfg(feature = "json")]
#[must_use]
pub fn parse_json(src: &str) -> ScriptValue {
    match serde_json::from_str::<serde_json::Value>(src) {
        Ok(v) => json_value(&v),
        Err(_) => ScriptValue::Unit,
    }
}

/// Recursive [`serde_json::Value`] -> [`ScriptValue`] projection. A JSON
/// number lands as [`ScriptValue::I64`] when it is integral and
/// [`ScriptValue::F64`] otherwise, so `as_int` works on `5` and `as_float` on
/// `5.5`.
#[cfg(feature = "json")]
fn json_value(v: &serde_json::Value) -> ScriptValue {
    match v {
        serde_json::Value::Null => ScriptValue::Unit,
        serde_json::Value::Bool(b) => ScriptValue::Bool(*b),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || ScriptValue::F64(n.as_f64().unwrap_or_default()),
            ScriptValue::I64,
        ),
        serde_json::Value::String(s) => ScriptValue::Str(s.clone()),
        serde_json::Value::Array(items) => {
            ScriptValue::Array(items.iter().map(json_value).collect())
        }
        serde_json::Value::Object(fields) => ScriptValue::Map(
            fields
                .iter()
                .map(|(k, val)| (k.clone(), json_value(val)))
                .collect(),
        ),
    }
}

/// The block kinds `parse_markdown` recognises.
#[cfg(feature = "markdown")]
#[derive(Clone, Copy)]
enum BlockKind {
    Heading(u8),
    Paragraph,
    CodeBlock,
    Item,
}

#[cfg(feature = "markdown")]
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
#[cfg(feature = "markdown")]
fn block(counter: &mut usize, kind: &str, level: i64, text: String, lang: String) -> ScriptValue {
    let id = format!("blk-{counter}");
    *counter += 1;
    ScriptValue::Map(std::collections::HashMap::from([
        ("id".to_owned(), ScriptValue::Str(id)),
        ("kind".to_owned(), ScriptValue::Str(kind.to_owned())),
        ("level".to_owned(), ScriptValue::I64(level)),
        ("text".to_owned(), ScriptValue::Str(text)),
        ("lang".to_owned(), ScriptValue::Str(lang)),
    ]))
}

/// Parse `src` as markdown into a list of block records, one per heading,
/// paragraph, code block, list item, and horizontal rule, in document order.
///
/// Lumen labels render plain text, so inline emphasis keeps its markdown
/// delimiters in the block text rather than being dropped. Links flatten to
/// their text.
#[cfg(feature = "markdown")]
#[must_use]
pub fn parse_markdown(src: &str) -> ScriptValue {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let mut out: Vec<ScriptValue> = Vec::new();
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
    ScriptValue::Array(out)
}

#[cfg(all(test, any(feature = "json", feature = "markdown")))]
mod tests {
    use super::*;

    #[cfg(feature = "json")]
    fn field<'a>(v: &'a ScriptValue, key: &str) -> Option<&'a ScriptValue> {
        match v {
            ScriptValue::Map(m) => m.get(key),
            _ => None,
        }
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_keeps_scalar_types() {
        let v = parse_json(r#"{"n": 5, "f": 2.5, "b": true, "s": "x", "z": null}"#);
        assert_eq!(field(&v, "n"), Some(&ScriptValue::I64(5)));
        assert_eq!(field(&v, "f"), Some(&ScriptValue::F64(2.5)));
        assert_eq!(field(&v, "b"), Some(&ScriptValue::Bool(true)));
        assert_eq!(field(&v, "s"), Some(&ScriptValue::Str("x".to_owned())));
        assert_eq!(field(&v, "z"), Some(&ScriptValue::Unit));
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_malformed_is_unit() {
        assert_eq!(parse_json("{"), ScriptValue::Unit);
    }

    #[cfg(feature = "markdown")]
    fn field_md<'a>(v: &'a ScriptValue, key: &str) -> Option<&'a ScriptValue> {
        match v {
            ScriptValue::Map(m) => m.get(key),
            _ => None,
        }
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn markdown_blocks_carry_kind_level_and_lang() {
        let v = parse_markdown("# Title\n\nBody text\n\n```rust\nfn f() {}\n```\n\n---\n");
        let blocks = match &v {
            ScriptValue::Array(items) => items,
            _ => panic!("array of blocks"),
        };
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            field_md(&blocks[0], "kind"),
            Some(&ScriptValue::Str("h".to_owned()))
        );
        assert_eq!(field_md(&blocks[0], "level"), Some(&ScriptValue::I64(1)));
        assert_eq!(
            field_md(&blocks[1], "kind"),
            Some(&ScriptValue::Str("p".to_owned()))
        );
        assert_eq!(
            field_md(&blocks[2], "lang"),
            Some(&ScriptValue::Str("rust".to_owned()))
        );
        assert_eq!(
            field_md(&blocks[3], "kind"),
            Some(&ScriptValue::Str("hr".to_owned()))
        );
        assert_eq!(
            field_md(&blocks[0], "id"),
            Some(&ScriptValue::Str("blk-0".to_owned()))
        );
    }
}
