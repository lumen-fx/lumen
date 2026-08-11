//! Standalone MCP server binary.
//!
//! Speaks the Model Context Protocol over stdio (newline-delimited JSON-RPC
//! 2.0) and proxies tool calls to a running Lumen app's TCP introspection
//! port (default `127.0.0.1:7878`, provided by `lumen-mcp::LumenMcpPlugin`).
//!
//! ## Why hand-rolled
//!
//! The MCP stdio transport is documented as line-delimited JSON-RPC 2.0
//! with an `initialize` handshake, `tools/list`, `tools/call`. The needed
//! surface is small (~200 lines) and avoids dragging the full `rmcp` crate
//! (and its proc-macros + extra features) into a tool the user runs
//! per-keystroke from their editor.

mod bridge;
mod mcp;

use std::process::ExitCode;

use tracing::error;

fn parse_args() -> (String, u16) {
    let mut host = std::env::var("LUMEN_MCP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut port: u16 = std::env::var("LUMEN_MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7878);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--host" => {
                if let Some(v) = args.next() {
                    host = v;
                }
            }
            "--port" => {
                if let Some(v) = args.next()
                    && let Ok(p) = v.parse()
                {
                    port = p;
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "lumen-mcp-server: MCP server bridging Claude Code (stdio) to a Lumen app.\n\
                     usage: lumen-mcp-server [--host 127.0.0.1] [--port 7878]\n\
                     env:   LUMEN_MCP_HOST, LUMEN_MCP_PORT"
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }
    (host, port)
}

fn main() -> ExitCode {
    // tracing -> stderr (stdout is the MCP wire). Static level: stderr noise
    // is unwanted in Claude Code's normal flow; bump via `LUMEN_MCP_LOG=debug`
    // and a custom build if you need it.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    let (host, port) = parse_args();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    if let Err(e) = rt.block_on(mcp::run(host, port)) {
        error!("lumen-mcp-server: fatal: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
