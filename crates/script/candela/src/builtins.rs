//! The Lumen script builtins a candela program can call.
//!
//! Every host function the crate registers under the `lumen` namespace has a
//! matching entry in [`BUILTINS`]. Both candela hosts register the same list,
//! so the table describes what a compiled program and a `.cdlb` artifact each
//! reach. Unlike the Rhai host (where builtins are bare global
//! functions), candela reaches them through a typed `host "lumen" { ... }`
//! block the script declares; the declaration is type-checked against the
//! registered closure at compile time. The table is consumed by:
//!
//! - the Lumen LSP for completion / hover / signature help,
//! - the `builtins_parity` integration test, which synthesizes a `host`
//!   block from this table and compiles it - proving every entry is
//!   registered with a matching scalar signature, and
//! - `every_registered_lumen_fn_is_tabled`, which scans the host source for
//!   registrations and proves the other direction.
//!
//! Most entries have a concrete signature: scalars, homogeneous arrays
//! (`string[]`), and string-keyed maps of one value type (`{string: int}`).
//! An entry that names `any` in a parameter or its return carries a value with
//! no single concrete shape; those register variadically and are declared
//! `name(...)` in the prelude, with the `any` return type where they return
//! one. [`is_variadic`] is the single place that rule lives.
//!
//! The entries themselves live in `crates/script/api/builtins.ron`, shared
//! with the other hosts and filtered to this one at compile time. candela's
//! table is the largest because its dynamic DOM and event surface is free
//! functions over an `int` handle, where Rhai and Lua dispatch the same calls
//! as receiver methods that no table lists. Add a builtin there, listing
//! `Candela` among its hosts, and give it a candela override where the type
//! spelling differs from the other hosts.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Every scalar Lumen builtin registered on the candela engine under the
/// `lumen` host namespace.
pub const BUILTINS: &[BuiltinFn] = lumen_script::builtins::CANDELA_BUILTINS;

/// Look up a builtin by exact name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    lumen_script::builtins::lookup_in(BUILTINS, name)
}

/// Whether `b` is registered variadically, which is true exactly when it names
/// `any` in a parameter or its return type. Such a builtin is declared
/// `name(...)` in a `host` block; every other entry keeps its concrete
/// signature.
#[must_use]
pub fn is_variadic(b: &BuiltinFn) -> bool {
    b.ret == "any" || b.params.iter().any(|p| p.ty == "any")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
        }
    }

    /// Whether `ty` is a type a fixed host-fn signature can name: a scalar, a
    /// homogeneous array of scalars, or a string-keyed map of one scalar.
    fn is_concrete(ty: &str) -> bool {
        fn is_scalar(ty: &str) -> bool {
            matches!(ty, "int" | "float" | "bool" | "string")
        }
        is_scalar(ty)
            || ty.strip_suffix("[]").is_some_and(is_scalar)
            || ty
                .strip_prefix("{string: ")
                .and_then(|rest| rest.strip_suffix('}'))
                .is_some_and(is_scalar)
    }

    #[test]
    fn every_non_variadic_type_is_concrete() {
        // A fixed host-fn signature names one concrete type per position. An
        // entry that needs a dynamically-shaped value says so with `any`, which
        // makes it variadic; everything else must stay concrete.
        for b in BUILTINS {
            if is_variadic(b) {
                continue;
            }
            for param in b.params {
                assert!(
                    is_concrete(param.ty),
                    "builtin {} has non-marshallable param type {}",
                    b.name,
                    param.ty
                );
            }
            assert!(
                b.ret == "()" || is_concrete(b.ret),
                "builtin {} has non-marshallable return type {}",
                b.name,
                b.ret
            );
        }
    }

    #[test]
    fn variadic_entries_are_the_ones_naming_any() {
        for b in BUILTINS {
            let names_any = b.ret == "any" || b.params.iter().any(|p| p.ty == "any");
            assert_eq!(
                is_variadic(b),
                names_any,
                "builtin {} disagrees with the `any` marker",
                b.name
            );
        }
    }
}
