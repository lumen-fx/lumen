//! The Lumen candela prelude: a single `import "lumen.cdl";` line pulls in the
//! whole [`CandelaHost`](crate::CandelaHost) builtin surface, so a `.cdl` app never
//! has to hand-write a `host "lumen" { ... }` block.
//!
//! candela's own `import "name.cdl";` reads a file off disk, next to the
//! importing source with a fallback to the embedder's import search path. Lumen
//! resolves the prelude ahead of that: the [`CandelaHost`](crate::CandelaHost)
//! source-preparation step ([`resolve_prelude`]) detects the sentinel import
//! statement and splices the embedded host block in before the source reaches
//! [`candela::Engine::compile`], so candela only ever sees the `host` block.
//!
//! Opt-in is preserved: a source without the import gets no builtins. candela
//! resolves host fns lazily, so such a source still loads; calling
//! `lumen::signal_set(...)` unprepared fails at runtime.

use std::borrow::Cow;

/// The sentinel module id an app imports to pull in the entire Lumen host
/// surface: `import "lumen.cdl";`.
pub const PRELUDE_MODULE: &str = "lumen.cdl";

/// The embedded prelude source: a `host "lumen" { ... }` block declaring every
/// builtin [`CandelaHost`](crate::CandelaHost) registers. Kept in lock-step with
/// [`BUILTINS`](crate::BUILTINS) by the `prelude_declares_every_builtin` test.
pub const PRELUDE_SOURCE: &str = include_str!("../prelude/lumen.cdl");

/// Collapse the human-readable prelude into a single physical line: strip `//`
/// line comments and fold every run of whitespace to one space. Splicing the
/// block onto the import statement's own line then preserves *every following*
/// user line number, so compile diagnostics still point at the right line.
///
/// This is sound because the prelude contains no string literals bar the
/// `"lumen"` namespace (which holds no `//`), and each declaration already ends
/// in its own `;` - joining lines with spaces cannot fuse two declarations.
fn prelude_one_line() -> String {
    let mut out = String::with_capacity(PRELUDE_SOURCE.len());
    for line in PRELUDE_SOURCE.lines() {
        let code = match line.split_once("//") {
            Some((before, _)) => before,
            None => line,
        }
        .trim();
        if code.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(code);
    }
    out
}

/// Whether `line` is exactly the prelude import statement, i.e. (trimmed)
/// `import "lumen.cdl"` with an optional trailing `;`. Only the bare form is
/// recognised - aliasing (`import "..." as x;`) is intentionally unsupported so
/// the injected `lumen::` namespace is the single, predictable access path.
fn is_prelude_import(line: &str) -> bool {
    let stmt = line.trim();
    let stmt = stmt.strip_suffix(';').unwrap_or(stmt).trim_end();
    stmt == "import \"lumen.cdl\""
}

/// If `source` imports the Lumen prelude, replace that statement in place with
/// the embedded `host "lumen" { ... }` block so the app opts into the full
/// builtin surface. Sources without the sentinel are returned untouched
/// (builtins stay opt-in), and no allocation happens in that common case.
#[must_use]
pub fn resolve_prelude(source: &str) -> Cow<'_, str> {
    // Fast path: the sentinel string is absent, so nothing to splice.
    if !source.contains(PRELUDE_MODULE) {
        return Cow::Borrowed(source);
    }

    let block = prelude_one_line();
    let mut out = String::with_capacity(source.len() + block.len());
    let mut replaced = false;

    for line in source.split_inclusive('\n') {
        let (content, newline) = match line.strip_suffix('\n') {
            Some(content) => (content, "\n"),
            None => (line, ""),
        };
        if is_prelude_import(content) {
            out.push_str(&block);
            out.push_str(newline);
            replaced = true;
        } else {
            out.push_str(line);
        }
    }

    // The sentinel appeared only in a comment / string, not as a real import.
    if replaced {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_import_is_borrowed_untouched() {
        let src = "fn main() {}\n";
        assert!(matches!(resolve_prelude(src), Cow::Borrowed(_)));
    }

    #[test]
    fn import_is_spliced_and_line_count_preserved() {
        let src = "import \"lumen.cdl\";\nfn main() {}\n";
        let out = resolve_prelude(src);
        assert!(matches!(out, Cow::Owned(_)));
        assert!(out.contains("host \"lumen\" {"));
        assert!(!out.contains("import"));
        // The splice is single-line, so newline count is unchanged.
        assert_eq!(
            src.matches('\n').count(),
            out.matches('\n').count(),
            "prelude splice must preserve line count"
        );
    }

    #[test]
    fn trailing_semicolon_optional() {
        assert!(is_prelude_import("import \"lumen.cdl\";"));
        assert!(is_prelude_import("  import \"lumen.cdl\"  "));
        assert!(!is_prelude_import("import \"com.other.cdl\";"));
        assert!(!is_prelude_import("// import \"lumen.cdl\";"));
    }
}
