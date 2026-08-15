//! `tower-lsp` `LanguageServer` implementation.
//!
//! The server routes each request by document kind, derived from the
//! file extension:
//!
//! - `.lmn` - Lumen markup: parse diagnostics + lint findings, tag/attr
//!   completion, hover, template goto-def, id references/rename, element
//!   document symbols, and formatting.
//! - `.css` - stylesheet: parse errors + apply-time warnings.
//! - `.rhai` - script: builtin-aware diagnostics, completion, hover,
//!   signature help, function document symbols, and id
//!   completion/goto-def/references/rename against the sibling markup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams,
    DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams,
    Location, MarkupContent, MarkupKind, MessageType, OneOf, Position, Range, ReferenceParams,
    RenameParams, ServerCapabilities, ServerInfo, SignatureHelp, SignatureHelpOptions,
    SignatureHelpParams, SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    Url, WorkspaceEdit,
};
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::{byte_to_position, diagnostic_from_error};
use crate::hover::{doc_for, target_at};
use crate::{completion, crossfile, css, script_lang};

/// Which Lumen source language a document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// `.lmn` markup.
    Markup,
    /// `.css` stylesheet.
    Css,
    /// `.rhai` script.
    Rhai,
    /// Anything else - we hold the text but offer no intelligence.
    Other,
}

impl DocKind {
    /// Classify a document by its URI's file extension.
    ///
    /// Reads the extension off the URI path rather than a converted filesystem
    /// path: `Url::to_file_path` rejects a URI that is not a valid local path
    /// on the host (`file:///proj/main.rhai` has no drive letter on Windows),
    /// which would classify a perfectly good document as [`DocKind::Other`].
    pub fn from_uri(uri: &Url) -> DocKind {
        match Path::new(uri.path()).extension().and_then(|e| e.to_str()) {
            Some("lmn") => DocKind::Markup,
            Some("css") => DocKind::Css,
            Some("rhai") => DocKind::Rhai,
            _ => DocKind::Other,
        }
    }
}

/// Per-document cached source + kind.
#[derive(Debug, Clone)]
struct DocState {
    text: String,
    kind: DocKind,
}

/// The sibling source files that make up one Lumen project directory.
#[derive(Debug, Clone, Default)]
struct Project {
    markup: Option<PathBuf>,
    css: Option<PathBuf>,
    rhai: Option<PathBuf>,
}

/// LSP backend.
pub struct Backend {
    pub(crate) client: Client,
    docs: Mutex<HashMap<Url, DocState>>,
}

