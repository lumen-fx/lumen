//! Lua (mlua / Lua 5.4) implementation of [`lumen_script::ScriptHost`].
//!
//! A selectable alternative to `lumen-script-rhai`, exposing the SAME
//! engine-function surface (same builtin names + semantics) so an app
//! author can write handlers and logic in Lua instead of Rhai. Rhai
//! stays the default/compat host; this crate is purely additive.
//!
//! Like the Rhai host, this crate is engine + builtins + value
//! conversion only. All host-generic machinery - the event dispatch
//! surface, the derivation fixed-point driver, the store->mirror sync
//! driver, timers, fetch, the load-failure banner, and the tick wiring -
//! lives in `lumen-script` as `ScriptPlugin<H: ScriptHost>`; the
//! items are re-exported here so embedders keep their import paths.
//!
//! The host owns an `mlua::Lua`, a shared `Arc<Mutex<Vec<ScriptCommand>>>`
//! command sink the registered builtins push into, the signal mirror,
//! and the per-id handler + derivation registries. The generic runtime
//! drives all of them through the [`lumen_script::ScriptHost`] trait.
//!
//! ## Lua idiom vs Rhai idiom
//!
//! - `Signal` / `ArraySignal` handles use Lua **method** (colon) calls:
//!   `local c = signal("count", 0); c:set(c:get() + 1)`.
//! - The chained `signals` accessor uses **dot** (or colon) terminal
//!   methods, matching the Rhai form: `signals.count.set(5)` /
//!   `signals.user.name.set("Alice")` / `signals.bg.set_color("#ff8800")`.
//! - Arrays/tables are 1-indexed (Lua native); `ArraySignal:get(i)` and
//!   the numeric `signals.users[i]` subscript are 1-based.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod audio;
pub mod builtins;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_script::{
    CallOutcome, CommandFn, ScriptCommand, ScriptContext, ScriptError, ScriptHost, ScriptValue,
};
use mlua::{
    Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value as LuaValue, Variadic,
};
use parking_lot::Mutex;

// Host-generic runtime re-exports - mirror lumen-script-rhai so embedders
// (lumenc, tests) can import the runtime system set + plugin from this
// crate. The system fns are generic over the host: instantiate as e.g.
// `tick_script::<LuaHost>` when expressing ordering constraints.
pub use lumen_script::{
    FetchRegistry, ScriptCommandEvent, ScriptLoadFailure, ScriptPlugin, ScriptStartedAt,
    TimerRegistry, apply_derivations, dispatch_clicks_and_doubles, dispatch_close_to_script,
    drain_fetch_commands, drain_timer_commands, fire_due_timers, fire_fetched_responses,
    reload_script, sync_signals_into_host, tick_script,
};

// ---------------------------------------------------------------------
// Shared registry aliases
// ---------------------------------------------------------------------

/// Shared command sink builtins push into; drained each tick.
type Sink = Arc<Mutex<Vec<ScriptCommand>>>;
/// Host-local rich-typed signal mirror. Stores host-neutral
/// [`ScriptValue`]s (Lua values marshal cleanly to/from these - nil,
/// bool, integer, number, string, array/table, map/table).
type SignalMirror = Arc<Mutex<HashMap<String, ScriptValue>>>;
/// Per-event handler registry: `(event, id) -> fn_name`. `RwLock` so the
/// hot dispatch path takes a read lock; the only writers are the `on()`
/// builtin and the hot-reload swap.
type HandlerMap = Arc<RwLock<HashMap<(String, String), String>>>;
/// Derived-signal registry: `name -> (dep names, closure)`. The closure
/// is a Lua [`Function`] (`Send + Sync + 'static` under mlua's `send`
/// feature). See [`LuaHost`]'s `Drop` for the reference-cycle note.
type DerivationMap = Arc<Mutex<HashMap<String, (Vec<String>, Function)>>>;
/// Names of derivations registered but never evaluated.
type PendingSet = Arc<Mutex<HashSet<String>>>;

// ---------------------------------------------------------------------
// Value conversion helpers
// ---------------------------------------------------------------------

/// Owned-string extraction from an `mlua::LuaString` (lossless UTF-8 path,
/// empty on the rare non-UTF-8 case).
fn lua_string(s: &mlua::LuaString) -> String {
    s.to_str().map(|b| b.to_string()).unwrap_or_default()
}

/// Translate a [`ScriptValue`] into an `mlua::Value`. Arrays become
/// 1-indexed sequence tables; maps become string-keyed tables.
fn script_value_to_lua(lua: &Lua, v: &ScriptValue) -> mlua::Result<LuaValue> {
    Ok(match v {
        ScriptValue::Unit => LuaValue::Nil,
        ScriptValue::Bool(b) => LuaValue::Boolean(*b),
        ScriptValue::I64(i) => LuaValue::Integer(*i),
        ScriptValue::F64(f) => LuaValue::Number(*f),
        ScriptValue::Str(s) => LuaValue::String(lua.create_string(s)?),
        ScriptValue::Array(arr) => {
            let t = lua.create_table()?;
            for (i, e) in arr.iter().enumerate() {
                t.set(i as i64 + 1, script_value_to_lua(lua, e)?)?;
            }
            LuaValue::Table(t)
        }
        ScriptValue::Map(m) => {
            let t = lua.create_table()?;
            for (k, val) in m {
                t.set(k.as_str(), script_value_to_lua(lua, val)?)?;
            }
            LuaValue::Table(t)
        }
    })
}

/// Translate an `mlua::Value` into a [`ScriptValue`]. Functions /
/// userdata / other non-data values fold to [`ScriptValue::Unit`].
fn lua_value_to_script_value(v: &LuaValue) -> ScriptValue {
    match v {
        LuaValue::Nil => ScriptValue::Unit,
        LuaValue::Boolean(b) => ScriptValue::Bool(*b),
        LuaValue::Integer(i) => ScriptValue::I64(*i),
        LuaValue::Number(n) => ScriptValue::F64(*n),
        LuaValue::String(s) => ScriptValue::Str(lua_string(s)),
        LuaValue::Table(t) => lua_table_to_script_value(t),
        _ => ScriptValue::Unit,
    }
}

/// A table with a non-empty sequence part (`t[1..=n]`) becomes an
/// [`ScriptValue::Array`]; otherwise its string/number keys become an
/// [`ScriptValue::Map`]. An empty table folds to an empty array.
fn lua_table_to_script_value(t: &Table) -> ScriptValue {
    let len = t.raw_len() as i64;
    if len > 0 {
        let mut arr = Vec::with_capacity(len as usize);
        for i in 1..=len {
            let v: LuaValue = t.get(i).unwrap_or(LuaValue::Nil);
            arr.push(lua_value_to_script_value(&v));
        }
        ScriptValue::Array(arr)
    } else {
        let mut map = HashMap::new();
        for pair in t.clone().pairs::<LuaValue, LuaValue>().flatten() {
            let (k, v) = pair;
            let key = match &k {
                LuaValue::String(s) => lua_string(s),
                LuaValue::Integer(i) => i.to_string(),
                LuaValue::Number(n) => n.to_string(),
                _ => continue,
            };
            map.insert(key, lua_value_to_script_value(&v));
        }
        if map.is_empty() {
            ScriptValue::Array(Vec::new())
        } else {
            ScriptValue::Map(map)
        }
    }
}

/// Lua-canonical float rendering: integral floats keep a trailing `.0`
/// (Lua's `tostring(10.0)` == "10.0"), matching what a Lua author sees
/// when they concatenate a number into a `bind-text` string.
fn lua_num_string(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// Host-canonical stringify of a [`ScriptValue`] for the ECS signal
/// mirror. Uses Lua float rendering for scalars; strings verbatim.
fn lua_stringify(v: &ScriptValue) -> String {
    match v {
        ScriptValue::F64(f) => lua_num_string(*f),
        other => other.stringify(),
    }
}

/// Stringify an `mlua::Value` directly (used by `print` and the native
/// derivation path so the store string is Lua-canonical without a lossy
/// [`ScriptValue`] round-trip for scalars).
fn lua_value_stringify(v: &LuaValue) -> String {
    match v {
        LuaValue::Nil => String::new(),
        LuaValue::Boolean(b) => b.to_string(),
        LuaValue::Integer(i) => i.to_string(),
        LuaValue::Number(n) => lua_num_string(*n),
        LuaValue::String(s) => lua_string(s),
        LuaValue::Table(_) => lua_stringify(&lua_value_to_script_value(v)),
        _ => String::new(),
    }
}

/// Parse a `"#rrggbb"` / `"#rrggbbaa"` hex color into RGBA bytes. Leading
/// `#` optional. `None` when the input matches neither shape.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8, u8)> {
    let s = s.strip_prefix('#').unwrap_or(s);
    let (r, g, b, a) = match s.len() {
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            0xffu8,
        ),
        8 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some((r, g, b, a))
}

/// Project a typed [`PropertyValue`] into a host-neutral [`ScriptValue`].
/// Color becomes a `{ r, g, b, a }` map of 0-255 ints; `Vec2` / `Custom`
/// have no script projection and land as [`ScriptValue::Unit`].
fn property_value_to_script_value(v: &PropertyValue) -> ScriptValue {
    match v {
        PropertyValue::I64(n) => ScriptValue::I64(*n),
        PropertyValue::F64(f) => ScriptValue::F64(*f),
        PropertyValue::Bool(b) => ScriptValue::Bool(*b),
        PropertyValue::Color(c) => ScriptValue::Map(color_map(c.r, c.g, c.b, c.a)),
        PropertyValue::Str(s) => ScriptValue::Str(s.to_string()),
        PropertyValue::Vec2(_) | PropertyValue::Custom(_) => ScriptValue::Unit,
    }
}

/// Build a `{ r, g, b, a }` (0-255 int) [`ScriptValue::Map`] from linear
/// 0..1 channel floats.
fn color_map(r: f32, g: f32, b: f32, a: f32) -> HashMap<String, ScriptValue> {
    let mut m = HashMap::with_capacity(4);
    m.insert("r".into(), ScriptValue::I64((r * 255.0).round() as i64));
    m.insert("g".into(), ScriptValue::I64((g * 255.0).round() as i64));
    m.insert("b".into(), ScriptValue::I64((b * 255.0).round() as i64));
    m.insert("a".into(), ScriptValue::I64((a * 255.0).round() as i64));
    m
}

