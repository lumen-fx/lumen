//! Array signals on the candela host: the reactive lists `<for each="name">`
//! renders, driven from a script through the name-keyed `signal_array_*`
//! builtins and the prelude's `ArraySignal` method sugar.
//!
//! The end-to-end proof that a candela app drives `<for each>` lives in
//! `crates/lumenc/tests/candela_for_each.rs`; this file pins the semantics.
//!
//! The sugar tests bind each record or list to a variable before passing it to
//! a method. A collection literal written directly in a call to a script-level
//! function aborts the candela compiler
//! (`compiler/registers.rs`, `set_tgt_id`), which is why the prelude and the
//! reference page spell it that way too. A collection literal passed straight
//! to a `lumen::` host function is unaffected, and the first tests below use
//! that form.

use std::collections::HashMap;

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The rows of the last `SetArray` command for `name`.
fn rows(cmds: &[ScriptCommand], name: &str) -> Option<Vec<HashMap<String, String>>> {
    cmds.iter().rev().find_map(|c| match c {
        ScriptCommand::SetArray { name: n, items } if n == name => Some(items.clone()),
        _ => None,
    })
}

/// `set` replaces the whole array and `push` appends one record; both flush a
/// `SetArray` carrying the stringified fields `<for>` binds by name.
#[test]
fn set_and_push_flush_rows() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    lumen::signal_array_set("notes", [{"id": "a", "title": "First"}]);
    lumen::signal_array_push("notes", {"id": "b", "title": "Second"});
}
fn main() {}
"#;
    host.load(src, "rows.cdl")
        .expect("compiles via the prelude");

    let out = host.call("seed", &[]).expect("seed ok");
    let items = rows(&out.commands, "notes").expect("a SetArray for notes");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].get("id").map(String::as_str), Some("a"));
    assert_eq!(items[1].get("title").map(String::as_str), Some("Second"));
}

/// `len`, `get`, and `all` read the array back. `get` is zero-based and yields
/// null out of range.
#[test]
fn len_get_and_all_read_back() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    lumen::signal_array_set("rows", [{"id": "a", "n": "1"}, {"id": "b", "n": "2"}]);
}
fn count() { return lumen::signal_array_len("rows"); }
fn second_id() {
    let row = as_map(lumen::signal_array_get("rows", 1));
    return as_str(row.get("id"));
}
fn past_end() { return is_null(lumen::signal_array_get("rows", 9)); }
fn total() { return as_list(lumen::signal_array_all("rows")).len(); }
fn main() {}
"#;
    host.load(src, "read.cdl")
        .expect("compiles via the prelude");
    host.call("seed", &[]).expect("seed ok");

    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(2))
    );
    assert_eq!(
        host.call("second_id", &[]).unwrap().ret,
        Some(ScriptValue::Str("b".to_owned()))
    );
    assert_eq!(
        host.call("past_end", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
    assert_eq!(
        host.call("total", &[]).unwrap().ret,
        Some(ScriptValue::I64(2))
    );
}

/// A record whose fields are not all one type keeps every field's type across
/// the boundary. A candela map literal holds one value type, so such a record
/// comes from somewhere else: a `parse_json` result, a response body, or the
/// host side.
#[test]
fn records_of_mixed_field_types_round_trip() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    let row = as_map(lumen::parse_json("{\"id\": \"a\", \"n\": 7, \"done\": true}"));
    lumen::signal_array_push("rows", row);
}
fn number() {
    let row = as_map(lumen::signal_array_get("rows", 0));
    return as_int(row.get("n"));
}
fn flag() {
    let row = as_map(lumen::signal_array_get("rows", 0));
    return as_bool(row.get("done"));
}
fn main() {}
"#;
    host.load(src, "mixed.cdl").expect("compiles");
    let out = host.call("seed", &[]).expect("seed ok");
    let items = rows(&out.commands, "rows").expect("a SetArray for rows");
    assert_eq!(items[0].get("n").map(String::as_str), Some("7"));

    assert_eq!(
        host.call("number", &[]).unwrap().ret,
        Some(ScriptValue::I64(7))
    );
    assert_eq!(
        host.call("flag", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
}

/// A record field whose key is longer than candela's inline-string limit is
/// still reachable, because host-returned map keys are interned on the way in.
#[test]
fn long_record_keys_are_reachable() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() { lumen::signal_array_push("rows", {"completion_state": "done"}); }
fn read_it() {
    let row = as_map(lumen::signal_array_get("rows", 0));
    return as_str(row.get("completion_state"));
}
fn main() {}
"#;
    host.load(src, "keys.cdl").expect("compiles");
    host.call("seed", &[]).expect("seed ok");
    assert_eq!(
        host.call("read_it", &[]).unwrap().ret,
        Some(ScriptValue::Str("done".to_owned()))
    );
}