impl Backend {
    /// Build a new backend.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
        }
    }

    async fn store(&self, uri: Url, text: String, kind: DocKind) {
        self.docs.lock().await.insert(uri, DocState { text, kind });
    }

    async fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.lock().await.get(uri).map(|d| d.text.clone())
    }

    async fn get_kind(&self, uri: &Url) -> Option<DocKind> {
        self.docs.lock().await.get(uri).map(|d| d.kind)
    }

    /// Read a project file's text, preferring an open in-memory buffer
    /// over the on-disk copy.
    async fn read_project_file(&self, path: &Path) -> Option<String> {
        if let Ok(url) = Url::from_file_path(path)
            && let Some(t) = self.get_text(&url).await
        {
            return Some(t);
        }
        std::fs::read_to_string(path).ok()
    }

    /// Fetch the sibling markup source for the project `uri` belongs to,
    /// if any (used for CSS scratch trees + rhai id resolution).
    async fn sibling_markup(&self, uri: &Url) -> Option<String> {
        let proj = discover_project(uri)?;
        let markup = proj.markup?;
        self.read_project_file(&markup).await
    }

    /// The diagnostics an open document has, routed by its kind. `None` when
    /// nothing is open under `uri`.
    ///
    /// Split out of [`Self::publish_diagnostics`] so the routing can be read
    /// without a client on the other end of the socket.
    async fn diagnostics_for(&self, uri: &Url) -> Option<Vec<Diagnostic>> {
        let (text, kind) = self
            .docs
            .lock()
            .await
            .get(uri)
            .map(|d| (d.text.clone(), d.kind))?;
        Some(match kind {
            DocKind::Markup => {
                // Resolve `<include>` against the on-disk project so missing
                // files and cycles surface as diagnostics. Falls back to the
                // include-dropping string path when the URI isn't a file.
                compute_diagnostics_at(&text, uri.to_file_path().ok().as_deref())
            }
            DocKind::Rhai => script_lang::diagnostics(&text),
            DocKind::Css => {
                let markup = self.sibling_markup(uri).await;
                css::compute_css_diagnostics(&text, markup.as_deref())
            }
            DocKind::Other => Vec::new(),
        })
    }

    async fn publish_diagnostics(&self, uri: Url) {
        let Some(diags) = self.diagnostics_for(&uri).await else {
            return;
        };
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

/// Run `lumenc::parse_html` and convert the result into diagnostics.
/// On a parse error, returns that single error. On success, surfaces
/// every parse-time [`lumenc::LintFinding`] as its own diagnostic - so a
/// clean-parsing document can still report multiple issues.
pub fn compute_diagnostics(src: &str) -> Vec<Diagnostic> {
    compute_diagnostics_at(src, None)
}

/// Like [`compute_diagnostics`] but, when `self_path` is a real file,
/// resolves `<include src="..."/>` directives against the filesystem so
/// missing-include and include-cycle errors become diagnostics. With
/// `None`, includes are dropped (the string-only path) so the parser never
/// chokes on markup that references files the LSP can't see.
pub fn compute_diagnostics_at(src: &str, self_path: Option<&Path>) -> Vec<Diagnostic> {
    let parsed = match self_path {
        Some(p) => lumenc::parse_html_with_loader(src, p, &lumenc::FsLoader),
        None => lumenc::parse_html(src),
    };
    match parsed {
        Ok(ir) => ir
            .lint_findings
            .iter()
            .map(|f| lint_to_diagnostic(src, f))
            .collect(),
        Err(e) => vec![diagnostic_from_error(src, &e)],
    }
}

fn lint_to_diagnostic(src: &str, f: &lumenc::LintFinding) -> Diagnostic {
    let severity = match f.severity {
        lumenc::LintSeverity::Error => DiagnosticSeverity::ERROR,
        lumenc::LintSeverity::Warn => DiagnosticSeverity::WARNING,
        lumenc::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
        lumenc::LintSeverity::Hint => DiagnosticSeverity::HINT,
    };
    // LintFinding carries 1-based line/col. Anchor a single-char range,
    // clamped to the actual line length.
    let line = f.line.saturating_sub(1) as u32;
    let character = f.col.saturating_sub(1) as u32;
    let start = Position { line, character };
    let message = match &f.suggest {
        Some(s) => format!("{} (suggest: {})", f.message, s),
        None => f.message.clone(),
    };
    let _ = src;
    Diagnostic {
        range: Range {
            start,
            end: Position {
                line,
                character: character + 1,
            },
        },
        severity: Some(severity),
        source: Some("lumen-lsp".into()),
        message,
        ..Default::default()
    }
}

/// Find the UTF-8 byte offset in `src` that corresponds to the given LSP
/// `Position` (line + UTF-16 character offset).
pub fn position_to_byte(src: &str, pos: Position) -> usize {
    let mut line = 0u32;
    let mut byte = 0usize;
    let line_target = pos.line;
    let bytes = src.as_bytes();
    while byte < bytes.len() && line < line_target {
        if bytes[byte] == b'\n' {
            line += 1;
        }
        byte += 1;
    }
    if line < line_target {
        return src.len();
    }
    let line_start = byte;
    let line_end = src[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(src.len());
    let line_text = &src[line_start..line_end];
    let mut utf16 = 0u32;
    for (i, c) in line_text.char_indices() {
        if utf16 >= pos.character {
            return line_start + i;
        }
        utf16 += c.len_utf16() as u32;
    }
    line_end
}

/// Discover the sibling `main.*` (or first-of-kind) files sharing a
/// directory with `uri`.
fn discover_project(uri: &Url) -> Option<Project> {
    let path = uri.to_file_path().ok()?;
    let dir = path.parent()?;
    let mut proj = Project::default();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let is_main = p
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s == "main")
            .unwrap_or(false);
        let slot = match ext {
            "lmn" => &mut proj.markup,
            "css" => &mut proj.css,
            "rhai" => &mut proj.rhai,
            _ => continue,
        };
        // Prefer `main.<ext>`; otherwise take the first one seen.
        if slot.is_none() || is_main {
            *slot = Some(p);
        }
    }
    Some(proj)
}

