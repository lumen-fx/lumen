//! W6.7 atomic hot-reload: when `replace_ast`'s new source compiles
//! but the top-level eval fails, the previously-registered handlers +
//! derivations must remain intact so the next event still fires the
//! OLD handler instead of crashing on an empty registry.

use lumen_script_rhai::RhaiHost;

#[test]
fn replace_ast_with_runtime_error_preserves_old_handlers() {
    let mut host = RhaiHost::new();
    // First load: register a click handler.
    host.load(
        r#"
        on("click", "save", "save_handler");
        fn save_handler(id) {}
        "#,
    )
    .expect("initial load");
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_handler".to_string()),
        "post-load handler is reachable"
    );

    // Replace with source that compiles cleanly but fails at eval
    // time (calling an undefined function at top level).
    let bad_source = r#"
        // Calls a nonexistent fn at top-level -> eval-time error.
        nonexistent_function_that_will_error();
    "#;
    let result = host.replace_ast(bad_source);
    assert!(result.is_err(), "bad source must fail replace_ast");

    // The original handler must still be present - atomicity contract.
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_handler".to_string()),
        "old handler must survive failed replace_ast",
    );
}

#[test]
fn replace_ast_with_compile_error_preserves_old_handlers() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        on("click", "save", "save_handler");
        fn save_handler(id) {}
        "#,
    )
    .expect("initial load");
    assert!(host.lookup_handler("click", "save").is_some());

    // Source with a syntax error -> parse failure, doesn't reach
    // the clear-and-eval path at all.
    let bad_source = "fn @@@ broken syntax {{{";
    let result = host.replace_ast(bad_source);
    assert!(result.is_err(), "bad source must fail replace_ast");

    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_handler".to_string()),
        "old handler must survive parse-time failure",
    );
}

#[test]
fn replace_ast_success_swaps_handlers() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        on("click", "save", "save_v1");
        fn save_v1(id) {}
        "#,
    )
    .expect("initial load");
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_v1".to_string())
    );

    host.replace_ast(
        r#"
        on("click", "save", "save_v2");
        fn save_v2(id) {}
        "#,
    )
    .expect("successful replace");

    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_v2".to_string()),
        "successful replace_ast must install the new handler",
    );
}
