//! Script-language intelligence, keyed to the languages this build carries.
//!
//! The server routes a script buffer through here instead of naming a host
//! directly. A build with the `lang-rhai` feature (the default) forwards to
//! [`crate::rhai_lsp`]; a build without it keeps every markup, CSS, and
//! cross-file id feature and answers script-only requests with nothing.

use tower_lsp::lsp_types::{CompletionItem, Diagnostic, SignatureHelp};

/// Compile errors for a script buffer.
pub(crate) fn diagnostics(src: &str) -> Vec<Diagnostic> {
    #[cfg(feature = "lang-rhai")]
    {
        crate::rhai_lsp::compute_rhai_diagnostics(src)
    }
    #[cfg(not(feature = "lang-rhai"))]
    {
        let _ = src;
        Vec::new()
    }
}

/// Builtin-function completions at `cursor`.
pub(crate) fn completions(src: &str, cursor: usize) -> Vec<CompletionItem> {
    #[cfg(feature = "lang-rhai")]
    {
        crate::rhai_lsp::completions(src, cursor)
    }
    #[cfg(not(feature = "lang-rhai"))]
    {
        let _ = (src, cursor);
        Vec::new()
    }
}

/// Hover markdown for the builtin under `cursor`.
pub(crate) fn hover(src: &str, cursor: usize) -> Option<String> {
    #[cfg(feature = "lang-rhai")]
    {
        crate::rhai_lsp::hover(src, cursor)
    }
    #[cfg(not(feature = "lang-rhai"))]
    {
        let _ = (src, cursor);
        None
    }
}

/// Signature help for the call being typed at `cursor`.
pub(crate) fn signature_help(src: &str, cursor: usize) -> Option<SignatureHelp> {
    #[cfg(feature = "lang-rhai")]
    {
        crate::rhai_lsp::signature_help(src, cursor)
    }
    #[cfg(not(feature = "lang-rhai"))]
    {
        let _ = (src, cursor);
        None
    }
}
