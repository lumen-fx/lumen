//! Per-id handler routing including template-namespaced suffix fallback
//! (parity with the Rhai host's `handler_lookup` test).

use lumen_script_lua::LuaHost;

#[test]
fn exact_id_match_takes_precedence() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "exact_save")
        function exact_save(id) print("exact") end
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
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "shared_save")
        function shared_save(id) end
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
    let mut host = LuaHost::new();
    host.load(
        r#"
        on("click", "save", "shared_save")
        on("click", "user:save", "user_specific")
        function shared_save(id) end
        function user_specific(id) end
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
    let host = LuaHost::new();
    assert_eq!(host.lookup_handler("click", "missing"), None);
    assert_eq!(host.lookup_handler("click", "a:b:missing"), None);
}
