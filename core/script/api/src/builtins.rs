//! Builtin-function metadata: the types, and the per-host tables.
//!
//! Every builtin the script hosts register is described once in
//! `builtins.ron` beside this crate. The build script reads that file and
//! generates [`RHAI_BUILTINS`], [`LUA_BUILTINS`], and [`CANDELA_BUILTINS`],
//! each holding the entries its host exposes with that host's own spelling of
//! the signature. A host crate re-exports its table as `BUILTINS`.
//!
//! The tables are consumed by:
//!
//! - the Lumen LSP (`lumen-lsp`) for completion, hover, and signature
//!   help, and
//! - each host's `builtins_parity` test, which asserts every entry names a
//!   function registered on a fresh engine, plus the candela host's reverse
//!   scan, which asserts every registration has an entry.
//!
//! Generation happens at compile time, so a table is a plain static and no
//! RON parser reaches a shipped binary. To add or change a builtin, edit
//! `builtins.ron`.

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
    /// (`"rhai"`, `"lua"`, `"candela"`).
    pub fn hover_markdown(&self, lang: &str) -> String {
        format!("```{lang}\n{}\n```\n\n{}", self.signature(), self.doc)
    }
}

// The per-host tables, generated from `builtins.ron` by `build.rs`.
include!(concat!(env!("OUT_DIR"), "/builtins_table.rs"));

/// Look up a builtin by exact name in one host's table.
#[must_use]
pub fn lookup_in(table: &'static [BuiltinFn], name: &str) -> Option<&'static BuiltinFn> {
    table.iter().find(|b| b.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each generated table holds distinct names, and a builtin every host
    /// registers as a bare function reaches all three tables.
    ///
    /// `page` and `signal_set` are deliberately not such a case: `page` is
    /// registered by the runtime as a per-host extension rather than by a host
    /// crate, and Rhai and Lua reach a signal through a handle method rather
    /// than a bare `signal_set`, so neither is tabled for those hosts.
    #[test]
    fn generated_tables_are_well_formed() {
        for table in [RHAI_BUILTINS, LUA_BUILTINS, CANDELA_BUILTINS] {
            assert!(!table.is_empty(), "a host table should not be empty");
            let mut seen = std::collections::HashSet::new();
            for b in table {
                assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
            }
        }
        // Builtins every host registers as a bare free function.
        for name in ["set_timeout", "notify", "set_class", "set_string"] {
            for table in [RHAI_BUILTINS, LUA_BUILTINS, CANDELA_BUILTINS] {
                assert!(
                    lookup_in(table, name).is_some(),
                    "`{name}` should reach every host"
                );
            }
        }
    }

    /// An override replaces only the fields it names, and leaves the rest of
    /// the shared entry alone.
    #[test]
    fn per_host_overrides_apply() {
        // `derive` overrides all three fields for candela: the recompute body
        // is a function name there, not a closure.
        let rhai = lookup_in(RHAI_BUILTINS, "derive").expect("rhai derive");
        let candela = lookup_in(CANDELA_BUILTINS, "derive").expect("candela derive");
        assert_eq!(rhai.ret, "Signal");
        assert_eq!(candela.ret, "()");
        assert_eq!(candela.params[2].ty, "string");
        assert_eq!(rhai.params[2].ty, "fn");

        // `get_by_id` overrides the doc line alone, so the signature is shared.
        let rhai = lookup_in(RHAI_BUILTINS, "get_by_id").expect("rhai get_by_id");
        let lua = lookup_in(LUA_BUILTINS, "get_by_id").expect("lua get_by_id");
        assert_eq!(rhai.params, lua.params);
        assert_eq!(rhai.ret, lua.ret);
        assert!(rhai.doc.ends_with("Node or ()."));
        assert!(lua.doc.ends_with("Node or nil."));
    }

    /// A builtin listed for only some hosts stays out of the others.
    #[test]
    fn host_availability_is_honoured() {
        // Handle-returning lookups are Rhai/Lua only: candela reaches the same
        // elements through its prelude wrappers over the `node_*` free
        // functions, which are candela-only entries in the same file.
        for name in ["query", "get_by_id", "document", "signal", "signal_array"] {
            assert!(lookup_in(RHAI_BUILTINS, name).is_some(), "rhai has {name}");
            assert!(lookup_in(LUA_BUILTINS, name).is_some(), "lua has {name}");
            assert!(
                lookup_in(CANDELA_BUILTINS, name).is_none(),
                "candela should not table `{name}`"
            );
        }
        for name in ["node_spawn", "event_on", "node_computed_style_all"] {
            assert!(
                lookup_in(CANDELA_BUILTINS, name).is_some(),
                "candela {name}"
            );
            assert!(
                lookup_in(RHAI_BUILTINS, name).is_none(),
                "rhai exposes `{name}` as a method, so it should not be tabled"
            );
        }
    }

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
        assert!(b.hover_markdown("lua").starts_with("```lua\n"));
        assert!(b.hover_markdown("rhai").starts_with("```rhai\n"));
        assert!(b.hover_markdown("candela").contains("The active page key."));
    }
}
