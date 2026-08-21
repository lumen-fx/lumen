//! Rhai 1.24 implementation of [`lumen_script::ScriptHost`] (v2).
//!
//! This crate is engine + builtins + value conversion only. All
//! host-generic machinery - the 18-event dispatch surface, the derivation
//! fixed-point driver, the store->mirror sync driver, timers, fetch, the
//! load-failure banner protocol, and the tick wiring - lives in
//! `lumen-script` as `ScriptPlugin<H: ScriptHost>`; the items are
//! re-exported here so embedders keep their historical import paths.
//!
//! The host owns a `rhai::Engine`, a compiled `rhai::AST`, a persistent
//! `rhai::Scope`, a shared `Arc<Mutex<Vec<ScriptCommand>>>` command sink
//! the registered builtins push into, the rich-typed signal mirror, and
//! the per-id handler + derivation registries. The generic runtime
//! drives all of them through the [`lumen_script::ScriptHost`] trait.
//!
//! [`ScriptRhaiPlugin`] wraps a host + an embedded script source: it
//! builds a [`RhaiHost`], applies embedder engine extensions, and
//! delegates to the generic [`lumen_script::ScriptPlugin`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod audio;
pub mod builtins;

use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_script::{
    CallOutcome, MAX_VARIADIC_ARITY, ScriptCommand, ScriptContext, ScriptError, ScriptFn,
    ScriptFnStore, ScriptHost, ScriptNs, ScriptTy, ScriptValue,
};
use parking_lot::Mutex;
use rhai::{AST, CallFnOptions, Dynamic, Engine, EvalAltResult, Module, Scope};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// Host-generic runtime re-exports: these lived in this crate before the
// ScriptHost-v2 extraction; embedders (lumenc, tests) keep importing
// them from `lumen_script_rhai`. The system fns are generic over the
// host - instantiate as e.g. `tick_script::<RhaiHost>` when expressing
// ordering constraints against them.
pub use lumen_script::{
    FetchRegistry, ScriptCommandEvent, ScriptLoadFailure, ScriptPlugin, ScriptStartedAt,
    TimerRegistry, apply_derivations, dispatch_clicks_and_doubles, dispatch_close_to_script,
    drain_fetch_commands, drain_timer_commands, fire_due_timers, fire_fetched_responses,
    reload_script, sync_signals_into_host, tick_script,
};

/// Rhai handle to a live element (`Node`). Wraps the packed `u64` handle
/// the host-neutral query surface returns; reads resolve against the
/// per-tick DOM snapshot. Phase 1 exposes traversal + liveness; mutators
/// arrive in later phases.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Packed handle (`0` = no node).
    pub handle: u64,
}

/// Rhai `NodeQuery` result set: packed handles in document order, with the
/// Bevy-flavored consumers (`single` / `get_single` / `first` / `nth` /
/// `iter` / `collect`).
#[derive(Debug, Clone)]
pub struct NodeQuery {
    /// Matched handles, document order.
    pub nodes: Vec<u64>,
}

/// Rhai handle to the current event delivered to an `on(...)` handler
/// (phase 4). Zero-sized: every accessor reads the process-global
/// current-event cell in [`lumen_script::event`], which the dispatcher
/// populates before invoking the handler.
#[derive(Debug, Clone, Copy)]
pub struct Event;

/// `token -> handler closure` registry for `n.on(type, handler)` bindings.
/// The host holds the `FnPtr`; the host-neutral binding registry keys the
/// same token to `(node, type, capture)`.
type EventClosureMap = Arc<RwLock<std::collections::HashMap<u64, rhai::FnPtr>>>;

fn node_to_dynamic(handle: Option<u64>) -> rhai::Dynamic {
    match handle {
        Some(h) => rhai::Dynamic::from(Node { handle: h }),
        None => rhai::Dynamic::UNIT,
    }
}

fn nodes_to_array(handles: Vec<u64>) -> rhai::Array {
    handles
        .into_iter()
        .map(|h| rhai::Dynamic::from(Node { handle: h }))
        .collect()
}

/// Register the read-side DOM query surface (`query` / `get_by_id` /
/// `document` globals + the `Node` / `NodeQuery` types) on `engine`. All
/// calls read the process-shared snapshot, so no host state is captured.
fn register_dom_query(engine: &mut Engine) {
    use lumen_script::node_query;

    engine.register_type_with_name::<Node>("Node");
    engine.register_type_with_name::<NodeQuery>("NodeQuery");

    // Globals.
    engine.register_fn(
        "query",
        |selector: rhai::ImmutableString| -> Result<NodeQuery, Box<EvalAltResult>> {
            match node_query::run_query(selector.as_str()) {
                Ok(q) => Ok(NodeQuery { nodes: q.nodes }),
                Err(e) => Err(e.into()),
            }
        },
    );
    engine.register_fn("get_by_id", |id: rhai::ImmutableString| -> rhai::Dynamic {
        node_to_dynamic(node_query::run_get_by_id(id.as_str()))
    });
    engine.register_fn("document", || -> rhai::Dynamic {
        node_to_dynamic(node_query::run_document())
    });

    // NodeQuery consumers.
    engine.register_fn("len", |q: &mut NodeQuery| q.nodes.len() as i64);
    engine.register_fn("is_empty", |q: &mut NodeQuery| q.nodes.is_empty());
    engine.register_fn("first", |q: &mut NodeQuery| {
        node_to_dynamic(q.nodes.first().copied())
    });
    engine.register_fn("nth", |q: &mut NodeQuery, i: i64| {
        node_to_dynamic(
            usize::try_from(i)
                .ok()
                .and_then(|i| q.nodes.get(i).copied()),
        )
    });
    engine.register_fn("iter", |q: &mut NodeQuery| nodes_to_array(q.nodes.clone()));
    engine.register_fn("collect", |q: &mut NodeQuery| {
        nodes_to_array(q.nodes.clone())
    });
    engine.register_fn(
        "single",
        |q: &mut NodeQuery| -> Result<Node, Box<EvalAltResult>> {
            match q.nodes.len() {
                1 => Ok(Node { handle: q.nodes[0] }),
                n => Err(format!("query.single(): expected exactly 1 match, found {n}").into()),
            }
        },
    );
    engine.register_fn("get_single", |q: &mut NodeQuery| -> rhai::Dynamic {
        if q.nodes.len() == 1 {
            rhai::Dynamic::from(Node { handle: q.nodes[0] })
        } else {
            rhai::Dynamic::UNIT
        }
    });

    // Node traversal + liveness.
    engine.register_fn("parent", |n: &mut Node| {
        node_to_dynamic(node_query::node_parent(n.handle))
    });
    engine.register_fn("first_child", |n: &mut Node| {
        node_to_dynamic(node_query::node_first_child(n.handle))
    });
    engine.register_fn("last_child", |n: &mut Node| {
        node_to_dynamic(node_query::node_last_child(n.handle))
    });
    engine.register_fn("next", |n: &mut Node| {
        node_to_dynamic(node_query::node_next(n.handle))
    });
    engine.register_fn("prev", |n: &mut Node| {
        node_to_dynamic(node_query::node_prev(n.handle))
    });
    engine.register_fn("children", |n: &mut Node| {
        nodes_to_array(node_query::node_children(n.handle))
    });
    engine.register_fn(
        "closest",
        |n: &mut Node,
         selector: rhai::ImmutableString|
         -> Result<rhai::Dynamic, Box<EvalAltResult>> {
            match node_query::node_closest(n.handle, selector.as_str()) {
                Ok(h) => Ok(node_to_dynamic(h)),
                Err(e) => Err(e.into()),
            }
        },
    );
    engine.register_fn("exists", |n: &mut Node| node_query::node_valid(n.handle));
    engine.register_fn("valid", |n: &mut Node| node_query::node_valid(n.handle));
    engine.register_fn("handle", |n: &mut Node| n.handle as i64);
}

/// The `window` namespace handle (unit; navigation / window state is
/// process-global). Pushed into the persistent scope so `window.set_href`
/// resolves.
#[derive(Debug, Clone, Copy)]
pub struct Window;
/// The `document` namespace handle.
#[derive(Debug, Clone, Copy)]
pub struct Document;
/// The `history` namespace handle.
#[derive(Debug, Clone, Copy)]
pub struct History;
/// The `window.location` handle (parsed current URL parts).
#[derive(Debug, Clone, Copy)]
pub struct Location;

/// Register the fluent DOM mutators on `Node` (phases 2 + 3). Every mutator
/// pushes a [`ScriptCommand`] into the shared sink and returns the receiver
/// handle so calls chain; read-backs return their value and end the chain.
fn register_dom_mutators(engine: &mut Engine, sink: Arc<Mutex<Vec<ScriptCommand>>>) {
    use lumen_script::node_query;

    macro_rules! mutate {
        ($name:literal, |$n:ident $(, $arg:ident : $ty:ty)*| $build:expr) => {{
            let s = sink.clone();
            engine.register_fn($name, move |$n: &mut Node $(, $arg: $ty)*| -> Node {
                s.lock().push($build);
                *$n
            });
        }};
    }

    // Attributes / id / text.
    mutate!(
        "set_attr",
        |n, name: rhai::ImmutableString, value: rhai::ImmutableString| {
            ScriptCommand::SetAttr {
                node: n.handle,
                name: name.to_string(),
                value: value.to_string(),
            }
        }
    );
    mutate!("remove_attr", |n, name: rhai::ImmutableString| {
        ScriptCommand::RemoveAttr {
            node: n.handle,
            name: name.to_string(),
        }
    });
    mutate!("set_id", |n, id: rhai::ImmutableString| {
        ScriptCommand::SetAttr {
            node: n.handle,
            name: "id".to_string(),
            value: id.to_string(),
        }
    });
    mutate!("set_text", |n, text: rhai::ImmutableString| {
        ScriptCommand::SetNodeText {
            node: n.handle,
            text: text.to_string(),
        }
    });
    // Guarded markup injection (design 4.4). Do not feed untrusted content.
    mutate!("set_inner_markup", |n, markup: rhai::ImmutableString| {
        ScriptCommand::SetInnerMarkup {
            node: n.handle,
            markup: markup.to_string(),
        }
    });

    // Class list (incremental).
    mutate!("add_class", |n, class: rhai::ImmutableString| {
        ScriptCommand::ClassAdd {
            node: n.handle,
            class: class.to_string(),
        }
    });
    mutate!("remove_class", |n, class: rhai::ImmutableString| {
        ScriptCommand::ClassRemove {
            node: n.handle,
            class: class.to_string(),
        }
    });
    mutate!("toggle_class", |n, class: rhai::ImmutableString| {
        ScriptCommand::ClassToggle {
            node: n.handle,
            class: class.to_string(),
        }
    });
    mutate!("set_class", |n, classes: rhai::ImmutableString| {
        ScriptCommand::SetAttr {
            node: n.handle,
            name: "class".to_string(),
            value: classes.to_string(),
        }
    });

    // Inline style.
    mutate!(
        "set_style",
        |n, name: rhai::ImmutableString, value: rhai::ImmutableString| {
            ScriptCommand::SetStyleProp {
                node: n.handle,
                name: name.to_string(),
                value: value.to_string(),
            }
        }
    );
    mutate!(
        "style_set",
        |n, name: rhai::ImmutableString, value: rhai::ImmutableString| {
            ScriptCommand::SetStyleProp {
                node: n.handle,
                name: name.to_string(),
                value: value.to_string(),
            }
        }
    );
    mutate!("style_remove", |n, name: rhai::ImmutableString| {
        ScriptCommand::RemoveStyleProp {
            node: n.handle,
            name: name.to_string(),
        }
    });

    // Structure. `set_parent` / `move_to` attach the receiver under a
    // parent; `append` / `insert_before` attach a child under the receiver.
    mutate!("set_parent", |n, parent: Node| {
        ScriptCommand::Insert {
            parent: parent.handle,
            node: n.handle,
            before: 0,
        }
    });
    mutate!("move_to", |n, parent: Node| {
        ScriptCommand::Insert {
            parent: parent.handle,
            node: n.handle,
            before: 0,
        }
    });
    mutate!("append", |n, child: Node| {
        ScriptCommand::Insert {
            parent: n.handle,
            node: child.handle,
            before: 0,
        }
    });
    mutate!("insert_before", |n, child: Node, reference: Node| {
        ScriptCommand::Insert {
            parent: n.handle,
            node: child.handle,
            before: reference.handle,
        }
    });

    // `replace_with` swaps the receiver for `new` and returns the live node.
    {
        let s = sink.clone();
        engine.register_fn("replace_with", move |n: &mut Node, new: Node| -> Node {
            s.lock().push(ScriptCommand::ReplaceWith {
                old: n.handle,
                new: new.handle,
            });
            new
        });
    }
    // `remove` is terminal (detaches + despawns the subtree).
    {
        let s = sink.clone();
        engine.register_fn("remove", move |n: &mut Node| {
            s.lock().push(ScriptCommand::RemoveNode { node: n.handle });
        });
    }
    // `clone_deep` returns a fresh detached node.
    {
        let s = sink.clone();
        engine.register_fn("clone_deep", move |n: &mut Node| -> Node {
            let (handle, cmd) = node_query::build_clone(n.handle);
            s.lock().push(cmd);
            Node { handle }
        });
    }

    // Read-backs (end the chain).
    engine.register_fn(
        "get_attr",
        |n: &mut Node, name: rhai::ImmutableString| -> rhai::Dynamic {
            node_query::node_get_attr(n.handle, name.as_str())
                .map(rhai::Dynamic::from)
                .unwrap_or(rhai::Dynamic::UNIT)
        },
    );
    engine.register_fn("id", |n: &mut Node| -> rhai::Dynamic {
        node_query::node_id(n.handle)
            .map(rhai::Dynamic::from)
            .unwrap_or(rhai::Dynamic::UNIT)
    });
    engine.register_fn("text", |n: &mut Node| -> rhai::Dynamic {
        node_query::node_text(n.handle)
            .map(rhai::Dynamic::from)
            .unwrap_or(rhai::Dynamic::UNIT)
    });
    engine.register_fn("has_class", |n: &mut Node, class: rhai::ImmutableString| {
        node_query::node_class_contains(n.handle, class.as_str())
    });
    engine.register_fn(
        "style_get",
        |n: &mut Node, name: rhai::ImmutableString| -> rhai::Dynamic {
            node_query::node_style_get(n.handle, name.as_str())
                .map(rhai::Dynamic::from)
                .unwrap_or(rhai::Dynamic::UNIT)
        },
    );
    engine.register_fn(
        "computed_style",
        |n: &mut Node, name: rhai::ImmutableString| -> rhai::Dynamic {
            node_query::node_computed_style(n.handle, name.as_str())
                .map(rhai::Dynamic::from)
                .unwrap_or(rhai::Dynamic::UNIT)
        },
    );
}

/// Register the `window` / `document` / `history` global namespaces
/// (section 4.8). Navigation binds onto the host-neutral
/// [`lumen_core::nav`] bus; window state onto
/// [`lumen_core::window_state`]; document entry points reuse the read +
/// spawn surface.
/// Build a rhai object map from `(key, value)` string pairs.
fn kv_map(pairs: Vec<(String, String)>) -> rhai::Map {
    let mut m = rhai::Map::new();
    for (k, v) in pairs {
        m.insert(k.into(), rhai::Dynamic::from(v));
    }
    m
}

