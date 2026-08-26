//! What a loaded set puts on the app: the functions, in manifest order, with
//! the namespace, signature, and host set the plugin declared, plus the
//! language source it ships.

mod common;

use common::install_fixture;
use lumen_script::{HostSet, ScriptNs, ScriptTy};

#[test]
fn the_functions_land_in_manifest_order_with_what_they_declared() {
    let plugin = install_fixture("install-order", "fn_count = 2");
    let registry = plugin.registry();
    let names: Vec<&str> = registry.fns().iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "fixture_echo",
            "fixture_shape",
            "fixture_fail",
            "fixture_emit",
            "fixture_emit_then_fail",
            "fixture_panic",
            "fixture_event",
            "fixture_commands",
            "fixture_log",
            "fixture_push_signal",
            "fixture_pad0",
            "fixture_pad1",
        ]
    );

    let echo = &registry.fns()[0];
    assert_eq!(echo.ns, ScriptNs::Extension);
    assert_eq!(echo.sig.params.len(), 1);
    assert_eq!(echo.sig.params[0].name, "value");
    assert_eq!(echo.sig.params[0].ty, ScriptTy::Any);
    assert!(echo.hosts.contains(HostSet::ALL));

    let shape = &registry.fns()[1];
    assert_eq!(shape.sig.params[0].ty, ScriptTy::Str);
    assert!(shape.visible_to("candela"));
}

#[test]
fn a_declared_namespace_and_prelude_reach_the_app() {
    let plugin = install_fixture(
        "install-ns",
        "ns = \"named:fixture\"\nprelude = \"fn wrapped() {}\"",
    );
    let registry = plugin.registry();
    assert!(
        registry
            .fns()
            .iter()
            .all(|f| f.ns == ScriptNs::Named("fixture".to_string()))
    );

    let preludes = registry.preludes_for_lang("candela");
    assert_eq!(preludes.len(), 1);
    assert_eq!(preludes[0].ns, "fixture");
    assert_eq!(preludes[0].source, "fn wrapped() {}");
    assert!(registry.preludes_for_lang("rhai").is_empty());
}

#[test]
fn shutting_a_set_down_is_a_no_op_for_a_plugin_that_owns_nothing() {
    let plugin = install_fixture("install-shutdown", "");
    plugin.set.shutdown();
    // Still bound: shutdown tells the plugin, it does not unbind anything.
    assert!(!plugin.registry().fns().is_empty());
}
