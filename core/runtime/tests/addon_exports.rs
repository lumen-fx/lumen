//! Reading a compiled program's exports back when the app was compiled against
//! a browser add-on: the add-on's functions are declared in the program, so the
//! read binds them the way a desktop run does.

#![cfg(all(feature = "runtime-parse", feature = "host-candela"))]

use lumen_ir::addon::{Addon, AddonFunction, AddonParam};
use lumen_ir::artifact::CompiledScript;
use lumen_runtime::run::script_exports;

fn script(bytecode: Option<Vec<u8>>) -> CompiledScript {
    CompiledScript {
        engine: "candela".to_string(),
        source: String::new(),
        bytecode,
    }
}

#[test]
fn a_script_with_no_compiled_form_has_nothing_to_read_back() {
    assert!(script_exports(&script(None), &[]).is_none());
}

#[test]
fn an_addon_whose_types_no_host_reads_fails_the_read_back_by_name() {
    let addon = Addon {
        name: "echo".to_string(),
        namespace: "echo".to_string(),
        functions: vec![AddonFunction {
            name: "shout".to_string(),
            params: vec![AddonParam {
                name: "text".to_string(),
                ty: "text".to_string(),
            }],
            returns: "string".to_string(),
            event: None,
            doc: String::new(),
        }],
        elements: Vec::new(),
    };
    let read =
        script_exports(&script(Some(vec![0])), &[addon]).expect("a compiled program is read back");
    let err = read.expect_err("the add-on's signature does not bind");
    assert!(err.contains("echo::shout"), "{err}");
}