/// Register the low-level introspection surface (design 4.7): post-layout
/// geometry, full computed style + provenance, typed component reads, tree
/// serialization, and global runtime state. All read-only over the per-tick
/// snapshot.
fn register_introspection(engine: &mut Engine) {
    use lumen_script::introspect as ins;

    // Geometry.
    engine.register_fn("rect", |n: &mut Node| -> rhai::Dynamic {
        match ins::node_rect(n.handle) {
            Some(r) => {
                let mut m = rhai::Map::new();
                m.insert("x".into(), (r.x as f64).into());
                m.insert("y".into(), (r.y as f64).into());
                m.insert("width".into(), (r.width as f64).into());
                m.insert("height".into(), (r.height as f64).into());
                m.insert("client_x".into(), (r.client_x as f64).into());
                m.insert("client_y".into(), (r.client_y as f64).into());
                rhai::Dynamic::from_map(m)
            }
            None => rhai::Dynamic::UNIT,
        }
    });
    engine.register_fn("content_rect", |n: &mut Node| -> rhai::Dynamic {
        match ins::node_content_rect(n.handle) {
            Some(r) => {
                let mut m = rhai::Map::new();
                m.insert("x".into(), (r.x as f64).into());
                m.insert("y".into(), (r.y as f64).into());
                m.insert("width".into(), (r.width as f64).into());
                m.insert("height".into(), (r.height as f64).into());
                m.insert("client_x".into(), (r.client_x as f64).into());
                m.insert("client_y".into(), (r.client_y as f64).into());
                rhai::Dynamic::from_map(m)
            }
            None => rhai::Dynamic::UNIT,
        }
    });
    engine.register_fn("scroll", |n: &mut Node| -> rhai::Dynamic {
        match ins::node_scroll(n.handle) {
            Some(s) => {
                let mut m = rhai::Map::new();
                m.insert("x".into(), (s.x as f64).into());
                m.insert("y".into(), (s.y as f64).into());
                m.insert("max_x".into(), (s.max_x as f64).into());
                m.insert("max_y".into(), (s.max_y as f64).into());
                rhai::Dynamic::from_map(m)
            }
            None => rhai::Dynamic::UNIT,
        }
    });
    engine.register_fn("is_visible", |n: &mut Node| ins::node_is_visible(n.handle));
    engine.register_fn("z_index", |n: &mut Node| ins::node_z_index(n.handle) as i64);

    // Computed style + provenance. The 0-arg `computed_style()` returns the
    // full map; the 1-arg form (single property) is registered by
    // `register_dom_mutators`.
    engine.register_fn("computed_style", |n: &mut Node| -> rhai::Map {
        kv_map(ins::node_computed_style_map(n.handle))
    });
    // Explicit spelling of the no-arg form, so the whole-map read has one name
    // on every host (candela cannot overload on arity).
    engine.register_fn("computed_style_all", |n: &mut Node| -> rhai::Map {
        kv_map(ins::node_computed_style_map(n.handle))
    });
    engine.register_fn("inline_style", |n: &mut Node| -> rhai::Map {
        kv_map(ins::node_inline_style(n.handle))
    });
    engine.register_fn("attrs", |n: &mut Node| -> rhai::Map {
        kv_map(ins::node_attrs(n.handle))
    });
    engine.register_fn("classes", |n: &mut Node| -> rhai::Array {
        ins::node_classes(n.handle)
            .into_iter()
            .map(rhai::Dynamic::from)
            .collect()
    });
    engine.register_fn("matched_rules", |n: &mut Node| -> rhai::Array {
        ins::node_matched_rules(n.handle)
            .into_iter()
            .map(|r| {
                let mut m = rhai::Map::new();
                m.insert("selector".into(), rhai::Dynamic::from(r.selector));
                let spec: rhai::Array = vec![
                    (r.specificity.0 as i64).into(),
                    (r.specificity.1 as i64).into(),
                    (r.specificity.2 as i64).into(),
                ];
                m.insert("specificity".into(), spec.into());
                m.insert("source".into(), rhai::Dynamic::from(r.source));
                m.insert("source_order".into(), (r.source_order as i64).into());
                m.insert(
                    "declarations".into(),
                    rhai::Dynamic::from_map(kv_map(r.declarations)),
                );
                rhai::Dynamic::from_map(m)
            })
            .collect()
    });

    // ECS introspection.
    engine.register_fn("entity_id", |n: &mut Node| -> rhai::Dynamic {
        match ins::node_entity_id(n.handle) {
            Some((index, generation)) => {
                let mut m = rhai::Map::new();
                m.insert("index".into(), (index as i64).into());
                m.insert("generation".into(), (generation as i64).into());
                rhai::Dynamic::from_map(m)
            }
            None => rhai::Dynamic::UNIT,
        }
    });
    engine.register_fn("components", |n: &mut Node| -> rhai::Array {
        ins::node_components(n.handle)
            .into_iter()
            .map(rhai::Dynamic::from)
            .collect()
    });
    engine.register_fn(
        "component",
        |n: &mut Node, name: rhai::ImmutableString| -> Result<rhai::Dynamic, Box<EvalAltResult>> {
            match ins::node_component(n.handle, name.as_str()) {
                Ok(Some(map)) => Ok(rhai::Dynamic::from_map(kv_map(map))),
                Ok(None) => Ok(rhai::Dynamic::UNIT),
                Err(e) => Err(e.into()),
            }
        },
    );
    engine.register_fn("outer_markup", |n: &mut Node| -> String {
        ins::outer_markup(n.handle)
    });
    engine.register_fn("inner_markup", |n: &mut Node| -> String {
        ins::inner_markup(n.handle)
    });

    // Global runtime state.
    engine.register_fn("dump_tree", || -> String { ins::dump_tree() });
    engine.register_fn("pointer_state", || -> rhai::Map {
        let p = ins::pointer_state();
        let mut m = rhai::Map::new();
        m.insert("x".into(), (p.x as f64).into());
        m.insert("y".into(), (p.y as f64).into());
        m.insert("inside".into(), p.inside.into());
        m.insert("buttons".into(), (p.buttons as i64).into());
        let mut mods = rhai::Map::new();
        mods.insert("shift".into(), p.shift.into());
        mods.insert("ctrl".into(), p.ctrl.into());
        mods.insert("alt".into(), p.alt.into());
        mods.insert("super".into(), p.super_.into());
        m.insert("modifiers".into(), rhai::Dynamic::from_map(mods));
        m
    });
    engine.register_fn("frame_info", || -> rhai::Map {
        let f = ins::frame_info();
        let mut m = rhai::Map::new();
        m.insert("frame".into(), (f.frame as i64).into());
        m.insert("dt_ms".into(), f.dt_ms.into());
        m.insert("dirty_count".into(), (f.dirty_count as i64).into());
        m
    });
    engine.register_fn("signals_all", || -> rhai::Map {
        kv_map(ins::signals_all())
    });
}

fn register_web_namespaces(engine: &mut Engine, sink: Arc<Mutex<Vec<ScriptCommand>>>) {
    use lumen_script::node_query;

    engine.register_type_with_name::<Window>("Window");
    engine.register_type_with_name::<Document>("Document");
    engine.register_type_with_name::<History>("History");
    engine.register_type_with_name::<Location>("Location");

    // window navigation + state.
    engine.register_fn(
        "set_href",
        |_w: &mut Window, path: rhai::ImmutableString| {
            lumen_core::nav::navigate(path.to_string());
        },
    );
    engine.register_fn("href", |_w: &mut Window| -> rhai::ImmutableString {
        lumen_core::nav::current().into()
    });
    engine.register_fn("reload", |_w: &mut Window| {
        lumen_core::nav::navigate(lumen_core::nav::current());
    });
    engine.register_fn("title", |_w: &mut Window| -> rhai::ImmutableString {
        lumen_core::window_state::title().into()
    });
    engine.register_fn("dpr", |_w: &mut Window| {
        lumen_core::window_state::dpr() as f64
    });
    engine.register_fn("size", |_w: &mut Window| -> rhai::Array {
        let (w, h) = lumen_core::window_state::size();
        vec![rhai::Dynamic::from(w as f64), rhai::Dynamic::from(h as f64)]
    });
    engine.register_get("location", |_w: &mut Window| Location);
    {
        let s = sink.clone();
        engine.register_fn(
            "set_title",
            move |_w: &mut Window, title: rhai::ImmutableString| {
                s.lock().push(ScriptCommand::WindowSetTitle {
                    title: title.to_string(),
                });
            },
        );
    }
    {
        let s = sink.clone();
        engine.register_fn(
            "set_size",
            move |_w: &mut Window, width: f64, height: f64| {
                s.lock().push(ScriptCommand::WindowSetSize {
                    width: width as f32,
                    height: height as f32,
                });
            },
        );
    }

    // window.location parts. The path is the page Lumen resolved; the query
    // and the fragment come from the request the document is being rendered
    // for, and are empty when there is none.
    engine.register_fn("path", |_l: &mut Location| -> rhai::ImmutableString {
        lumen_core::nav::current().into()
    });
    engine.register_fn("query", |_l: &mut Location| -> rhai::ImmutableString {
        lumen_core::request::query().into()
    });
    engine.register_fn("hash", |_l: &mut Location| -> rhai::ImmutableString {
        lumen_core::request::hash().into()
    });

    // history.
    engine.register_fn("back", |_h: &mut History| {
        lumen_core::nav::back();
    });
    engine.register_fn("forward", |_h: &mut History| {
        lumen_core::nav::forward();
    });
    engine.register_fn("go", |_h: &mut History, delta: i64| {
        // Negative steps back, positive forward; 0 is a no-op reload of the
        // in-memory stack cursor.
        let step = if delta < 0 {
            lumen_core::nav::back as fn() -> bool
        } else {
            lumen_core::nav::forward as fn() -> bool
        };
        for _ in 0..delta.unsigned_abs() {
            step();
        }
    });

    // document entry points.
    engine.register_fn("root", |_d: &mut Document| -> rhai::Dynamic {
        node_to_dynamic(node_query::run_document())
    });
    engine.register_fn(
        "query",
        |_d: &mut Document,
         selector: rhai::ImmutableString|
         -> Result<NodeQuery, Box<EvalAltResult>> {
            match node_query::run_query(selector.as_str()) {
                Ok(q) => Ok(NodeQuery { nodes: q.nodes }),
                Err(e) => Err(e.into()),
            }
        },
    );
    engine.register_fn(
        "get_by_id",
        |_d: &mut Document, id: rhai::ImmutableString| -> rhai::Dynamic {
            node_to_dynamic(node_query::run_get_by_id(id.as_str()))
        },
    );
    engine.register_fn("focused", |_d: &mut Document| -> rhai::Dynamic {
        node_to_dynamic(node_query::focused_node())
    });
    engine.register_fn("hovered", |_d: &mut Document| -> rhai::Dynamic {
        node_to_dynamic(node_query::hovered_node())
    });
    // Create verb. Rhai's tokenizer reserves `spawn`, so a rhai script writes
    // `create` (`document.create("div")` / `create("div")`) where the other
    // hosts also accept `spawn`.
    let s = sink.clone();
    engine.register_fn(
        "create",
        move |_d: &mut Document, tag: rhai::ImmutableString| -> Node {
            let (handle, cmd) = node_query::build_spawn(tag.as_str());
            s.lock().push(cmd);
            Node { handle }
        },
    );
    let s = sink.clone();
    engine.register_fn("create", move |tag: rhai::ImmutableString| -> Node {
        let (handle, cmd) = node_query::build_spawn(tag.as_str());
        s.lock().push(cmd);
        Node { handle }
    });
}

/// Register the phase-4 event surface: the `Event` handle type + accessor
/// methods, `n.on(type, handler)` / `on(type, handler, capture)` /
/// `on_capture(type, handler)` returning an off token, and the
/// `__lumen_off` native unbind the off token curries.
///
/// rhai calls a function-pointer value through its `.call()` method (a bare
/// `off()` is not valid rhai for a variable holding a `FnPtr`), so the off
/// token is invoked as `off.call()`. This mirrors the `create` synonym
/// adaptation for the reserved `spawn` keyword.
fn register_dom_events(
    engine: &mut Engine,
    sink: Arc<Mutex<Vec<ScriptCommand>>>,
    closures: EventClosureMap,
) {
    use lumen_script::event as ev;

    engine.register_type_with_name::<Event>("Event");
    engine.register_fn("target", |_e: &mut Event| {
        node_to_dynamic(Some(ev::event_target()))
    });
    engine.register_fn("current_target", |_e: &mut Event| {
        node_to_dynamic(Some(ev::event_current_target()))
    });
    engine.register_fn("event_type", |_e: &mut Event| -> String {
        ev::event_type()
    });
    engine.register_fn("key", |_e: &mut Event| -> String { ev::event_key() });
    engine.register_fn("value", |_e: &mut Event| -> String { ev::event_value() });
    engine.register_fn("button", |_e: &mut Event| -> i64 { ev::event_button() });
    engine.register_fn("x", |_e: &mut Event| -> f64 {
        ev::event_position_local().0
    });
    engine.register_fn("y", |_e: &mut Event| -> f64 {
        ev::event_position_local().1
    });
    engine.register_fn("client_x", |_e: &mut Event| -> f64 {
        ev::event_position_client().0
    });
    engine.register_fn("client_y", |_e: &mut Event| -> f64 {
        ev::event_position_client().1
    });
    engine.register_fn("delta_x", |_e: &mut Event| -> f64 { ev::event_delta().0 });
    engine.register_fn("delta_y", |_e: &mut Event| -> f64 { ev::event_delta().1 });
    engine.register_fn("position", |_e: &mut Event| -> rhai::Map {
        let (x, y) = ev::event_position_local();
        let (cx, cy) = ev::event_position_client();
        let mut m = rhai::Map::new();
        m.insert("x".into(), rhai::Dynamic::from(x));
        m.insert("y".into(), rhai::Dynamic::from(y));
        m.insert("client_x".into(), rhai::Dynamic::from(cx));
        m.insert("client_y".into(), rhai::Dynamic::from(cy));
        m
    });
    engine.register_fn("modifiers", |_e: &mut Event| -> rhai::Map {
        let (shift, ctrl, alt, super_) = ev::event_modifiers();
        let mut m = rhai::Map::new();
        m.insert("shift".into(), rhai::Dynamic::from(shift));
        m.insert("ctrl".into(), rhai::Dynamic::from(ctrl));
        m.insert("alt".into(), rhai::Dynamic::from(alt));
        m.insert("super".into(), rhai::Dynamic::from(super_));
        m
    });
    engine.register_fn("prevent_default", |_e: &mut Event| {
        ev::event_prevent_default()
    });
    engine.register_fn("stop_propagation", |_e: &mut Event| {
        ev::event_stop_propagation()
    });
    engine.register_fn("stop_immediate_propagation", |_e: &mut Event| {
        ev::event_stop_immediate_propagation()
    });

    // Native unbind: the off token curries its `token` and calls this.
    {
        let s = sink.clone();
        let cl = closures.clone();
        engine.register_fn("__lumen_off", move |token: i64| {
            let tok = token as u64;
            if let Ok(mut c) = cl.write() {
                c.remove(&tok);
            }
            s.lock().push(ScriptCommand::UnbindEvent { token: tok });
        });
    }

    // on(node, type, handler) + capture variants. Each mints a token,
    // stores the handler, emits BindEvent, and returns the off token.
    for (name, forced_capture) in [("on", None), ("on_capture", Some(true))] {
        let s = sink.clone();
        let cl = closures.clone();
        engine.register_fn(
            name,
            move |n: &mut Node,
                  event_type: rhai::ImmutableString,
                  handler: rhai::FnPtr|
                  -> rhai::Dynamic {
                bind_event(
                    n.handle,
                    &event_type,
                    forced_capture.unwrap_or(false),
                    handler,
                    &s,
                    &cl,
                )
            },
        );
    }
    // Explicit capture-bool overload of `on`.
    {
        let s = sink.clone();
        let cl = closures.clone();
        engine.register_fn(
            "on",
            move |n: &mut Node,
                  event_type: rhai::ImmutableString,
                  handler: rhai::FnPtr,
                  capture: bool|
                  -> rhai::Dynamic {
                bind_event(n.handle, &event_type, capture, handler, &s, &cl)
            },
        );
    }
}

/// Mint a token, store `handler`, emit a [`ScriptCommand::BindEvent`], and
/// return the off token (a curried `__lumen_off` `FnPtr`, invoked as
/// `off.call()`).
fn bind_event(
    node: u64,
    event_type: &str,
    capture: bool,
    handler: rhai::FnPtr,
    sink: &Arc<Mutex<Vec<ScriptCommand>>>,
    closures: &EventClosureMap,
) -> rhai::Dynamic {
    let token = lumen_script::event::mint_event_token();
    if let Ok(mut c) = closures.write() {
        c.insert(token, handler);
    }
    sink.lock().push(ScriptCommand::BindEvent {
        node,
        event_type: event_type.to_string(),
        capture,
        token,
    });
    match rhai::FnPtr::new("__lumen_off") {
        Ok(mut fp) => {
            fp.add_curry(rhai::Dynamic::from(token as i64));
            rhai::Dynamic::from(fp)
        }
        Err(_) => rhai::Dynamic::UNIT,
    }
}

/// Shared per-event handler registry: `(event, id) -> fn_name`.
///
/// Wrapped in [`RwLock`] (not [`parking_lot::Mutex`]) so the hot
/// dispatch path can take a read lock - multiple dispatchers can
/// look up handlers concurrently, and the only writer is the
/// rarely-invoked `on(event, id, fn)` builtin (and the hot-reload
/// swap). Killing the parking_lot::Mutex also kills the re-entry
/// deadlock hazard flagged in the audit: a read lock taken inside a
/// derivation closure that triggers another lookup no longer
/// deadlocks.
type HandlerMap = Arc<RwLock<std::collections::HashMap<(String, String), String>>>;

