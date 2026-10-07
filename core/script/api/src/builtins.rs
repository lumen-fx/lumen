//! Builtin-function metadata: the shape of one entry in a host's table.
//!
//! Each script host keeps a table of the builtins it registers, spelled the
//! way its own language names their types, and hands it out through
//! [`ScriptHost::builtins`](crate::ScriptHost::builtins). Editor tooling
//! (`lumen-lsp`) reads it for completion, hover, and signature help.

/// One parameter of a builtin function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinParam {
    /// Parameter name, used for signature help and snippet placeholders.
    pub name: &'static str,
    /// Human-readable script-side type (`string`, `int`, `float`,
    /// `bool`, `array`, `fn`, `any`).
    pub ty: &'static str,
}

/// One Lumen script builtin: its name, parameters, return type, and a
/// one-line documentation string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFn {
    /// Function name as called from script.
    pub name: &'static str,
    /// Ordered parameter list.
    pub params: &'static [BuiltinParam],
    /// Return type (`()` for commands that return nothing).
    pub ret: &'static str,
    /// One-line summary shown in completion detail and hover.
    pub doc: &'static str,
}

impl BuiltinFn {
    /// Render a human-readable signature, e.g.
    /// `set_timeout(name: string, ms: int) -> ()`.
    pub fn signature(&self) -> String {
        let params = self
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({}) -> {}", self.name, params, self.ret)
    }

    /// Render an LSP snippet insert string with tab-stops for each
    /// parameter, e.g. `set_timeout(${1:name}, ${2:ms})`.
    pub fn snippet(&self) -> String {
        if self.params.is_empty() {
            return format!("{}()", self.name);
        }
        let params = self
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| format!("${{{}:{}}}", i + 1, p.name))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({})", self.name, params)
    }

    /// Render markdown documentation for hover: a fenced signature line
    /// followed by the doc summary.
    ///
    /// `lang` is the fence language, so each host labels its own buffers
    /// with its language name.
    pub fn hover_markdown(&self, lang: &str) -> String {
        format!("```{lang}\n{}\n```\n\n{}", self.signature(), self.doc)
    }
}

/// Look up a builtin by exact name in one host's table.
#[must_use]
pub fn lookup_in(table: &'static [BuiltinFn], name: &str) -> Option<&'static BuiltinFn> {
    table.iter().find(|b| b.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arg_snippet() {
        let b = BuiltinFn {
            name: "tick",
            params: &[],
            ret: "()",
            doc: "",
        };
        assert_eq!(b.snippet(), "tick()");
    }

    #[test]
    fn hover_fence_takes_the_language() {
        let b = BuiltinFn {
            name: "page_current",
            params: &[],
            ret: "string",
            doc: "The active page key.",
        };
        assert!(b.hover_markdown("toy").starts_with("```toy\n"));
        assert!(b.hover_markdown("candela").contains("The active page key."));
    }
}
