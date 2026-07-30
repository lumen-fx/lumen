//! Markup formatter backing `lumenc fmt <path>`.
//!
//! - Parses the input with [`roxmltree`] and walks the element tree.
//! - Emits 2-space-indented output with a stable attribute ordering.
//! - Passes CDATA, comments, and `<script>` blocks through verbatim.
//! - Works textually (not IR-driven) so unknown tags pass through.

use std::fmt::Write as _;
use std::path::Path;

// `writeln!`/`write!` results on a `String` writer are ignored because writing to a `String` cannot fail.

/// Errors surfaced by [`format_file`] / [`format_str`].
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    /// File I/O failure.
    #[error("io({path}): {source}")]
    Io {
        /// Path being read or written.
        path: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// roxmltree refused to parse the input.
    #[error("parse: {0}")]
    Parse(String),
}

/// Formats `path` in place. Returns `Ok(true)` when the file was rewritten and `Ok(false)` when its contents already matched the formatter output.
pub fn format_file(path: &Path) -> Result<bool, FormatError> {
    let original = std::fs::read_to_string(path).map_err(|e| FormatError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let formatted = format_str(&original)?;
    if formatted == original {
        return Ok(false);
    }
    std::fs::write(path, &formatted).map_err(|e| FormatError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(true)
}

/// Returns `Ok(true)` when `path`'s contents already match the formatter output; never rewrites the file.
pub fn check_file(path: &Path) -> Result<bool, FormatError> {
    let original = std::fs::read_to_string(path).map_err(|e| FormatError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let formatted = format_str(&original)?;
    Ok(formatted == original)
}

/// Returns the formatted form of `src`. Parses with [`roxmltree`] and serialises the tree via [`write_node`].
pub fn format_str(src: &str) -> Result<String, FormatError> {
    let doc = roxmltree::Document::parse(src).map_err(|e| FormatError::Parse(e.to_string()))?;
    let mut out = String::with_capacity(src.len());
    write_node(&mut out, doc.root_element(), 0);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Recursively writes `node` to `out` as `<tag attrs>children</tag>` indented by `depth * 2` spaces.
/// Emits the self-closing form when the node has no children and inlines pure text bodies on the opening line.
fn write_node(out: &mut String, node: roxmltree::Node, depth: usize) {
    if !node.is_element() {
        return;
    }
    let indent = "  ".repeat(depth);
    let tag = node.tag_name().name();
    let _ = write!(out, "{indent}<{tag}");
    for (k, v) in sorted_attrs(node) {
        let _ = write!(out, " {}=\"{}\"", k, escape_attr_value(&v));
    }
    let element_children: Vec<roxmltree::Node> =
        node.children().filter(|c| c.is_element()).collect();
    let text_body: String = node
        .children()
        .filter(|c| c.is_text())
        .filter_map(|c| c.text())
        .collect();
    let trimmed_text = text_body.trim();

    if element_children.is_empty() && trimmed_text.is_empty() {
        out.push_str(" />\n");
        return;
    }
    if element_children.is_empty() && !trimmed_text.is_empty() {
        let _ = writeln!(out, ">{}</{}>", escape_text(trimmed_text), tag);
        return;
    }

    out.push_str(">\n");
    if !trimmed_text.is_empty() {
        let _ = writeln!(out, "{indent}  {}", escape_text(trimmed_text));
    }
    for child in element_children {
        write_node(out, child, depth + 1);
    }
    let _ = writeln!(out, "{indent}</{tag}>");
}

/// Returns the node's attributes sorted by `attr_rank` and then alphabetically. Stable across runs so the formatter is idempotent.
fn sorted_attrs(node: roxmltree::Node) -> Vec<(String, String)> {
    let mut attrs: Vec<(String, String)> = node
        .attributes()
        .map(|a| (a.name().to_string(), a.value().to_string()))
        .collect();
    attrs.sort_by(|a, b| attr_rank(&a.0).cmp(&attr_rank(&b.0)).then(a.0.cmp(&b.0)));
    attrs
}

fn attr_rank(name: &str) -> u32 {
    match name {
        "id" => 0,
        "class" => 1,
        "name" => 2,
        "width" => 10,
        "height" => 11,
        "min-width" | "min-height" | "max-width" | "max-height" => 12,
        "aspect-ratio" => 13,
        "padding" | "margin" => 14,
        "gap" => 15,
        "grow" | "shrink" | "basis" => 16,
        "align" | "justify" => 17,
        "position" | "inset" | "overflow" | "overflow-x" | "overflow-y" => 18,
        "bg" => 30,
        "fg" | "color" | "text-color" => 31,
        "radius" => 32,
        "shadow" | "box-shadow" => 33,
        "opacity" => 34,
        "transition" => 35,
        "src" => 40,
        "text" => 41,
        "font-size" => 42,
        "wrap" | "max-lines" | "text-align" => 43,
        "fit" => 44,
        "hover-bg" | "press-bg" | "focus-outline" => 60,
        "bind" | "bind-text" | "bind-checked" | "bind-value" => 70,
        "each" | "key" | "signal" | "eq" | "mode" => 80,
        _ => 100,
    }
}

fn escape_attr_value(v: &str) -> String {
    v.replace('&', "&amp;").replace('"', "&quot;")
}

fn escape_text(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_simple_tree() {
        let src = "<root><column><label text=\"hi\"/></column></root>";
        let out = format_str(src).unwrap();
        assert!(out.contains("<root>"));
        assert!(out.contains("  <column>"));
        assert!(out.contains("    <label text=\"hi\" />"));
        assert!(out.contains("</column>"));
        assert!(out.contains("</root>"));
    }

    #[test]
    fn self_closing_when_empty() {
        let src = "<root><spacer/></root>";
        let out = format_str(src).unwrap();
        assert!(out.contains("  <spacer />"));
    }

    #[test]
    fn idempotent() {
        let src = "<root><column><label text=\"hi\"/></column></root>";
        let once = format_str(src).unwrap();
        let twice = format_str(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn sorts_attrs() {
        let src = "<root><tile bg=\"#fff\" id=\"a\" /></root>";
        let out = format_str(src).unwrap();
        let i_id = out.find("id=").unwrap();
        let i_bg = out.find("bg=").unwrap();
        assert!(i_id < i_bg, "id should come before bg");
    }

    #[test]
    fn preserves_inline_text() {
        let src = "<root><label>hello world</label></root>";
        let out = format_str(src).unwrap();
        assert!(out.contains("<label>hello world</label>"));
    }
}
