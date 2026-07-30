//! Cross-file intelligence over a Lumen project's `.lmn` / `.css` /
//! `.rhai` sources.
//!
//! Everything here is pure text analysis operating on byte offsets, so
//! it is unit-testable without an LSP client. The server layer
//! ([`crate::server`]) owns file IO + the project model and converts the
//! byte spans returned here into LSP ranges.
//!
//! Features backed by this module:
//!
//! - **Document symbols** - the `.lmn` element tree and `.rhai` function
//!   list.
//! - **Id completion** - inside `on("click", "<id>", ...)` (and the other
//!   id-taking builtins) in `.rhai`, offer the ids declared in the
//!   sibling markup.
//! - **Goto-definition** - from an id string in `.rhai` to the `<... id="X">`
//!   element in the markup.
//! - **References + rename** - every `id="X"` (markup), `"X"` string
//!   literal (rhai), and `#X` selector (css) that names the same id.

/// A named span of source, given as UTF-8 byte offsets `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    /// The identifier / name the span covers.
    pub name: String,
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

/// A node in a document-symbol tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymNode {
    /// Display name (tag, optionally with `#id`).
    pub name: String,
    /// Kind tag: `"element"` or `"function"`.
    pub kind: &'static str,
    /// Byte span of the whole construct.
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
    /// Nested children (markup elements).
    pub children: Vec<SymNode>,
}

// -- Id extraction ------------------------------------------------------

/// Extract every `id="X"` declaration in the markup, returning the span
/// of the id *value* (excluding the quotes).
pub fn markup_id_defs(markup: &str) -> Vec<TextSpan> {
    let mut out = Vec::new();
    let bytes = markup.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = markup[i..].find("id") {
        let at = i + rel;
        i = at + 2;
        // `id` must be a standalone attribute name: preceded by
        // whitespace / `<`, followed by optional ws then `=`.
        let prev_ok = at == 0 || matches!(bytes[at - 1], b' ' | b'\t' | b'\n' | b'\r' | b'<');
        if !prev_ok {
            continue;
        }
        let mut j = at + 2;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'"' {
            continue;
        }
        let val_start = j + 1;
        let Some(close_rel) = markup[val_start..].find('"') else {
            break;
        };
        let val_end = val_start + close_rel;
        out.push(TextSpan {
            name: markup[val_start..val_end].to_string(),
            start: val_start,
            end: val_end,
        });
        i = val_end + 1;
    }
    out
}

/// All distinct ids declared in the markup, in first-seen order.
pub fn markup_ids(markup: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    markup_id_defs(markup)
        .into_iter()
        .filter(|s| seen.insert(s.name.clone()))
        .map(|s| s.name)
        .collect()
}

// -- Document symbols ---------------------------------------------------

/// Build a nested document-symbol tree for markup via a lightweight tag
/// walk (no IR needed, so we keep real source ranges). Self-closing and
/// unbalanced tags degrade gracefully.
pub fn markup_document_symbols(markup: &str) -> Vec<SymNode> {
    let bytes = markup.as_bytes();
    let mut roots: Vec<SymNode> = Vec::new();
    let mut stack: Vec<SymNode> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Skip comments / declarations.
        if markup[i..].starts_with("<!") || markup[i..].starts_with("<?") {
            if let Some(rel) = markup[i..].find('>') {
                i += rel + 1;
            } else {
                break;
            }
            continue;
        }
        let closing = markup[i..].starts_with("</");
        let name_start = if closing { i + 2 } else { i + 1 };
        let mut k = name_start;
        while k < bytes.len()
            && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'-' || bytes[k] == b'_')
        {
            k += 1;
        }
        if k == name_start {
            i += 1;
            continue;
        }
        let tag = markup[name_start..k].to_string();
        // Find the end of this tag `>`.
        let Some(gt_rel) = markup[k..].find('>') else {
            break;
        };
        let gt = k + gt_rel;
        let self_closing = gt > 0 && bytes[gt - 1] == b'/';
        let tag_span = &markup[i..gt + 1];

        if closing {
            if let Some(mut node) = stack.pop() {
                node.end = gt + 1;
                push_node(&mut roots, &mut stack, node);
            }
            i = gt + 1;
            continue;
        }

        // Opening (or self-closing) tag. Compose a label with #id if present.
        let label = match attr_value(tag_span, "id") {
            Some(id) => format!("{tag}#{id}"),
            None => tag,
        };
        let node = SymNode {
            name: label,
            kind: "element",
            start: i,
            end: gt + 1,
            children: Vec::new(),
        };
        if self_closing || is_void_tag(&node.name) {
            push_node(&mut roots, &mut stack, node);
        } else {
            stack.push(node);
        }
        i = gt + 1;
    }
    // Any unclosed tags flush as-is.
    while let Some(node) = stack.pop() {
        push_node(&mut roots, &mut stack, node);
    }
    roots
}

