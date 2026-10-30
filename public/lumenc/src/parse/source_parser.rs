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
use lumen_ir::layout_ir::{LayoutIR, ParseError, ScriptRefs};
use lumen_ir::source_map::{SourceError, SourceMap};
use std::path::{Path, PathBuf};

/// A front-end error as the runtime takes it: the message, and the line and
/// column in `src` when the error names a place there.
fn source_error(e: &ParseError, src: &str) -> SourceError {
    let error = SourceError::new(e.unplaced().to_string());
    match e.offset() {
        Some(offset) => error.at_offset(src, offset),
        None => error,
    }
}

/// Splice the `<include>`s of `src`, which was read from `self_path`, run
/// `parse` over the result, and place an error it raises in the file it is
/// in. Hands back what `parse` built and every file the splice read.
fn with_includes<T>(
    src: &str,
    self_path: &Path,
    parse: impl FnOnce(&str) -> Result<T, ParseError>,
) -> Result<(T, Vec<PathBuf>), SourceError> {
    let mut files = Vec::new();
    let (text, map) = crate::parse::resolve::resolve_includes_mapped(
        src,
        self_path,
        Some(&crate::parse::resolve::FsLoader),
        &mut files,
    )
    .map_err(|e| source_error(&e, src))?;
    let built = parse(&text).map_err(|e| source_error(&e, &text).placed_with(&map))?;
    Ok((built, files))
}

/// Zero-sized [`SourceParser`](lumen_runtime::SourceParser) backed by
/// `lumenc`'s front-end. Construct with `LumencParser` and hand it to
/// [`RunOptions::with_parser`](lumen_runtime::RunOptions::with_parser) (or use
/// [`crate::default_parser`]).
pub struct LumencParser;

impl lumen_runtime::SourceParser for LumencParser {
    fn parse_html(&self, src: &str, fragments: &FragmentTable) -> Result<LayoutIR, SourceError> {
        crate::parse_markup(src, Path::new(""), None, fragments)
            .map(|parsed| parsed.ir)
            .map_err(|e| source_error(&e, src))
    }

    fn parse_css(&self, src: &str) -> Result<Stylesheet, SourceError> {
        crate::parse_css(src).map_err(|e| source_error(&e, src))
    }

    fn resolve_includes(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<(String, SourceMap), String> {
        crate::parse::resolve::resolve_includes_mapped(
            src,
            self_path,
            Some(&crate::parse::resolve::FsLoader),
            out,
        )
        .map_err(|e| e.to_string())
    }

    fn resolve_css_imports(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<(String, SourceMap), String> {
        crate::parse::resolve::resolve_css_imports_mapped(
            src,
            self_path,
            &crate::parse::resolve::FsLoader,
            out,
        )
        .map_err(|e| e.to_string())
    }

    fn parse_html_with_loader(
        &self,
        src: &str,
        self_path: &Path,
        fragments: &FragmentTable,
    ) -> Result<LayoutIR, SourceError> {
        let (mut ir, files) = with_includes(src, self_path, |text| {
            crate::parse_markup(text, self_path, None, fragments).map(|parsed| parsed.ir)
        })?;
        ir.included_files = files;
        Ok(ir)
    }

    fn collect_fragments(&self, src: &str, self_path: &Path) -> Result<FragmentTable, SourceError> {
        with_includes(src, self_path, |text| {
            crate::collect_fragments(text, self_path, None)
        })
        .map(|(table, _)| table)
    }

    fn block_fragments(
        &self,
        src: &str,
        uri: &str,
        blocks: Vec<lumen_script::MarkupBlock>,
    ) -> Result<FragmentTable, String> {
        crate::parse::lmn::block_fragments(src, uri, blocks)
    }

    fn script_refs(&self, src: &str, self_path: &Path) -> Result<ScriptRefs, SourceError> {
        with_includes(src, self_path, |text| {
            crate::collect_script_refs(text, self_path, None)
        })
        .map(|(refs, _)| refs)
    }

    fn link_fragments(&self, mut table: FragmentTable) -> Result<FragmentTable, String> {
        crate::parse::fragments::link(&mut table).map_err(|e| e.to_string())?;
        Ok(table)
    }
}