/// Parse a `"#rrggbb"` or `"#rrggbbaa"` hex color into RGBA bytes. Leading `#`
/// is optional. Returns `None` when the input doesn't match either shape.
///
/// Helper for the typed `signal_set_color` / `signal_get_color` Rhai builtins;
/// the parsed channels are stored as a Rhai `Map` with `{ r, g, b, a }` i64
/// fields so script code reads channels as plain integers.
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

/// Stringify a Rhai `Dynamic` for the ECS-side signal mirror. Strings
/// stay verbatim; everything else takes the canonical `Display` form.
fn stringify_dynamic(v: &rhai::Dynamic) -> String {
    if v.is_string() {
        v.clone()
            .into_immutable_string()
            .map(|s| s.to_string())
            .unwrap_or_default()
    } else {
        v.to_string()
    }
}

/// Project a typed [`PropertyValue`] into the closest Rhai `Dynamic`
/// flavour: i64 / f64 / bool / `{ r, g, b, a }` map for Color / String.
/// `Vec2` and `Custom` have no script-side projection and land as UNIT.
/// Shared by [`SignalRef::get`] and the `signal(name, default)` factory
/// (which seeds the host mirror from a pre-existing store value).
fn property_value_to_dynamic(v: &PropertyValue) -> rhai::Dynamic {
    match v {
        PropertyValue::I64(n) => rhai::Dynamic::from(*n),
        PropertyValue::F64(f) => rhai::Dynamic::from(*f),
        PropertyValue::Bool(b) => rhai::Dynamic::from(*b),
        PropertyValue::Color(c) => {
            let mut m = rhai::Map::new();
            m.insert(
                "r".into(),
                rhai::Dynamic::from((c.r * 255.0).round() as i64),
            );
            m.insert(
                "g".into(),
                rhai::Dynamic::from((c.g * 255.0).round() as i64),
            );
            m.insert(
                "b".into(),
                rhai::Dynamic::from((c.b * 255.0).round() as i64),
            );
            m.insert(
                "a".into(),
                rhai::Dynamic::from((c.a * 255.0).round() as i64),
            );
            rhai::Dynamic::from(m)
        }
        PropertyValue::Str(s) => rhai::Dynamic::from(s.to_string()),
        PropertyValue::Vec2(_) | PropertyValue::Custom(_) => rhai::Dynamic::UNIT,
    }
}

/// Rhai-facing handle to one named scalar signal. Returned by the
/// `signal(name, default)` builtin. `.get()` reads the rich Dynamic
/// from the host-local mirror; `.set(v)` writes both the local mirror
/// (so subsequent reads see the new value within the same tick) and
/// queues a `ScriptCommand::SetSignal` so markup `bind-text` bindings
/// observe the stringified form on the next tick.
#[derive(Clone)]
pub struct Signal {
    name: rhai::ImmutableString,
    host: Arc<Mutex<std::collections::HashMap<String, rhai::Dynamic>>>,
    sink: Arc<Mutex<Vec<ScriptCommand>>>,
}

impl Signal {
    /// Read the current value. Falls back to `()` (Rhai UNIT) if the
    /// signal was somehow removed from the host mirror - practically
    /// never, since `signal(name, default)` auto-initialises.
    pub fn get(&mut self) -> rhai::Dynamic {
        self.host
            .lock()
            .get(self.name.as_str())
            .cloned()
            .unwrap_or(rhai::Dynamic::UNIT)
    }

    /// Replace the value; queue the stringified version for the ECS
    /// `Signals` mirror.
    pub fn set(&mut self, value: rhai::Dynamic) {
        let text = stringify_dynamic(&value);
        self.host.lock().insert(self.name.to_string(), value);
        self.sink.lock().push(ScriptCommand::SetSignal {
            name: self.name.to_string(),
            value: text,
        });
    }

    /// `register_get_set` setter needs a `(&mut Self, Dynamic) -> ()`
    /// shape; we delegate to [`Self::set`] for the implementation.
    pub fn set_proxy(&mut self, value: rhai::Dynamic) {
        self.set(value);
    }
}

/// Rhai-facing handle to one named reactive array. Returned by
/// `signal_array(name)`. The backing store is the same host-local
/// signal mirror used by [`Signal`] - array signals just happen to
/// hold a `rhai::Array` Dynamic.
#[derive(Clone)]
pub struct ArraySignal {
    name: rhai::ImmutableString,
    host: Arc<Mutex<std::collections::HashMap<String, rhai::Dynamic>>>,
    sink: Arc<Mutex<Vec<ScriptCommand>>>,
}

impl ArraySignal {
    fn items_clone(&self) -> rhai::Array {
        self.host
            .lock()
            .get(self.name.as_str())
            .cloned()
            .and_then(|d| d.try_cast::<rhai::Array>())
            .unwrap_or_default()
    }

    fn flush(&self, array: &rhai::Array) {
        let mut items: Vec<std::collections::HashMap<String, String>> =
            Vec::with_capacity(array.len());
        for elt in array {
            if let Some(map) = elt.clone().try_cast::<rhai::Map>() {
                let mut record = std::collections::HashMap::with_capacity(map.len());
                for (k, v) in map {
                    record.insert(k.to_string(), stringify_dynamic(&v));
                }
                items.push(record);
            }
        }
        self.sink.lock().push(ScriptCommand::SetArray {
            name: self.name.to_string(),
            items,
        });
    }

    /// Replace the entire array (each item a `Map` of stringifiable
    /// fields). Drives `<for each="name">` reconciliation on the next
    /// tick.
    pub fn set(&mut self, array: rhai::Array) {
        self.host
            .lock()
            .insert(self.name.to_string(), array.clone().into());
        self.flush(&array);
    }

    /// Append one record. Equivalent to reading the current array,
    /// pushing `item`, and calling `set(...)` - but skips a clone.
    pub fn push(&mut self, item: rhai::Dynamic) {
        let mut current = self.items_clone();
        current.push(item);
        self.host
            .lock()
            .insert(self.name.to_string(), current.clone().into());
        self.flush(&current);
    }

    /// Number of items currently in the array.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&mut self) -> i64 {
        self.items_clone().len() as i64
    }

    /// Read the item at `index` as a Rhai value. Returns `()` (UNIT) on
    /// out-of-bounds; matches `signal_get`'s previous default semantics.
    pub fn index(&mut self, index: i64) -> rhai::Dynamic {
        let items = self.items_clone();
        if index < 0 {
            return rhai::Dynamic::UNIT;
        }
        items
            .into_iter()
            .nth(index as usize)
            .unwrap_or(rhai::Dynamic::UNIT)
    }

    /// Return a snapshot of the entire backing array. The result is
    /// owned by the caller - mutations need a follow-up `set(...)` to
    /// propagate back to the host.
    pub fn all(&mut self) -> rhai::Array {
        self.items_clone()
    }
}

/// Rhai-facing chained-access handle for the typed property bus.
///
/// Replaces the procedural `signal_set_int(name, v)` / `signal_get_int(name)`
/// pairs with a chained idiom:
///
/// ```text
/// signals.count.set(5)
/// signals.user.name.set("Alice")
/// signals.users[0].name.set("Bob")
/// signals.bg.set_color("#ff8800")
/// let v = signals.count.get();
/// ```
///
/// A `signals` constant of type `SignalRef { path: [] }` is pushed into the
/// engine scope at construction. Rhai's property-access chaining falls back
/// to the registered string indexer (and i64 indexer for `[0]`), so every
/// `.name` / `[i]` returns a fresh `SignalRef` with the segment appended to
/// `path`. The terminal `.set(v)` / `.get()` / `.set_color(s)` method joins
/// the path into a single `PropertyKey::Global` and routes through the same
/// `push_external_property` bus the round-4 typed setters use.
///
/// Path joining: name segments are `.`-joined; index segments are appended
/// directly (no separator), so `users[0].name` produces the literal key
/// `"users[0].name"`. Lumen resolves nested storage at bind time; a future
/// revision may migrate this onto real array storage in `PropertyStore`.
#[derive(Clone, Debug)]
pub struct SignalRef {
    /// Path segments accumulated by chained property / indexer access.
    /// Plain names (`count`, `user`, `name`) appear verbatim; array
    /// indices appear as `"[N]"` so `to_key()` can recognise them and
    /// append without a `.` separator.
    path: Vec<String>,
    /// Shared host-local signal mirror. Used by `get()` as a fallback
    /// when the typed-property snapshot has no entry for the joined key.
    host: Arc<Mutex<std::collections::HashMap<String, rhai::Dynamic>>>,
}

impl SignalRef {
    /// Append a name segment, returning a new ref. Cheap clone of the
    /// (typically tiny) `path` vec.
    fn with_name(&self, name: &str) -> Self {
        let mut path = self.path.clone();
        path.push(name.to_string());
        Self {
            path,
            host: self.host.clone(),
        }
    }

    /// Append an index segment as `"[N]"`. The `[` prefix is the marker
    /// that [`Self::to_key`] uses to avoid emitting a `.` before this
    /// segment, so `users` + `[0]` joins to `"users[0]"`.
    fn with_index(&self, idx: i64) -> Self {
        let mut path = self.path.clone();
        path.push(format!("[{idx}]"));
        Self {
            path,
            host: self.host.clone(),
        }
    }

    /// Join the accumulated path into a single dotted-with-brackets key
    /// suitable for `PropertyKey::Global`. Name segments are
    /// `.`-separated; index segments (`"[0]"`) attach directly to the
    /// preceding segment, matching the natural script syntax.
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

    /// Indexer-get / property-get fallback used by Rhai for `signals.foo`
    /// and `signals["foo"]`. Returns a new `SignalRef` with `name` pushed
    /// onto the path.
    pub fn index_by_name(&mut self, name: rhai::ImmutableString) -> SignalRef {
        self.with_name(name.as_str())
    }

    /// Indexer-get for numeric subscripts (`signals.users[0]`). The index
    /// segment lands on the path as `"[0]"`, so the eventual key keeps
    /// the array-literal shape downstream code already recognises.
    pub fn index_by_int(&mut self, idx: i64) -> SignalRef {
        self.with_index(idx)
    }

    /// Push `PropertyValue::I64` through the typed-property bus, keyed
    /// on the joined path. Mirrors the legacy `signal_set_int(name, v)`
    /// builtin; bypasses both the Rhai `ScriptCommand::SetSignal` sink
    /// and the string-typed `Signals` resource.
    pub fn set_int(&mut self, value: i64) {
        let key = self.to_key();
        self.host
            .lock()
            .insert(key.clone(), rhai::Dynamic::from(value));
        lumen_core::property_store::push_external_property(
            PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str())),
            PropertyValue::I64(value),
        );
    }

    /// Push `PropertyValue::F64` through the typed-property bus.
    pub fn set_float(&mut self, value: f64) {
        let key = self.to_key();
        self.host
            .lock()
            .insert(key.clone(), rhai::Dynamic::from(value));
        lumen_core::property_store::push_external_property(
            PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str())),
            PropertyValue::F64(value),
        );
    }

    /// Push `PropertyValue::Bool` through the typed-property bus.
    pub fn set_bool(&mut self, value: bool) {
        let key = self.to_key();
        self.host
            .lock()
            .insert(key.clone(), rhai::Dynamic::from(value));
        lumen_core::property_store::push_external_property(
            PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str())),
            PropertyValue::Bool(value),
        );
    }

    /// Push `PropertyValue::Str` through the typed-property bus. Strings
    /// arrive as `rhai::ImmutableString`; we move them into an `Arc<str>`
    /// for the typed payload.
    pub fn set_string(&mut self, value: rhai::ImmutableString) {
        let key = self.to_key();
        self.host
            .lock()
            .insert(key.clone(), rhai::Dynamic::from(value.clone()));
        lumen_core::property_store::push_external_property(
            PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str())),
            PropertyValue::Str(std::sync::Arc::<str>::from(value.as_str())),
        );
    }

    /// Push `PropertyValue::Color` from a `"#rrggbb"` / `"#rrggbbaa"`
    /// hex literal. Invalid hex inputs no-op (matches the legacy
    /// `signal_set_color` behaviour). Hex detection is opt-in via the
    /// explicit `set_color` method; `set("#ff8800")` lands as
    /// `PropertyValue::Str` deliberately - automatic detection by string
    /// content would be too magical.
    pub fn set_color(&mut self, hex: rhai::ImmutableString) {
        if let Some((r, g, b, a)) = parse_hex_color(hex.as_str()) {
            let key = self.to_key();
            let mut m = rhai::Map::new();
            m.insert("r".into(), rhai::Dynamic::from(r as i64));
            m.insert("g".into(), rhai::Dynamic::from(g as i64));
            m.insert("b".into(), rhai::Dynamic::from(b as i64));
            m.insert("a".into(), rhai::Dynamic::from(a as i64));
            self.host.lock().insert(key.clone(), rhai::Dynamic::from(m));
            let color = lumen_core::components::Color::rgba(
                (r as f32) / 255.0,
                (g as f32) / 255.0,
                (b as f32) / 255.0,
                (a as f32) / 255.0,
            );
            lumen_core::property_store::push_external_property(
                PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str())),
                PropertyValue::Color(color),
            );
        }
    }

    /// Read the value for this path. Consults the cross-thread typed
    /// property snapshot first (so writes from any thread are visible);
    /// falls back to the Rhai host-local mirror; finally returns UNIT.
    ///
    /// Typed property variants are projected back into the corresponding
    /// `Dynamic` flavour (i64 / f64 / bool / Map for Color / String).
    pub fn get(&mut self) -> rhai::Dynamic {
        let key = self.to_key();
        let typed_key = PropertyKey::Global(std::sync::Arc::<str>::from(key.as_str()));
        let snapshot = lumen_core::property_store::typed_property_snapshot();
        if let Some(v) = snapshot.get(&typed_key) {
            return property_value_to_dynamic(v);
        }
        self.host
            .lock()
            .get(&key)
            .cloned()
            .unwrap_or(rhai::Dynamic::UNIT)
    }
}

/// Rhai-backed [`ScriptHost`]. With Rhai's `sync` feature, `Engine`, `AST`,
/// `Scope`, and `Dynamic` are all `Send + Sync`, so the host can be a
/// regular [`Resource`] and participate in parallel scheduling.
#[derive(Resource)]
pub struct RhaiHost {
    engine: Engine,
    ast: Option<AST>,
    scope: Scope<'static>,
    sink: Arc<Mutex<Vec<ScriptCommand>>>,
    /// Host-side mirror of the reactive signal store. Scripts read +
    /// write via `signal_get` / `signal_set`; this map is the
    /// authoritative copy *for the script side*. The ECS-side
    /// [`lumen_core::signals::Signals`] resource is the *stringified*
    /// mirror, populated via [`ScriptCommand::SetSignal`] when
    /// `signal_set` runs - it drives `bind-text="..."` markup
    /// bindings. Keeping a rich-typed local map lets scripts work with
    /// numbers, bools, and arrays natively (deserialized via `parse_json`,
    /// for example) without round-tripping through string at every read.
    signals_local: Arc<Mutex<std::collections::HashMap<String, rhai::Dynamic>>>,
    /// Per-id event handler registry. Keys are `(event_name, lumen_id)`
    /// pairs (e.g. `("click", "search")`) populated by the Rhai
    /// `on(event, id, fn_name)` builtin. When a dispatcher fires it
    /// looks up `(event, id)` here first; on hit it calls the registered
    /// fn name and SKIPS the global `on_<event>(id)` fallback. Lets
    /// authors write `on("click", "search", "handle_search")` instead
    /// of an `if id == "search" { ... }` chain inside one giant
    /// `on_click`.
    ///
    /// Read-mostly; lookups take a read lock so concurrent dispatchers
    /// don't serialise. Writes happen at script load (`on(...)` builtin)
    /// and during hot-reload swap.
    handlers: HandlerMap,
    /// Derived-signal registry: `signal_name -> (dep names, closure)`.
    /// Populated by the `derive(name, deps, fn)` Rhai builtin and
    /// consumed by [`apply_derivations`] each tick. The closure is a
    /// Rhai `FnPtr` - a captured anonymous fn, called with the current
    /// dep values to produce the new derived value.
    derivations: DerivationMap,
    /// Names of derivations that have been registered but never
    /// evaluated. On the next [`apply_derivations`] pass they all run
    /// regardless of dirty status so the derived signal gets its first
    /// value before any dep ever changes.
    pending_initial: Arc<Mutex<std::collections::HashSet<String>>>,
    /// `token -> FnPtr` for `n.on(type, handler)` bindings (phase 4). The
    /// dispatcher looks a handler up here by token; the host-neutral
    /// registry keys the same token to `(node, type, capture)`.
    event_closures: EventClosureMap,
    /// The [`ScriptFn`]s an embedder registered, kept so [`RhaiHost::reset`]
    /// can put them back.
    script_fns: ScriptFnStore,
    /// One `Module` per [`ScriptNs::Named`] namespace. Rhai takes a static
    /// module by value, so a namespace that gains a function is rebuilt and
    /// re-registered whole; the map is what makes the second registration keep
    /// the first one's functions.
    modules: HashMap<String, Module>,
}

