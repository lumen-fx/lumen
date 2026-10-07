//! The builtins a candela host binds, against the metadata table editor
//! tooling reads for them.
//!
//! The two sides are written apart: the bodies in `lumen-script`'s shared
//! table and this crate's own entries, the rows in `src/builtins.rs`. A
//! function bound without its row would work in a script and be invisible in
//! every editor; a row that drifted from its signature would describe a call
//! the body does not accept.

use lumen_candela_host::{BUILTINS, builtin_fns};
use lumen_script::{ScriptFn, ScriptTy, ScriptValue};

/// The entry of that name a candela host binds.
fn bound(name: &str) -> ScriptFn {
    builtin_fns()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no builtin `{name}`"))
}

/// How a candela row spells a declared type.
fn spelling(ty: &ScriptTy) -> String {
    match ty {
        ScriptTy::Int => "int".to_string(),
        ScriptTy::Float => "float".to_string(),
        ScriptTy::Bool => "bool".to_string(),
        ScriptTy::Str => "string".to_string(),
        ScriptTy::Unit => "()".to_string(),
        ScriptTy::Any | ScriptTy::Dynamic => "any".to_string(),
        ScriptTy::Array(inner) => format!("{}[]", spelling(inner)),
        ScriptTy::Map(value) => format!("{{string: {}}}", spelling(value)),
        ScriptTy::Struct(shape) => shape.name.clone(),
    }
}

#[test]
fn every_bound_builtin_has_a_metadata_row() {
    let rows: std::collections::HashSet<&str> = BUILTINS.iter().map(|b| b.name).collect();
    let mut missing: Vec<String> = builtin_fns()
        .into_iter()
        .filter(|f| !rows.contains(f.name.as_str()))
        .map(|f| f.name)
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "these builtins have no row in the metadata table, so the LSP cannot offer them: \
         {missing:?}"
    );
}

/// A row's parameter count and types match the signature behind it, so hover
/// text describes the call the body accepts. A variadic or optional entry is
/// exempt from the parameter check: one registration serves a range of
/// arities and the row spells the shape an author writes.
#[test]
fn a_row_spells_the_signature_behind_it() {
    let mut drifted: Vec<String> = Vec::new();
    for b in BUILTINS {
        let Some(f) = builtin_fns().into_iter().find(|f| f.name == b.name) else {
            continue;
        };
        if spelling(&f.sig.ret) != b.ret {
            drifted.push(format!(
                "{}: returns {}, the row says {}",
                b.name,
                spelling(&f.sig.ret),
                b.ret
            ));
        }
        if f.sig.variadic || f.sig.min_arity != f.sig.params.len() {
            continue;
        }
        if f.sig.params.len() != b.params.len() {
            drifted.push(format!(
                "{}: the table takes {} argument(s), the row spells {}",
                b.name,
                f.sig.params.len(),
                b.params.len()
            ));
            continue;
        }
        for (param, row) in f.sig.params.iter().zip(b.params) {
            if spelling(&param.ty) != row.ty {
                drifted.push(format!(
                    "{}: `{}` is {}, the row says {}",
                    b.name,
                    param.name,
                    spelling(&param.ty),
                    row.ty
                ));
            }
        }
    }
    drifted.sort_unstable();
    assert!(drifted.is_empty(), "{drifted:#?}");
}

/// candela cannot overload a host function on arity or return a value its
/// declaration does not name, so it takes the writer-only `page` and
/// unit-valued history steps.
#[test]
fn the_navigation_family_takes_the_declarable_shape() {
    let page = bound("page");
    assert_eq!(page.sig.arity_range(), 1..=1);
    assert_eq!(page.sig.ret, ScriptTy::Unit);

    let back = bound("page_back");
    assert_eq!(back.sig.ret, ScriptTy::Unit);
    assert_eq!(back.invoke(&[]).0, Ok(ScriptValue::Unit));
    assert_eq!(bound("page_forward").sig.ret, ScriptTy::Unit);
    assert_eq!(bound("page_current").sig.ret, ScriptTy::Str);
}

/// The DOM and event surface candela reaches through free functions over an
/// `int` node id is part of what it binds.
#[test]
fn the_free_function_dom_surface_is_bound() {
    let dom = builtin_fns()
        .into_iter()
        .filter(|f| f.name.starts_with("node_") || f.name.starts_with("event_"))
        .count();
    assert!(dom > 50, "the DOM surface is much larger than {dom}");
}
