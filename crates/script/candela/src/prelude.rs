//! The Lumen candela prelude: a single `import "lumen.cdl";` line pulls in the
//! whole Lumen builtin surface, so a `.cdl` app never has to hand-write a
//! `host "lumen" { ... }` block.
//!
//! candela's own `import "name.cdl";` reads a file off disk, next to the
//! importing source with a fallback to the embedder's import search path. Lumen
//! resolves the prelude ahead of that: [`resolve_prelude`] detects the sentinel
//! import statement and splices the embedded host block in before the source
//! reaches the compiler, so candela only ever sees the `host` block. Every path
//! that compiles Lumen candela goes through it, so a program built to a `.cdlb`
//! declares exactly what a program compiled in process does.
//!
//! Opt-in is preserved: a source without the import gets no builtins. candela
//! resolves host fns lazily, so such a source still loads; calling
//! `lumen::signal_set(...)` unprepared fails at runtime.

use std::borrow::Cow;

use lumen_core::warn_line;

/// The sentinel module id an app imports to pull in the entire Lumen host
/// surface: `import "lumen.cdl";`.
pub const PRELUDE_MODULE: &str = "lumen.cdl";

/// The embedded prelude source: the generated declarations that bind the whole
/// Lumen host surface, followed by the hand-written method sugar over them.
///
/// The generated half is kept current by the `prelude_generated` test; the
/// sugar half is edited by hand.
pub const PRELUDE_SOURCE: &str = concat!(
    include_str!("../prelude/declarations.cdl"),
    include_str!("../prelude/wrappers.cdl")
);

/// Write the generated half of the prelude from the shared builtin table and
/// the declarations beside the host's own registrations.
///
/// The `prelude_generated` test compares this against the checked-in file and
/// refreshes it on request.
#[must_use]
pub fn generate_declarations() -> String {
    crate::declare::generated_prelude()
}

/// A source with everything candela needs in front of it, and what that cost
/// the line numbers.
///
/// Two things go ahead of the author's own text: a `host "<ns>" { .. }` block
/// for each namespace an embedder registered functions under, and any `.cdl`
/// wrapper a plugin ships with them. Both shift the source down, so the offset
/// travels with the text and every diagnostic subtracts it again.
#[derive(Debug, Clone, Default)]
pub struct PreparedSource {
    /// What the compiler is handed.
    pub text: String,
    /// How many lines were put ahead of the author's first line.
    pub line_offset: u32,
    /// Where each plugin wrapper landed, so an error inside one names the
    /// plugin instead of a line the author never wrote.
    pub wrappers: Vec<WrapperSpan>,
}

/// The lines one plugin's wrapper source occupies in a [`PreparedSource`].
#[derive(Debug, Clone)]
pub struct WrapperSpan {
    /// The namespace the wrapper belongs to.
    pub ns: String,
    /// First line of the wrapper, 1-based.
    pub first_line: u32,
    /// Last line of the wrapper, 1-based.
    pub last_line: u32,
}

/// Where a byte offset in a [`PreparedSource`] falls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Line in the author's source, or in the wrapper when `wrapper` is set.
    pub line: u32,
    /// Column, 1-based.
    pub col: u32,
    /// The plugin namespace whose wrapper this position is inside.
    pub wrapper: Option<String>,
}

impl PreparedSource {
    /// A source with nothing put in front of it.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            line_offset: 0,
            wrappers: Vec::new(),
        }
    }

    /// Resolve a byte offset in [`Self::text`] to the position an author can
    /// act on.
    #[must_use]
    pub fn locate(&self, byte: usize) -> Location {
        let (line, col) = line_col(&self.text, byte);
        if line == 0 || line > self.line_offset {
            return Location {
                line: line.saturating_sub(self.line_offset),
                col,
                wrapper: None,
            };
        }
        match self
            .wrappers
            .iter()
            .find(|w| line >= w.first_line && line <= w.last_line)
        {
            Some(w) => Location {
                line: line - w.first_line + 1,
                col,
                wrapper: Some(w.ns.clone()),
            },
            None => Location {
                line,
                col,
                wrapper: None,
            },
        }
    }
}