/// `signal_name -> (dep names, closure)`. Threaded through
/// `RhaiHost.derivations` and the `derive(...)` Rhai builtin.
type DerivationMap = Arc<Mutex<std::collections::HashMap<String, (Vec<String>, rhai::FnPtr)>>>;

impl Default for RhaiHost {
    fn default() -> Self {
        Self::new()
    }
}

impl RhaiHost {
    /// Construct a fresh host with the lumen builtin module registered.
    pub fn new() -> Self {
        let sink: Arc<Mutex<Vec<ScriptCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let signals_local: Arc<Mutex<std::collections::HashMap<String, rhai::Dynamic>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let handlers: HandlerMap = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let derivations: DerivationMap = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pending_initial: Arc<Mutex<std::collections::HashSet<String>>> =
            Arc::new(Mutex::new(std::collections::HashSet::new()));
        let event_closures: EventClosureMap =
            Arc::new(RwLock::new(std::collections::HashMap::new()));
        let mut engine = Engine::new();

        // --- Engine limits (deliberate) ------------------------------
        //
        // Rhai's release-build parser defaults are conservative:
        // `max_expr_depth` 64 (32 inside function bodies) and call
        // stack 64; debug builds are far lower (32/16/8). Real app
        // scripts - long `if/else-if` chains, big literal maps,
        // chained string concatenation - blow past the expression cap
        // and the whole script dies at load with "Expression exceeds
        // maximum complexity" while the window keeps rendering.
        //
        // Raise the parse-depth caps generously and pin them so debug
        // and release builds parse identically. The runtime safety
        // knobs (`max_operations`, array/string/map sizes, modules)
        // deliberately stay at their unlimited defaults: a Lumen
        // script is the app author's own trusted code, not sandboxed
        // third-party input. `lumenc check` compiles with this same
        // engine (see [`RhaiHost::compile_check`]) so check and run
        // agree on what parses.
        engine.set_max_expr_depths(512, 512);
        engine.set_max_call_levels(128);

        // `print(...)` is a Rhai built-in; we override to capture instead
        // of writing to stdout.
        let sink_for_print = sink.clone();
        engine.on_print(move |s| {
            sink_for_print
                .lock()
                .push(ScriptCommand::Print(s.to_string()));
        });

        // Register a builtin whose whole body is a single
        // `sink.push(<command>)`. Each builtin's argument list AND its
        // `ScriptCommand` construction stay inline at the call site
        // (passed as macro args, not hidden behind another layer), so a
        // reviewer can diff the three script hosts builtin-by-builtin.
        macro_rules! enqueue {
            ($name:literal, |$($arg:ident : $ty:ty),* $(,)?| $build:expr $(,)?) => {{
                let sink = sink.clone();
                engine.register_fn($name, move |$($arg: $ty),*| {
                    sink.lock().push($build);
                });
            }};
        }

        // add_clicks(n)
        enqueue!("add_clicks", |n: i64| ScriptCommand::AddClicks(n as i32));

        // set_string(key, value)
        enqueue!(
            "set_string",
            |key: rhai::ImmutableString, value: rhai::ImmutableString| ScriptCommand::SetString {
                key: key.to_string(),
                value: value.to_string(),
            }
        );

        // set_text(target_id, text)
        enqueue!(
            "set_text",
            |target_id: rhai::ImmutableString, text: rhai::ImmutableString| {
                ScriptCommand::SetText {
                    target_id: target_id.to_string(),
                    text: text.to_string(),
                }
            }
        );

        // set_src(target_id, path) - swap an <image>'s asset path at
        // runtime. The runtime side strips the old loaded asset and
        // queues a fresh decode. Path is taken verbatim and resolved
        // against the app dir by `apply_script_commands` so authors
        // pass app-relative paths like "icons/sun.png".
        enqueue!(
            "set_src",
            |target_id: rhai::ImmutableString, path: rhai::ImmutableString| ScriptCommand::SetSrc {
                target_id: target_id.to_string(),
                path: path.to_string(),
            }
        );

        // Signal / ArraySignal Rhai custom types: handle objects that
        // wrap the host-local signal mirror + the script command sink so
        // `let s = signal("name", default); s.set(v); s.get()` reads
        // naturally and stays close to Solid / Vue idiom. Replaces the
        // prior procedural `signal_get` / `signal_set` /
        // `signal_array_set` builtins.
        engine.register_type_with_name::<Signal>("Signal");
        engine.register_fn("get", Signal::get);
        engine.register_fn("set", Signal::set);
        engine.register_get_set("value", Signal::get, Signal::set_proxy);

        engine.register_type_with_name::<ArraySignal>("ArraySignal");
        engine.register_fn("set", ArraySignal::set);
        engine.register_fn("push", ArraySignal::push);
        engine.register_fn("len", ArraySignal::len);
        engine.register_fn("get", ArraySignal::index);
        engine.register_fn("all", ArraySignal::all);

        // Dynamic DOM read side: query / get_by_id / document + Node /
        // NodeQuery traversal. Stateless (reads the per-tick snapshot).
        register_dom_query(&mut engine);
        register_dom_events(&mut engine, sink.clone(), event_closures.clone());
        // Dynamic DOM write side (phases 2 + 3): fluent Node mutators +
        // the window / document / history namespaces (section 4.8).
        register_dom_mutators(&mut engine, sink.clone());
        register_web_namespaces(&mut engine, sink.clone());
        // Low-level introspection (phase 5): geometry / computed style /
        // matched rules / component reads + global runtime state. Read-only.
        register_introspection(&mut engine);

        // signal(name, default) - return a handle into the named signal,
        // auto-initialising the host-local mirror with `default` if the
        // signal has never been written. Repeat calls with the same name
        // are cheap: the handle is just three Arc clones.
        //
        // RC7: the declaration ALSO publishes `default` to the ECS store
        // (via the ScriptCommand sink) so a markup `bind-text` on a
        // declared-but-never-`set` signal renders the default instead of
        // blank. Non-clobbering: publication only happens the first time
        // a name is seen AND only when no pre-existing value is found -
        // a value the SDK / FFI pushed before script load (still queued
        // on the external bus, or already committed + mirrored) wins and
        // seeds the host mirror instead.
        let signals_for_factory = signals_local.clone();
        let sink_for_factory = sink.clone();
        engine.register_fn(
            "signal",
            move |name: rhai::ImmutableString, default: rhai::Dynamic| -> Signal {
                let publish: Option<String> = {
                    let mut map = signals_for_factory.lock();
                    if map.contains_key(name.as_str()) {
                        None
                    } else {
                        let key = PropertyKey::Global(std::sync::Arc::<str>::from(name.as_str()));
                        // Pending external-bus writes first (pre-run SDK /
                        // FFI pushes not yet drained into the store), then
                        // the committed typed-cell mirror.
                        let existing = lumen_core::property_store::external_property_snapshot()
                            .remove(&key)
                            .or_else(|| {
                                lumen_core::property_store::typed_property_snapshot().remove(&key)
                            });
                        if let Some(v) = existing {
                            map.insert(name.to_string(), property_value_to_dynamic(&v));
                            None
                        } else {
                            map.insert(name.to_string(), default.clone());
                            Some(stringify_dynamic(&default))
                        }
                    }
                };
                if let Some(text) = publish {
                    sink_for_factory.lock().push(ScriptCommand::SetSignal {
                        name: name.to_string(),
                        value: text,
                    });
                }
                Signal {
                    name,
                    host: signals_for_factory.clone(),
                    sink: sink_for_factory.clone(),
                }
            },
        );

        // signal_array(name) - return a handle into the named reactive
        // array. The array itself is created lazily on the first .set()
        // / .push() call; .get(i) / .len() before that return UNIT / 0.
        let signals_for_array_factory = signals_local.clone();
        let sink_for_array_factory = sink.clone();
        engine.register_fn(
            "signal_array",
            move |name: rhai::ImmutableString| -> ArraySignal {
                ArraySignal {
                    name,
                    host: signals_for_array_factory.clone(),
                    sink: sink_for_array_factory.clone(),
                }
            },
        );

        // --- Typed signal builtins (W7.x ergonomics) ------------------
        //
        // Procedural setters/getters for the four scalar types Lumen's
        // PropertyStore models natively (i64, f64, bool, Color). They
        // skip the parse-back step the legacy `signal_get(name) -> string`
        // path takes: the typed read returns the same Dynamic the typed
        // setter stored. Writes still mirror through the ScriptCommand
        // sink so markup `bind-text="..."` sees a stringified value next
        // tick; embedders that read PropertyStore directly observe the
        // typed PropertyValue::I64 / F64 / Bool / Color cell via the
        // `mirror_signals_to_property_store` system that runs at the
        // end of `TickStage::Systems`.
        //
        // Prefer the typed variants over the string-typed `signal_set` /
        // `signal_get` whenever the value fits one of the enumerated
        // PropertyValue variants. The string-typed `signal(name, default)
        // .set(value)` handle still works for arbitrary Dynamic.

        // signal_set_int(name, value) - round 4 typed-signal closure:
        // pushes the typed `PropertyValue::I64` directly through
        // `lumen_core::property_store`'s external typed-property bus.
        // The PropertyStore cell lands the I64 variant on the next tick
        // via `drain_external_properties`; bypasses both the Rhai sink's
        // `ScriptCommand::SetSignal` mirror AND the legacy `Signals`
        // string layer.
        //
        // #[doc(hidden)] - DEPRECATED, use the chained `signals.name.set(v)`
        // form instead (registered below via the `SignalRef` newtype +
        // `register_indexer_get` chain). Kept functional for scripts that
        // already use the string-keyed builtin; emits no warning. The
        // chained equivalent of this call is:
        //     signals.name.set(v)
        // where `name` is the property segment chain (`count`, `user.name`,
        // `users[0].name`, ...) and the terminal `.set(i64)` dispatches the
        // same `PropertyValue::I64` write.
        let signals_for_set_int = signals_local.clone();
        engine.register_fn(
            "signal_set_int",
            move |name: rhai::ImmutableString, value: i64| {
                signals_for_set_int
                    .lock()
                    .insert(name.to_string(), rhai::Dynamic::from(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(std::sync::Arc::<str>::from(name.as_str())),
                    PropertyValue::I64(value),
                );
            },
        );

        // signal_get_int(name) -> Option<i64>; Dynamic::UNIT on miss / wrong type.
        let signals_for_get_int = signals_local.clone();
        engine.register_fn(
            "signal_get_int",
            move |name: rhai::ImmutableString| -> rhai::Dynamic {
                let map = signals_for_get_int.lock();
                match map.get(name.as_str()) {
                    Some(d) if d.is_int() => rhai::Dynamic::from(d.as_int().unwrap()),
                    Some(d) if d.is_float() => rhai::Dynamic::from(d.as_float().unwrap() as i64),
                    Some(d) if d.is_bool() => rhai::Dynamic::from(d.as_bool().unwrap() as i64),
                    Some(d) if d.is_string() => d
                        .clone()
                        .into_immutable_string()
                        .ok()
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(rhai::Dynamic::from)
                        .unwrap_or(rhai::Dynamic::UNIT),
                    _ => rhai::Dynamic::UNIT,
                }
            },
        );

        // signal_set_float(name, value) - pushes `PropertyValue::F64`
        // through the foundation typed-property bus for direct
        // PropertyStore writes; no stringify, no `Signals` mirror.
        //
        // #[doc(hidden)] - DEPRECATED, use the chained `signals.name.set(v)`
        // form. Rhai dispatches the typed `set(f64)` overload on the
        // `SignalRef` newtype, routing the same `PropertyValue::F64`
        // through `push_external_property`.
        let signals_for_set_float = signals_local.clone();
        engine.register_fn(
            "signal_set_float",
            move |name: rhai::ImmutableString, value: f64| {
                signals_for_set_float
                    .lock()
                    .insert(name.to_string(), rhai::Dynamic::from(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(std::sync::Arc::<str>::from(name.as_str())),
                    PropertyValue::F64(value),
                );
            },
        );

        // signal_get_float(name) -> Option<f64>; UNIT on miss / wrong type.
        let signals_for_get_float = signals_local.clone();
        engine.register_fn(
            "signal_get_float",
            move |name: rhai::ImmutableString| -> rhai::Dynamic {
                let map = signals_for_get_float.lock();
                match map.get(name.as_str()) {
                    Some(d) if d.is_float() => rhai::Dynamic::from(d.as_float().unwrap()),
                    Some(d) if d.is_int() => rhai::Dynamic::from(d.as_int().unwrap() as f64),
                    Some(d) if d.is_string() => d
                        .clone()
                        .into_immutable_string()
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(rhai::Dynamic::from)
                        .unwrap_or(rhai::Dynamic::UNIT),
                    _ => rhai::Dynamic::UNIT,
                }
            },
        );

        // signal_set_bool(name, value) - pushes `PropertyValue::Bool`
        // through the foundation typed-property bus for direct
        // PropertyStore writes.
        //
        // #[doc(hidden)] - DEPRECATED, use the chained `signals.name.set(v)`
        // form. The `set(bool)` method on `SignalRef` is the chained
        // equivalent.
        let signals_for_set_bool = signals_local.clone();
        engine.register_fn(
            "signal_set_bool",
            move |name: rhai::ImmutableString, value: bool| {
                signals_for_set_bool
                    .lock()
                    .insert(name.to_string(), rhai::Dynamic::from(value));
                lumen_core::property_store::push_external_property(
                    PropertyKey::Global(std::sync::Arc::<str>::from(name.as_str())),
                    PropertyValue::Bool(value),
                );
            },
        );

        // signal_get_bool(name) -> Option<bool>; UNIT on miss / wrong type.
        let signals_for_get_bool = signals_local.clone();
        engine.register_fn(
            "signal_get_bool",
            move |name: rhai::ImmutableString| -> rhai::Dynamic {
                let map = signals_for_get_bool.lock();
                match map.get(name.as_str()) {
                    Some(d) if d.is_bool() => rhai::Dynamic::from(d.as_bool().unwrap()),
                    Some(d) if d.is_int() => rhai::Dynamic::from(d.as_int().unwrap() != 0),
                    Some(d) if d.is_string() => {
                        let s = d
                            .clone()
                            .into_immutable_string()
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        match s.as_str() {
                            "true" | "1" => rhai::Dynamic::from(true),
                            "false" | "0" | "" => rhai::Dynamic::from(false),
                            _ => rhai::Dynamic::UNIT,
                        }
                    }
                    _ => rhai::Dynamic::UNIT,
                }
            },
        );

        // signal_set_color(name, "#rrggbb" | "#rrggbbaa") - pushes
        // `PropertyValue::Color` through the typed-property bus so the
        // PropertyStore receives a typed Color cell; Rhai-side mirror
        // still stores the `{ r, g, b, a }` map for direct script
        // reads via signal_get_color. Invalid hex inputs no-op
        // (UNIT-style failure mode matches the other typed setters).
        //
        // #[doc(hidden)] - DEPRECATED, use the chained
        // `signals.name.set_color("#rrggbb")` form. `set_color` on
        // `SignalRef` is the explicit hex entry point - `set(string)`
        // lands as `PropertyValue::Str` (auto-detection by string
        // content was rejected as too magical).
        let signals_for_set_color = signals_local.clone();
        engine.register_fn(
            "signal_set_color",
            move |name: rhai::ImmutableString, hex: rhai::ImmutableString| {
                if let Some((r, g, b, a)) = parse_hex_color(hex.as_str()) {
                    let mut m = rhai::Map::new();
                    m.insert("r".into(), rhai::Dynamic::from(r as i64));
                    m.insert("g".into(), rhai::Dynamic::from(g as i64));
                    m.insert("b".into(), rhai::Dynamic::from(b as i64));
                    m.insert("a".into(), rhai::Dynamic::from(a as i64));
                    signals_for_set_color
                        .lock()
                        .insert(name.to_string(), rhai::Dynamic::from(m));
                    let color = lumen_core::components::Color::rgba(
                        (r as f32) / 255.0,
                        (g as f32) / 255.0,
                        (b as f32) / 255.0,
                        (a as f32) / 255.0,
                    );
                    lumen_core::property_store::push_external_property(
                        PropertyKey::Global(std::sync::Arc::<str>::from(name.as_str())),
                        PropertyValue::Color(color),
                    );
                }
            },
        );

        // signal_get_color(name) -> Option<Map>; UNIT on miss / wrong type.
        let signals_for_get_color = signals_local.clone();
        engine.register_fn(
            "signal_get_color",
            move |name: rhai::ImmutableString| -> rhai::Dynamic {
                let map = signals_for_get_color.lock();
                match map.get(name.as_str()) {
                    Some(d) if d.is_map() => d.clone(),
                    Some(d) if d.is_string() => {
                        let s = d
                            .clone()
                            .into_immutable_string()
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        if let Some((r, g, b, a)) = parse_hex_color(&s) {
                            let mut m = rhai::Map::new();
                            m.insert("r".into(), rhai::Dynamic::from(r as i64));
                            m.insert("g".into(), rhai::Dynamic::from(g as i64));
                            m.insert("b".into(), rhai::Dynamic::from(b as i64));
                            m.insert("a".into(), rhai::Dynamic::from(a as i64));
                            rhai::Dynamic::from(m)
                        } else {
                            rhai::Dynamic::UNIT
                        }
                    }
                    _ => rhai::Dynamic::UNIT,
                }
            },
        );

        // --- Chained signal access (`signals.count.set(5)`) ------------
        //
        // Rhai 1.24 falls back to a registered string indexer when no
        // static property getter exists, so `signals.foo` dispatches to
        // `SignalRef::index_by_name(self, "foo")` and returns a fresh
        // `SignalRef` with `"foo"` pushed onto `path`. Likewise
        // `signals.users[0]` chains through the i64 indexer. The
        // terminal `.set(v)` / `.get()` / `.set_color(s)` methods join
        // the path and route through the same typed-property bus the
        // legacy `signal_set_int` / `_float` / `_bool` / `_color`
        // builtins (above) use.
        //
        // Dispatch limits: Rhai resolves `.set(v)` by the runtime type of
        // `v`. Integer literals dispatch `set(i64)` -> `PropertyValue::I64`;
        // float literals -> F64; string literals -> Str. Hex colors require
        // the explicit `set_color("#...")` method - auto-detecting hex from
        // a string payload was rejected as too magical. Untyped Dynamic
        // payloads (e.g. a value from `parse_json`) currently fall back
        // to `set(string)` via Rhai's string coercion; embedders that
        // need to preserve typed payloads should call the specific typed
        // setter explicitly.
        engine.register_type_with_name::<SignalRef>("SignalRef");
        engine.register_indexer_get(SignalRef::index_by_name);
        engine.register_indexer_get(SignalRef::index_by_int);
        engine.register_fn("set", SignalRef::set_int);
        engine.register_fn("set", SignalRef::set_float);
        engine.register_fn("set", SignalRef::set_bool);
        engine.register_fn("set", SignalRef::set_string);
        engine.register_fn("set_color", SignalRef::set_color);
        engine.register_fn("get", SignalRef::get);
        // `signals` constant: every script gets a fresh root `SignalRef`
        // with an empty path. Property / indexer chaining clones the
        // host Arc onto each derived ref - the root never mutates.
        let scope_signals_root = SignalRef {
            path: Vec::new(),
            host: signals_local.clone(),
        };

        // `is_valid(id)` returns `true` when `signal("valid:<id>")` is set to `"true"`. `apply_validation` populates this signal each tick from the entity's `Validation` component.
        let signals_for_is_valid = signals_local.clone();
        engine.register_fn("is_valid", move |id: rhai::ImmutableString| -> bool {
            let key = format!("valid:{}", id);
            let map = signals_for_is_valid.lock();
            match map.get(&key) {
                Some(d) if d.is_string() => d
                    .clone()
                    .into_immutable_string()
                    .map(|s| s.as_str() == "true")
                    .unwrap_or(false),
                Some(d) => d.as_bool().unwrap_or(false),
                None => true,
            }
        });

        // set_timeout(name, ms) - one-shot timer; fires `on_timer(name)`.
        enqueue!("set_timeout", |name: rhai::ImmutableString, ms: i64| {
            ScriptCommand::SetTimer {
                name: name.to_string(),
                millis: ms.max(0) as u64,
                repeat: false,
            }
        });

        // set_interval(name, ms) - repeating timer; fires `on_timer(name)`
        // every `ms` until cancelled.
        enqueue!("set_interval", |name: rhai::ImmutableString, ms: i64| {
            ScriptCommand::SetTimer {
                name: name.to_string(),
                millis: ms.max(0) as u64,
                repeat: true,
            }
        });

        // cancel_timer(name)
        enqueue!("cancel_timer", |name: rhai::ImmutableString| {
            ScriptCommand::CancelTimer {
                name: name.to_string(),
            }
        });

        // notify(title, body) - fire an OS notification via
        // notify-rust. Runs synchronously inside apply_script_commands;
        // backends typically return quickly because the daemon owns
        // the actual display.
        enqueue!(
            "notify",
            |title: rhai::ImmutableString, body: rhai::ImmutableString| ScriptCommand::Notify {
                title: title.to_string(),
                body: body.to_string(),
            }
        );

        // notify_ex(id, title, body, options, actions) - the same
        // notification with an icon, an urgency, and buttons. `options`
        // is `"icon:name-or-path|urgency:critical"` and `actions` is
        // `"id:Label|id2:Label2"`; pressing one fires
        // `on_notification_action(id, action_id)`. Empty strings mean
        // "defaults" and "no buttons".
        enqueue!(
            "notify_ex",
            |id: rhai::ImmutableString,
             title: rhai::ImmutableString,
             body: rhai::ImmutableString,
             options: rhai::ImmutableString,
             actions: rhai::ImmutableString| ScriptCommand::NotifyEx {
                id: id.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                options: options.to_string(),
                actions: actions.to_string(),
            }
        );

        // clipboard_write(text) / clipboard_read(tag) - system clipboard
        // text. The read is answered next tick by `on_clipboard(tag, text)`
        // because the clipboard lives on the main thread's OS handle, not
        // in the script engine.
        enqueue!("clipboard_write", |text: rhai::ImmutableString| {
            ScriptCommand::ClipboardWrite {
                text: text.to_string(),
            }
        });
        enqueue!("clipboard_read", |tag: rhai::ImmutableString| {
            ScriptCommand::ClipboardRead {
                tag: tag.to_string(),
            }
        });

        // open_url(url) / open_path(path) / reveal_path(path) - hand a
        // URL or file to the platform's default handler, or show a file
        // in the file manager. Paths resolve relative to the app dir.
        enqueue!("open_url", |url: rhai::ImmutableString| {
            ScriptCommand::OpenUrl {
                url: url.to_string(),
            }
        });
        enqueue!("open_path", |path: rhai::ImmutableString| {
            ScriptCommand::OpenPath {
                path: path.to_string(),
            }
        });
        enqueue!("reveal_path", |path: rhai::ImmutableString| {
            ScriptCommand::RevealPath {
                path: path.to_string(),
            }
        });

        // keep_awake(name, reason) / allow_sleep(name) - hold off the
        // screensaver and system sleep. Paired by name, like
        // register_hotkey / unregister_hotkey.
        enqueue!(
            "keep_awake",
            |name: rhai::ImmutableString, reason: rhai::ImmutableString| ScriptCommand::KeepAwake {
                name: name.to_string(),
                reason: reason.to_string(),
            }
        );
        enqueue!("allow_sleep", |name: rhai::ImmutableString| {
            ScriptCommand::AllowSleep {
                name: name.to_string(),
            }
        });

        // `copy_image(path)` enqueues a `CopyImageToClipboard` command. Paths resolve relative to the app directory at runtime.
        enqueue!("copy_image", |path: rhai::ImmutableString| {
            ScriptCommand::CopyImageToClipboard {
                path: path.to_string(),
            }
        });

        // `save_clipboard_image(path)` enqueues a `SaveClipboardImage` command writing the current clipboard image to `path` as PNG. Failures log to stderr.
        enqueue!("save_clipboard_image", |path: rhai::ImmutableString| {
            ScriptCommand::SaveClipboardImage {
                path: path.to_string(),
            }
        });

        // `tray_icon(id, icon_path, tooltip)` registers or replaces a system tray icon. Clicks invoke `on_tray(id)`. An empty `tooltip` string disables the tooltip.
        enqueue!(
            "tray_icon",
            |id: rhai::ImmutableString,
             icon_path: rhai::ImmutableString,
             tooltip: rhai::ImmutableString| ScriptCommand::RegisterTrayIcon {
                id: id.to_string(),
                icon_path: icon_path.to_string(),
                tooltip: if tooltip.is_empty() {
                    None
                } else {
                    Some(tooltip.to_string())
                },
                menu: String::new(),
                template: false,
            }
        );

        // `tray_icon_menu(id, icon_path, tooltip, menu, template)` adds a
        // context menu and the macOS template-image flag. `menu` is
        // `"id:Label|-|id2:Label2"` where `-` is a separator; picking an
        // item fires `on_menu(id)`.
        enqueue!(
            "tray_icon_menu",
            |id: rhai::ImmutableString,
             icon_path: rhai::ImmutableString,
             tooltip: rhai::ImmutableString,
             menu: rhai::ImmutableString,
             template: bool| ScriptCommand::RegisterTrayIcon {
                id: id.to_string(),
                icon_path: icon_path.to_string(),
                tooltip: if tooltip.is_empty() {
                    None
                } else {
                    Some(tooltip.to_string())
                },
                menu: menu.to_string(),
                template,
            }
        );

        // unregister_tray(id) - drop a previously-registered tray icon.
        enqueue!("unregister_tray", |id: rhai::ImmutableString| {
            ScriptCommand::UnregisterTrayIcon { id: id.to_string() }
        });

        // `open_menu(id)` / `close_menu(id)` set `__menu_open:<id>` to `"true"` / `"false"`.
        enqueue!("open_menu", |id: rhai::ImmutableString| {
            ScriptCommand::SetSignal {
                name: format!("__menu_open:{id}"),
                value: "true".to_string(),
            }
        });
        enqueue!("close_menu", |id: rhai::ImmutableString| {
            ScriptCommand::SetSignal {
                name: format!("__menu_open:{id}"),
                value: "false".to_string(),
            }
        });

        // pick_file(tag) / pick_files(tag) / pick_folder(tag) /
        // save_file(tag, default_name) - show a native file dialog.
        // The runtime opens the dialog on the main thread via `rfd`,
        // then fires `on_file_picked(tag, path)`,
        // `on_files_picked(tag, paths_joined_by_pipe)`, or
        // `on_folder_picked(tag, path)` once the user closes it. A
        // cancelled dialog still fires once with an empty path so
        // scripts can clean up modal state.
        for (name, kind) in [
            ("pick_file", lumen_script::FileDialogKind::Open),
            ("pick_files", lumen_script::FileDialogKind::OpenMulti),
            ("pick_folder", lumen_script::FileDialogKind::PickFolder),
        ] {
            let sink_for = sink.clone();
            engine.register_fn(name, move |tag: rhai::ImmutableString| {
                sink_for.lock().push(ScriptCommand::OpenFileDialog {
                    kind,
                    tag: tag.to_string(),
                    filters: Vec::new(),
                    default_name: None,
                });
            });
        }
        enqueue!(
            "save_file",
            |tag: rhai::ImmutableString, default_name: rhai::ImmutableString| {
                ScriptCommand::OpenFileDialog {
                    kind: lumen_script::FileDialogKind::Save,
                    tag: tag.to_string(),
                    filters: Vec::new(),
                    default_name: Some(default_name.to_string()),
                }
            }
        );

        // register_hotkey(name, accel) / unregister_hotkey(name) -
        // hook an OS-level global accelerator. `on_hotkey(name)`
        // fires every time the OS dispatches the chord (window
        // focus optional). Accelerator syntax follows global-hotkey
        // / Electron conventions: `"CommandOrControl+S"`,
        // `"Alt+Space"`, `"F11"`.
        enqueue!(
            "register_hotkey",
            |name: rhai::ImmutableString, accelerator: rhai::ImmutableString| {
                ScriptCommand::RegisterHotkey {
                    name: name.to_string(),
                    accelerator: accelerator.to_string(),
                }
            }
        );
        enqueue!("unregister_hotkey", |name: rhai::ImmutableString| {
            ScriptCommand::UnregisterHotkey {
                name: name.to_string(),
            }
        });

        // set_class(id, classes) / set_root_class(classes) - mutate
        // `LumenClasses` on a `LumenId`-tagged entity (or the root).
        // The runtime side detects `Changed<LumenClasses>` on the root
        // and re-applies CSS so theme-token selectors light up live.
        enqueue!(
            "set_class",
            |id: rhai::ImmutableString, classes: rhai::ImmutableString| ScriptCommand::SetClasses {
                target_id: id.to_string(),
                classes: classes.to_string(),
            }
        );
        enqueue!("set_root_class", |classes: rhai::ImmutableString| {
            ScriptCommand::SetClasses {
                target_id: "<root>".to_string(),
                classes: classes.to_string(),
            }
        });

        // pick_file_filtered(tag, "Images:png,jpg|All:*") - same as
        // pick_file but with an `rfd`-style filter list. The spec is
        // pipe-separated `<label>:<ext1>,<ext2>,...` groups; the bare
        // `*` extension means "no filter, all files".
        enqueue!(
            "pick_file_filtered",
            |tag: rhai::ImmutableString, spec: rhai::ImmutableString| {
                ScriptCommand::OpenFileDialog {
                    kind: lumen_script::FileDialogKind::Open,
                    tag: tag.to_string(),
                    filters: parse_dialog_filter_spec(spec.as_str()),
                    default_name: None,
                }
            }
        );

        // fetch(url, tag) - issue HTTP GET; on_fetch(tag, body) fires
        // once the response lands. Simple sugar over `http` (below);
        // both share the runtime's single off-thread transport.
        enqueue!(
            "fetch",
            |url: rhai::ImmutableString, tag: rhai::ImmutableString| ScriptCommand::Fetch {
                url: url.to_string(),
                tag: tag.to_string(),
            }
        );

        // http(#{ method, url, headers, body, timeout_ms, tag }) - issue
        // a general HTTP request. `on_http(tag, response)` fires once the
        // reply lands, where `response` is
        // `#{ ok, status, headers, body, error }`. Only `url` and `tag`
        // are required; `method` defaults to "GET", `headers` to `#{}`,
        // `body` / `timeout_ms` are optional. Runs off-thread; the reply
        // is marshalled onto the world thread before any signal is
        // touched (see script-runtime `fire_fetched_responses`).
        let sink_for_http = sink.clone();
        engine.register_fn("http", move |req: rhai::Map| {
            let get_str = |m: &rhai::Map, k: &str| -> Option<String> {
                m.get(k).and_then(|d| {
                    d.clone()
                        .into_immutable_string()
                        .ok()
                        .map(|s| s.to_string())
                })
            };
            let method = get_str(&req, "method").unwrap_or_else(|| "GET".to_string());
            let url = get_str(&req, "url").unwrap_or_default();
            let tag = get_str(&req, "tag").unwrap_or_default();
            // `body`: a string is sent verbatim; `()` / missing = no body.
            let body = req.get("body").and_then(|d| {
                if d.is_unit() {
                    None
                } else {
                    d.clone()
                        .into_immutable_string()
                        .ok()
                        .map(|s| s.to_string())
                }
            });
            // `timeout_ms`: accept an int; ignore non-positive / non-int.
            let timeout_ms = req
                .get("timeout_ms")
                .and_then(|d| d.as_int().ok())
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0);
            // `headers`: a `#{ "Header": "value" }` map. Values are
            // stringified (a bare int/float header value still works).
            let headers = req
                .get("headers")
                .and_then(|d| d.read_lock::<rhai::Map>().map(|m| m.clone()))
                .map(|m| {
                    m.into_iter()
                        .map(|(k, v)| {
                            let vs = v
                                .clone()
                                .into_immutable_string()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|_| v.to_string());
                            (k.to_string(), vs)
                        })
                        .collect::<Vec<(String, String)>>()
                })
                .unwrap_or_default();
            sink_for_http.lock().push(ScriptCommand::Http {
                method,
                url,
                headers,
                body,
                timeout_ms,
                tag,
            });
        });

        // request_header(name) / request_cookie(name) / request_body() -
        // read the request the document is being rendered for. The
        // headers, the cookies and the body are too large to publish as
        // signals, so they stay in the per-thread `lumen_core::request`
        // context and a script asks for one part at a time; the address
        // parts are reserved `request.*` signals instead. Outside a
        // server render nothing is installed and each reader gives back
        // an empty string.
        engine.register_fn(
            "request_header",
            |name: rhai::ImmutableString| -> rhai::ImmutableString {
                lumen_core::request::header(name.as_str()).into()
            },
        );
        engine.register_fn(
            "request_cookie",
            |name: rhai::ImmutableString| -> rhai::ImmutableString {
                lumen_core::request::cookie(name.as_str()).into()
            },
        );
        engine.register_fn("request_body", || -> rhai::ImmutableString {
            lumen_core::request::body().into()
        });

        // response_status(status) / response_header(name, value) /
        // redirect(location) - answer the request with something other
        // than a plain 200 document. Only a server render applies these;
        // elsewhere the command is drained and dropped.
        enqueue!("response_status", |status: i64| {
            ScriptCommand::SetResponseStatus {
                status: status.clamp(100, 599) as u16,
            }
        });
        enqueue!(
            "response_header",
            |name: rhai::ImmutableString, value: rhai::ImmutableString| {
                ScriptCommand::SetResponseHeader {
                    name: name.to_string(),
                    value: value.to_string(),
                }
            }
        );
        enqueue!("redirect", |location: rhai::ImmutableString| {
            ScriptCommand::Redirect {
                location: location.to_string(),
            }
        });

        // parse_json(s) - convert a JSON string to a Rhai Map/Array/scalar
        // for ergonomic field access from scripts.
        engine.register_fn(
            "parse_json",
            move |s: rhai::ImmutableString| -> rhai::Dynamic {
                match serde_json::from_str::<serde_json::Value>(s.as_str()) {
                    Ok(v) => json_to_dynamic(v),
                    Err(_) => rhai::Dynamic::UNIT,
                }
            },
        );

        // derive(name, deps, fn) - register a computed signal. `deps`
        // is either an array of `Signal` handles (`derive("sum", [a,b],
        // |a,b| a+b)`) or an array of strings (`derive("sum", ["a",
        // "b"], |a,b| a+b)`). The closure is called with the current
        // dep values whenever any dep changes; its return value is
        // stringified into the matching signal. Returns a `Signal`
        // handle so the result reads naturally:
        //   `let sum = derive("sum", [a, b], |a, b| a + b);`
        let derivations_for_derive = derivations.clone();
        let pending_for_derive = pending_initial.clone();
        let signals_for_derive = signals_local.clone();
        let sink_for_derive = sink.clone();
        engine.register_fn(
            "derive",
            move |name: rhai::ImmutableString, deps: rhai::Array, fn_ptr: rhai::FnPtr| -> Signal {
                let dep_names: Vec<String> = deps
                    .into_iter()
                    .filter_map(|d| {
                        if let Some(sig) = d.clone().try_cast::<Signal>() {
                            Some(sig.name.to_string())
                        } else if d.is_string() {
                            d.into_immutable_string().ok().map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                derivations_for_derive
                    .lock()
                    .insert(name.to_string(), (dep_names, fn_ptr));
                // Mark this derivation as needing a first computation.
                // `apply_derivations` will run any name listed here on
                // the next tick regardless of dirty state, then clear
                // it. This solves the "deps never changed since
                // registration" problem.
                pending_for_derive.lock().insert(name.to_string());
                // Auto-initialise the signal entry to UNIT so reads
                // before the first derivation run return something
                // defined. The first apply_derivations tick will
                // replace it with the real value.
                {
                    let mut map = signals_for_derive.lock();
                    map.entry(name.to_string()).or_insert(rhai::Dynamic::UNIT);
                }
                Signal {
                    name,
                    host: signals_for_derive.clone(),
                    sink: sink_for_derive.clone(),
                }
            },
        );

        // on(event, id, fn_name) - register a per-id handler. Cuts
        // boilerplate `if id == "foo" { ... }` chains inside the default
        // on_<event>(id) handler. Per-id handlers SKIP the global
        // fallback for that (event, id) pair only - other ids still
        // route through on_<event>.
        let handlers_for_on = handlers.clone();
        engine.register_fn(
            "on",
            move |event: rhai::ImmutableString,
                  id: rhai::ImmutableString,
                  fn_name: rhai::ImmutableString| {
                if let Ok(mut h) = handlers_for_on.write() {
                    h.insert((event.to_string(), id.to_string()), fn_name.to_string());
                }
            },
        );

        // local_id(source, suffix) - return a sibling id in the same
        // template instance as `source`. If `source` is `user-card:btn`,
        // `local_id(source, "label")` is `user-card:label`. Source
        // without a `:` returns `suffix` unchanged. Multi-level prefixes
        // (`a:b:btn`) stack: result is `a:b:label`.
        engine.register_fn(
            "local_id",
            |source: rhai::ImmutableString, suffix: rhai::ImmutableString| -> String {
                if let Some(colon) = source.rfind(':') {
                    format!("{}:{}", &source[..colon], suffix.as_str())
                } else {
                    suffix.to_string()
                }
            },
        );

        // Translate markdown into a block list by walking pulldown-cmark events and folding inline text into the surrounding block.
        // Recognised block kinds: `h` (h1-h6), `p`, `code` (with optional lang), `li`, `hr`.
        // Inline emphasis, links, and code-span flatten to plain text inside the current block.
        engine.register_fn(
            "parse_markdown",
            move |src: rhai::ImmutableString| -> rhai::Array {
                use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
                #[derive(Clone, Copy)]
                enum BlockKind {
                    Heading(u8),
                    Paragraph,
                    CodeBlock,
                    Item,
                }
                let mut out: rhai::Array = Vec::new();
                let mut counter: usize = 0;
                let mut cur_kind: Option<BlockKind> = None;
                let mut cur_text = String::new();
                let mut cur_lang = String::new();

                fn push_block(
                    out: &mut rhai::Array,
                    counter: &mut usize,
                    kind: BlockKind,
                    text: String,
                    lang: String,
                ) {
                    let mut m = rhai::Map::new();
                    m.insert("id".into(), rhai::Dynamic::from(format!("blk-{counter}")));
                    *counter += 1;
                    match kind {
                        BlockKind::Heading(level) => {
                            m.insert("kind".into(), rhai::Dynamic::from("h".to_string()));
                            m.insert("level".into(), rhai::Dynamic::from(level as i64));
                            m.insert("text".into(), rhai::Dynamic::from(text));
                            m.insert("lang".into(), rhai::Dynamic::from(String::new()));
                        }
                        BlockKind::Paragraph => {
                            m.insert("kind".into(), rhai::Dynamic::from("p".to_string()));
                            m.insert("level".into(), rhai::Dynamic::from(0_i64));
                            m.insert("text".into(), rhai::Dynamic::from(text));
                            m.insert("lang".into(), rhai::Dynamic::from(String::new()));
                        }
                        BlockKind::CodeBlock => {
                            m.insert("kind".into(), rhai::Dynamic::from("code".to_string()));
                            m.insert("level".into(), rhai::Dynamic::from(0_i64));
                            m.insert("text".into(), rhai::Dynamic::from(text));
                            m.insert("lang".into(), rhai::Dynamic::from(lang));
                        }
                        BlockKind::Item => {
                            m.insert("kind".into(), rhai::Dynamic::from("li".to_string()));
                            m.insert("level".into(), rhai::Dynamic::from(0_i64));
                            m.insert("text".into(), rhai::Dynamic::from(text));
                            m.insert("lang".into(), rhai::Dynamic::from(String::new()));
                        }
                    }
                    out.push(rhai::Dynamic::from(m));
                }

                for ev in Parser::new(src.as_str()) {
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
                        // Lumen labels render plain text only - no
                        // rich-text runs in one entity. Preserve the
                        // markdown delimiters around inline emphasis
                        // so the rendered preview at least shows the
                        // author's intent until a span renderer
                        // ships.
                        Event::Start(Tag::Emphasis) => cur_text.push('*'),
                        Event::End(TagEnd::Emphasis) => cur_text.push('*'),
                        Event::Start(Tag::Strong) => cur_text.push_str("**"),
                        Event::End(TagEnd::Strong) => cur_text.push_str("**"),
                        Event::Start(Tag::Strikethrough) => cur_text.push('~'),
                        Event::End(TagEnd::Strikethrough) => cur_text.push('~'),
                        Event::End(TagEnd::Heading(_))
                        | Event::End(TagEnd::Paragraph)
                        | Event::End(TagEnd::CodeBlock)
                        | Event::End(TagEnd::Item) => {
                            if let Some(kind) = cur_kind.take() {
                                push_block(
                                    &mut out,
                                    &mut counter,
                                    kind,
                                    std::mem::take(&mut cur_text),
                                    std::mem::take(&mut cur_lang),
                                );
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
                            let mut m = rhai::Map::new();
                            m.insert("id".into(), rhai::Dynamic::from(format!("blk-{counter}")));
                            counter += 1;
                            m.insert("kind".into(), rhai::Dynamic::from("hr".to_string()));
                            m.insert("level".into(), rhai::Dynamic::from(0_i64));
                            m.insert("text".into(), rhai::Dynamic::from(String::new()));
                            m.insert("lang".into(), rhai::Dynamic::from(String::new()));
                            out.push(rhai::Dynamic::from(m));
                        }
                        _ => {}
                    }
                }
                out
            },
        );

        // Translation. `t("key")` returns the string the app's active
        // locale carries for `key`, or `key` itself when no catalogue
        // does - an untranslated app still renders something readable.
        // The catalogue lives behind the process-wide
        // `lumen_core::i18n` hook the runtime installs, so this host
        // links no Fluent/ICU code and needs no world access.
        engine.register_fn("t", |key: rhai::ImmutableString| -> rhai::ImmutableString {
            lumen_core::i18n::translate(key.as_str()).into()
        });
        // Qt's spelling of the same call.
        engine.register_fn(
            "tr",
            |key: rhai::ImmutableString| -> rhai::ImmutableString {
                lumen_core::i18n::translate(key.as_str()).into()
            },
        );

        // D1.2: tiny file I/O for apps that load/save user files
        // (markdown editor, future image viewer, etc.). Empty string
        // on read error so scripts can branch on `len() > 0`.
        engine.register_fn("read_file", move |path: rhai::ImmutableString| -> rhai::ImmutableString {
            match std::fs::read_to_string(path.as_str()) {
                Ok(s) => s.into(),
                Err(e) => {
                    tracing::warn!(target: "lumen.script.rhai", path = %path.as_str(), error = %e, "read_file failed");
                    rhai::ImmutableString::from("")
                }
            }
        });

        engine.register_fn(
            "write_file",
            move |path: rhai::ImmutableString, contents: rhai::ImmutableString| -> bool {
                match std::fs::write(path.as_str(), contents.as_str()) {
                    Ok(_) => true,
                    Err(e) => {
                        tracing::warn!(target: "lumen.script.rhai", path = %path.as_str(), error = %e, "write_file failed");
                        false
                    }
                }
            },
        );

        // audio_play / _pause / _resume / _stop / _seek / _volume. Kept in
        // `crate::audio` so the audio surface barely touches this file.
        crate::audio::register(&mut engine, &sink);

        let mut scope = Scope::new();
        // Push the `signals` chained-access root into the persistent
        // scope as a constant - every script can reach the typed
        // property bus through `signals.foo.set(v)` without an explicit
        // `signal(name, default)` factory call. The Arc inside the root
        // is shared (clones are cheap); the path on the root is always
        // empty - derived `SignalRef`s receive a fresh path on each
        // chain step.
        scope.push_constant("signals", scope_signals_root);
        // The web-idiomatic global namespaces (section 4.8) live as scope
        // constants so `window.set_href(..)` / `document.query(..)` /
        // `history.back()` resolve without a factory call.
        scope.push_constant("window", Window);
        scope.push_constant("document", Document);
        scope.push_constant("history", History);

        Self {
            engine,
            ast: None,
            scope,
            sink,
            signals_local,
            handlers,
            derivations,
            pending_initial,
            event_closures,
            script_fns: ScriptFnStore::default(),
            modules: HashMap::new(),
        }
    }

    /// Mutable access to the inner Rhai `Engine` so embedders can
    /// register additional native functions (FFI hooks, OS bindings,
    /// app-specific math, etc.) before the script source is loaded.
    /// Lumen itself only registers UI/script primitives - any OS-level
    /// or third-party-crate integration is the responsibility of the
    /// embedding binary, kept out of the framework.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Compile `source` with the exact engine settings `lumenc run`
    /// loads with, WITHOUT evaluating the top level (no side effects -
    /// no file I/O, no notifications, no signal writes).
    ///
    /// `lumenc check` calls this so a script that would die at load -
    /// e.g. one exceeding the parser's expression-depth limit - fails
    /// the check instead of false-passing while the window renders with
    /// every handler dead. Returns the same structured
    /// [`ScriptError::Compile`] shape [`ScriptHost::load`] produces.
    pub fn compile_check(&self, source: &str) -> Result<(), ScriptError> {
        self.engine
            .compile(source)
            .map(|_| ())
            .map_err(|e| parse_compile_error(e, "<inline>"))
    }

    /// Look up a per-id handler installed via the `on(event, id, fn)`
    /// Rhai builtin. Returns the fn name to call instead of
    /// `on_<event>(id)` for this specific id, or `None` to fall back to
    /// the global handler.
    ///
    /// Templates auto-namespace inner ids as `<use-id>:<inner-id>` (e.g.
    /// `user-card:save`). A handler registered as `on("click", "save",
    /// fn)` still fires for `user-card:save` via the suffix fallback -
    /// useful when the handler is template-internal logic that should
    /// match any instance.
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

    /// Variadic event call: invoke a script-defined function with
    /// the supplied [`ScriptValue`] args, translating each into the
    /// backend's native `rhai::Dynamic`. Returns the commands the
    /// builtins pushed into the sink during the call.
    ///
    /// Replaces the previous five `call_event_*` variants (no-args,
    /// `(id)`, `(id, bool)`, `(id, f64)`, `(id, text)`). Missing
    /// functions silently succeed (return an empty `Vec`); a function
    /// arity mismatch surfaces as [`ScriptError::Runtime`] from
    /// Rhai's own error.
    ///
    /// Detection of "function not found" goes through
    /// `EvalAltResult::ErrorFunctionNotFound` instead of the brittle
    /// `to_string().contains("Function not found")` fallback the old
    /// code used.
    pub fn call_event_values(
        &mut self,
        fn_name: &str,
        args: &[ScriptValue],
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        let dyn_args: Vec<rhai::Dynamic> = args.iter().map(script_value_to_dynamic).collect();
        self.call_event_dyn(fn_name, dyn_args)
    }

    /// Internal entry point: same as [`Self::call_event_values`] but
    /// takes already-converted `rhai::Dynamic`s. Lets dispatchers
    /// avoid an extra round-trip through [`ScriptValue`].
    pub fn call_event_dyn(
        &mut self,
        fn_name: &str,
        args: Vec<rhai::Dynamic>,
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_dyn_with_result(fn_name, args)
            .map(|(cmds, _)| cmds)
    }

    /// Same as [`Self::call_event_dyn`], but also returns the script
    /// function's return value. `None` when no function with that name
    /// exists (or no AST is loaded) - callers use it for hooks whose
    /// return value carries meaning, e.g. `on_close()` returning `false`
    /// to veto a window close.
    pub fn call_event_dyn_with_result(
        &mut self,
        fn_name: &str,
        args: Vec<rhai::Dynamic>,
    ) -> Result<(Vec<ScriptCommand>, Option<rhai::Dynamic>), ScriptError> {
        let Some(ast) = self.ast.as_ref() else {
            return Ok((Vec::new(), None));
        };
        let opts = CallFnOptions::new().rewind_scope(false).eval_ast(false);
        let result: Result<rhai::Dynamic, _> =
            self.engine
                .call_fn_with_options(opts, &mut self.scope, ast, fn_name, args);
        let ret = match result {
            Ok(v) => Some(v),
            Err(e) if is_function_not_found(&e) => None,
            Err(e) => {
                // A handler that queued commands (audio_play, set_text,
                // set_signal, fetch, ...) and *then* errored must contribute
                // NO commands: draining only on the success path would leak
                // them into the sink, where the next unrelated event's
                // outcome would apply them. Discard the partial batch.
                self.sink.lock().clear();
                return Err(ScriptError::Runtime(e.to_string()));
            }
        };
        Ok((std::mem::take(&mut *self.sink.lock()), ret))
    }

    /// Backward-compatible wrappers for callers that haven't migrated
    /// to the variadic API yet. Each is a one-liner over
    /// [`Self::call_event_dyn`].
    pub fn call_event_no_args(&mut self, fn_name: &str) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_dyn(fn_name, Vec::new())
    }

    /// Call a script-defined function with `(id, bool)` args.
    /// Convenience wrapper kept for backward source compat; new code
    /// should call [`Self::call_event_values`] / [`Self::call_event_dyn`].
    pub fn call_event_id_bool(
        &mut self,
        fn_name: &str,
        id: &str,
        value: bool,
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_dyn(fn_name, vec![id.to_string().into(), value.into()])
    }

    /// Call a script-defined function with `(id, f64)` args.
    /// Convenience wrapper kept for backward source compat; new code
    /// should call [`Self::call_event_values`] / [`Self::call_event_dyn`].
    pub fn call_event_id_f64(
        &mut self,
        fn_name: &str,
        id: &str,
        value: f64,
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_dyn(fn_name, vec![id.to_string().into(), value.into()])
    }

    /// Call a script-defined function with two string args.
    /// Convenience wrapper kept for backward source compat; new code
    /// should call [`Self::call_event_values`] / [`Self::call_event_dyn`].
    pub fn call_event_two_args(
        &mut self,
        fn_name: &str,
        arg1: &str,
        arg2: &str,
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_dyn(
            fn_name,
            vec![arg1.to_string().into(), arg2.to_string().into()],
        )
    }

    /// Put commands back into the per-host sink so they flush on the
    /// next `tick`. Used after `call_event_no_args("on_start")` which
    /// returns the commands directly - we need them to flow through the
    /// usual `ScriptCommandEvent` dispatch instead of being applied here.
    pub fn push_commands_back(&mut self, cmds: Vec<ScriptCommand>) {
        self.sink.lock().extend(cmds);
    }

    /// Borrow a [`ScriptContext`] backed by the host's signal mirror +
    /// sink. Mirrors `QQmlEngine::rootContext()`. Lets external systems
    /// drive the script's reactive store without re-encoding through
    /// the `ScriptCommand` bus.
    pub fn root_context(&mut self) -> RhaiScriptContext<'_> {
        RhaiScriptContext { host: self }
    }

    /// Drop the compiled AST + scope + persistent state. Used only by
    /// callers that genuinely want a clean re-start (tests; future
    /// "restart" command). Hot reload should prefer
    /// [`Self::replace_ast`] which keeps the persistent VM state.
    pub fn reset(&mut self) {
        self.ast = None;
        self.scope = Scope::new();
        // Re-push the `signals` chained-access root: a fresh scope
        // loses every constant, so without this the chained
        // `signals.foo.set(v)` form would error with "variable not
        // found" on the next load.
        self.scope.push_constant(
            "signals",
            SignalRef {
                path: Vec::new(),
                host: self.signals_local.clone(),
            },
        );
        // Re-push the section-4.8 namespace constants (see `new`).
        self.scope.push_constant("window", Window);
        self.scope.push_constant("document", Document);
        self.scope.push_constant("history", History);
        self.sink.lock().clear();
        self.signals_local.lock().clear();
        if let Ok(mut h) = self.handlers.write() {
            h.clear();
        }
        self.derivations.lock().clear();
        self.pending_initial.lock().clear();
        if let Ok(mut c) = self.event_closures.write() {
            c.clear();
        }
        lumen_script::event::clear_host_bindings();
        // Engine registrations survive a reset, so a replay only has to cover
        // what a rebuilt engine would lose. It runs anyway: re-registering the
        // same name and parameter types overwrites the entry rather than
        // adding a second one, and a host whose reset grows to rebuild the
        // engine stays correct without a second fix.
        let stored = std::mem::take(&mut self.script_fns);
        for f in stored.iter() {
            let _ = self.register_script_fn(f);
        }
    }

    /// Bind `f` into the engine's global namespace, one registration per
    /// accepted argument count.
    ///
    /// Rhai resolves a call by name and argument types, so a declared
    /// parameter binds as its own Rust type and a call that passes the wrong
    /// one fails at the call site rather than inside the body. An
    /// optional trailing parameter is a separate registration at the shorter
    /// arity, which is how one `page(path)` also answers `page()`.
    fn bind_globally(&mut self, f: &ScriptFn) {
        for arity in f.sig.arity_range() {
            let arg_types: Vec<TypeId> = (0..arity)
                .map(|i| rhai_arg_type(f.sig.params.get(i).map_or(&ScriptTy::Any, |p| &p.ty)))
                .collect();
            let sink = self.sink.clone();
            let f = f.clone();
            self.engine
                .register_raw_fn::<Dynamic>(f.name.clone(), arg_types, move |_ctx, args| {
                    let vals: Vec<ScriptValue> =
                        args.iter().map(|d| dynamic_to_script_value(d)).collect();
                    invoke_into_sink(&f, &sink, &vals)
                });
        }
    }

    /// Bind `f` into the static module named by [`ScriptNs::Named`], so a
    /// script calls it as `ns::name(...)`.
    ///
    /// A module takes typed closures only, so every slot here is `Dynamic` and
    /// the declared types are checked against the arguments inside the
    /// adapter; the script sees a runtime error naming the parameter instead
    /// of an unresolved call.
    fn bind_in_module(&mut self, ns: String, f: &ScriptFn) {
        let host_sink = self.sink.clone();
        let module = self.modules.entry(ns.clone()).or_default();
        macro_rules! bind_arity {
            ($($arg:ident),*) => {{
                let sink = host_sink.clone();
                let f = f.clone();
                module.set_native_fn(
                    f.name.clone(),
                    move |$($arg: Dynamic),*| {
                        let vals: Vec<ScriptValue> = vec![$(dynamic_to_script_value(&$arg)),*];
                        if f.sig.is_typed()
                            && let Err(message) = f.sig.check_args(&vals)
                        {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                format!("{}: {message}", f.name).into(),
                                rhai::Position::NONE,
                            )));
                        }
                        invoke_into_sink(&f, &sink, &vals)
                    },
                );
            }};
        }
        for arity in f.sig.arity_range() {
            match arity {
                0 => bind_arity!(),
                1 => bind_arity!(a),
                2 => bind_arity!(a, b),
                3 => bind_arity!(a, b, c),
                4 => bind_arity!(a, b, c, d),
                5 => bind_arity!(a, b, c, d, e),
                6 => bind_arity!(a, b, c, d, e, g),
                7 => bind_arity!(a, b, c, d, e, g, h),
                8 => bind_arity!(a, b, c, d, e, g, h, i),
                n => tracing::warn!(
                    "rhai: `{ns}::{}` takes {n} arguments; a namespaced function binds up to \
                     {MAX_VARIADIC_ARITY}",
                    f.name
                ),
            }
        }
        let module = self.modules[&ns].clone();
        self.engine
            .register_static_module(&ns, rhai::Shared::new(module));
    }

    /// Compiles `source` to a fresh AST and runs its top-level body
    /// against the existing scope.
    ///
    /// **Atomicity**: handlers + derivations + pending are snapshotted
    /// before the clear-and-eval pass. If the compile or eval fails, the
    /// snapshots are restored so the live host retains the old
    /// registrations, rather than being left with an empty registry and a
    /// crash on the next event.
    ///
    /// Preserved across the call:
    /// - [`Engine`] (registered Rust types and builtin fns).
    /// - [`Scope`] (top-level `let` bindings).
    /// - `signals_local`; `signal(name, default)` uses `or_insert_with(default)`.
    /// - In-flight `sink` (drained on the next tick).
    /// - Registrations the new body does not make again. The re-run only
    ///   repeats what the top level does; a handler an app binds from
    ///   `on_start` never gets a second registration pass, so the snapshot is
    ///   merged back underneath (see [`lumen_script::carry_forward`]).
    ///
    /// Rebuilt:
    /// - AST bytecode.
    /// - Every `handlers` / `derivations` / `pending_initial` entry the new
    ///   body registers; those win over the merged-back snapshot.
    pub fn replace_ast(&mut self, source: &str) -> Result<(), ScriptError> {
        self.replace_with_uri(source, "<inline>")
    }

    /// [`Self::replace_ast`] with an explicit source URI for compile
    /// errors. Backs the [`ScriptHost::replace`] trait method.
    fn replace_with_uri(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        // Compile FIRST - if the new source has a parse error we don't
        // touch the live state at all. Parse errors are surfaced
        // structurally via `parse_compile_error` so the LSP / banner
        // layer gets `(line, col)` without a regex.
        let ast = self
            .engine
            .compile(source)
            .map_err(|e| parse_compile_error(e, uri))?;
        // Snapshot the current live state so we can roll back if the
        // eval pass fails. Cheap clones - these maps are typically
        // small (one entry per registered handler / derivation).
        let prior_handlers: std::collections::HashMap<(String, String), String> =
            self.handlers.read().map(|h| h.clone()).unwrap_or_default();
        let prior_derivations: std::collections::HashMap<String, (Vec<String>, rhai::FnPtr)> =
            self.derivations.lock().clone();
        let prior_pending: std::collections::HashSet<String> = self.pending_initial.lock().clone();
        let prior_event_closures: std::collections::HashMap<u64, rhai::FnPtr> = self
            .event_closures
            .read()
            .map(|c| c.clone())
            .unwrap_or_default();
        let prior_ast = self.ast.take();
        // Clear the live registries so the new body's `on(...)` /
        // `derive(...)` calls populate fresh entries.
        if let Ok(mut h) = self.handlers.write() {
            h.clear();
        }
        self.derivations.lock().clear();
        self.pending_initial.lock().clear();
        if let Ok(mut c) = self.event_closures.write() {
            c.clear();
        }
        let prior_bindings = lumen_script::event::take_host_bindings();
        // Eval the new AST top-level into the existing scope.
        // `eval_ast_with_scope` re-runs `let` declarations - that resets
        // raw `let foo = ...` to their literal defaults, which is the
        // intended hot-reload semantics. Persistent state lives in
        // `signals_local`, not in raw scope vars.
        match self
            .engine
            .eval_ast_with_scope::<rhai::Dynamic>(&mut self.scope, &ast)
        {
            Ok(_) => {
                self.ast = Some(ast);
                // Merge the snapshot back under what the new body registered.
                // The top-level re-run covers only top-level registrations;
                // anything an app binds from `on_start` would otherwise vanish,
                // leaving its clicks as silent no-ops until restart.
                if let Ok(mut h) = self.handlers.write() {
                    lumen_script::carry_forward(&mut h, prior_handlers);
                }
                lumen_script::carry_forward(&mut self.derivations.lock(), prior_derivations);
                self.pending_initial.lock().extend(prior_pending);
                let dropped = lumen_script::event::restore_host_bindings(prior_bindings);
                if let Ok(mut c) = self.event_closures.write() {
                    lumen_script::carry_forward(&mut c, prior_event_closures);
                    for token in dropped {
                        c.remove(&token);
                    }
                }
                Ok(())
            }
            Err(e) => {
                // Rollback: restore handlers / derivations / pending /
                // ast so the live host stays usable on the old source.
                if let Ok(mut h) = self.handlers.write() {
                    *h = prior_handlers;
                }
                *self.derivations.lock() = prior_derivations;
                *self.pending_initial.lock() = prior_pending;
                if let Ok(mut c) = self.event_closures.write() {
                    *c = prior_event_closures;
                }
                lumen_script::event::clear_host_bindings();
                lumen_script::event::restore_host_bindings(prior_bindings);
                self.ast = prior_ast;
                Err(ScriptError::Runtime(e.to_string()))
            }
        }
    }

    /// Snapshot only the derivations that need to run this tick: those
    /// pending their initial evaluation, or whose deps intersect the
    /// dirty set. Filtering under the registry lock avoids cloning every
    /// derivation (name + deps + `FnPtr`) on quiescent ticks - the common
    /// case once the app has settled.
    fn derivations_snapshot_matching(
        &self,
        dirty: &std::collections::HashSet<&str>,
        pending: &std::collections::HashSet<String>,
    ) -> Vec<(String, Vec<String>, rhai::FnPtr)> {
        self.derivations
            .lock()
            .iter()
            .filter(|(n, (d, _))| {
                pending.contains(n.as_str()) || d.iter().any(|dep| dirty.contains(dep.as_str()))
            })
            .map(|(n, (d, f))| (n.clone(), d.clone(), f.clone()))
            .collect()
    }

    /// Read the current Rhai-side value of a named signal. Falls back
    /// to UNIT when missing - matches `Signal::get` behaviour.
    fn signal_value(&self, name: &str) -> rhai::Dynamic {
        self.signals_local
            .lock()
            .get(name)
            .cloned()
            .unwrap_or(rhai::Dynamic::UNIT)
    }

    /// Call a Rhai `FnPtr` with positional `Dynamic` args, returning
    /// the result as a `Dynamic`. Splits the borrows on `&self` so the
    /// engine + AST hand off cleanly. Used by [`apply_derivations`] to
    /// invoke the closure registered via `derive(...)`.
    fn call_fn_ptr(
        &self,
        fn_ptr: &rhai::FnPtr,
        args: Vec<rhai::Dynamic>,
    ) -> Result<rhai::Dynamic, ScriptError> {
        let Some(ast) = self.ast.as_ref() else {
            return Err(ScriptError::Runtime("no AST loaded".into()));
        };
        fn_ptr
            .call::<rhai::Dynamic>(&self.engine, ast, args)
            .map_err(|e| ScriptError::Runtime(e.to_string()))
    }
}

