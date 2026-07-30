//! candela's native JSON parser is reachable from a `.cdl` script run through
//! [`CandelaHost`], so the host does not expose its own `parse_json` builtin.
//!
//! The dep bump brings candela's language-level `json_parse` (backed by the
//! `std::json` module / VM intrinsic). It returns a value typed `any`, read back
//! with the `as_map` / `as_list` / `as_str` / ... downcasts and `map.get(key)`.
//! Because a host function cannot return an `any`-typed value across the
//! embedding boundary (the host-fn return type must be a concrete `HostType`), a
//! host-side `parse_json` could not carry a nested heterogeneous structure
//! anyway; the native parser is the right path, and it works on runtime strings
//! (e.g. a fetch body), not just compile-time literals.
//!
//! Coverage note: on the pinned candela dep, `json_parse` on a runtime body
//! reaches flat objects, homogeneous nested objects, scalar arrays, and
//! top-level arrays. Retrieving a value that is a HETEROGENEOUS collection (a
//! nested object with mixed value types, or an array of objects) via
//! `map.get(key)` still raises `unknown_map_key` at runtime, because candela
//! maps are homogeneously typed. That is a candela-side limitation, flagged for
//! upstream; the demo apps that read such shapes are blocked on it.

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
