//! Parity guard: every entry in `builtins::BUILTINS` must resolve to a
//! Lua function global on a fresh `LuaHost` (the equivalent of the Rhai
//! host's `gen_fn_signatures` parity check - mlua has no signature
//! introspection, so we probe the globals table directly).

use lumen_script_lua::LuaHost;
use lumen_script_lua::builtins::BUILTINS;
use mlua::{Function, Value};

#[test]
fn every_table_entry_is_a_registered_global_function() {
    let host = LuaHost::new();
    let globals = host.lua().globals();
    for b in BUILTINS {
        let v: Value = globals
            .get(b.name)
            .unwrap_or_else(|e| panic!("failed reading global `{}`: {e}", b.name));
        // `document` graduated to a namespace TABLE in section 4.8
        // (`document.root()` / `document.query(..)`), staying callable via a
        // `__call` metamethod for the phase-1 `document()` form. Everything
        // else is a plain function global.
        if b.name == "document" {
            assert!(
                matches!(v, Value::Table(_)),
                "`document` must be the namespace table; got {v:?}"
            );
            let doc: mlua::Table = globals.get("document").unwrap();
            assert!(matches!(
                doc.get::<Value>("root").unwrap(),
                Value::Function(_)
            ));
            assert!(matches!(
                doc.get::<Value>("query").unwrap(),
                Value::Function(_)
            ));
            continue;
        }
        assert!(
            matches!(v, Value::Function(_)),
            "builtin `{}` is in BUILTINS but not registered as a Lua function global (got {:?})",
            b.name,
            v
        );
        // Sanity: it is fetchable as a Function too.
        let _f: Function = globals
            .get(b.name)
            .unwrap_or_else(|_| panic!("`{}` not a Function", b.name));
    }
}

#[test]
fn web_namespaces_are_registered() {
    // window / document / history are namespace tables (section 4.8), the
    // one object-ish form every host supports.
    let host = LuaHost::new();
    let globals = host.lua().globals();
    for ns in ["window", "document", "history"] {
        let v: Value = globals.get(ns).unwrap_or_else(|_| panic!("`{ns}` global"));
        assert!(
            matches!(v, Value::Table(_)),
            "`{ns}` must be a namespace table; got {v:?}"
        );
    }
    // `create` verb is a plain function global.
    assert!(matches!(
        globals.get::<Value>("create").unwrap(),
        Value::Function(_)
    ));
}

#[test]
fn signals_chained_root_is_registered() {
    // Not a free function (so not in BUILTINS), but the chained accessor
    // root must exist as a userdata global.
    let host = LuaHost::new();
    let v: Value = host.lua().globals().get("signals").expect("signals global");
    assert!(
        matches!(v, Value::UserData(_)),
        "`signals` chained-access root must be a userdata global; got {v:?}"
    );
}
