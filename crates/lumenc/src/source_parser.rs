//! `LumencParser` - the compiler's implementation of the runtime's injected
//! [`lumen_runtime::SourceParser`] boundary.
//!
//! `lumen-runtime` links no markup / CSS parser (that would form a dependency
//! cycle, since `lumenc` depends on `lumen-runtime`). Instead the runtime calls
//! an injected [`SourceParser`](lumen_runtime::SourceParser); this is the impl
//! `lumenc` (and, through it, the SDKs) hand to
//! [`RunOptions::parser`](lumen_runtime::RunOptions::parser). It wraps
//! `lumenc`'s `roxmltree` markup parser, the hand-rolled CSS parser, the
//! `<include>` / `@import` resolver, and the real-filesystem `FsLoader`.
//!
//! Gated on `runtime-parse`: the parser front-end it wraps is itself gated
//! there, so a parser-free compiler build carries neither.

use lumen_ir::css::Stylesheet;
use lumen_ir::fragment::FragmentTable;
use lumen_ir::layout_ir::LayoutIR;
use std::path::{Path, PathBuf};

/// Zero-sized [`SourceParser`](lumen_runtime::SourceParser) backed by
/// `lumenc`'s front-end. Construct with `LumencParser` and hand it to
/// [`RunOptions::with_parser`](lumen_runtime::RunOptions::with_parser) (or use
/// [`crate::default_parser`]).
pub struct LumencParser;

impl lumen_runtime::SourceParser for LumencParser {
    fn parse_html(&self, src: &str, fragments: &FragmentTable) -> Result<LayoutIR, String> {
        crate::parse_markup(src, Path::new(""), None, fragments)
            .map(|parsed| parsed.ir)
            .map_err(|e| e.to_string())
    }

    fn parse_css(&self, src: &str) -> Result<Stylesheet, String> {
        crate::parse_css(src).map_err(|e| e.to_string())
    }

    fn resolve_includes(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<String, String> {
        crate::resolve::resolve_includes(src, self_path, Some(&crate::resolve::FsLoader), out)
            .map_err(|e| e.to_string())
    }

    fn resolve_css_imports(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<String, String> {
        crate::resolve::resolve_css_imports(src, self_path, &crate::resolve::FsLoader, out)
            .map_err(|e| e.to_string())
    }

    fn parse_html_with_loader(
        &self,
        src: &str,
        self_path: &Path,
        fragments: &FragmentTable,
    ) -> Result<LayoutIR, String> {
        crate::parse_markup(src, self_path, Some(&crate::resolve::FsLoader), fragments)
            .map(|parsed| parsed.ir)
            .map_err(|e| e.to_string())
    }

    fn collect_fragments(&self, src: &str, self_path: &Path) -> Result<FragmentTable, String> {
        crate::collect_fragments(src, self_path, Some(&crate::resolve::FsLoader))
            .map_err(|e| e.to_string())
    }
}