impl RhaiHost {
    /// Parse + load `source`, replacing any previously loaded program.
    /// Concrete-caller convenience over [`ScriptHost::load`] with the
    /// historical `"<inline>"` URI.
    pub fn load(&mut self, source: &str) -> Result<(), ScriptError> {
        self.load_with_uri(source, "<inline>")
    }

    /// [`Self::load`] with an explicit source URI for compile errors.
    /// Backs the [`ScriptHost::load`] trait method.
    fn load_with_uri(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        let ast = self
            .engine
            .compile(source)
            .map_err(|e| parse_compile_error(e, uri))?;
        // Evaluating the AST against the host's scope binds top-level
        // `let` declarations into that scope. Subsequent `call_fn` calls
        // with `rewind_scope: false` can then read + mutate those
        // bindings, giving the script a place to keep state between ticks.
        let _ = self
            .engine
            .eval_ast_with_scope::<rhai::Dynamic>(&mut self.scope, &ast)
            .map_err(|e| ScriptError::Runtime(e.to_string()))?;
        self.ast = Some(ast);
        Ok(())
    }

    /// Variadic event call returning only the drained commands.
    /// Concrete-caller convenience; the trait-level entry is
    /// [`ScriptHost::call`], which also surfaces the return value and
    /// the found flag.
    pub fn call_event(
        &mut self,
        fn_name: &str,
        args: &[ScriptValue],
    ) -> Result<Vec<ScriptCommand>, ScriptError> {
        self.call_event_values(fn_name, args)
    }
}