/// The element id the cursor sits on, given the document kind. Markup:
/// inside an `id="X"` value. Rhai: inside an id-argument string literal.
/// Css: inside a `#id` selector.
fn id_under_cursor(kind: DocKind, text: &str, cursor: usize) -> Option<String> {
    match kind {
        DocKind::Markup => crossfile::markup_id_defs(text)
            .into_iter()
            .find(|s| cursor >= s.start && cursor <= s.end)
            .map(|s| s.name),
        DocKind::Rhai => {
            let ctx = crossfile::rhai_string_context(text, cursor)?;
            let call = ctx.call.as_deref()?;
            if crossfile::is_id_argument(call, ctx.arg_index) {
                Some(ctx.value)
            } else {
                None
            }
        }
        DocKind::Css => css_id_at(text, cursor),
        DocKind::Other => None,
    }
}

/// The id of a `#id` selector token containing `cursor`, if any.
fn css_id_at(css: &str, cursor: usize) -> Option<String> {
    let bytes = css.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let cursor = cursor.min(css.len());
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    // The identifier must be introduced by `#`.
    if start == 0 || bytes[start - 1] != b'#' {
        return None;
    }
    // Distinguish an id *selector* (`#save {`) from a hex color in a
    // declaration *value* (`color: #fff`). Walk back from the `#` to the
    // nearest structural marker; if it's a `:` we are inside a value.
    let mut b = start - 1;
    while b > 0 {
        match bytes[b - 1] {
            b':' => return None,
            b'{' | b'}' | b';' => break,
            _ => {}
        }
        b -= 1;
    }
    let mut end = cursor;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(css[start..end].to_string())
}

/// Convert a byte offset to an LSP position (UTF-16 aware).
fn pos(src: &str, byte: usize) -> Position {
    byte_to_position(src, byte)
}

/// Convert a byte span to an LSP range.
fn span_range(src: &str, start: usize, end: usize) -> Range {
    Range {
        start: pos(src, start),
        end: pos(src, end),
    }
}