fn is_void_tag(_label: &str) -> bool {
    false
}

/// Attach `node` to the current parent on the stack, or to the roots.
fn push_node(roots: &mut Vec<SymNode>, stack: &mut [SymNode], node: SymNode) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

/// Read the value of attribute `attr` from a single tag string like
/// `<tile id="foo" bg="#000">`.
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let mut from = 0usize;
    while let Some(rel) = tag[from..].find(&needle) {
        let at = from + rel;
        let prev_ok =
            at == 0 || matches!(tag.as_bytes()[at - 1], b' ' | b'\t' | b'\n' | b'\r' | b'<');
        let vstart = at + needle.len();
        if prev_ok {
            let close = tag[vstart..].find('"')?;
            return Some(tag[vstart..vstart + close].to_string());
        }
        from = vstart;
    }
    None
}

/// Function symbols for a `.rhai` source: every `fn NAME(` declaration,
/// spanning the `NAME` token.
pub fn rhai_function_symbols(rhai: &str) -> Vec<SymNode> {
    let mut out = Vec::new();
    let bytes = rhai.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = rhai[i..].find("fn") {
        let at = i + rel;
        i = at + 2;
        // `fn` must be a keyword: boundary before, whitespace after.
        let prev_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        if !prev_ok || at + 2 >= bytes.len() || !bytes[at + 2].is_ascii_whitespace() {
            continue;
        }
        let mut j = at + 2;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && is_ident_byte(bytes[j]) {
            j += 1;
        }
        if j == name_start {
            continue;
        }
        out.push(SymNode {
            name: rhai[name_start..j].to_string(),
            kind: "function",
            start: name_start,
            end: j,
            children: Vec::new(),
        });
        i = j;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// -- Rhai string / call analysis ----------------------------------------

/// Context of the cursor inside a `.rhai` buffer for id-aware features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhaiStringContext {
    /// Name of the enclosing call, if the string is a call argument.
    pub call: Option<String>,
    /// Zero-based argument index within that call.
    pub arg_index: usize,
    /// The full string literal value (cursor's string).
    pub value: String,
    /// Byte span of the string *contents* (excluding quotes).
    pub value_start: usize,
    /// End byte offset (exclusive) of the string contents.
    pub value_end: usize,
}

/// If `cursor` sits inside a double-quoted string literal, analyse it:
/// the enclosing call name + arg index (for id completion / goto-def) and
/// the full literal value. Returns `None` outside any string.
pub fn rhai_string_context(rhai: &str, cursor: usize) -> Option<RhaiStringContext> {
    let cursor = cursor.min(rhai.len());
    let bytes = rhai.as_bytes();

    // Forward scan tracking string state + a call stack.
    let mut in_string = false;
    let mut escape = false;
    let mut cur_string_start = 0usize;
    let mut stack: Vec<(String, usize)> = Vec::new(); // (fn name, arg index)
    let mut last_ident: Option<(usize, usize)> = None; // span of last identifier

    let mut i = 0usize;
    while i < cursor {
        let c = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_string = true;
                cur_string_start = i + 1;
            }
            b'(' => {
                let name = last_ident
                    .map(|(s, e)| rhai[s..e].to_string())
                    .unwrap_or_default();
                stack.push((name, 0));
            }
            b')' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
            }
            b';' | b'{' | b'}' => {
                // Statement / block boundary - a stray call name before a
                // block isn't an argument context.
                stack.clear();
            }
            _ => {}
        }
        // Track identifier tokens so `(` can name its callee.
        if is_ident_byte(c) {
            let start = last_ident
                .filter(|(_, e)| *e == i)
                .map(|(s, _)| s)
                .unwrap_or(i);
            last_ident = Some((start, i + 1));
        } else if !c.is_ascii_whitespace() {
            last_ident = None;
        }
        i += 1;
    }

    if !in_string {
        return None;
    }
    // Find the closing quote to recover the whole value.
    let mut end = cursor;
    let mut esc = escape;
    while end < bytes.len() {
        let c = bytes[end];
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'"' {
            break;
        }
        end += 1;
    }
    let (call, arg_index) = stack
        .last()
        .map(|(n, a)| (Some(n.clone()).filter(|s| !s.is_empty()), *a))
        .unwrap_or((None, 0));
    Some(RhaiStringContext {
        call,
        arg_index,
        value: rhai[cur_string_start..end].to_string(),
        value_start: cur_string_start,
        value_end: end,
    })
}

/// Whether `(call, arg_index)` names an element id, and so should offer
/// id completion / goto-definition.
pub fn is_id_argument(call: &str, arg_index: usize) -> bool {
    matches!(
        (call, arg_index),
        ("on", 1)
            | ("set_text", 0)
            | ("set_src", 0)
            | ("set_class", 0)
            | ("is_valid", 0)
            | ("open_menu", 0)
            | ("close_menu", 0)
            | ("unregister_tray", 0)
    )
}

