//! JSON through the candela host: candela's own `json_parse` is a language
//! builtin reachable from any `.cdl` script, and the host adds
//! `lumen::parse_json`, which marshals a parsed document into the same pools.
//! Both return a value typed `any`, read back with the `as_map` / `as_list` /
//! `as_str` / ... downcasts and `map.get(key)`, and both work on runtime
//! strings such as a fetch body, not only compile-time literals.

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

#[test]
fn native_json_parse_is_reachable_for_nested_runtime_bodies() {
    let mut host = CandelaHost::new();
    // No host block / no import needed: json_parse and the as_* downcasts are
    // language builtins, not `lumen`-namespace host functions.
    let src = r#"
fn city_name(body) {
    let root = as_map(json_parse(body));
    let geo = as_map(root.get("geo"));
    return as_str(geo.get("city"));
}
fn main() {}
"#;
    host.load(src, "json.cdl").expect("script compiles");

    // A runtime body (not a compile-time literal), with a nested object.
    let body = ScriptValue::Str(r#"{"geo":{"city":"Paris"}}"#.to_owned());
    let outcome = host.call("city_name", &[body]).expect("call ok");
    assert_eq!(outcome.ret, Some(ScriptValue::Str("Paris".to_owned())));
}

/// A parsed document's arrays hang off its root map, so they are reachable
/// only through a map in a register. The VM's array collector has to follow
/// that map, or the first collection after the parse frees the arrays while
/// the script still holds the map, and a later allocation takes the slot
/// (#307: a launcher read a manifest's 107 libraries as none). The document
/// is large enough to arm the collector and the loop allocates until it
/// fires; the assertion is on the length, since the handle's tag never broke.
#[test]
fn parsed_array_survives_a_collection_while_its_map_is_live() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn libs_len(body) {
    let root = as_map(lumen::parse_json(body));
    let s = "a,b,c";
    let i = 0;
    while i < 50 {
        let parts = s.split(",");
        i = i + 1;
    }
    return as_list(root.get("libraries")).len();
}
fn main() {}
"#;
    host.load(src, "json.cdl").expect("script compiles");

    let libraries = (0..300)
        .map(|i| format!("[{i}]"))
        .collect::<Vec<_>>()
        .join(",");
    let body = ScriptValue::Str(format!(r#"{{"libraries": [{libraries}], "other": "x"}}"#));
    let outcome = host.call("libs_len", &[body]).expect("call ok");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(300)));
}
