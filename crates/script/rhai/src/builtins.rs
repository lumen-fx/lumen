//! The Lumen script builtins exposed on the Rhai [`Engine`](rhai::Engine).
//!
//! Every free function registered via `engine.register_fn(...)` in
//! [`crate::RhaiHost::new`] has a matching entry in [`BUILTINS`]. The
//! table is consumed by:
//!
//! - the Lumen LSP (`lumen-lsp`) for completion, hover, and signature
//!   help so authors see the builtins their scripts can call, and
//! - the `builtins_parity` integration test, which asserts every name
//!   in the table is actually registered on a fresh engine (guarding
//!   against the table drifting away from the registration code).
//!
//! Custom-type *methods* (`Signal::get` / `ArraySignal::push` / the
//! `signals.foo.set(v)` chained accessors) are intentionally not listed
//! here: they dispatch on receiver type, which the text-only LSP cannot
//! resolve without type inference. Only top-level free functions (the ones
//! an author calls bare) belong in the table.
//!
//! The entries themselves live in `crates/script/api/builtins.ron`, shared
//! with the other hosts and filtered to this one at compile time. Add a
//! builtin there, listing `Rhai` among its hosts.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Every Lumen free-function builtin registered on the Rhai engine.
pub const BUILTINS: &[BuiltinFn] = lumen_script::builtins::RHAI_BUILTINS;

/// Look up a builtin by exact name.
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    lumen_script::builtins::lookup_in(BUILTINS, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_render() {
        let b = lookup("set_timeout").unwrap();
        assert_eq!(b.signature(), "set_timeout(name: string, ms: int) -> ()");
    }

    #[test]
    fn snippet_render() {
        let b = lookup("set_timeout").unwrap();
        assert_eq!(b.snippet(), "set_timeout(${1:name}, ${2:ms})");
    }

    #[test]
    fn no_arg_snippet() {
        // No zero-arg builtins today, but the render must stay valid.
        let b = BuiltinFn {
            name: "tick",
            params: &[],
            ret: "()",
            doc: "",
        };
        assert_eq!(b.snippet(), "tick()");
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
        }
    }
}