// -- References / rename ------------------------------------------------

/// Byte spans of every double-quoted string literal in `rhai` whose
/// contents equal `id`.
pub fn rhai_string_literal_spans(rhai: &str, id: &str) -> Vec<TextSpan> {
    let mut out = Vec::new();
    let bytes = rhai.as_bytes();
    let mut i = 0usize;
    let mut escape = false;
    let mut in_string = false;
    let mut start = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                let content = &rhai[start..i];
                if content == id {
                    out.push(TextSpan {
                        name: id.to_string(),
                        start,
                        end: i,
                    });
                }
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
            start = i + 1;
        }
        i += 1;
    }
    out
}

/// Byte spans of every `#id` selector in `css` whose identifier equals
/// `id`. The span covers the identifier only (not the `#`).
pub fn css_id_selector_spans(css: &str, id: &str) -> Vec<TextSpan> {
    let mut out = Vec::new();
    let needle = format!("#{id}");
    let bytes = css.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = css[from..].find(&needle) {
        let at = from + rel;
        let after = at + needle.len();
        // Boundary: next byte must not extend the identifier.
        let boundary =
            after >= bytes.len() || !(is_ident_byte(bytes[after]) || bytes[after] == b'-');
        if boundary {
            out.push(TextSpan {
                name: id.to_string(),
                start: at + 1, // skip '#'
                end: after,
            });
        }
        from = after;
    }
    out
}

/// Markup id-definition spans whose value equals `id`.
pub fn markup_id_spans(markup: &str, id: &str) -> Vec<TextSpan> {
    markup_id_defs(markup)
        .into_iter()
        .filter(|s| s.name == id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_markup_ids() {
        let m = r#"<root><tile id="save"/><label id="title">Hi</label></root>"#;
        assert_eq!(markup_ids(m), vec!["save", "title"]);
    }

    #[test]
    fn skips_non_id_attrs() {
        // `grid` contains "id" but is not the `id` attribute.
        let m = r#"<root><tile grid="a" valid="x"/></root>"#;
        assert!(markup_ids(m).is_empty());
    }

    #[test]
    fn document_symbols_nest() {
        let m = "<root>\n  <column>\n    <tile id=\"a\"/>\n  </column>\n</root>";
        let syms = markup_document_symbols(m);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "root");
        assert_eq!(syms[0].children[0].name, "column");
        assert_eq!(syms[0].children[0].children[0].name, "tile#a");
    }

    #[test]
    fn rhai_functions_found() {
        let r = "fn on_click(id) {}\nfn tick(dt) {}\n";
        let syms = rhai_function_symbols(r);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["on_click", "tick"]);
    }

    #[test]
    fn on_call_id_context_detected() {
        let r = "fn f() { on(\"click\", \"sa";
        let ctx = rhai_string_context(r, r.len()).unwrap();
        assert_eq!(ctx.call.as_deref(), Some("on"));
        assert_eq!(ctx.arg_index, 1);
        assert!(is_id_argument(ctx.call.as_deref().unwrap(), ctx.arg_index));
    }

    #[test]
    fn event_arg_is_not_id_context() {
        let r = "fn f() { on(\"cl";
        let ctx = rhai_string_context(r, r.len()).unwrap();
        assert_eq!(ctx.call.as_deref(), Some("on"));
        assert_eq!(ctx.arg_index, 0);
        assert!(!is_id_argument("on", 0));
    }

    #[test]
    fn string_context_recovers_full_value() {
        let r = "set_text(\"title\", \"hello\")";
        // Cursor inside the "title" literal.
        let ctx = rhai_string_context(r, 12).unwrap();
        assert_eq!(ctx.value, "title");
        assert_eq!(ctx.call.as_deref(), Some("set_text"));
        assert_eq!(ctx.arg_index, 0);
    }

    #[test]
    fn rename_spans_across_files() {
        let markup = r#"<root><tile id="save"/></root>"#;
        let rhai = r#"fn go() { on("click", "save", "handle"); set_text("save", "x"); }"#;
        let css = "#save { background: #fff; } #saved { color: #000; }";

        let m = markup_id_spans(markup, "save");
        assert_eq!(m.len(), 1);
        assert_eq!(&markup[m[0].start..m[0].end], "save");

        let r = rhai_string_literal_spans(rhai, "save");
        assert_eq!(r.len(), 2);
        for s in &r {
            assert_eq!(&rhai[s.start..s.end], "save");
        }

        // `#saved` must NOT match `#save`.
        let c = css_id_selector_spans(css, "save");
        assert_eq!(c.len(), 1);
        assert_eq!(&css[c[0].start..c[0].end], "save");
    }
}
