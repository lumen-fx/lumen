//! The page builtins in the shape this host gives them.
//!
//! `page()` with no argument reads the current page and `page(path)`
//! navigates; a history step reports whether the request reached the
//! navigation bus, so a script can branch on it. Every entry rides the
//! `lumen_core::nav` bus, the one an `<a href>` click, the C ABI, and the Rust
//! SDK write; the shared `page_current` reader comes from the shared table.

use lumen_script::{ScriptFn, ScriptNs, ScriptTy as T, ScriptValue};

/// `page([path])`, `page_back()` and `page_forward()`.
pub(crate) fn page_fns() -> Vec<ScriptFn> {
    let mut fns = vec![
        ScriptFn::new("page")
            .ns(ScriptNs::Builtin)
            .param("path", T::Str)
            .min_arity(0)
            // The current path when read, nothing when navigating: a result
            // whose shape depends on how the call was written.
            .ret(T::Dynamic)
            .doc("Navigate to a page, or read the current one when called with no argument.")
            .build(|cx| {
                Ok(match cx.arg(0) {
                    ScriptValue::Unit => ScriptValue::Str(lumen_core::nav::current()),
                    path => {
                        lumen_core::nav::navigate(path.stringify());
                        ScriptValue::Unit
                    }
                })
            }),
    ];
    for (name, doc, go) in [
        (
            "page_back",
            "Step back through the page history.",
            lumen_core::nav::back as fn() -> bool,
        ),
        (
            "page_forward",
            "Step forward through the page history.",
            lumen_core::nav::forward as fn() -> bool,
        ),
    ] {
        fns.push(
            ScriptFn::new(name)
                .ns(ScriptNs::Builtin)
                .ret(T::Bool)
                .doc(doc)
                .build(move |_| Ok(ScriptValue::Bool(go()))),
        );
    }
    fns
}