/// Recursively translate a [`serde_json::Value`] into an `mlua::Value`
/// (numbers -> integer or float, objects -> string-keyed tables, arrays ->
/// 1-indexed tables). Backs the `parse_json` builtin.
fn json_to_lua(lua: &Lua, v: serde_json::Value) -> mlua::Result<LuaValue> {
    Ok(match v {
        serde_json::Value::Null => LuaValue::Nil,
        serde_json::Value::Bool(b) => LuaValue::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                LuaValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                LuaValue::Number(f)
            } else {
                LuaValue::Nil
            }
        }
        serde_json::Value::String(s) => LuaValue::String(lua.create_string(&s)?),
        serde_json::Value::Array(arr) => {
            let t = lua.create_table()?;
            for (i, e) in arr.into_iter().enumerate() {
                t.set(i as i64 + 1, json_to_lua(lua, e)?)?;
            }
            LuaValue::Table(t)
        }
        serde_json::Value::Object(obj) => {
            let t = lua.create_table()?;
            for (k, val) in obj {
                t.set(k, json_to_lua(lua, val)?)?;
            }
            LuaValue::Table(t)
        }
    })
}

// ---------------------------------------------------------------------
// Lua-facing signal handles (UserData)
// ---------------------------------------------------------------------

/// Lua-facing handle to one named scalar signal. Returned by the
/// `signal(name, default)` builtin. Method (colon) access:
/// `sig:get()` reads the mirror; `sig:set(v)` writes the mirror (so
/// same-tick reads see the new value) and queues a
/// `ScriptCommand::SetSignal` so markup `bind-text` observes the
/// stringified form next tick.
#[derive(Clone)]
pub struct Signal {
    name: String,
    signals: SignalMirror,
    sink: Sink,
}

impl UserData for Signal {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |lua, this, ()| {
            let v = this
                .signals
                .lock()
                .get(&this.name)
                .cloned()
                .unwrap_or(ScriptValue::Unit);
            script_value_to_lua(lua, &v)
        });
        methods.add_method("set", |_, this, value: LuaValue| {
            let sv = lua_value_to_script_value(&value);
            let text = lua_stringify(&sv);
            this.signals.lock().insert(this.name.clone(), sv);
            this.sink.lock().push(ScriptCommand::SetSignal {
                name: this.name.clone(),
                value: text,
            });
            Ok(())
        });
    }
}

/// Lua-facing handle to one named reactive array. Returned by
/// `signal_array(name)`. Each item is a record (string-keyed table);
/// `set` / `push` flush a `ScriptCommand::SetArray` driving
/// `<for each="name">` reconciliation.
#[derive(Clone)]
pub struct ArraySignal {
    name: String,
    signals: SignalMirror,
    sink: Sink,
}

impl ArraySignal {
    fn items_clone(&self) -> Vec<ScriptValue> {
        match self.signals.lock().get(&self.name) {
            Some(ScriptValue::Array(a)) => a.clone(),
            _ => Vec::new(),
        }
    }

    fn store_and_flush(&self, items: Vec<ScriptValue>) {
        let mut records: Vec<HashMap<String, String>> = Vec::with_capacity(items.len());
        for item in &items {
            if let ScriptValue::Map(m) = item {
                let mut record = HashMap::with_capacity(m.len());
                for (k, v) in m {
                    record.insert(k.clone(), lua_stringify(v));
                }
                records.push(record);
            }
        }
        self.signals
            .lock()
            .insert(self.name.clone(), ScriptValue::Array(items));
        self.sink.lock().push(ScriptCommand::SetArray {
            name: self.name.clone(),
            items: records,
        });
    }
}

impl UserData for ArraySignal {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("set", |_, this, arr: Table| {
            let items = match lua_table_to_script_value(&arr) {
                ScriptValue::Array(a) => a,
                other => vec![other],
            };
            this.store_and_flush(items);
            Ok(())
        });
        methods.add_method("push", |_, this, item: LuaValue| {
            let mut items = this.items_clone();
            items.push(lua_value_to_script_value(&item));
            this.store_and_flush(items);
            Ok(())
        });
        methods.add_method("len", |_, this, ()| Ok(this.items_clone().len() as i64));
        // 1-indexed (Lua native): `arr:get(1)` is the first item.
        methods.add_method("get", |lua, this, index: i64| {
            let items = this.items_clone();
            if index < 1 {
                return Ok(LuaValue::Nil);
            }
            match items.get((index - 1) as usize) {
                Some(v) => script_value_to_lua(lua, v),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("all", |lua, this, ()| {
            script_value_to_lua(lua, &ScriptValue::Array(this.items_clone()))
        });
    }
}

// ---------------------------------------------------------------------
// Lua-facing DOM node handles (UserData)
// ---------------------------------------------------------------------

/// Lua handle to a live element (`Node`). Wraps the packed `u64` handle
/// the host-neutral query surface returns. Phase 1 exposes traversal +
/// liveness (`node:parent()`, `node:closest(sel)`, `node:exists()`).
#[derive(Clone, Copy)]
pub struct Node {
    handle: u64,
}

/// Lua `NodeQuery` result set: packed handles in document order, with the
/// Bevy-flavored consumers.
#[derive(Clone)]
pub struct NodeQuery {
    nodes: Vec<u64>,
}

impl mlua::FromLua for Node {
    fn from_lua(value: LuaValue, _lua: &Lua) -> mlua::Result<Self> {
        match value {
            LuaValue::UserData(ud) => Ok(*ud.borrow::<Node>()?),
            other => Err(mlua::Error::runtime(format!(
                "expected a Node, got {}",
                other.type_name()
            ))),
        }
    }
}

fn nodes_to_lua_table(lua: &Lua, handles: Vec<u64>) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    for (i, h) in handles.into_iter().enumerate() {
        tbl.set(i as i64 + 1, Node { handle: h })?;
    }
    Ok(tbl)
}

/// Lua handle to the current event delivered to an `on(...)` handler
/// (phase 4). Zero-sized: every accessor reads the process-global
/// current-event cell in [`lumen_script::event`].
#[derive(Debug, Clone, Copy)]
pub struct LuaEvent;

impl UserData for LuaEvent {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        use lumen_script::event as ev;
        methods.add_method("target", |_, _this, ()| {
            Ok(Node {
                handle: ev::event_target(),
            })
        });
        methods.add_method("current_target", |_, _this, ()| {
            Ok(Node {
                handle: ev::event_current_target(),
            })
        });
        methods.add_method("event_type", |_, _this, ()| Ok(ev::event_type()));
        methods.add_method("key", |_, _this, ()| Ok(ev::event_key()));
        methods.add_method("value", |_, _this, ()| Ok(ev::event_value()));
        methods.add_method("button", |_, _this, ()| Ok(ev::event_button()));
        methods.add_method("x", |_, _this, ()| Ok(ev::event_position_local().0));
        methods.add_method("y", |_, _this, ()| Ok(ev::event_position_local().1));
        methods.add_method("client_x", |_, _this, ()| Ok(ev::event_position_client().0));
        methods.add_method("client_y", |_, _this, ()| Ok(ev::event_position_client().1));
        methods.add_method("delta_x", |_, _this, ()| Ok(ev::event_delta().0));
        methods.add_method("delta_y", |_, _this, ()| Ok(ev::event_delta().1));
        methods.add_method("position", |lua, _this, ()| {
            let (x, y) = ev::event_position_local();
            let (cx, cy) = ev::event_position_client();
            let t = lua.create_table()?;
            t.set("x", x)?;
            t.set("y", y)?;
            t.set("client_x", cx)?;
            t.set("client_y", cy)?;
            Ok(t)
        });
        methods.add_method("modifiers", |lua, _this, ()| {
            let (shift, ctrl, alt, super_) = ev::event_modifiers();
            let t = lua.create_table()?;
            t.set("shift", shift)?;
            t.set("ctrl", ctrl)?;
            t.set("alt", alt)?;
            t.set("super", super_)?;
            Ok(t)
        });
        methods.add_method("prevent_default", |_, _this, ()| {
            ev::event_prevent_default();
            Ok(())
        });
        methods.add_method("stop_propagation", |_, _this, ()| {
            ev::event_stop_propagation();
            Ok(())
        });
        methods.add_method("stop_immediate_propagation", |_, _this, ()| {
            ev::event_stop_immediate_propagation();
            Ok(())
        });
    }
}

/// Name of the Lua global table holding `token -> handler function` for
/// phase-4 event bindings. A `Node` `on(...)` method has no way to capture
/// the per-host closure map, so the handler lives in the Lua state itself
/// (keyed by token) and the host retrieves it by token at dispatch.
const LUA_HANDLERS: &str = "__lumen_event_handlers";

/// Build a Lua string-keyed table from `(key, value)` string pairs, for the
/// introspection map getters (`computed_style()`, `attrs()`, ...).
fn kv_table(lua: &Lua, pairs: Vec<(String, String)>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (k, v) in pairs {
        t.set(k, v)?;
    }
    Ok(t)
}

impl UserData for Node {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        use lumen_script::node_query;
        methods.add_method("parent", |_, this, ()| {
            Ok(node_query::node_parent(this.handle).map(|h| Node { handle: h }))
        });
        methods.add_method("first_child", |_, this, ()| {
            Ok(node_query::node_first_child(this.handle).map(|h| Node { handle: h }))
        });
        methods.add_method("last_child", |_, this, ()| {
            Ok(node_query::node_last_child(this.handle).map(|h| Node { handle: h }))
        });
        methods.add_method("next", |_, this, ()| {
            Ok(node_query::node_next(this.handle).map(|h| Node { handle: h }))
        });
        methods.add_method("prev", |_, this, ()| {
            Ok(node_query::node_prev(this.handle).map(|h| Node { handle: h }))
        });
        methods.add_method("children", |lua, this, ()| {
            nodes_to_lua_table(lua, node_query::node_children(this.handle))
        });
        methods.add_method("closest", |_, this, sel: String| {
            node_query::node_closest(this.handle, &sel)
                .map(|opt| opt.map(|h| Node { handle: h }))
                .map_err(mlua::Error::runtime)
        });
        methods.add_method("exists", |_, this, ()| {
            Ok(node_query::node_valid(this.handle))
        });
        methods.add_method("valid", |_, this, ()| {
            Ok(node_query::node_valid(this.handle))
        });
        methods.add_method("handle", |_, this, ()| Ok(this.handle as i64));

