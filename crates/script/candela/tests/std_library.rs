//! The candela standard library resolves under the Lumen host.
//!
//! candela reads `import "std/..."` off disk from the library tree beside the
//! running executable, and it loads `std/list` on its own so array methods
//! resolve. Lumen links the compiler rather than shipping candela's binary, so
//! the tree travels with Lumen's own build; these cover the three ways a
//! program reaches it, and what a module the library does not carry still
//! reports.

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The epoch second the fixture clock has to be past: 2020-01-01. A wall clock
/// reading zero would mean the binding resolved to nothing.
const AFTER_2020: i64 = 1_577_836_800;

#[test]
fn text_module_imports() {
    let mut host = CandelaHost::new();
    host.load("import \"std/string\";\nfn main() {}\n", "app.cdl")
        .expect("a text module in the shipped library imports");
}

#[test]
fn c_backed_module_binds_its_library() {
    let mut host = CandelaHost::new();
    host.load(
        "import \"std/time\";\nfn go() { return now(); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("the wall clock module imports");

    let out = host.call("go", &[]).expect("now() runs");
    let Some(ScriptValue::I64(seconds)) = out.ret else {
        panic!("now() returns an epoch second, got {:?}", out.ret);
    };
    assert!(
        seconds > AFTER_2020,
        "now() must read the wall clock, got {seconds}"
    );
}

#[test]
fn array_methods_resolve_without_an_import() {
    let mut host = CandelaHost::new();
    host.load(
        "fn go() { let a = [1, 2, 3]; return a.sum(); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("the array methods come from std/list with no import");

    let out = host.call("go", &[]).expect("sum() runs");
    assert!(
        matches!(out.ret, Some(ScriptValue::I64(6))),
        "sum() over [1, 2, 3] is 6, got {:?}",
        out.ret
    );
}

#[test]
fn a_module_that_does_not_exist_still_fails() {
    let mut host = CandelaHost::new();
    let error = host
        .load("import \"std/telepathy\";\nfn main() {}\n", "app.cdl")
        .expect_err("a module the library does not carry has nothing to read");
    assert!(
        error.to_string().contains("Cannot read file"),
        "the compiler reports the missing file, got {error}"
    );
}
