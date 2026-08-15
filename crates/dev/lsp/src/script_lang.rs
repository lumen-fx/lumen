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

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test states both answers: what a build carrying the language
    /// returns, and what a build without it returns. Running the suite under
    /// `--no-default-features` therefore checks the empty half rather than
    /// skipping it.
    #[test]
    fn a_broken_script_reports_a_compile_error() {
        let diags = diagnostics("fn broken( {\n let x = ;\n}");

        #[cfg(feature = "lang-rhai")]
        {
            assert_eq!(diags.len(), 1, "the syntax error should surface once");
            assert!(
                diags[0].message.starts_with("Rhai:"),
                "the message names the language: {}",
                diags[0].message
            );
        }
        #[cfg(not(feature = "lang-rhai"))]
        assert!(
            diags.is_empty(),
            "a server without the language analyses nothing"
        );
    }

    #[test]
    fn a_clean_script_reports_nothing_either_way() {
        assert!(
            diagnostics("fn on_click(id) { notify(\"a\", \"b\"); }").is_empty(),
            "a script using only builtins is clean"
        );
    }

    #[test]
    fn completions_offer_the_builtins_under_the_prefix() {
        let labels: Vec<String> = completions("set_t", 5)
            .into_iter()
            .map(|i| i.label)
            .collect();

        #[cfg(feature = "lang-rhai")]
        {
            assert!(
                labels.iter().any(|l| l == "set_timeout"),
                "expected set_timeout in {labels:?}"
            );
            assert!(
                !labels.iter().any(|l| l == "notify"),
                "the prefix should filter unrelated builtins out"
            );
        }
        #[cfg(not(feature = "lang-rhai"))]
        assert!(labels.is_empty(), "no language, no builtin list");
    }

    #[test]
    fn hover_documents_the_builtin_under_the_cursor() {
        let md = hover("notify(\"a\", \"b\");", 2);

        #[cfg(feature = "lang-rhai")]
        assert!(
            md.as_deref().is_some_and(|m| m.contains("notify")),
            "expected notify's documentation, got {md:?}"
        );
        #[cfg(not(feature = "lang-rhai"))]
        assert!(md.is_none(), "no language, no documentation");
    }

    #[test]
    fn signature_help_tracks_the_argument_being_typed() {
        let src = "set_timeout(\"tick\", ";
        let help = signature_help(src, src.len());

        #[cfg(feature = "lang-rhai")]
        {
            let help = help.expect("a call in progress has a signature");
            assert_eq!(
                help.active_parameter,
                Some(1),
                "the cursor sits on the second argument"
            );
        }
        #[cfg(not(feature = "lang-rhai"))]
        assert!(help.is_none(), "no language, no signature");
    }
}