        // -- mutators (phases 2 + 3) -----------------------------------
        //
        // Lua `UserData` methods cannot capture the per-host sink, so
        // mutations route through the process-global external DOM bus.
        // The runtime drains it into the same FIFO applier as the sink,
        // this same tick, so a fluent chain still materializes together.
        // Every mutator returns the receiver so `n:set_attr(..):set_text(..)`
        // chains; read-backs return a value.
        macro_rules! mutate {
            ($name:literal, |$this:ident $(, $arg:ident : $ty:ty)*| $build:expr) => {
                methods.add_method($name, |_, $this, ($($arg,)*): ($($ty,)*)| {
                    lumen_script::node_query::push_external_dom_command($build);
                    Ok(*$this)
                });
            };
        }
        mutate!("set_attr", |this, name: String, value: String| {
            ScriptCommand::SetAttr {
                node: this.handle,
                name,
                value,
            }
        });
        mutate!("remove_attr", |this, name: String| {
            ScriptCommand::RemoveAttr {
                node: this.handle,
                name,
            }
        });
        mutate!("set_id", |this, id: String| {
            ScriptCommand::SetAttr {
                node: this.handle,
                name: "id".to_string(),
                value: id,
            }
        });
        mutate!("set_text", |this, text: String| {
            ScriptCommand::SetNodeText {
                node: this.handle,
                text,
            }
        });
        // Guarded markup injection (design 4.4). Do not feed untrusted content.
        mutate!("set_inner_markup", |this, markup: String| {
            ScriptCommand::SetInnerMarkup {
                node: this.handle,
                markup,
            }
        });
        mutate!("add_class", |this, class: String| {
            ScriptCommand::ClassAdd {
                node: this.handle,
                class,
            }
        });
        mutate!("remove_class", |this, class: String| {
            ScriptCommand::ClassRemove {
                node: this.handle,
                class,
            }
        });
        mutate!("toggle_class", |this, class: String| {
            ScriptCommand::ClassToggle {
                node: this.handle,
                class,
            }
        });
        mutate!("set_class", |this, classes: String| {
            ScriptCommand::SetAttr {
                node: this.handle,
                name: "class".to_string(),
                value: classes,
            }
        });
        mutate!("set_style", |this, name: String, value: String| {
            ScriptCommand::SetStyleProp {
                node: this.handle,
                name,
                value,
            }
        });
        mutate!("style_set", |this, name: String, value: String| {
            ScriptCommand::SetStyleProp {
                node: this.handle,
                name,
                value,
            }
        });
        mutate!("style_remove", |this, name: String| {
            ScriptCommand::RemoveStyleProp {
                node: this.handle,
                name,
            }
        });
        mutate!("set_parent", |this, parent: Node| {
            ScriptCommand::Insert {
                parent: parent.handle,
                node: this.handle,
                before: 0,
            }
        });
        mutate!("move_to", |this, parent: Node| {
            ScriptCommand::Insert {
                parent: parent.handle,
                node: this.handle,
                before: 0,
            }
        });
        mutate!("append", |this, child: Node| {
            ScriptCommand::Insert {
                parent: this.handle,
                node: child.handle,
                before: 0,
            }
        });
        mutate!("insert_before", |this, child: Node, reference: Node| {
            ScriptCommand::Insert {
                parent: this.handle,
                node: child.handle,
                before: reference.handle,
            }
        });
        methods.add_method("replace_with", |_, this, new: Node| {
            lumen_script::node_query::push_external_dom_command(ScriptCommand::ReplaceWith {
                old: this.handle,
                new: new.handle,
            });
            Ok(new)
        });
        methods.add_method("remove", |_, this, ()| {
            lumen_script::node_query::push_external_dom_command(ScriptCommand::RemoveNode {
                node: this.handle,
            });
            Ok(())
        });
        methods.add_method("clone_deep", |_, this, ()| {
            let (handle, cmd) = node_query::build_clone(this.handle);
            lumen_script::node_query::push_external_dom_command(cmd);
            Ok(Node { handle })
        });

        // Read-backs (end the chain).
        methods.add_method("get_attr", |_, this, name: String| {
            Ok(node_query::node_get_attr(this.handle, &name))
        });
        methods.add_method("id", |_, this, ()| Ok(node_query::node_id(this.handle)));
        methods.add_method("text", |_, this, ()| Ok(node_query::node_text(this.handle)));
        methods.add_method("has_class", |_, this, class: String| {
            Ok(node_query::node_class_contains(this.handle, &class))
        });
        methods.add_method("style_get", |_, this, name: String| {
            Ok(node_query::node_style_get(this.handle, &name))
        });
        // `computed_style(prop)` returns one resolved value; `computed_style()`
        // (no arg) returns the full property map (design 4.7).
        methods.add_method("computed_style", |lua, this, name: Option<String>| {
            use lumen_script::introspect as ins;
            match name {
                Some(prop) => Ok(LuaValue::String(
                    match node_query::node_computed_style(this.handle, &prop) {
                        Some(v) => lua.create_string(&v)?,
                        None => return Ok(LuaValue::Nil),
                    },
                )),
                None => Ok(LuaValue::Table(kv_table(
                    lua,
                    ins::node_computed_style_map(this.handle),
                )?)),
            }
        });

