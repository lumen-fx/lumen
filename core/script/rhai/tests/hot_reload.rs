//! Hot reload on [`RhaiHost`].
//!
//! Two contracts are under test. When `replace_ast`'s new source compiles but
//! the top-level eval fails, the previously-registered handlers and
//! derivations must remain intact, so the next event still fires the old
//! handler instead of crashing on an empty registry. When the reload succeeds,
//! registrations the new top level does not repeat must carry forward.

use lumen_script::{ScriptCommand, ScriptHost};
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

/// The reload regression: an app binds its handlers from `on_start`, which the
/// runtime fires once at app construction and never re-fires. A reload has to
/// carry that registration forward, and the carried handler has to dispatch
/// against the reloaded source.
#[test]
fn on_start_registered_handler_survives_reload() {
    let src = |clicks: i32| {
        format!(
            r#"
            fn on_start() {{ on("click", "bump", "handle_bump"); }}
            fn handle_bump(id) {{ add_clicks({clicks}); }}
            "#
        )
    };
    let mut host = RhaiHost::new();
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
