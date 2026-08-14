//! Lumen Language Server - diagnostics + completion + hover for Lumen
//! markup (`.lmn` files).
//!
//! Built on `tower-lsp` 0.20. The binary entry point is in `main.rs`;
//! this library exposes the pieces that are unit-testable without an
//! LSP client (diagnostic conversion, completion classification, hover
//! lookup).
//!
//! Architectural choice: we re-parse the document on every change via
//! `lumenc::parse_html`. The parser is cheap and treating the compiler
//! as the source of truth guarantees the LSP can never disagree with
//! `lumenc` about what counts as a valid Lumen file.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod completion;
pub mod crossfile;
pub mod css;
pub mod definition;
pub mod diagnostics;
pub mod docs;
pub mod hover;
#[cfg(feature = "lang-rhai")]
pub mod rhai_lsp;
mod script_lang;
pub mod server;

pub use server::{Backend, DocKind, compute_diagnostics, position_to_byte};