impl ScriptHost for RhaiHost {
    type Closure = rhai::FnPtr;

    fn compile_check(&self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.engine
            .compile(source)
            .map(|_| ())
            .map_err(|e| parse_compile_error(e, uri))
    }

    fn load(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.load_with_uri(source, uri)
    }

    fn replace(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        self.replace_with_uri(source, uri)
    }

    fn reset(&mut self) {
        // Inherent impls take precedence in resolution - this dispatches
        // to `RhaiHost::reset` (the full state drop), not back here.
        RhaiHost::reset(self);
    }

    fn call(&mut self, fn_name: &str, args: &[ScriptValue]) -> Result<CallOutcome, ScriptError> {
        let dyn_args: Vec<rhai::Dynamic> = args.iter().map(script_value_to_dynamic).collect();
        let (commands, ret) = self.call_event_dyn_with_result(fn_name, dyn_args)?;
        Ok(CallOutcome {
            commands,
            found: ret.is_some(),
            ret: ret.as_ref().map(dynamic_to_script_value),
        })
    }

    fn call_closure(
        &mut self,
        closure: &rhai::FnPtr,
        args: &[ScriptValue],
    ) -> Result<ScriptValue, ScriptError> {
        let dyn_args: Vec<rhai::Dynamic> = args.iter().map(script_value_to_dynamic).collect();
        self.call_fn_ptr(closure, dyn_args)
            .map(|d| dynamic_to_script_value(&d))
    }

