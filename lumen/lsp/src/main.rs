//! `lumen-lsp` binary - stdio-driven `tower-lsp` server.

use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    // Tracing logs go to stderr so they don't corrupt the stdio LSP
    // channel.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(lumen_lsp::Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
