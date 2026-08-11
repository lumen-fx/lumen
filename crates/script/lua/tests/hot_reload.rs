//! Hot reload on [`LuaHost`] (parity with the Rhai host's `hot_reload` test).
//!
//! When `replace_ast`'s new source compiles but the top-level run fails, the
//! previously-registered handlers must remain intact. When the reload
//! succeeds, registrations the new top level does not repeat must carry
//! forward.

use lumen_script::{ScriptCommand, ScriptHost};
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

/// The reload regression: an app binds its handlers from `on_start`, which the
/// runtime fires once at app construction and never re-fires. A reload has to
/// carry that registration forward, and the carried handler has to dispatch
/// against the reloaded source.
#[test]
fn on_start_registered_handler_survives_reload() {
    let src = |clicks: i32| {
        format!(
            r#"
            function on_start() on("click", "bump", "handle_bump") end
            function handle_bump(id) add_clicks({clicks}) end
            "#
        )
    };
    let mut host = LuaHost::new();
    host.load(&src(1)).expect("initial load");
    // Drive `on_start` the way `ScriptPlugin::build` does.
    let outcome = host.call("on_start", &[]).expect("on_start ok");
    host.push_commands(outcome.commands);
    assert_eq!(
        host.lookup_handler("click", "bump"),
        Some("handle_bump".to_string()),
        "on_start registered the handler"
    );

    host.replace_ast(&src(7)).expect("successful replace");

    assert_eq!(
        host.lookup_handler("click", "bump"),
        Some("handle_bump".to_string()),
        "a reload carries the on_start registration forward",
    );

    let out = host
        .call(
            "handle_bump",
            &[lumen_script::ScriptValue::Str("bump".into())],
        )
        .expect("handler dispatches");
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::AddClicks(7))),
        "the carried handler runs the reloaded body, got {:?}",
        out.commands
    );
}