        // -- low-level introspection (phase 5) ----------------------------
        methods.add_method("rect", |lua, this, ()| {
            use lumen_script::introspect as ins;
            match ins::node_rect(this.handle) {
                Some(r) => {
                    let t = lua.create_table()?;
                    t.set("x", r.x)?;
                    t.set("y", r.y)?;
                    t.set("width", r.width)?;
                    t.set("height", r.height)?;
                    t.set("client_x", r.client_x)?;
                    t.set("client_y", r.client_y)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("content_rect", |lua, this, ()| {
            use lumen_script::introspect as ins;
            match ins::node_content_rect(this.handle) {
                Some(r) => {
                    let t = lua.create_table()?;
                    t.set("x", r.x)?;
                    t.set("y", r.y)?;
                    t.set("width", r.width)?;
                    t.set("height", r.height)?;
                    t.set("client_x", r.client_x)?;
                    t.set("client_y", r.client_y)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("scroll", |lua, this, ()| {
            use lumen_script::introspect as ins;
            match ins::node_scroll(this.handle) {
                Some(s) => {
                    let t = lua.create_table()?;
                    t.set("x", s.x)?;
                    t.set("y", s.y)?;
                    t.set("max_x", s.max_x)?;
                    t.set("max_y", s.max_y)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("is_visible", |_, this, ()| {
            Ok(lumen_script::introspect::node_is_visible(this.handle))
        });
        methods.add_method("z_index", |_, this, ()| {
            Ok(lumen_script::introspect::node_z_index(this.handle) as i64)
        });
        methods.add_method("inline_style", |lua, this, ()| {
            kv_table(
                lua,
                lumen_script::introspect::node_inline_style(this.handle),
            )
        });
        methods.add_method("attrs", |lua, this, ()| {
            kv_table(lua, lumen_script::introspect::node_attrs(this.handle))
        });
        methods.add_method("classes", |lua, this, ()| {
            let t = lua.create_table()?;
            for (i, c) in lumen_script::introspect::node_classes(this.handle)
                .into_iter()
                .enumerate()
            {
                t.set(i as i64 + 1, c)?;
            }
            Ok(t)
        });
        methods.add_method("matched_rules", |lua, this, ()| {
            let out = lua.create_table()?;
            for (i, r) in lumen_script::introspect::node_matched_rules(this.handle)
                .into_iter()
                .enumerate()
            {
                let t = lua.create_table()?;
                t.set("selector", r.selector)?;
                let spec = lua.create_table()?;
                spec.set(1, r.specificity.0 as i64)?;
                spec.set(2, r.specificity.1 as i64)?;
                spec.set(3, r.specificity.2 as i64)?;
                t.set("specificity", spec)?;
                t.set("source", r.source)?;
                t.set("source_order", r.source_order as i64)?;
                t.set("declarations", kv_table(lua, r.declarations)?)?;
                out.set(i as i64 + 1, t)?;
            }
            Ok(out)
        });
        methods.add_method("entity_id", |lua, this, ()| {
            match lumen_script::introspect::node_entity_id(this.handle) {
                Some((index, generation)) => {
                    let t = lua.create_table()?;
                    t.set("index", index as i64)?;
                    t.set("generation", generation as i64)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("components", |lua, this, ()| {
            let t = lua.create_table()?;
            for (i, n) in lumen_script::introspect::node_components(this.handle)
                .into_iter()
                .enumerate()
            {
                t.set(i as i64 + 1, n)?;
            }
            Ok(t)
        });
        methods.add_method("component", |lua, this, name: String| {
            match lumen_script::introspect::node_component(this.handle, &name) {
                Ok(Some(map)) => Ok(LuaValue::Table(kv_table(lua, map)?)),
                Ok(None) => Ok(LuaValue::Nil),
                Err(e) => Err(mlua::Error::runtime(e)),
            }
        });
        methods.add_method("outer_markup", |_, this, ()| {
            Ok(lumen_script::introspect::outer_markup(this.handle))
        });
        methods.add_method("inner_markup", |_, this, ()| {
            Ok(lumen_script::introspect::inner_markup(this.handle))
        });

        // -- events (phase 4) ---------------------------------------------
        //
        // `on(type, handler)` / `on(type, handler, capture)` bind a Lua
        // closure to the node; `on_capture(type, handler)` is the
        // capture-phase form. The handler is stashed in the Lua global
        // handler table keyed by token (UserData methods cannot capture the
        // host), the binding metadata rides the external DOM bus as a
        // `BindEvent`, and the call returns an `off()` closure that unbinds.
        methods.add_method(
            "on",
            |lua, this, (etype, handler, capture): (String, Function, Option<bool>)| {
                bind_lua_event(lua, this.handle, &etype, capture.unwrap_or(false), handler)
            },
        );
        methods.add_method(
            "on_capture",
            |lua, this, (etype, handler): (String, Function)| {
                bind_lua_event(lua, this.handle, &etype, true, handler)
            },
        );
    }
}

/// Stash `handler` in the Lua handler table, emit a `BindEvent`, and return
/// an `off()` closure that unbinds (removes the handler + emits
/// `UnbindEvent`). Shared by `on` / `on_capture`.
fn bind_lua_event(
    lua: &Lua,
    node: u64,
    event_type: &str,
    capture: bool,
    handler: Function,
) -> mlua::Result<Function> {
    let token = lumen_script::event::mint_event_token();
    let table = lua_handler_table(lua)?;
    table.set(token as i64, handler)?;
    lumen_script::node_query::push_external_dom_command(ScriptCommand::BindEvent {
        node,
        event_type: event_type.to_string(),
        capture,
        token,
    });
    lua.create_function(move |lua, ()| {
        if let Ok(t) = lua_handler_table(lua) {
            let _ = t.set(token as i64, LuaValue::Nil);
        }
        lumen_script::node_query::push_external_dom_command(ScriptCommand::UnbindEvent { token });
        Ok(())
    })
}

/// Get (creating if absent) the Lua global handler table.
fn lua_handler_table(lua: &Lua) -> mlua::Result<Table> {
    let globals = lua.globals();
    match globals.get::<Option<Table>>(LUA_HANDLERS)? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            globals.set(LUA_HANDLERS, &t)?;
            Ok(t)
        }
    }
}

impl UserData for NodeQuery {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("len", |_, this, ()| Ok(this.nodes.len() as i64));
        methods.add_method("is_empty", |_, this, ()| Ok(this.nodes.is_empty()));
        methods.add_method("first", |_, this, ()| {
            Ok(this.nodes.first().copied().map(|h| Node { handle: h }))
        });
        methods.add_method("nth", |_, this, i: i64| {
            Ok(usize::try_from(i)
                .ok()
                .and_then(|i| this.nodes.get(i).copied())
                .map(|h| Node { handle: h }))
        });
        methods.add_method("iter", |lua, this, ()| {
            nodes_to_lua_table(lua, this.nodes.clone())
        });
        methods.add_method("collect", |lua, this, ()| {
            nodes_to_lua_table(lua, this.nodes.clone())
        });
        methods.add_method("single", |_, this, ()| match this.nodes.len() {
            1 => Ok(Node {
                handle: this.nodes[0],
            }),
            n => Err(mlua::Error::runtime(format!(
                "query:single(): expected exactly 1 match, found {n}"
            ))),
        });
        methods.add_method("get_single", |_, this, ()| {
            Ok(if this.nodes.len() == 1 {
                Some(Node {
                    handle: this.nodes[0],
                })
            } else {
                None
            })
        });
    }
}

/// Lua-facing chained-access handle for the typed property bus. A
/// `signals` userdata with an empty path is installed as a global; each
/// `.name` / `[i]` step returns a fresh `SignalRef` with the segment
/// appended (via the `__index` metamethod), and the terminal `.set(v)` /
/// `.get()` / `.set_color(hex)` joins the path into one
/// `PropertyKey::Global` and routes through `push_external_property`.
///
/// Name segments are `.`-joined; index segments render as `[N]` and
/// attach directly, so `signals.users[1].name` yields the key
/// `"users[1].name"`. Index subscripts are 1-based (Lua native).
#[derive(Clone)]
pub struct SignalRef {
    path: Vec<String>,
    signals: SignalMirror,
}

impl SignalRef {
    fn to_key(&self) -> String {
        let mut out = String::new();
        for seg in &self.path {
            if seg.starts_with('[') {
                out.push_str(seg);
            } else {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(seg);
            }
        }
        out
    }

    fn do_set(&self, value: &LuaValue) {
        let key = self.to_key();
        let (sv, pv) = match value {
            LuaValue::Integer(i) => (ScriptValue::I64(*i), PropertyValue::I64(*i)),
            LuaValue::Number(n) => (ScriptValue::F64(*n), PropertyValue::F64(*n)),
            LuaValue::Boolean(b) => (ScriptValue::Bool(*b), PropertyValue::Bool(*b)),
            LuaValue::String(s) => {
                let s = lua_string(s);
                (
                    ScriptValue::Str(s.clone()),
                    PropertyValue::Str(Arc::<str>::from(s.as_str())),
                )
            }
            _ => return,
        };
        self.signals.lock().insert(key.clone(), sv);
        lumen_core::property_store::push_external_property(
            PropertyKey::Global(Arc::<str>::from(key.as_str())),
            pv,
        );
    }

    fn do_set_color(&self, hex: &str) {
        if let Some((r, g, b, a)) = parse_hex_color(hex) {
            let key = self.to_key();
            self.signals.lock().insert(
                key.clone(),
                ScriptValue::Map(color_map(
                    r as f32 / 255.0,
                    g as f32 / 255.0,
                    b as f32 / 255.0,
                    a as f32 / 255.0,
                )),
            );
            let color = lumen_core::components::Color::rgba(
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                a as f32 / 255.0,
            );
            lumen_core::property_store::push_external_property(
                PropertyKey::Global(Arc::<str>::from(key.as_str())),
                PropertyValue::Color(color),
            );
        }
    }

    fn do_get(&self, lua: &Lua) -> mlua::Result<LuaValue> {
        let key = self.to_key();
        let typed_key = PropertyKey::Global(Arc::<str>::from(key.as_str()));
        let snapshot = lumen_core::property_store::typed_property_snapshot();
        if let Some(v) = snapshot.get(&typed_key) {
            return script_value_to_lua(lua, &property_value_to_script_value(v));
        }
        let sv = self
            .signals
            .lock()
            .get(&key)
            .cloned()
            .unwrap_or(ScriptValue::Unit);
        script_value_to_lua(lua, &sv)
    }

    fn child_name(&self, name: &str) -> SignalRef {
        let mut path = self.path.clone();
        path.push(name.to_string());
        SignalRef {
            path,
            signals: self.signals.clone(),
        }
    }

    fn child_index(&self, idx: i64) -> SignalRef {
        let mut path = self.path.clone();
        path.push(format!("[{idx}]"));
        SignalRef {
            path,
            signals: self.signals.clone(),
        }
    }
}

impl UserData for SignalRef {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: LuaValue| match key {
            LuaValue::String(s) => {
                let seg = lua_string(&s);
                match seg.as_str() {
                    // Terminal methods. Returned as bound closures so
                    // both dot (`signals.foo.set(5)`) and colon
                    // (`signals.foo:set(5)`) forms work - the value is
                    // the LAST argument either way.
                    "set" => {
                        let this = this.clone();
                        Ok(LuaValue::Function(lua.create_function(
                            move |_, args: Variadic<LuaValue>| {
                                this.do_set(args.last().unwrap_or(&LuaValue::Nil));
                                Ok(())
                            },
                        )?))
                    }
                    "set_color" => {
                        let this = this.clone();
                        Ok(LuaValue::Function(lua.create_function(
                            move |_, args: Variadic<LuaValue>| {
                                if let Some(LuaValue::String(hex)) = args.last() {
                                    this.do_set_color(&lua_string(hex));
                                }
                                Ok(())
                            },
                        )?))
                    }
                    "get" => {
                        let this = this.clone();
                        Ok(LuaValue::Function(lua.create_function(
                            move |lua, _: Variadic<LuaValue>| this.do_get(lua),
                        )?))
                    }
                    _ => Ok(LuaValue::UserData(
                        lua.create_userdata(this.child_name(&seg))?,
                    )),
                }
            }
            LuaValue::Integer(i) => Ok(LuaValue::UserData(
                lua.create_userdata(this.child_index(i))?,
            )),
            _ => Ok(LuaValue::Nil),
        });
    }
}

// ---------------------------------------------------------------------
// Markdown -> block list (parity with the Rhai `parse_markdown` builtin)
// ---------------------------------------------------------------------

/// Translate markdown into a block-record list. Each record carries
/// `id` / `kind` (`h`/`p`/`code`/`li`/`hr`) / `level` / `text` / `lang`.
fn parse_markdown_blocks(src: &str) -> Vec<HashMap<String, ScriptValue>> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    #[derive(Clone, Copy)]
    enum BlockKind {
        Heading(u8),
        Paragraph,
        CodeBlock,
        Item,
    }

    let mut out: Vec<HashMap<String, ScriptValue>> = Vec::new();
    let mut counter: usize = 0;
    let mut cur_kind: Option<BlockKind> = None;
    let mut cur_text = String::new();
    let mut cur_lang = String::new();

    fn record(
        counter: &mut usize,
        kind: &str,
        level: i64,
        text: String,
        lang: String,
    ) -> HashMap<String, ScriptValue> {
        let mut m = HashMap::with_capacity(5);
        m.insert("id".into(), ScriptValue::Str(format!("blk-{counter}")));
        *counter += 1;
        m.insert("kind".into(), ScriptValue::Str(kind.to_string()));
        m.insert("level".into(), ScriptValue::I64(level));
        m.insert("text".into(), ScriptValue::Str(text));
        m.insert("lang".into(), ScriptValue::Str(lang));
        m
    }

    for ev in Parser::new(src) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                cur_kind = Some(BlockKind::Heading(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                }));
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::Paragraph) => {
                cur_kind = Some(BlockKind::Paragraph);
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                cur_kind = Some(BlockKind::CodeBlock);
                cur_text.clear();
                cur_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
            }
            Event::Start(Tag::Item) => {
                cur_kind = Some(BlockKind::Item);
                cur_text.clear();
                cur_lang.clear();
            }
            Event::Start(Tag::Emphasis) | Event::End(TagEnd::Emphasis) => cur_text.push('*'),
            Event::Start(Tag::Strong) | Event::End(TagEnd::Strong) => cur_text.push_str("**"),
            Event::Start(Tag::Strikethrough) | Event::End(TagEnd::Strikethrough) => {
                cur_text.push('~')
            }
            Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::CodeBlock)
            | Event::End(TagEnd::Item) => {
                if let Some(kind) = cur_kind.take() {
                    let text = std::mem::take(&mut cur_text);
                    let lang = std::mem::take(&mut cur_lang);
                    let rec = match kind {
                        BlockKind::Heading(level) => {
                            record(&mut counter, "h", level as i64, text, String::new())
                        }
                        BlockKind::Paragraph => record(&mut counter, "p", 0, text, String::new()),
                        BlockKind::CodeBlock => record(&mut counter, "code", 0, text, lang),
                        BlockKind::Item => record(&mut counter, "li", 0, text, String::new()),
                    };
                    out.push(rec);
                }
            }
            Event::Text(t) => cur_text.push_str(&t),
            Event::Code(t) => {
                cur_text.push('`');
                cur_text.push_str(&t);
                cur_text.push('`');
            }
            Event::SoftBreak => cur_text.push(' '),
            Event::HardBreak => cur_text.push('\n'),
            Event::Rule => {
                out.push(record(&mut counter, "hr", 0, String::new(), String::new()));
            }
            _ => {}
        }
    }
    out
}

/// Parse a `pick_file_filtered` spec (`"Images:png,jpg|All:*"`) into
/// `(label, [exts])` pairs. A literal `*` extension is dropped.
fn parse_dialog_filter_spec(spec: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for group in spec.split('|') {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let (label, exts) = match group.split_once(':') {
            Some((l, e)) => (l.trim().to_string(), e),
            None => (group.to_string(), ""),
        };
        let exts: Vec<String> = exts
            .split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty() && e != "*")
            .collect();
        out.push((label, exts));
    }
    out
}

// ---------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------

/// Lua-backed [`ScriptHost`]. With mlua's `send` feature, `Lua`,
/// `Value`, and `Function` are all `Send + Sync + 'static`, so the host
/// can be a regular bevy [`Resource`] and satisfy `ScriptHost: Send +
/// Sync` - exactly as rhai's `sync` feature does for `RhaiHost`.
#[derive(Resource)]
pub struct LuaHost {
    lua: Lua,
    /// `true` once a program has been successfully loaded.
    loaded: bool,
    sink: Sink,
    signals_local: SignalMirror,
    handlers: HandlerMap,
    derivations: DerivationMap,
    pending_initial: PendingSet,
}

impl Default for LuaHost {
    fn default() -> Self {
        Self::new()
    }
}

impl LuaHost {
    /// Construct a fresh host with the lumen builtins registered.
    pub fn new() -> Self {
        let sink: Sink = Arc::new(Mutex::new(Vec::new()));
        let signals_local: SignalMirror = Arc::new(Mutex::new(HashMap::new()));
        let handlers: HandlerMap = Arc::new(RwLock::new(HashMap::new()));
        let derivations: DerivationMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_initial: PendingSet = Arc::new(Mutex::new(HashSet::new()));

        let lua = build_lua(
            &sink,
            &signals_local,
            &handlers,
            &derivations,
            &pending_initial,
        )
        .expect("register lumen Lua builtins");

        Self {
            lua,
            loaded: false,
            sink,
            signals_local,
            handlers,
            derivations,
            pending_initial,
        }
    }

    /// Immutable access to the inner `mlua::Lua` (parity test / embedder
    /// introspection).
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Mutable access to the inner `mlua::Lua` so embedders can register
    /// additional native globals (FFI hooks, OS bindings, `page()`, ...)
    /// before the script source is loaded. Lumen itself only registers
    /// UI/script primitives.
    pub fn lua_mut(&mut self) -> &mut Lua {
        &mut self.lua
    }

    /// Compile `source` WITHOUT running it (no side effects). Backs
    /// `lumenc check` so a syntactically-broken script fails the check
    /// instead of silently disabling every handler at load.
    pub fn compile_check_uri(&self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.lua
            .load(source)
            .set_name(uri)
            .into_function()
            .map(|_| ())
            .map_err(|e| lua_compile_error(e, uri))
    }

    /// Parse + load `source`, replacing any previously loaded program.
    pub fn load(&mut self, source: &str) -> Result<(), ScriptError> {
        self.load_with_uri(source, "<inline>")
    }

    fn load_with_uri(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        // Compile first (syntax errors -> Compile, no state touched), then
        // run the top level (defines globals; runs `on(...)` / `derive`
        // registrations). Runtime failure -> Runtime.
        let chunk = self
            .lua
            .load(source)
            .set_name(uri)
            .into_function()
            .map_err(|e| lua_compile_error(e, uri))?;
        chunk
            .call::<()>(())
            .map_err(|e| ScriptError::Runtime(e.to_string()))?;
        self.loaded = true;
        Ok(())
    }

    /// Look up a per-id handler installed via `on(event, id, fn)`,
    /// including the template-suffix fallback: a handler registered for
    /// `save` also matches `user-card:save` via the last-`:` suffix.
    pub fn lookup_handler(&self, event: &str, id: &str) -> Option<String> {
        let handlers = self.handlers.read().ok()?;
        if let Some(h) = handlers.get(&(event.to_string(), id.to_string())) {
            return Some(h.clone());
        }
        if let Some(colon) = id.rfind(':') {
            let suffix = &id[colon + 1..];
            if let Some(h) = handlers.get(&(event.to_string(), suffix.to_string())) {
                return Some(h.clone());
            }
        }
        None
    }

    /// Variadic event call returning only the drained commands.
    /// Concrete-caller convenience; the trait entry is
    /// [`ScriptHost::call`].
    pub fn call_event(
        &mut self,
        fn_name: &str,
        args: &[ScriptValue],
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        Ok(self.call(fn_name, args)?.commands)
    }

    /// Put commands back into the sink so they flush on the next tick.
    /// Used after `on_start` fires during plugin build.
    pub fn push_commands_back(&mut self, cmds: Vec<ScriptCommand>) {
        self.sink.lock().extend(cmds);
    }

    /// Compile a fresh chunk and run its top level against the live
    /// engine (hot reload). Compile FIRST (no state touched on parse
    /// error); snapshot handlers/derivations/pending; clear; re-run;
    /// FULL rollback on run failure so the live host keeps the old
    /// registrations.
    pub fn replace_ast(&mut self, source: &str) -> Result<(), ScriptError> {
        self.replace_with_uri(source, "<inline>")
    }

    fn replace_with_uri(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        let chunk = self
            .lua
            .load(source)
            .set_name(uri)
            .into_function()
            .map_err(|e| lua_compile_error(e, uri))?;

        let prior_handlers = self.handlers.read().map(|h| h.clone()).unwrap_or_default();
        let prior_derivations = self.derivations.lock().clone();
        let prior_pending = self.pending_initial.lock().clone();

        if let Ok(mut h) = self.handlers.write() {
            h.clear();
        }
        self.derivations.lock().clear();
        self.pending_initial.lock().clear();
        // Phase-4 event bindings re-register when the body re-runs; clear the
        // old handler table and the host-neutral registry first.
        if let Ok(t) = lua_handler_table(&self.lua) {
            let _ = t.clear();
        }
        lumen_script::event::clear_host_bindings();

        match chunk.call::<()>(()) {
            Ok(()) => {
                self.loaded = true;
                Ok(())
            }
            Err(e) => {
                if let Ok(mut h) = self.handlers.write() {
                    *h = prior_handlers;
                }
                *self.derivations.lock() = prior_derivations;
                *self.pending_initial.lock() = prior_pending;
                Err(ScriptError::Runtime(e.to_string()))
            }
        }
    }

    /// Drop the loaded program and all persistent state. Genuine
    /// restart; hot reload should use [`Self::replace_ast`].
    pub fn reset(&mut self) {
        // Clear the registries first - dropping the stored `Function`s
        // releases their handles to the OLD `Lua`, breaking the
        // engine<->closure reference cycle so the old VM frees when the
        // fresh one replaces it below.
        self.sink.lock().clear();
        self.signals_local.lock().clear();
        if let Ok(mut h) = self.handlers.write() {
            h.clear();
        }
        self.derivations.lock().clear();
        self.pending_initial.lock().clear();
        // The rebuilt Lua below drops the old handler table with the VM;
        // purge the host-neutral event registry to match.
        lumen_script::event::clear_host_bindings();
        match build_lua(
            &self.sink,
            &self.signals_local,
            &self.handlers,
            &self.derivations,
            &self.pending_initial,
        ) {
            Ok(lua) => self.lua = lua,
            Err(e) => {
                tracing::error!(target: "lumen.script.lua", error = %e, "reset: rebuild failed")
            }
        }
        self.loaded = false;
    }

    /// Borrow a [`ScriptContext`] backed by the host's signal mirror +
    /// sink (external systems drive the reactive store without encoding
    /// through the `ScriptCommand` bus).
    pub fn root_context(&mut self) -> LuaScriptContext<'_> {
        LuaScriptContext { host: self }
    }
}

/// Break the engine<->closure reference cycle on drop: stored derivation
/// `Function`s hold handles to `self.lua`, and the engine holds the
/// registered builtins that hold the derivation map - clearing the map
/// drops those `Function`s so the `Lua` frees when the struct drops.
impl Drop for LuaHost {
    fn drop(&mut self) {
        self.derivations.lock().clear();
    }
}

impl ScriptHost for LuaHost {
    type Closure = Function;

    fn compile_check(&self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.compile_check_uri(source, uri)
    }

    fn load(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.load_with_uri(source, uri)
    }

    fn replace(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.replace_with_uri(source, uri)
    }

    fn reset(&mut self) {
        LuaHost::reset(self);
    }

    fn call(&mut self, fn_name: &str, args: &[ScriptValue]) -> Result<CallOutcome, ScriptError> {
        // A missing global (or a non-function global) is silent success:
        // `found: false`, `ret: None`. Commands are drained regardless
        // (builtins invoked before/outside the call may have queued).
        let func: Option<Function> = match self.lua.globals().get::<LuaValue>(fn_name) {
            Ok(LuaValue::Function(f)) => Some(f),
            _ => None,
        };
        let ret = match func {
            None => None,
            Some(f) => {
                let mut lua_args: Vec<LuaValue> = Vec::with_capacity(args.len());
                for a in args {
                    lua_args.push(
                        script_value_to_lua(&self.lua, a)
                            .map_err(|e| ScriptError::Runtime(e.to_string()))?,
                    );
                }
                match f.call::<LuaValue>(Variadic::from_iter(lua_args)) {
                    Ok(v) => Some(lua_value_to_script_value(&v)),
                    Err(e) => {
                        // A handler that queued commands (set_text, set_signal,
                        // audio_play, fetch, ...) and *then* errored must
                        // contribute NO commands: draining only on the success
                        // path would leak them into the sink, where the next
                        // unrelated event's outcome would apply them. Discard
                        // the partial batch before propagating the error.
                        // Mirrors `lumen-script-rhai`'s
                        // `call_event_dyn_with_result`.
                        self.sink.lock().clear();
                        return Err(ScriptError::Runtime(e.to_string()));
                    }
                }
            }
        };
        let commands = std::mem::take(&mut *self.sink.lock());
        Ok(CallOutcome {
            commands,
            found: ret.is_some(),
            ret,
        })
    }

    fn call_closure(
        &mut self,
        closure: &Function,
        args: &[ScriptValue],
    ) -> Result<ScriptValue, ScriptError> {
        let mut lua_args: Vec<LuaValue> = Vec::with_capacity(args.len());
        for a in args {
            lua_args.push(
                script_value_to_lua(&self.lua, a)
                    .map_err(|e| ScriptError::Runtime(e.to_string()))?,
            );
        }
        closure
            .call::<LuaValue>(Variadic::from_iter(lua_args))
            .map(|v| lua_value_to_script_value(&v))
            .map_err(|e| ScriptError::Runtime(e.to_string()))
    }

    fn dispatch_event_handler(&mut self, token: u64) -> Result<bool, ScriptError> {
        let table = match lua_handler_table(&self.lua) {
            Ok(t) => t,
            Err(e) => return Err(ScriptError::Runtime(e.to_string())),
        };
        let handler: Option<Function> = table.get(token as i64).ok();
        let Some(handler) = handler else {
            return Ok(false);
        };
        let event = self
            .lua
            .create_userdata(LuaEvent)
            .map_err(|e| ScriptError::Runtime(e.to_string()))?;
        handler
            .call::<()>(event)
            .map(|_| true)
            .map_err(|e| ScriptError::Runtime(e.to_string()))
    }

    fn drop_event_handler(&mut self, token: u64) {
        if let Ok(t) = lua_handler_table(&self.lua) {
            let _ = t.set(token as i64, LuaValue::Nil);
        }
    }

    /// Native override: dep values come off the mirror, the result is
    /// stringified Lua-canonically (integral floats keep `.0`), and the
    /// rich result is written back to the mirror without a lossy
    /// round-trip.
    fn eval_derivation(
        &mut self,
        closure: &Function,
        deps: &[String],
        name: &str,
    ) -> Result<String, ScriptError> {
        let mut lua_args: Vec<LuaValue> = Vec::with_capacity(deps.len());
        for d in deps {
            let sv = self
                .signals_local
                .lock()
                .get(d)
                .cloned()
                .unwrap_or(ScriptValue::Unit);
            lua_args.push(
                script_value_to_lua(&self.lua, &sv)
                    .map_err(|e| ScriptError::Runtime(e.to_string()))?,
            );
        }
        let value = closure
            .call::<LuaValue>(Variadic::from_iter(lua_args))
            .map_err(|e| ScriptError::Runtime(e.to_string()))?;
        let text = lua_value_stringify(&value);
        self.signals_local
            .lock()
            .insert(name.to_string(), lua_value_to_script_value(&value));
        Ok(text)
    }

    fn drain_commands(&mut self) -> Vec<ScriptCommand> {
        std::mem::take(&mut *self.sink.lock())
    }

    fn push_commands(&mut self, cmds: Vec<ScriptCommand>) {
        self.push_commands_back(cmds);
    }

    fn mirror_get(&self, name: &str) -> Option<ScriptValue> {
        self.signals_local.lock().get(name).cloned()
    }

    fn mirror_set(&mut self, name: &str, value: ScriptValue) {
        self.signals_local.lock().insert(name.to_string(), value);
    }

    fn mirror_sync_str(&mut self, name: &str, value: &str) {
        let mut local = self.signals_local.lock();
        // section 1.3 parse-back by existing mirror type:
        //  - absent / string -> take the store string (skip no-op writes);
        //  - scalar (bool / int / float) -> parse the store string back
        //    into the SAME type; unparseable strings leave it untouched;
        //  - structured (array / map) stays authoritative.
        let next: Option<ScriptValue> = match local.get(name) {
            None => Some(ScriptValue::Str(value.to_string())),
            Some(ScriptValue::Str(s)) => (s != value).then(|| ScriptValue::Str(value.to_string())),
            Some(ScriptValue::Bool(b)) => match value {
                "true" | "1" => (!*b).then_some(ScriptValue::Bool(true)),
                "false" | "0" => (*b).then_some(ScriptValue::Bool(false)),
                _ => None,
            },
            Some(ScriptValue::I64(i)) => value
                .parse::<i64>()
                .ok()
                .filter(|n| n != i)
                .map(ScriptValue::I64),
            Some(ScriptValue::F64(f)) => value
                .parse::<f64>()
                .ok()
                .filter(|n| n != f)
                .map(ScriptValue::F64),
            Some(_) => None,
        };
        if let Some(next) = next {
            local.insert(name.to_string(), next);
        }
    }

    fn handler_for(&self, event: &str, key: &str) -> Option<String> {
        self.lookup_handler(event, key)
    }

    fn derivations_matching(
        &self,
        dirty: &HashSet<&str>,
        pending: &HashSet<String>,
    ) -> Vec<(String, Vec<String>, Function)> {
        self.derivations
            .lock()
            .iter()
            .filter(|(n, (d, _))| {
                pending.contains(n.as_str()) || d.iter().any(|dep| dirty.contains(dep.as_str()))
            })
            .map(|(n, (d, f))| (n.clone(), d.clone(), f.clone()))
            .collect()
    }

    fn pending_initial(&self) -> HashSet<String> {
        self.pending_initial.lock().iter().cloned().collect()
    }

    fn clear_pending(&mut self, evaluated: &[String]) {
        let mut pending = self.pending_initial.lock();
        for name in evaluated {
            pending.remove(name);
        }
    }

    fn register_command_fn(
        &mut self,
        name: &str,
        _arity: usize,
        f: CommandFn,
    ) -> Result<(), ScriptError> {
        // Lua functions are variadic natively - one closure handles any
        // arity (no per-arity dispatch like the Rhai host needs).
        let sink = self.sink.clone();
        let func = self
            .lua
            .create_function(move |_, args: Variadic<LuaValue>| {
                let svs: Vec<ScriptValue> = args.iter().map(lua_value_to_script_value).collect();
                sink.lock().extend(f(&svs));
                Ok(())
            })
            .map_err(|e| ScriptError::Runtime(e.to_string()))?;
        self.lua
            .globals()
            .set(name, func)
            .map_err(|e| ScriptError::Runtime(e.to_string()))
    }

    fn lang(&self) -> &'static str {
        "lua"
    }

    fn builtins(&self) -> &'static [lumen_script::BuiltinFn] {
        builtins::BUILTINS
    }
}

