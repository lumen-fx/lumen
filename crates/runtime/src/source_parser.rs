//! Injected source front-end - the markup + CSS parser boundary.
//!
//! `lumen-runtime` never links the `.lmn` / CSS parser. That front-end stays
//! in the compiler (`lumenc`), and `lumenc` depends on this crate for its CLI
//! `run` / `build` paths, so a direct parser dependency here would form a
//! dependency cycle. The parser is injected instead:
//!
//! - The CLI (`lumenc run`), the Rust SDK, and the C-ABI dev paths hand a
//!   [`SourceParser`] to [`crate::RunOptions::parser`].
//! - The dev source-load path ([`crate::run`]) and hot reload call it.
//! - The precompiled-artifact path ([`RunOptions::artifact`](crate::RunOptions::artifact))
//!   needs no parser at all and ignores this hook.

use bevy_ecs::prelude::Resource;
use lumen_ir::css::Stylesheet;
use lumen_ir::fragment::FragmentTable;
use lumen_ir::layout_ir::LayoutIR;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The markup/CSS front-end, injected by whoever drives the runtime from
/// source. Implemented in `lumenc` (`LumencParser`) over its `roxmltree`
/// markup parser, the hand-rolled CSS parser, the `<include>` / `@import`
/// resolver, and the real-filesystem `FsLoader`.
///
/// Errors are surfaced as `String` (rendered from the front-end's own error
/// types) so the trait carries no dependency on the parser's error model.
pub trait SourceParser: Send + Sync {
    /// Parse markup text (with `<include>` directives already spliced away)
    /// into a [`LayoutIR`], instantiating any of `fragments` it names.
    fn parse_html(&self, src: &str, fragments: &FragmentTable) -> Result<LayoutIR, String>;

    /// Parse CSS text into a [`Stylesheet`].
    fn parse_css(&self, src: &str) -> Result<Stylesheet, String>;

    /// Resolve `<include src="...">` directives in `src` against
    /// `self_path`'s directory (real filesystem). Every resolved file path is
    /// appended to `out` so the hot-reload watcher can poll it.
    fn resolve_includes(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<String, String>;

    /// Resolve `@import "..."` directives in `src` against `self_path`.
    /// Every imported file path is appended to `out`.
    fn resolve_css_imports(
        &self,
        src: &str,
        self_path: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<String, String>;

    /// Parse markup, resolving `<include>` directives against `self_path`'s
    /// directory via the real-filesystem loader (the file-based-pages
    /// assembly path).
    fn parse_html_with_loader(
        &self,
        src: &str,
        self_path: &Path,
        fragments: &FragmentTable,
    ) -> Result<LayoutIR, String>;

    /// Read the fragments `src` declares, without building its tree. An app
    /// collects these from every one of its `.lmn` files so a fragment
    /// declared in one is usable from all of them.
    fn collect_fragments(&self, src: &str, self_path: &Path) -> Result<FragmentTable, String>;

    /// Read the fragments the `lmn!` blocks in one candela script declare.
    /// `uri` is where the script came from, and lands on each fragment's
    /// origin. An app collects these alongside what its markup declares, so a
    /// shipped artifact carries every fragment and parses no markup itself.
    fn script_fragments(&self, src: &str, uri: &str) -> Result<FragmentTable, String>;
}

/// World-resource wrapper so the hot-reload system (a `&mut World` system that
/// cannot take the parser as a param) can reach the injected [`SourceParser`].
/// Inserted by `build_app` only when hot reload is active and a parser was
/// supplied.
#[derive(Resource, Clone)]
pub(crate) struct RuntimeParser(pub(crate) Arc<dyn SourceParser>);
