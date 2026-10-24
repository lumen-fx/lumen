//! JSON through the candela host: candela's own `json::parse` comes from the
//! `std/json` module any `.cdl` script can import, and the host adds
//! `lumen::parse_json`, which marshals a parsed document into the same pools.
//! Both return a value typed `any`, read back with the `as` downcast and
//! `map[key]`, and both work on runtime strings such as a fetch body, not only
//! compile-time literals.

use lumen_candela_dev::CandelaHost;
use lumen_script::{ScriptHost, ScriptValue};

#[test]
fn native_json_parse_is_reachable_for_nested_runtime_bodies() {
    let mut host = CandelaHost::new();
    // No host block needed: `json::parse` is candela's standard library and
    // `as` is an operator, neither a `lumen`-namespace host function.
    let src = r#"
import "std/json";
fn city_name(body) {
    let root = json::parse(body) as {string: any};
    let geo = root["geo"] as {string: any};
    return geo["city"] as string;
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
    let root = lumen::parse_json(body) as {string: any};
    let s = "a,b,c";
    let i = 0;
    while i < 50 {
        let parts = s.split(",");
        i = i + 1;
    }
    return (root["libraries"] as any[]).len();
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

/// A downcast collection's entries are `any`, so it takes entries of any type
/// in any order and needs no wrapper function to hold them. A parsed document
/// mixes types, so a downcast that handed back an entry-less map would let the
/// first entry write pin the key and value types and reject the next one
/// (candela `4668cc5`, "keep a downcast collection's entries dynamic"). If this
/// fails, the candela pin is what moved.
#[test]
fn downcast_map_takes_inserts_of_mixed_types() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn grow_map(body) {
    let root = lumen::parse_json(body) as {string: any};
    root["added"] = 1;
    root["note"] = "two";
    return root.len();
}
fn grow_list(body) {
    let l = lumen::parse_json(body) as any[];
    l.push(2);
    l.push("three");
    return l.len();
}
fn main() {}
"#;
    host.load(src, "json.cdl").expect("script compiles");

    // Two inserts of different value types: one alone passes either way.
    let body = ScriptValue::Str(r#"{"n": 7, "s": "x"}"#.to_owned());
    let outcome = host.call("grow_map", &[body]).expect("call ok");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(4)));

    let body = ScriptValue::Str("[1]".to_owned());
    let outcome = host.call("grow_list", &[body]).expect("call ok");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(3)));
}

/// The boundary the rule above must not erode: an empty `{}` literal has no
/// entry types either, but it takes them from its first write and keeps them,
/// so a second write of another value type is an error. The assertion
/// is on the failure alone; the wording belongs to candela's diagnostics.
#[test]
fn empty_map_literal_still_pins_its_entry_types() {
    let mut host = CandelaHost::new();
    let src = r#"
fn grow(n) {
    let m = {};
    m["a"] = 1;
    m["b"] = "two";
    return m.len();
}
fn main() {}
"#;
    host.load(src, "json.cdl").expect("script compiles");

    let outcome = host.call("grow", &[ScriptValue::I64(0)]);
    assert!(outcome.is_err());
}