    fn dispatch_event_handler(&mut self, token: u64) -> Result<bool, ScriptError> {
        let closure = self
            .event_closures
            .read()
            .ok()
            .and_then(|c| c.get(&token).cloned());
        let Some(closure) = closure else {
            return Ok(false);
        };
        // The handler receives the `Event` handle; its accessors read the
        // process-global current-event cell the dispatcher populated.
        self.call_fn_ptr(&closure, vec![rhai::Dynamic::from(Event)])
            .map(|_| true)
    }

    fn drop_event_handler(&mut self, token: u64) {
        if let Ok(mut c) = self.event_closures.write() {
            c.remove(&token);
        }
    }

    /// Native override of the default composition: dep values come off
    /// the mirror as rich `Dynamic`s (no lossy [`ScriptValue`]
    /// round-trip) and the result is stringified with Rhai's canonical
    /// `Display` (`1.0` stays `1.0`, not `1`). Snapshot-then-call: the
    /// mirror lock is released before the `FnPtr` runs so re-entrant
    /// builtins inside the closure can't deadlock.
    fn eval_derivation(
        &mut self,
        closure: &rhai::FnPtr,
        deps: &[String],
        name: &str,
    ) -> Result<String, ScriptError> {
        let args: Vec<rhai::Dynamic> = deps.iter().map(|d| self.signal_value(d)).collect();
        let value = self.call_fn_ptr(closure, args)?;
        let text = stringify_dynamic(&value);
        self.signals_local.lock().insert(name.to_string(), value);
        Ok(text)
    }

