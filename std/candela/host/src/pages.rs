//! The page builtins in the shape a candela host function takes.
//!
//! A candela host function is neither arity-overloaded nor allowed to return
//! a value its declaration does not name, so candela gets a single-argument
//! writer, the shared `page_current` reader, and unit-valued history steps.
//! Every entry rides the `lumen_core::nav` bus, the one an `<a href>` click,
//! the C ABI, and the Rust SDK write.

use lumen_script::{ScriptFn, ScriptNs, ScriptTy as T, ScriptValue};

/// `page(path)`: navigate to a page.
pub(crate) fn page() -> ScriptFn {
    ScriptFn::new("page")
        .ns(ScriptNs::Builtin)
        .param("path", T::Str)
        .ret(T::Unit)
        .doc("Navigate to a page.")
        .build(|cx| {
            lumen_core::nav::navigate(cx.arg(0).stringify());
            Ok(ScriptValue::Unit)
        })
}

/// `page_back()` and `page_forward()`: step through the page history.
pub(crate) fn steps() -> Vec<ScriptFn> {
    [
        ("page_back", "Step back through the page history."),
        ("page_forward", "Step forward through the page history."),
    ]
    .into_iter()
    .map(|(name, doc)| {
        let forward = name == "page_forward";
        ScriptFn::new(name)
            .ns(ScriptNs::Builtin)
            .ret(T::Unit)
            .doc(doc)
            .build(move |_| {
                if forward {
                    lumen_core::nav::forward();
                } else {
                    lumen_core::nav::back();
                }
                Ok(ScriptValue::Unit)
            })
    })
    .collect()
}
