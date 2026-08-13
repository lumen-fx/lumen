//! The Lumen script builtins exposed on the Lua [`Lua`](mlua::Lua) engine.
//!
//! Every free function registered as a Lua global in
//! [`crate::LuaHost::new`] has a matching entry in [`BUILTINS`]. The
//! table is consumed by:
//!
//! - the Lumen LSP (`lumen-lsp`) for completion, hover, and signature
//!   help, and
//! - the `builtins_parity` integration test, which asserts every name
//!   in the table resolves to a Lua function global on a fresh host
//!   (guarding against the table drifting away from the registration
//!   code).
//!
//! Custom-type *methods* (`Signal:get` / `ArraySignal:push` / the
//! `signals.foo.set(v)` chained accessors) are intentionally not listed
//! here: they dispatch on a receiver, which the text-only LSP cannot
//! resolve. Only top-level free functions belong in the table.
//!
//! The entries themselves live in `crates/script/api/builtins.ron`, shared
//! with the other hosts and filtered to this one at compile time, so the
//! name and doc surface matches the Rhai host's wherever the two agree and
//! differs only where Lua really differs (a `nil` miss where Rhai returns
//! `()`). Add a builtin there, listing `Lua` among its hosts.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Every Lumen free-function builtin registered as a Lua global.
pub const BUILTINS: &[BuiltinFn] = lumen_script::builtins::LUA_BUILTINS;

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
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
        }
    }
}