    fn drain_commands(&mut self) -> Vec<ScriptCommand> {
        std::mem::take(&mut *self.sink.lock())
    }

    fn push_commands(&mut self, cmds: Vec<ScriptCommand>) {
        self.push_commands_back(cmds);
    }

    fn mirror_get(&self, name: &str) -> Option<ScriptValue> {
        self.signals_local
            .lock()
            .get(name)
            .map(dynamic_to_script_value)
    }

    fn mirror_set(&mut self, name: &str, value: ScriptValue) {
        self.signals_local
            .lock()
            .insert(name.to_string(), script_value_to_dynamic(&value));
    }

    fn mirror_sync_str(&mut self, name: &str, value: &str) {
        let mut local = self.signals_local.lock();
        // Overwrite policy by existing mirror type (section 1.3, pinned by the
        // trait):
        // - absent / string -> take the store string (compare first to
        //   skip no-op writes).
        // - scalar rich values (bool / int / float) -> parse the store
        //   string back into the SAME type. A two-way binding push
        //   (toggle flip, slider drag) writes the store as a canonical
        //   string; the mirror must follow or `derive()` closures read
        //   the stale scalar forever (the widget garden's frozen
        //   `toggle_status`). Unparseable strings leave the scalar
        //   untouched.
        // - structured rich values (arrays, maps) stay authoritative - a
        //   stringified mirror of the same data would clobber them.
        let next: Option<rhai::Dynamic> = match local.get(name) {
            None => Some(rhai::Dynamic::from(rhai::ImmutableString::from(value))),
            // Compare against the stored `&str` without allocating.
            Some(d) if d.is_string() => d
                .clone()
                .into_immutable_string()
                .map(|s| s.as_str() != value)
                .unwrap_or(true)
                .then(|| rhai::Dynamic::from(rhai::ImmutableString::from(value))),
            Some(d) if d.is_bool() => match value {
                "true" | "1" => (d.as_bool() != Ok(true)).then(|| rhai::Dynamic::from(true)),
                "false" | "0" => (d.as_bool() != Ok(false)).then(|| rhai::Dynamic::from(false)),
                _ => None,
            },
            Some(d) if d.is_int() => value
                .parse::<i64>()
                .ok()
                .filter(|n| d.as_int() != Ok(*n))
                .map(rhai::Dynamic::from),
            Some(d) if d.is_float() => value
                .parse::<f64>()
                .ok()
                .filter(|n| d.as_float() != Ok(*n))
                .map(rhai::Dynamic::from),
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
        dirty: &std::collections::HashSet<&str>,
        pending: &std::collections::HashSet<String>,
    ) -> Vec<(String, Vec<String>, rhai::FnPtr)> {
        self.derivations_snapshot_matching(dirty, pending)
    }

    fn pending_initial(&self) -> std::collections::HashSet<String> {
        self.pending_initial.lock().iter().cloned().collect()
    }

    fn clear_pending(&mut self, evaluated: &[String]) {
        let mut pending = self.pending_initial.lock();
        for name in evaluated {
            pending.remove(name);
        }
    }

    fn register_script_fn(&mut self, f: &ScriptFn) -> Result<(), ScriptError> {
        match &f.ns {
            ScriptNs::Named(ns) => self.bind_in_module(ns.clone(), f),
            // Rhai has no global namespace beyond the engine itself, so the
            // runtime's own surface and an embedder's share it. Registration
            // order decides a collision: the later one shadows the earlier.
            ScriptNs::Builtin | ScriptNs::Extension => self.bind_globally(f),
        }
        self.script_fns.record(f);
        Ok(())
    }

    fn lang(&self) -> &'static str {
        "rhai"
    }

    fn builtins(&self) -> &'static [lumen_script::BuiltinFn] {
        builtins::BUILTINS
    }
}

/// The Rhai type a declared [`ScriptTy`] resolves a call by.
///
/// [`ScriptTy::Any`] takes a `Dynamic` slot, which matches whatever the script
/// passes; every other type narrows the slot, so a call passing something else
/// does not resolve.
fn rhai_arg_type(ty: &ScriptTy) -> TypeId {
    match ty {
        ScriptTy::Any => TypeId::of::<Dynamic>(),
        ScriptTy::Unit => TypeId::of::<()>(),
        ScriptTy::Bool => TypeId::of::<bool>(),
        ScriptTy::Int => TypeId::of::<rhai::INT>(),
        ScriptTy::Float => TypeId::of::<rhai::FLOAT>(),
        ScriptTy::Str => TypeId::of::<rhai::ImmutableString>(),
        ScriptTy::Array(_) => TypeId::of::<rhai::Array>(),
        ScriptTy::Map(_) => TypeId::of::<rhai::Map>(),
    }
}

/// Run a [`ScriptFn`] body and hand its result back to Rhai.
///
/// The body emits into a scratch buffer and the sink lock is taken once, after
/// it returns: a body that calls back into a builtin would otherwise meet a
/// lock its own call is holding.
fn invoke_into_sink(
    f: &ScriptFn,
    sink: &Arc<Mutex<Vec<ScriptCommand>>>,
    args: &[ScriptValue],
) -> Result<Dynamic, Box<EvalAltResult>> {
    let (ret, commands) = f.invoke(args);
    if !commands.is_empty() {
        sink.lock().extend(commands);
    }
    Ok(script_value_to_dynamic(&ret))
}

/// Translate a Rhai `ParseError` into the structured
/// [`ScriptError::Compile`] shape so downstream layers (LSP, error
/// banner) get `(line, col)` without scraping the message.
fn parse_compile_error(e: rhai::ParseError, uri: &str) -> ScriptError {
    let pos = e.1;
    let line = pos.line().map(|l| l as u32).unwrap_or(0);
    let col = pos.position().map(|c| c as u32).unwrap_or(0);
    ScriptError::Compile {
        uri: uri.to_string(),
        line,
        col,
        message: e.0.to_string(),
    }
}

/// Robust "function not found" detection - replaces the previous
/// `e.to_string().contains("Function not found")` substring check
/// (brittle across Rhai versions and locales).
fn is_function_not_found(e: &EvalAltResult) -> bool {
    matches!(e, EvalAltResult::ErrorFunctionNotFound(_, _))
}

/// Translate a [`ScriptValue`] into a Rhai `Dynamic`. Used by the
/// variadic `call_event_values` entry point.
fn script_value_to_dynamic(v: &ScriptValue) -> rhai::Dynamic {
    match v {
        ScriptValue::Unit => rhai::Dynamic::UNIT,
        ScriptValue::Bool(b) => (*b).into(),
        ScriptValue::I64(i) => (*i).into(),
        ScriptValue::F64(f) => (*f).into(),
        ScriptValue::Str(s) => s.clone().into(),
        ScriptValue::Array(arr) => {
            let rhai_arr: rhai::Array = arr.iter().map(script_value_to_dynamic).collect();
            rhai_arr.into()
        }
        ScriptValue::Map(m) => {
            let mut rhai_map = rhai::Map::new();
            for (k, val) in m {
                rhai_map.insert(k.clone().into(), script_value_to_dynamic(val));
            }
            rhai_map.into()
        }
    }
}

/// Translate a Rhai `Dynamic` into a [`ScriptValue`]. Used by
/// [`RhaiScriptContext::get`] so callers across the host trait
/// boundary work in the backend-neutral type.
fn dynamic_to_script_value(d: &rhai::Dynamic) -> ScriptValue {
    if d.is_unit() {
        return ScriptValue::Unit;
    }
    if d.is_bool() {
        return ScriptValue::Bool(d.as_bool().unwrap_or(false));
    }
    if d.is_int() {
        return ScriptValue::I64(d.as_int().unwrap_or(0));
    }
    if d.is_float() {
        return ScriptValue::F64(d.as_float().unwrap_or(0.0));
    }
    if d.is_string() {
        return ScriptValue::Str(
            d.clone()
                .into_immutable_string()
                .map(|s| s.to_string())
                .unwrap_or_default(),
        );
    }
    if d.is_array() {
        let arr = d.clone().try_cast::<rhai::Array>().unwrap_or_default();
        return ScriptValue::Array(arr.iter().map(dynamic_to_script_value).collect());
    }
    if d.is_map() {
        let map = d.clone().try_cast::<rhai::Map>().unwrap_or_default();
        let mut out = std::collections::HashMap::with_capacity(map.len());
        for (k, v) in map {
            out.insert(k.to_string(), dynamic_to_script_value(&v));
        }
        return ScriptValue::Map(out);
    }
    // Fallthrough: stringify whatever Rhai gave us.
    ScriptValue::Str(d.to_string())
}

/// [`ScriptContext`] borrowing the live [`RhaiHost`] state. Mirrors
/// `QQmlContext` - reads + writes flow through the same `signals_local`
/// mirror the script side sees so context-driven mutations are
/// observable from inside the script without a tick boundary.
pub struct RhaiScriptContext<'a> {
    host: &'a mut RhaiHost,
}

impl<'a> ScriptContext for RhaiScriptContext<'a> {
    fn get(&self, name: &str) -> Option<ScriptValue> {
        let map = self.host.signals_local.lock();
        map.get(name).map(dynamic_to_script_value)
    }

    fn set(&mut self, name: &str, value: ScriptValue) {
        let text = value.stringify();
        let dyn_value = script_value_to_dynamic(&value);
        self.host
            .signals_local
            .lock()
            .insert(name.to_string(), dyn_value);
        self.host.sink.lock().push(ScriptCommand::SetSignal {
            name: name.to_string(),
            value: text,
        });
    }

    fn array_push(&mut self, name: &str, value: ScriptValue) {
        // Read the current array (cheap snapshot via try_cast), push,
        // and write the new array back. Sidesteps Rhai's internals-only
        // `DynamicWriteLock` so this compiles against the stable Rhai
        // surface; the cost is one extra clone of the array per push
        // (acceptable for the rarely-used `ScriptContext::array_push`
        // path - script-side `signal_array(name).push(item)` still
        // uses the more direct in-place borrow).
        let mut map = self.host.signals_local.lock();
        let mut current: rhai::Array = map
            .get(name)
            .and_then(|d| d.clone().try_cast::<rhai::Array>())
            .unwrap_or_default();
        current.push(script_value_to_dynamic(&value));
        map.insert(name.to_string(), current.clone().into());
        drop(map);
        let items: Vec<std::collections::HashMap<String, String>> = current
            .iter()
            .filter_map(|elt| elt.clone().try_cast::<rhai::Map>())
            .map(|m| {
                let mut row = std::collections::HashMap::with_capacity(m.len());
                for (k, v) in m {
                    row.insert(k.to_string(), v.to_string());
                }
                row
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
            .insert(name.to_string(), rhai::Array::new().into());
        self.host.sink.lock().push(ScriptCommand::SetArray {
            name: name.to_string(),
            items: Vec::new(),
        });
    }
}

/// A single `rhai::Engine` extension callback; factored into an alias to
/// keep clippy's `type_complexity` lint quiet.
type EngineExtension = Box<dyn FnOnce(&mut Engine) + Send + 'static>;

/// Plugin: build a [`RhaiHost`], apply embedder engine extensions, and
/// delegate to the host-generic
/// [`ScriptPlugin`](lumen_script::ScriptPlugin) - which loads
/// the source (stderr banner + [`ScriptLoadFailure`] on failure), fires
/// `on_start`, installs the host resource, and registers the full
/// dispatcher / derivation / timer / fetch system set.
pub struct ScriptRhaiPlugin {
    /// Inline Rhai source loaded on app start. Use a string literal or
    /// `include_str!("path.rhai")`.
    pub source: String,
    /// Extension callbacks invoked on the inner `rhai::Engine` after
    /// Lumen's built-in registrations but before the script AST is
    /// compiled. Use this to register app-specific native bindings
    /// (FFI, OS APIs, third-party Rust crates). Lumen itself only
    /// ships UI/script primitives - anything OS-level lives in the
    /// embedding binary.
    pub extensions: Vec<EngineExtension>,
}

impl ScriptRhaiPlugin {
    /// Wrap a static source string.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            extensions: Vec::new(),
        }
    }

    /// Register a callback that runs on the inner `rhai::Engine` before
    /// script compile. Lets the embedding binary expose extra native
    /// functions to Rhai without forking the framework crate.
    pub fn with_extension<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut Engine) + Send + 'static,
    {
        self.extensions.push(Box::new(f));
        self
    }
}

impl Plugin for ScriptRhaiPlugin {
    fn build(self, app: &mut App) {
        let mut host = RhaiHost::new();
        for ext in self.extensions {
            ext(host.engine_mut());
        }
        // Everything else - load + banner + `on_start` re-stash +
        // resource install + the full system set with its 7bfc0f2
        // ordering - is host-generic and lives in lumen-script.
        ScriptPlugin::new(host, self.source).build(app);
    }
}

/// Recursively translate a [`serde_json::Value`] into a Rhai
/// [`rhai::Dynamic`]. Numbers become i64 or f64, objects become
/// [`rhai::Map`], arrays become [`rhai::Array`]. Used by the
/// `parse_json` builtin so scripts can read JSON responses with
/// normal `.field` / `["key"]` syntax.
pub fn json_to_dynamic(v: serde_json::Value) -> rhai::Dynamic {
    match v {
        serde_json::Value::Null => rhai::Dynamic::UNIT,
        serde_json::Value::Bool(b) => b.into(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(f) = n.as_f64() {
                f.into()
            } else {
                rhai::Dynamic::UNIT
            }
        }
        serde_json::Value::String(s) => s.into(),
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(json_to_dynamic)
            .collect::<rhai::Array>()
            .into(),
        serde_json::Value::Object(obj) => {
            let mut map = rhai::Map::new();
            for (k, val) in obj {
                map.insert(k.into(), json_to_dynamic(val));
            }
            map.into()
        }
    }
}

/// Parse a `pick_file_filtered` spec like `"Images:png,jpg|All:*"`
/// into rfd's `(label, [exts])` list. A literal `*` extension is
/// stripped (rfd treats no-extension filter as "all files").
pub fn parse_dialog_filter_spec(spec: &str) -> Vec<(String, Vec<String>)> {
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

#[cfg(test)]
mod error_drain_tests {
    use super::*;

    /// A handler that queues a command and *then* errors must contribute
    /// NO commands: the failed batch is discarded so it cannot leak into an
    /// unrelated later event's outcome. Guards the error-path sink drain in
    /// `call_event_dyn_with_result`.
    #[test]
    fn erroring_handler_leaks_no_commands() {
        let mut host = RhaiHost::new();
        host.load(
            r#"
            fn on_ok() { set_text("lbl", "kept"); }
            fn on_boom() {
                set_text("lbl", "leaked");
                throw "deliberate failure";
            }
            "#,
        )
        .expect("compile inline script");

        // Positive control: a successful handler drains its one command and
        // leaves the sink empty.
        let cmds = host.call_event_no_args("on_ok").expect("ok handler runs");
        assert_eq!(cmds.len(), 1, "successful handler yields its command");
        assert!(host.sink.lock().is_empty(), "sink drained after success");

        // The erroring handler queued a SetText *before* throwing.
        let res = host.call_event_no_args("on_boom");
        assert!(res.is_err(), "handler error surfaces as Err, not Ok");

        // That queued command must not survive in the sink.
        assert!(
            host.sink.lock().is_empty(),
            "failed handler contributes no commands"
        );

        // End-to-end: the next unrelated event sees only its own command,
        // proving nothing leaked across the boundary.
        let next = host.call_event_no_args("on_ok").expect("ok handler runs");
        assert_eq!(next.len(), 1, "next event only sees its own command");
    }
}
