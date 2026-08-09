//! Atomic hot-reload (parity with the Rhai host's `hot_reload` test):
//! when `replace_ast`'s new source compiles but the top-level run fails,
//! the previously-registered handlers must remain intact.

use lumen_script_lua::LuaHost;

#[test]
fn replace_ast_with_runtime_error_preserves_old_handlers() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "save_handler")
        function save_handler(id) end
        "#,
    )
    .expect("initial load");
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_handler".to_string())
    );

    // Compiles cleanly but errors at run time (calling nil at top level).
    let bad_source = r#"
        nonexistent_function_that_will_error()
    "#;
    let result = host.replace_ast(bad_source);
    assert!(result.is_err(), "bad source must fail replace_ast");

    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_handler".to_string()),
        "old handler must survive failed replace_ast",
    );
}

#[test]
fn replace_ast_with_compile_error_preserves_old_handlers() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "save_handler")
        function save_handler(id) end
        "#,
    )
    .expect("initial load");
    assert!(host.lookup_handler("click", "save").is_some());

    let bad_source = "function @@@ broken syntax {{{";
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
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "save_v1")
        function save_v1(id) end
        "#,
    )
    .expect("initial load");
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_v1".to_string())
    );

    host.replace_ast(
        r#"
        on("click", "save", "save_v2")
        function save_v2(id) end
        "#,
    )
    .expect("successful replace");

    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("save_v2".to_string()),
        "successful replace_ast must install the new handler",
    );
}