/// [`ScriptContext`] borrowing the live [`LuaHost`] state - reads +
/// writes flow through the same mirror the script side sees.
pub struct LuaScriptContext<'a> {
    host: &'a mut LuaHost,
}

impl<'a> ScriptContext for LuaScriptContext<'a> {
    fn get(&self, name: &str) -> Option<ScriptValue> {
        self.host.signals_local.lock().get(name).cloned()
    }

    fn set(&mut self, name: &str, value: ScriptValue) {
        let text = lua_stringify(&value);
        self.host
            .signals_local
            .lock()
            .insert(name.to_string(), value);
        self.host.sink.lock().push(ScriptCommand::SetSignal {
            name: name.to_string(),
            value: text,
        });
    }

    fn array_push(&mut self, name: &str, value: ScriptValue) {
        let mut map = self.host.signals_local.lock();
        let mut current = match map.get(name) {
            Some(ScriptValue::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        current.push(value);
        map.insert(name.to_string(), ScriptValue::Array(current.clone()));
        drop(map);
        let items: Vec<HashMap<String, String>> = current
            .iter()
            .filter_map(|item| match item {
                ScriptValue::Map(m) => Some(
                    m.iter()
                        .map(|(k, v)| (k.clone(), lua_stringify(v)))
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        self.host.sink.lock().push(ScriptCommand::SetArray {
            name: name.to_string(),
            items,
        });
    }

    fn array_clear(&mut self, name: &str) {
        self.host
            .signals_local
            .lock()
            .insert(name.to_string(), ScriptValue::Array(Vec::new()));
        self.host.sink.lock().push(ScriptCommand::SetArray {
            name: name.to_string(),
            items: Vec::new(),
        });
    }
}

// ---------------------------------------------------------------------
// Builtin registration
// ---------------------------------------------------------------------

/// Build a fresh `Lua` with every Lumen builtin registered as a global
/// (plus the `signals` chained-access root). Shared by [`LuaHost::new`]
/// and [`LuaHost::reset`].
fn build_lua(
    sink: &Sink,
    signals: &SignalMirror,
    handlers: &HandlerMap,
    derivations: &DerivationMap,
    pending: &PendingSet,
) -> mlua::Result<Lua> {
    let lua = Lua::new();
    let g = lua.globals();

    // print(...) - override to capture into the command sink instead of
    // writing to stdout. Args joined with a tab (Lua's `print` idiom).
    {
        let sink = sink.clone();
        g.set(
            "print",
            lua.create_function(move |_, args: Variadic<LuaValue>| {
                let parts: Vec<String> = args.iter().map(lua_value_stringify).collect();
                sink.lock().push(ScriptCommand::Print(parts.join("\t")));
                Ok(())
            })?,
        )?;
    }

    // -- simple command-enqueue builtins -------------------------------
    //
    // One arity-generic macro: each builtin's argument list and its
    // `ScriptCommand` construction stay inline at the call site (passed
    // as macro args, not hidden behind a helper), so a reviewer can diff
    // the three script hosts builtin-by-builtin. Args marshal through
    // mlua's tuple `FromLuaMulti`; a single-arg builtin is a 1-tuple
    // `(n,): (i64,)`.
    macro_rules! enqueue {
        ($name:literal, ($($arg:ident : $ty:ty),+ $(,)?), $build:expr $(,)?) => {{
            let sink = sink.clone();
            g.set(
                $name,
                lua.create_function(move |_, ($($arg,)+): ($($ty,)+)| {
                    sink.lock().push($build);
                    Ok(())
                })?,
            )?;
        }};
    }

    enqueue!("add_clicks", (n: i64), ScriptCommand::AddClicks(n as i32));
    enqueue!("set_string", (key: String, value: String), ScriptCommand::SetString { key, value });
    enqueue!("set_text", (target_id: String, text: String), ScriptCommand::SetText { target_id, text });
    enqueue!("set_src", (target_id: String, path: String), ScriptCommand::SetSrc { target_id, path });

    // -- Dynamic DOM read side: query / get_by_id / document ------------
    // Stateless globals reading the per-tick snapshot; return Node /
    // NodeQuery userdata (nil for "no node").
    g.set(
        "query",
        lua.create_function(|_, selector: String| {
            lumen_script::node_query::run_query(&selector)
                .map(|q| NodeQuery { nodes: q.nodes })
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    g.set(
        "get_by_id",
        lua.create_function(|_, id: String| {
            Ok(lumen_script::node_query::run_get_by_id(&id).map(|h| Node { handle: h }))
        })?,
    )?;
    // -- Global introspection (phase 5) --------------------------------
    {
        use lumen_script::introspect as ins;
        g.set(
            "pointer_state",
            lua.create_function(|lua, ()| {
                let p = ins::pointer_state();
                let t = lua.create_table()?;
                t.set("x", p.x)?;
                t.set("y", p.y)?;
                t.set("inside", p.inside)?;
                t.set("buttons", p.buttons as i64)?;
                let m = lua.create_table()?;
                m.set("shift", p.shift)?;
                m.set("ctrl", p.ctrl)?;
                m.set("alt", p.alt)?;
                m.set("super", p.super_)?;
                t.set("modifiers", m)?;
                Ok(t)
            })?,
        )?;
        g.set(
            "frame_info",
            lua.create_function(|lua, ()| {
                let f = ins::frame_info();
                let t = lua.create_table()?;
                t.set("frame", f.frame as i64)?;
                t.set("dt_ms", f.dt_ms)?;
                t.set("dirty_count", f.dirty_count as i64)?;
                Ok(t)
            })?,
        )?;
        g.set(
            "signals_all",
            lua.create_function(|lua, ()| kv_table(lua, ins::signals_all()))?,
        )?;
        g.set(
            "dump_tree",
            lua.create_function(|_, ()| Ok(ins::dump_tree()))?,
        )?;
    }

    // `document` is a namespace table (section 4.8) that is ALSO callable
    // for back-compat: `document()` still returns the root node, while
    // `document.query(..)` / `document.root()` / `document.spawn(..)` are
    // the namespaced entry points.
    {
        use lumen_script::node_query;
        let document = lua.create_table()?;
        document.set(
            "root",
            lua.create_function(
                |_, ()| Ok(node_query::run_document().map(|h| Node { handle: h })),
            )?,
        )?;
        document.set(
            "query",
            lua.create_function(|_, selector: String| {
                node_query::run_query(&selector)
                    .map(|q| NodeQuery { nodes: q.nodes })
                    .map_err(mlua::Error::runtime)
            })?,
        )?;
        document.set(
            "get_by_id",
            lua.create_function(|_, id: String| {
                Ok(node_query::run_get_by_id(&id).map(|h| Node { handle: h }))
            })?,
        )?;
        document.set(
            "spawn",
            lua.create_function(|_, tag: String| {
                let (handle, cmd) = node_query::build_spawn(&tag);
                node_query::push_external_dom_command(cmd);
                Ok(Node { handle })
            })?,
        )?;
        document.set(
            "focused",
            lua.create_function(
                |_, ()| Ok(node_query::focused_node().map(|h| Node { handle: h })),
            )?,
        )?;
        document.set(
            "hovered",
            lua.create_function(
                |_, ()| Ok(node_query::hovered_node().map(|h| Node { handle: h })),
            )?,
        )?;
        let mt = lua.create_table()?;
        mt.set(
            "__call",
            lua.create_function(|_, _args: mlua::MultiValue| {
                Ok(node_query::run_document().map(|h| Node { handle: h }))
            })?,
        )?;
        document.set_metatable(Some(mt))?;
        g.set("document", document)?;
    }

    // Global `spawn(tag)` create verb.
    g.set(
        "spawn",
        lua.create_function(|_, tag: String| {
            let (handle, cmd) = lumen_script::node_query::build_spawn(&tag);
            lumen_script::node_query::push_external_dom_command(cmd);
            Ok(Node { handle })
        })?,
    )?;

    // `window` namespace: navigation + window state (section 4.8).
    {
        let window = lua.create_table()?;
        window.set(
            "set_href",
            lua.create_function(|_, path: String| {
                lumen_core::nav::navigate(path);
                Ok(())
            })?,
        )?;
        window.set(
            "href",
            lua.create_function(|_, ()| Ok(lumen_core::nav::current()))?,
        )?;
        window.set(
            "reload",
            lua.create_function(|_, ()| {
                lumen_core::nav::navigate(lumen_core::nav::current());
                Ok(())
            })?,
        )?;
        window.set(
            "title",
            lua.create_function(|_, ()| Ok(lumen_core::window_state::title()))?,
        )?;
        window.set(
            "set_title",
            lua.create_function(|_, title: String| {
                lumen_script::node_query::push_external_dom_command(
                    ScriptCommand::WindowSetTitle { title },
                );
                Ok(())
            })?,
        )?;
        window.set(
            "size",
            lua.create_function(|lua, ()| {
                let (w, h) = lumen_core::window_state::size();
                let t = lua.create_table()?;
                t.set(1, w)?;
                t.set(2, h)?;
                Ok(t)
            })?,
        )?;
        window.set(
            "set_size",
            lua.create_function(|_, (w, h): (f32, f32)| {
                lumen_script::node_query::push_external_dom_command(ScriptCommand::WindowSetSize {
                    width: w,
                    height: h,
                });
                Ok(())
            })?,
        )?;
        window.set(
            "dpr",
            lua.create_function(|_, ()| Ok(lumen_core::window_state::dpr()))?,
        )?;
        // window.location parts (path only; query / hash untracked).
        let location = lua.create_table()?;
        location.set(
            "path",
            lua.create_function(|_, ()| Ok(lumen_core::nav::current()))?,
        )?;
        location.set("query", lua.create_function(|_, ()| Ok(String::new()))?)?;
        location.set("hash", lua.create_function(|_, ()| Ok(String::new()))?)?;
        window.set("location", location)?;
        g.set("window", window)?;
    }

    // `history` namespace.
    {
        let history = lua.create_table()?;
        history.set(
            "back",
            lua.create_function(|_, ()| Ok(lumen_core::nav::back()))?,
        )?;
        history.set(
            "forward",
            lua.create_function(|_, ()| Ok(lumen_core::nav::forward()))?,
        )?;
        history.set(
            "go",
            lua.create_function(|_, delta: i64| {
                for _ in 0..delta.unsigned_abs() {
                    if delta < 0 {
                        lumen_core::nav::back();
                    } else {
                        lumen_core::nav::forward();
                    }
                }
                Ok(())
            })?,
        )?;
        g.set("history", history)?;
    }

    // -- Signal / ArraySignal factories --------------------------------
    {
        let signals = signals.clone();
        let sink = sink.clone();
        g.set(
            "signal",
            lua.create_function(move |_, (name, default): (String, LuaValue)| {
                // RC7: publish the default to the ECS store the first time
                // a name is seen AND no pre-existing (SDK/FFI-pushed)
                // value is found, so `bind-text` on a declared-but-unset
                // signal renders the default rather than blank.
                let publish: Option<String> = {
                    let mut map = signals.lock();
                    if map.contains_key(&name) {
                        None
                    } else {
                        let key = PropertyKey::Global(Arc::<str>::from(name.as_str()));
                        let existing = lumen_core::property_store::external_property_snapshot()
                            .remove(&key)
                            .or_else(|| {
                                lumen_core::property_store::typed_property_snapshot().remove(&key)
                            });
                        if let Some(v) = existing {
                            map.insert(name.clone(), property_value_to_script_value(&v));
                            None
                        } else {
                            let sv = lua_value_to_script_value(&default);
                            let text = lua_stringify(&sv);
                            map.insert(name.clone(), sv);
                            Some(text)
                        }
                    }
                };
                if let Some(text) = publish {
                    sink.lock().push(ScriptCommand::SetSignal {
                        name: name.clone(),
                        value: text,
                    });
                }
                Ok(Signal {
                    name,
                    signals: signals.clone(),
                    sink: sink.clone(),
                })
            })?,
        )?;
    }
    {
        let signals = signals.clone();
        let sink = sink.clone();
        g.set(
            "signal_array",
            lua.create_function(move |_, name: String| {
                Ok(ArraySignal {
                    name,
                    signals: signals.clone(),
                    sink: sink.clone(),
                })
            })?,
        )?;
    }

    // -- typed procedural signal builtins (deprecated; chained preferred)
    register_typed_signal_builtins(&lua, signals)?;

    // -- chained `signals` root ----------------------------------------
    g.set(
        "signals",
        SignalRef {
            path: Vec::new(),
            signals: signals.clone(),
        },
    )?;

    // is_valid(id)
    {
        let signals = signals.clone();
        g.set(
            "is_valid",
            lua.create_function(move |_, id: String| {
                let key = format!("valid:{id}");
                Ok(match signals.lock().get(&key) {
                    Some(ScriptValue::Str(s)) => s == "true",
                    Some(ScriptValue::Bool(b)) => *b,
                    Some(_) => false,
                    None => true,
                })
            })?,
        )?;
    }

    // -- timers --------------------------------------------------------
    enqueue!("set_timeout", (name: String, ms: i64), ScriptCommand::SetTimer {
        name,
        millis: ms.max(0) as u64,
        repeat: false,
    });
    enqueue!("set_interval", (name: String, ms: i64), ScriptCommand::SetTimer {
        name,
        millis: ms.max(0) as u64,
        repeat: true,
    });
    enqueue!("cancel_timer", (name: String), ScriptCommand::CancelTimer { name });

    // -- OS surface (all thin enqueues) --------------------------------
    enqueue!("notify", (title: String, body: String), ScriptCommand::Notify { title, body });
    enqueue!("copy_image", (path: String), ScriptCommand::CopyImageToClipboard { path });
    enqueue!("save_clipboard_image", (path: String), ScriptCommand::SaveClipboardImage { path });
    enqueue!("tray_icon", (id: String, icon_path: String, tooltip: String), ScriptCommand::RegisterTrayIcon {
        id,
        icon_path,
        tooltip: if tooltip.is_empty() {
            None
        } else {
            Some(tooltip)
        },
    });
    enqueue!("unregister_tray", (id: String), ScriptCommand::UnregisterTrayIcon { id });
    enqueue!("open_menu", (id: String), ScriptCommand::SetSignal {
        name: format!("__menu_open:{id}"),
        value: "true".to_string(),
    });
    enqueue!("close_menu", (id: String), ScriptCommand::SetSignal {
        name: format!("__menu_open:{id}"),
        value: "false".to_string(),
    });

    // file dialogs
    for (fname, kind) in [
        ("pick_file", lumen_script::FileDialogKind::Open),
        ("pick_files", lumen_script::FileDialogKind::OpenMulti),
        ("pick_folder", lumen_script::FileDialogKind::PickFolder),
    ] {
        let sink = sink.clone();
        g.set(
            fname,
            lua.create_function(move |_, tag: String| {
                sink.lock().push(ScriptCommand::OpenFileDialog {
                    kind,
                    tag,
                    filters: Vec::new(),
                    default_name: None,
                });
                Ok(())
            })?,
        )?;
    }
    enqueue!("save_file", (tag: String, default_name: String), ScriptCommand::OpenFileDialog {
        kind: lumen_script::FileDialogKind::Save,
        tag,
        filters: Vec::new(),
        default_name: Some(default_name),
    });
    enqueue!("pick_file_filtered", (tag: String, spec: String), ScriptCommand::OpenFileDialog {
        kind: lumen_script::FileDialogKind::Open,
        tag,
        filters: parse_dialog_filter_spec(&spec),
        default_name: None,
    });

    // hotkeys
    enqueue!("register_hotkey", (name: String, accelerator: String), ScriptCommand::RegisterHotkey { name, accelerator });
    enqueue!("unregister_hotkey", (name: String), ScriptCommand::UnregisterHotkey { name });

    // classes
    enqueue!("set_class", (id: String, classes: String), ScriptCommand::SetClasses {
        target_id: id,
        classes,
    });
    enqueue!("set_root_class", (classes: String), ScriptCommand::SetClasses {
        target_id: "<root>".to_string(),
        classes,
    });

    // -- HTTP ----------------------------------------------------------
    enqueue!("fetch", (url: String, tag: String), ScriptCommand::Fetch { url, tag });
    {
        let sink = sink.clone();
        g.set(
            "http",
            lua.create_function(move |_, req: Table| {
                let get_str = |t: &Table, k: &str| -> Option<String> {
                    match t.get::<LuaValue>(k) {
                        Ok(LuaValue::String(s)) => Some(lua_string(&s)),
                        Ok(LuaValue::Integer(i)) => Some(i.to_string()),
                        Ok(LuaValue::Number(n)) => Some(n.to_string()),
                        _ => None,
                    }
                };
                let method = get_str(&req, "method").unwrap_or_else(|| "GET".to_string());
                let url = get_str(&req, "url").unwrap_or_default();
                let tag = get_str(&req, "tag").unwrap_or_default();
                let body = match req.get::<LuaValue>("body") {
                    Ok(LuaValue::String(s)) => Some(lua_string(&s)),
                    _ => None,
                };
                let timeout_ms = match req.get::<LuaValue>("timeout_ms") {
                    Ok(LuaValue::Integer(i)) if i > 0 => Some(i as u64),
                    Ok(LuaValue::Number(n)) if n > 0.0 => Some(n as u64),
                    _ => None,
                };
                let headers = match req.get::<LuaValue>("headers") {
                    Ok(LuaValue::Table(h)) => {
                        let mut out = Vec::new();
                        for pair in h.pairs::<LuaValue, LuaValue>().flatten() {
                            let (k, v) = pair;
                            let key = match &k {
                                LuaValue::String(s) => lua_string(s),
                                _ => continue,
                            };
                            let val = match &v {
                                LuaValue::String(s) => lua_string(s),
                                LuaValue::Integer(i) => i.to_string(),
                                LuaValue::Number(n) => n.to_string(),
                                other => lua_value_stringify(other),
                            };
                            out.push((key, val));
                        }
                        out
                    }
                    _ => Vec::new(),
                };
                sink.lock().push(ScriptCommand::Http {
                    method,
                    url,
                    headers,
                    body,
                    timeout_ms,
                    tag,
                });
                Ok(())
            })?,
        )?;
    }

    // parse_json(s)
    g.set(
        "parse_json",
        lua.create_function(move |lua, s: String| {
            Ok(match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => json_to_lua(lua, v)?,
                Err(_) => LuaValue::Nil,
            })
        })?,
    )?;

    // -- derive(name, deps, fn) ----------------------------------------
    {
        let derivations = derivations.clone();
        let pending = pending.clone();
        let signals = signals.clone();
        let sink = sink.clone();
        g.set(
            "derive",
            lua.create_function(move |_, (name, deps, f): (String, Table, Function)| {
                let mut dep_names = Vec::new();
                let len = deps.raw_len() as i64;
                for i in 1..=len {
                    let v: LuaValue = deps.get(i).unwrap_or(LuaValue::Nil);
                    match &v {
                        LuaValue::String(s) => dep_names.push(lua_string(s)),
                        LuaValue::UserData(ud) => {
                            if let Ok(sig) = ud.borrow::<Signal>() {
                                dep_names.push(sig.name.clone());
                            }
                        }
                        _ => {}
                    }
                }
                derivations.lock().insert(name.clone(), (dep_names, f));
                pending.lock().insert(name.clone());
                signals
                    .lock()
                    .entry(name.clone())
                    .or_insert(ScriptValue::Unit);
                Ok(Signal {
                    name,
                    signals: signals.clone(),
                    sink: sink.clone(),
                })
            })?,
        )?;
    }

    // -- on(event, id, fn_name) ----------------------------------------
    {
        let handlers = handlers.clone();
        g.set(
            "on",
            lua.create_function(move |_, (event, id, fn_name): (String, String, String)| {
                if let Ok(mut h) = handlers.write() {
                    h.insert((event, id), fn_name);
                }
                Ok(())
            })?,
        )?;
    }

    // local_id(source, suffix)
    g.set(
        "local_id",
        lua.create_function(|_, (source, suffix): (String, String)| {
            Ok(match source.rfind(':') {
                Some(colon) => format!("{}:{}", &source[..colon], suffix),
                None => suffix,
            })
        })?,
    )?;

    // parse_markdown(src)
    g.set(
        "parse_markdown",
        lua.create_function(move |lua, src: String| {
            let blocks = parse_markdown_blocks(&src);
            let arr = ScriptValue::Array(blocks.into_iter().map(ScriptValue::Map).collect());
            script_value_to_lua(lua, &arr)
        })?,
    )?;

    // read_file / write_file
    g.set(
        "read_file",
        lua.create_function(|_, path: String| {
            Ok(std::fs::read_to_string(&path).unwrap_or_else(|e| {
                tracing::warn!(target: "lumen.script.lua", path = %path, error = %e, "read_file failed");
                String::new()
            }))
        })?,
    )?;
    g.set(
        "write_file",
        lua.create_function(|_, (path, contents): (String, String)| {
            Ok(match std::fs::write(&path, contents) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(target: "lumen.script.lua", path = %path, error = %e, "write_file failed");
                    false
                }
            })
        })?,
    )?;

    // audio_* transport
    audio::register(&lua, sink)?;

    drop(g);
    Ok(lua)
}

/// Register the deprecated procedural typed-signal builtins
/// (`signal_set_int` / `signal_get_int` / float / bool / color). Prefer
/// the chained `signals.name.set(v)` form.
fn register_typed_signal_builtins(lua: &Lua, signals: &SignalMirror) -> mlua::Result<()> {
    let g = lua.globals();

    // int
    {
        let signals = signals.clone();
        g.set(
            "signal_set_int",
            lua.create_function(move |_, (name, value): (String, i64)| {
                signals.lock().insert(name.clone(), ScriptValue::I64(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(Arc::<str>::from(name.as_str())),
                    PropertyValue::I64(value),
                );
                Ok(())
            })?,
        )?;
    }
    {
        let signals = signals.clone();
        g.set(
            "signal_get_int",
            lua.create_function(move |_, name: String| {
                Ok(match signals.lock().get(&name) {
                    Some(ScriptValue::I64(i)) => LuaValue::Integer(*i),
                    Some(ScriptValue::F64(f)) => LuaValue::Integer(*f as i64),
                    Some(ScriptValue::Bool(b)) => LuaValue::Integer(*b as i64),
                    Some(ScriptValue::Str(s)) => s
                        .parse::<i64>()
                        .map(LuaValue::Integer)
                        .unwrap_or(LuaValue::Nil),
                    _ => LuaValue::Nil,
                })
            })?,
        )?;
    }

    // float
    {
        let signals = signals.clone();
        g.set(
            "signal_set_float",
            lua.create_function(move |_, (name, value): (String, f64)| {
                signals.lock().insert(name.clone(), ScriptValue::F64(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(Arc::<str>::from(name.as_str())),
                    PropertyValue::F64(value),
                );
                Ok(())
            })?,
        )?;
    }
    {
        let signals = signals.clone();
        g.set(
            "signal_get_float",
            lua.create_function(move |_, name: String| {
                Ok(match signals.lock().get(&name) {
                    Some(ScriptValue::F64(f)) => LuaValue::Number(*f),
                    Some(ScriptValue::I64(i)) => LuaValue::Number(*i as f64),
                    Some(ScriptValue::Str(s)) => s
                        .parse::<f64>()
                        .map(LuaValue::Number)
                        .unwrap_or(LuaValue::Nil),
                    _ => LuaValue::Nil,
                })
            })?,
        )?;
    }

    // bool
    {
        let signals = signals.clone();
        g.set(
            "signal_set_bool",
            lua.create_function(move |_, (name, value): (String, bool)| {
                signals
                    .lock()
                    .insert(name.clone(), ScriptValue::Bool(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(Arc::<str>::from(name.as_str())),
                    PropertyValue::Bool(value),
                );
                Ok(())
            })?,
        )?;
    }
    {
        let signals = signals.clone();
        g.set(
            "signal_get_bool",
            lua.create_function(move |_, name: String| {
                Ok(match signals.lock().get(&name) {
                    Some(ScriptValue::Bool(b)) => LuaValue::Boolean(*b),
                    Some(ScriptValue::I64(i)) => LuaValue::Boolean(*i != 0),
                    Some(ScriptValue::Str(s)) => match s.as_str() {
                        "true" | "1" => LuaValue::Boolean(true),
                        "false" | "0" | "" => LuaValue::Boolean(false),
                        _ => LuaValue::Nil,
                    },
                    _ => LuaValue::Nil,
                })
            })?,
        )?;
    }

    // color
    {
        let signals = signals.clone();
        g.set(
            "signal_set_color",
            lua.create_function(move |_, (name, hex): (String, String)| {
                if let Some((r, gc, b, a)) = parse_hex_color(&hex) {
                    signals.lock().insert(
                        name.clone(),
                        ScriptValue::Map(color_map(
                            r as f32 / 255.0,
                            gc as f32 / 255.0,
                            b as f32 / 255.0,
                            a as f32 / 255.0,
                        )),
                    );
                    let color = lumen_core::components::Color::rgba(
                        r as f32 / 255.0,
                        gc as f32 / 255.0,
                        b as f32 / 255.0,
                        a as f32 / 255.0,
                    );
                    lumen_core::property_store::push_external_property(
                        PropertyKey::Global(Arc::<str>::from(name.as_str())),
                        PropertyValue::Color(color),
                    );
                }
                Ok(())
            })?,
        )?;
    }
    {
        let signals = signals.clone();
        g.set(
            "signal_get_color",
            lua.create_function(move |lua, name: String| {
                let sv = match signals.lock().get(&name) {
                    Some(ScriptValue::Map(m)) => Some(ScriptValue::Map(m.clone())),
                    Some(ScriptValue::Str(s)) => parse_hex_color(s).map(|(r, gc, b, a)| {
                        ScriptValue::Map(color_map(
                            r as f32 / 255.0,
                            gc as f32 / 255.0,
                            b as f32 / 255.0,
                            a as f32 / 255.0,
                        ))
                    }),
                    _ => None,
                };
                match sv {
                    Some(v) => script_value_to_lua(lua, &v),
                    None => Ok(LuaValue::Nil),
                }
            })?,
        )?;
    }

    Ok(())
}

/// Translate an mlua error into the structured [`ScriptError::Compile`]
/// shape. Lua embeds `[string "uri"]:LINE: message`; parse the line out
/// so LSP / banner layers get a position without re-parsing.
fn lua_compile_error(e: mlua::Error, uri: &str) -> ScriptError {
    let message = e.to_string();
    let line = message
        .split("]:")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .and_then(|n| n.trim().parse::<u32>().ok())
        .unwrap_or(0);
    ScriptError::Compile {
        uri: uri.to_string(),
        line,
        col: 0,
        message,
    }
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

/// A single `mlua::Lua` extension callback; aliased to keep clippy's
/// `type_complexity` lint quiet.
type LuaExtension = Box<dyn FnOnce(&mut Lua) + Send + 'static>;

/// Plugin: build a [`LuaHost`], apply embedder extensions, and delegate
/// to the host-generic [`ScriptPlugin`](lumen_script::ScriptPlugin),
/// which loads the source (stderr banner + [`ScriptLoadFailure`] on
/// failure), fires `on_start`, installs the host resource, and registers
/// the full dispatcher / derivation / timer / fetch system set.
///
/// Selectable alternative to `lumen_script_rhai::ScriptRhaiPlugin`;
/// identical shape so an embedder swaps one for the other.
pub struct ScriptLuaPlugin {
    /// Inline Lua source loaded on app start.
    pub source: String,
    /// Extension callbacks invoked on the inner `mlua::Lua` after
    /// Lumen's built-in registrations but before the script is loaded.
    /// Use this to register app-specific native globals (`page()`, FFI,
    /// OS APIs). Lumen itself only ships UI/script primitives.
    pub extensions: Vec<LuaExtension>,
}

impl ScriptLuaPlugin {
    /// Wrap a source string.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            extensions: Vec::new(),
        }
    }

    /// Register a callback that runs on the inner `mlua::Lua` before the
    /// script loads. Lets the embedding binary expose extra native
    /// globals without forking the framework crate.
    pub fn with_extension<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut Lua) + Send + 'static,
    {
        self.extensions.push(Box::new(f));
        self
    }
}

impl Plugin for ScriptLuaPlugin {
    fn build(self, app: &mut App) {
        let mut host = LuaHost::new();
        for ext in self.extensions {
            ext(host.lua_mut());
        }
        ScriptPlugin::new(host, self.source).build(app);
    }
}