/// Recursively convert a crossfile symbol node into an LSP DocumentSymbol.
#[allow(deprecated)]
fn to_document_symbol(src: &str, node: &crossfile::SymNode) -> DocumentSymbol {
    let kind = match node.kind {
        "function" => SymbolKind::FUNCTION,
        _ => SymbolKind::FIELD,
    };
    let range = span_range(src, node.start, node.end);
    DocumentSymbol {
        name: node.name.clone(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: if node.children.is_empty() {
            None
        } else {
            Some(
                node.children
                    .iter()
                    .map(|c| to_document_symbol(src, c))
                    .collect(),
            )
        },
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> RpcResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec!["<".into(), " ".into(), "\"".into(), ".".into()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: Some(vec![",".into()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "lumen-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "lumen-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let kind = DocKind::from_uri(&uri);
        self.store(uri.clone(), params.text_document.text, kind)
            .await;
        self.publish_diagnostics(uri).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        let kind = DocKind::from_uri(&uri);
        self.store(uri.clone(), change.text, kind).await;
        self.publish_diagnostics(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.lock().await.remove(&uri);
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn completion(&self, params: CompletionParams) -> RpcResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);
        let items: Vec<CompletionItem> = match kind {
            DocKind::Markup => {
                let ctx = completion::classify(&src, cursor);
                completion::items_for(&ctx)
            }
            DocKind::Rhai => {
                // Id completion inside an id-argument string wins; else
                // builtins.
                if let Some(ctx) = crossfile::rhai_string_context(&src, cursor)
                    && let Some(call) = ctx.call.as_deref()
                    && crossfile::is_id_argument(call, ctx.arg_index)
                {
                    let ids = self
                        .sibling_markup(&uri)
                        .await
                        .map(|m| crossfile::markup_ids(&m))
                        .unwrap_or_default();
                    ids.into_iter()
                        .filter(|id| id.starts_with(&ctx.value))
                        .map(id_completion_item)
                        .collect()
                } else {
                    script_lang::completions(&src, cursor)
                }
            }
            DocKind::Css | DocKind::Other => Vec::new(),
        };
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> RpcResult<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(kind) = self.get_kind(&uri).await else {
            return Ok(None);
        };
        if kind != DocKind::Rhai {
            return Ok(None);
        }
        let Some(src) = self.get_text(&uri).await else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);
        Ok(script_lang::signature_help(&src, cursor))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let pos = params.text_document_position_params.position;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);

        match kind {
            DocKind::Markup => {
                let Some(hit) = crate::definition::find_definition(&src, cursor) else {
                    return Ok(None);
                };
                let range = crate::definition::byte_range_to_lsp(&src, hit.start, hit.end);
                Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri,
                    range,
                })))
            }
            DocKind::Rhai => {
                // From an id string, jump to the markup element defining it.
                let Some(id) = id_under_cursor(DocKind::Rhai, &src, cursor) else {
                    return Ok(None);
                };
                let Some(proj) = discover_project(&uri) else {
                    return Ok(None);
                };
                let Some(markup_path) = proj.markup else {
                    return Ok(None);
                };
                let Some(markup) = self.read_project_file(&markup_path).await else {
                    return Ok(None);
                };
                let Some(def) = crossfile::markup_id_spans(&markup, &id).into_iter().next() else {
                    return Ok(None);
                };
                let Ok(markup_uri) = Url::from_file_path(&markup_path) else {
                    return Ok(None);
                };
                Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: markup_uri,
                    range: span_range(&markup, def.start, def.end),
                })))
            }
            _ => Ok(None),
        }
    }

    async fn references(&self, params: ReferenceParams) -> RpcResult<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);
        let Some(id) = id_under_cursor(kind, &src, cursor) else {
            return Ok(None);
        };
        Ok(Some(self.id_locations(&uri, &id).await))
    }

    async fn rename(&self, params: RenameParams) -> RpcResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);
        let Some(id) = id_under_cursor(kind, &src, cursor) else {
            return Ok(None);
        };

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (file_uri, text, spans) in self.id_spans_by_file(&uri, &id).await {
            let edits: Vec<TextEdit> = spans
                .iter()
                .map(|s| TextEdit {
                    range: span_range(&text, s.start, s.end),
                    new_text: new_name.clone(),
                })
                .collect();
            if !edits.is_empty() {
                changes.insert(file_uri, edits);
            }
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let nodes = match kind {
            DocKind::Markup => crossfile::markup_document_symbols(&src),
            DocKind::Rhai => crossfile::rhai_function_symbols(&src),
            _ => return Ok(None),
        };
        let syms: Vec<DocumentSymbol> = nodes.iter().map(|n| to_document_symbol(&src, n)).collect();
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> RpcResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        if kind != DocKind::Markup {
            return Ok(None);
        }
        match lumenc::formatter::format_str(&src) {
            Ok(formatted) if formatted != src => Ok(Some(vec![TextEdit {
                range: full_document_range(&src),
                new_text: formatted,
            }])),
            _ => Ok(None),
        }
    }

    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let pos = params.text_document_position_params.position;
        let Some((src, kind)) = self
            .docs
            .lock()
            .await
            .get(&uri)
            .map(|d| (d.text.clone(), d.kind))
        else {
            return Ok(None);
        };
        let cursor = position_to_byte(&src, pos);
        let md = match kind {
            DocKind::Markup => {
                let target = target_at(&src, cursor);
                target.and_then(|t| doc_for(&t)).map(|s| s.to_string())
            }
            DocKind::Rhai => script_lang::hover(&src, cursor),
            _ => None,
        };
        let Some(md) = md else {
            return Ok(None);
        };
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: None,
        }))
    }
}

impl Backend {
    /// Collect `(uri, text, spans)` triples for every file in the project
    /// that references id `id`.
    async fn id_spans_by_file(
        &self,
        from: &Url,
        id: &str,
    ) -> Vec<(Url, String, Vec<crossfile::TextSpan>)> {
        let mut out = Vec::new();
        let Some(proj) = discover_project(from) else {
            return out;
        };
        if let Some(path) = &proj.markup
            && let Some(text) = self.read_project_file(path).await
            && let Ok(uri) = Url::from_file_path(path)
        {
            let spans = crossfile::markup_id_spans(&text, id);
            out.push((uri, text, spans));
        }
        if let Some(path) = &proj.rhai
            && let Some(text) = self.read_project_file(path).await
            && let Ok(uri) = Url::from_file_path(path)
        {
            let spans = crossfile::rhai_string_literal_spans(&text, id);
            out.push((uri, text, spans));
        }
        if let Some(path) = &proj.css
            && let Some(text) = self.read_project_file(path).await
            && let Ok(uri) = Url::from_file_path(path)
        {
            let spans = crossfile::css_id_selector_spans(&text, id);
            out.push((uri, text, spans));
        }
        out
    }