/// Byte offset to `(line, col)`, both 1-based. `(0, 0)` for the
/// unknown-position sentinel candela uses when it has no span.
#[must_use]
pub fn line_col(source: &str, byte: usize) -> (u32, u32) {
    if byte == 0 {
        return (0, 0);
    }
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in source.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Put the declarations and wrappers an embedder registered in front of
/// `source`, after splicing the prelude into it.
///
/// `blocks` are `(namespace, declaration block)` pairs, each already folded
/// onto one line; `wrappers` are `(namespace, source)` pairs written by the
/// plugin that registered the namespace.
///
/// A namespace already declared is skipped: the check is a text search for
/// `host "<ns>"`, which is what a hand-written block looks like, so a source
/// carrying its own declarations compiles exactly as it did before and an app
/// built for the artifact path keeps working.
///
/// The search runs over the source with the prelude already spliced in, so the
/// namespaces the runtime owns (`window`, `document`, `history`) count as
/// declared for an app that imports it. A second block for one of those would
/// displace the prelude's, and every runtime function in it would fail to
/// resolve at run time while the program still compiled.
#[must_use]
pub fn prepare(
    source: &str,
    blocks: &[(String, String)],
    wrappers: &[(String, String)],
) -> PreparedSource {
    let resolved = resolve_prelude(source);
    if blocks.is_empty() && wrappers.is_empty() {
        return PreparedSource::plain(resolved.into_owned());
    }

    let mut prefix = String::new();
    let mut line = 0u32;
    let mut spans = Vec::new();
    for (ns, block) in blocks {
        if declares_namespace(&resolved, ns) {
            // Declared by the author is the supported case and says nothing.
            // Declared only once the prelude is in means an embedder took a
            // namespace the runtime owns, and its functions are unreachable.
            if !declares_namespace(source, ns) {
                warn_line!(
                    "lumen-script-candela: `{ns}` is the runtime's own namespace, so the \
                     functions registered under it are not declared for the app; register them \
                     under a namespace of your own"
                );
            }
            continue;
        }
        prefix.push_str(block);
        prefix.push('\n');
        line += 1;
    }
    for (ns, wrapper) in wrappers {
        let first_line = line + 1;
        prefix.push_str(wrapper);
        if !wrapper.ends_with('\n') {
            prefix.push('\n');
        }
        line = prefix.matches('\n').count() as u32;
        spans.push(WrapperSpan {
            ns: ns.clone(),
            first_line,
            last_line: line,
        });
    }

    PreparedSource {
        text: format!("{prefix}{resolved}"),
        line_offset: line,
        wrappers: spans,
    }
}

/// Whether `source` already opens a `host "<ns>"` block of its own.
fn declares_namespace(source: &str, ns: &str) -> bool {
    let needle = format!("host \"{ns}\"");
    source.contains(&needle)
}

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
///
/// The prelude lands once per program. An app's `.cdl` files are concatenated
/// into one candela module and each states the import it depends on, so the
/// first sentinel takes the block and every later one drops to an empty line.
/// Splicing all of them would define `signal` and its siblings twice and fail
/// the compile.
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
            if !replaced {
                out.push_str(&block);
            }
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
    fn repeated_imports_splice_once() {
        let src = "import \"lumen.cdl\";\nfn a() {}\nimport \"lumen.cdl\";\nfn b() {}\n";
        let out = resolve_prelude(src);
        assert_eq!(
            out.matches("host \"lumen\" {").count(),
            1,
            "each app's files import the prelude, but it may only be declared once"
        );
        assert!(!out.contains("import"), "every sentinel is consumed");
        assert_eq!(
            src.matches('\n').count(),
            out.matches('\n').count(),
            "a dropped import still costs its line"
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
