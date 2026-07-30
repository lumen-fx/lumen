//! Per-id handler routing including template-namespaced suffix fallback.
//!
//! `on(event, id, fn)` keys handlers by `(event, id)`. When markup
//! templates auto-namespace ids (`<my-card id="user">` produces
//! `id="user:save"` on its inner button), a handler registered as
//! `on("click", "save", ...)` still fires for `user:save` via the
//! suffix fallback, so widget-internal handlers don't have to know the
//! template instance prefix.

use lumen_script_rhai::RhaiHost;

#[test]
fn exact_id_match_takes_precedence() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        on("click", "save", "exact_save");
        fn exact_save(id) { print("exact"); }
        "#,
    )
    .expect("load");
    assert_eq!(
        host.lookup_handler("click", "save"),
        Some("exact_save".to_string())
    );
}

#[test]
fn suffix_fallback_fires_for_namespaced_id() {
    // Handler registered as `save` -> fires for `user:save` (template
    // prefix `user:`) and `team:save` alike.
    let mut host = RhaiHost::new();
    host.load(
        r#"
        on("click", "save", "shared_save");
        fn shared_save(id) {}
        "#,
    )
    .expect("load");
    assert_eq!(
        host.lookup_handler("click", "user:save"),
        Some("shared_save".to_string())
    );
    assert_eq!(
        host.lookup_handler("click", "team:save"),
        Some("shared_save".to_string())
    );
}

#[test]
fn fully_qualified_handler_beats_suffix() {
    // A specific `user:save` handler must win over a shared `save`
    // handler when both are registered.
    let mut host = RhaiHost::new();
    host.load(
        r#"
        on("click", "save", "shared_save");
        on("click", "user:save", "user_specific");
        fn shared_save(id) {}
        fn user_specific(id) {}
        "#,
    )
    .expect("load");
    assert_eq!(
        host.lookup_handler("click", "user:save"),
        Some("user_specific".to_string())
    );
    assert_eq!(
        host.lookup_handler("click", "team:save"),
        Some("shared_save".to_string())
    );
}

#[test]
fn no_match_returns_none() {
    let host = RhaiHost::new();
    assert_eq!(host.lookup_handler("click", "missing"), None);
    assert_eq!(host.lookup_handler("click", "a:b:missing"), None);
}