/// `remove` drops one record by index and `clear` empties the array; both
/// flush the new list. An out-of-range remove leaves the array alone.
#[test]
fn remove_and_clear_reflush() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    lumen::signal_array_set("rows", [{"id": "a"}, {"id": "b"}, {"id": "c"}]);
}
fn drop_middle() { lumen::signal_array_remove("rows", 1); }
fn drop_past_end() { lumen::signal_array_remove("rows", 40); }
fn wipe() { lumen::signal_array_clear("rows"); }
fn main() {}
"#;
    host.load(src, "remove.cdl").expect("compiles");
    host.call("seed", &[]).expect("seed ok");

    let out = host.call("drop_middle", &[]).expect("remove ok");
    let items = rows(&out.commands, "rows").expect("a SetArray after remove");
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].get("id").map(String::as_str), Some("c"));

    let out = host.call("drop_past_end", &[]).expect("remove ok");
    assert!(
        rows(&out.commands, "rows").is_none(),
        "an out-of-range remove must not reflush the array"
    );

    let out = host.call("wipe", &[]).expect("clear ok");
    assert_eq!(rows(&out.commands, "rows"), Some(Vec::new()));
}

/// An item that is not a record is carried as a one-field `value` row, so a
/// list of plain strings still binds through `{value}` in markup.
#[test]
fn scalar_items_land_in_a_value_field() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn seed() { lumen::signal_array_set("tags", ["red", "blue"]); }
fn main() {}
"#;
    host.load(src, "scalars.cdl").expect("compiles");
    let out = host.call("seed", &[]).expect("seed ok");
    let items = rows(&out.commands, "tags").expect("a SetArray for tags");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].get("value").map(String::as_str), Some("red"));
}

/// The prelude's `signal_array(name)` handle reaches the same store: every
/// method forwards to the matching name-keyed builtin.
#[test]
fn the_array_signal_handle_drives_the_same_store() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    let rows = signal_array("rows");
    let first = {"id": "a", "title": "First"};
    let second = {"id": "b", "title": "Second"};
    rows.push(first);
    rows.push(second);
}
fn count() { let rows = signal_array("rows"); return rows.len(); }
fn first_title() {
    let rows = signal_array("rows");
    let row = as_map(rows.get(0));
    return as_str(row.get("title"));
}
fn drop_first() { let rows = signal_array("rows"); rows.remove(0); }
fn wipe() { let rows = signal_array("rows"); rows.clear(); }
fn main() {}
"#;
    host.load(src, "sugar.cdl")
        .expect("compiles via the prelude");

    let out = host.call("seed", &[]).expect("seed ok");
    let items = rows(&out.commands, "rows").expect("a SetArray for rows");
    assert_eq!(items.len(), 2);
    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(2))
    );
    assert_eq!(
        host.call("first_title", &[]).unwrap().ret,
        Some(ScriptValue::Str("First".to_owned()))
    );

    host.call("drop_first", &[]).expect("remove ok");
    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(1))
    );
    host.call("wipe", &[]).expect("clear ok");
    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(0))
    );
}

/// The array lives in the same signal mirror the scalar signals use, so a host
/// write through `ScriptContext` is visible to the script and the other way
/// round.
#[test]
fn the_mirror_is_shared_with_the_host_side_context() {
    use lumen_script::ScriptContext;

    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn count() { return lumen::signal_array_len("rows"); }
fn main() {}
"#;
    host.load(src, "shared.cdl").expect("compiles");

    let mut ctx = lumen_script_candela::CandelaScriptContext::new(&mut host);
    ctx.array_push(
        "rows",
        ScriptValue::Map(HashMap::from([(
            "id".to_owned(),
            ScriptValue::Str("a".to_owned()),
        )])),
    );

    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(1))
    );
}