    /// Flatten [`Self::id_spans_by_file`] into LSP `Location`s.
    async fn id_locations(&self, from: &Url, id: &str) -> Vec<Location> {
        let mut locs = Vec::new();
        for (uri, text, spans) in self.id_spans_by_file(from, id).await {
            for s in spans {
                locs.push(Location {
                    uri: uri.clone(),
                    range: span_range(&text, s.start, s.end),
                });
            }
        }
        locs
    }
}

fn id_completion_item(id: String) -> CompletionItem {
    CompletionItem {
        label: id.clone(),
        kind: Some(CompletionItemKind::VALUE),
        detail: Some("element id".into()),
        insert_text: Some(id),
        ..Default::default()
    }
}

/// A range covering the entire document (for full-document formatting).
fn full_document_range(src: &str) -> Range {
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: byte_to_position(src, src.len()),
    }
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::{TextDocumentIdentifier, TextDocumentPositionParams};

    use super::*;

    #[test]
    fn diagnostics_clean_for_valid_markup() {
        let src = "<root><tile bg=\"#ff0000\"/></root>";
        assert!(compute_diagnostics(src).is_empty());
    }

    #[test]
    fn diagnostics_for_unknown_tag() {
        let src = "<root><nope/></root>";
        let diags = compute_diagnostics(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Unknown tag"));
    }

    #[test]
    fn diagnostics_for_bad_attr() {
        let src = "<root><tile bg=\"not-hex\"/></root>";
        let diags = compute_diagnostics(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("bg"));
    }

    #[test]
    fn script_tag_is_ignored_for_diagnostics() {
        let src = "<root><script>let x = 1;</script></root>";
        assert!(compute_diagnostics(src).is_empty());
    }

    #[test]
    fn position_to_byte_roundtrip_ascii() {
        let src = "abc\ndef\n";
        let p = Position {
            line: 1,
            character: 2,
        };
        assert_eq!(position_to_byte(src, p), 6);
    }

    #[test]
    fn doc_kind_from_extension() {
        let u = Url::parse("file:///proj/main.rhai").unwrap();
        assert_eq!(DocKind::from_uri(&u), DocKind::Rhai);
        let u = Url::parse("file:///proj/main.lmn").unwrap();
        assert_eq!(DocKind::from_uri(&u), DocKind::Markup);
        let u = Url::parse("file:///proj/main.css").unwrap();
        assert_eq!(DocKind::from_uri(&u), DocKind::Css);
    }

    #[test]
    fn css_id_at_finds_selector() {
        let css = "#save { color: #fff; }";
        // cursor on the `a` of `#save`.
        assert_eq!(css_id_at(css, 3), Some("save".to_string()));
        // cursor on the hex color, not an id selector.
        assert_eq!(css_id_at(css, 16), None);
    }

    #[test]
    fn id_under_cursor_rhai_id_arg() {
        let rhai = r#"on("click", "save", "h")"#;
        // cursor inside "save".
        let at = rhai.find("save").unwrap() + 1;
        assert_eq!(
            id_under_cursor(DocKind::Rhai, rhai, at),
            Some("save".to_string())
        );
        // cursor inside "click" (event, not id).
        let at = rhai.find("click").unwrap() + 1;
        assert_eq!(id_under_cursor(DocKind::Rhai, rhai, at), None);
    }

    /// A server with one document open, ready to answer requests about it.
    ///
    /// `LspService::new` is the only way to obtain a [`Client`], and it hands
    /// back the socket the server would write notifications to. The request
    /// handlers answer in band, so the socket only has to stay alive.
    async fn open(
        kind: DocKind,
        name: &str,
        text: &str,
    ) -> (tower_lsp::LspService<Backend>, tower_lsp::ClientSocket, Url) {
        let (service, socket) = tower_lsp::LspService::new(Backend::new);
        let uri = Url::parse(&format!("file:///proj/{name}")).unwrap();
        service
            .inner()
            .store(uri.clone(), text.to_string(), kind)
            .await;
        (service, socket, uri)
    }

    /// The end of `src`, as the position a client would send.
    fn end_of(src: &str) -> Position {
        byte_to_position(src, src.len())
    }

    /// Each document kind reaches its own analyser: a `.rhai` buffer goes
    /// through the script-language seam, a `.css` buffer through the
    /// stylesheet path, and a kind with no analyser answers with an empty
    /// list, which is what clears a client's stale squiggles. A URI with
    /// nothing open answers with nothing at all.
    #[tokio::test]
    async fn each_document_kind_gets_its_own_diagnostics() {
        let (svc, _sock, uri) =
            open(DocKind::Rhai, "main.rhai", "fn broken( {\n let x = ;\n}").await;
        let script = svc
            .inner()
            .diagnostics_for(&uri)
            .await
            .expect("the script is open");
        #[cfg(feature = "lang-rhai")]
        assert!(
            script.iter().any(|d| d.message.starts_with("Rhai:")),
            "the syntax error should surface: {script:?}"
        );
        #[cfg(not(feature = "lang-rhai"))]
        assert!(
            script.is_empty(),
            "without the language a script analyses to an empty list"
        );

        let (svc, _sock, uri) = open(DocKind::Css, "main.css", "#a { color: ").await;
        assert!(
            !svc.inner()
                .diagnostics_for(&uri)
                .await
                .expect("the stylesheet is open")
                .is_empty(),
            "an unterminated declaration block should be reported"
        );

        let (svc, _sock, uri) = open(DocKind::Other, "notes.txt", "plain text").await;
        assert_eq!(
            svc.inner().diagnostics_for(&uri).await,
            Some(Vec::new()),
            "an unanalysed kind still answers, with nothing in it"
        );

        let absent = Url::parse("file:///proj/never-opened.lmn").unwrap();
        assert!(
            svc.inner().diagnostics_for(&absent).await.is_none(),
            "a document that was never opened has no answer"
        );
    }

    /// Completion in a script buffer that is not inside an id argument falls
    /// to the script language's builtin list.
    #[tokio::test]
    async fn completion_in_a_script_offers_builtins() {
        let src = "set_t";
        let (svc, _sock, uri) = open(DocKind::Rhai, "main.rhai", src).await;
        let resp = svc
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: end_of(src),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .expect("completion answers");
        let Some(CompletionResponse::Array(items)) = resp else {
            panic!("the server answers with a plain array");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

        #[cfg(feature = "lang-rhai")]
        assert!(
            labels.contains(&"set_timeout"),
            "expected set_timeout in {labels:?}"
        );
        #[cfg(not(feature = "lang-rhai"))]
        assert!(labels.is_empty(), "no language, no builtin list");
    }

    /// Hover over a script buffer documents the builtin under the cursor.
    #[tokio::test]
    async fn hover_in_a_script_documents_the_builtin() {
        let src = "notify(\"a\", \"b\");";
        let (svc, _sock, uri) = open(DocKind::Rhai, "main.rhai", src).await;
        let hovered = svc
            .inner()
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: byte_to_position(src, 2),
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .expect("hover answers");

        #[cfg(feature = "lang-rhai")]
        {
            let Some(Hover {
                contents: HoverContents::Markup(md),
                ..
            }) = hovered
            else {
                panic!("a builtin under the cursor has markdown documentation");
            };
            assert!(
                md.value.contains("notify"),
                "the documentation names the builtin: {}",
                md.value
            );
        }
        #[cfg(not(feature = "lang-rhai"))]
        assert!(hovered.is_none(), "no language, no documentation");
    }

    /// Signature help follows the call being typed in a script buffer, and
    /// answers nothing for a kind that has no calls.
    #[tokio::test]
    async fn signature_help_follows_the_call_being_typed() {
        let src = "set_timeout(\"tick\", ";
        let (svc, _sock, uri) = open(DocKind::Rhai, "main.rhai", src).await;
        let help = svc
            .inner()
            .signature_help(SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: end_of(src),
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .expect("signature help answers");

        #[cfg(feature = "lang-rhai")]
        assert_eq!(
            help.expect("a call in progress has a signature")
                .active_parameter,
            Some(1),
            "the cursor sits on the second argument"
        );
        #[cfg(not(feature = "lang-rhai"))]
        assert!(help.is_none(), "no language, no signature");

        let markup = "<root/>";
        let (svc, _sock, uri) = open(DocKind::Markup, "index.lmn", markup).await;
        let help = svc
            .inner()
            .signature_help(SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: end_of(markup),
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .expect("signature help answers");
        assert!(help.is_none(), "markup has no call signatures");
    }
}
