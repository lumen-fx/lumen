//! The DOM and event surface a language reaches through plain functions.
//!
//! Rhai and Lua give a node a receiver type, so a script writes
//! `n.set_text("hi")` and the handle stays inside the engine's own value.
//! candela's value type is a small integer and its pinned version has no
//! user-struct methods, so every call there is a free function over an
//! interned `int` id: `lumen::node_set_text(n, "hi")`. Those free functions are
//! what this module describes; the id interning is
//! [`lumen_core::node`]'s, the bodies are the same [`node_query`],
//! [`introspect`] and [`event`] calls the receiver methods make.
//!
//! Every entry carries [`HostSet::CANDELA`]. When candela gains user-struct
//! methods, the sugar goes in its prelude and these stay as the surface it
//! calls.

use std::collections::HashMap;

use crate::{
    HostSet, ScriptCommand, ScriptFn, ScriptFnCx, ScriptNs, ScriptTy as T, ScriptValue, event,
    introspect as ins, node_query,
};

/// Resolve a script-side id to the packed handle of a live node.
fn packed(id: i64) -> Option<u64> {
    i32::try_from(id)
        .ok()
        .and_then(lumen_core::node::resolve_node)
        .map(|h| h.pack())
}

/// Intern a packed handle back into a script-side id (`0` for none).
fn id_of(packed: u64) -> i64 {
    match lumen_core::node::NodeHandle::unpack(packed) {
        Some(h) => lumen_core::node::intern_node(h.entity, h.generation) as i64,
        None => 0,
    }
}

/// Resolve a script-side id to its raw packed bits: a live handle, or a
/// reserved token a spawn minted earlier this tick.
fn raw(id: i64) -> Option<u64> {
    i32::try_from(id)
        .ok()
        .and_then(lumen_core::node::resolve_node_raw)
}

/// Intern any packed handle, live or reserved, into a script-side id.
fn intern_raw(packed: u64) -> i64 {
    lumen_core::node::intern_node_raw(packed) as i64
}

/// A node id, the parameter every call in this module starts with.
fn node_param() -> (&'static str, T) {
    ("node", T::Int)
}

/// Describe a free function candela reaches under the `lumen` namespace.
fn f<F>(name: &str, doc: &str, params: &[(&str, T)], ret: T, body: F) -> ScriptFn
where
    F: Fn(&mut ScriptFnCx<'_>) -> ScriptValue + Send + Sync + 'static,
{
    let mut b = ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .ret(ret)
        .doc(doc)
        .hosts(HostSet::CANDELA);
    for (pname, ty) in params {
        b = b.param(*pname, ty.clone());
    }
    b.build(move |cx| body(cx))
}

/// Describe a mutator: resolve the node id, queue one command, return nothing.
fn mutate<F>(name: &str, doc: &str, params: &[(&str, T)], build: F) -> ScriptFn
where
    F: Fn(u64, &ScriptFnCx<'_>) -> ScriptCommand + Send + Sync + 'static,
{
    let mut all = vec![node_param()];
    all.extend(params.iter().cloned());
    let mut b = ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .ret(T::Unit)
        .doc(doc)
        .hosts(HostSet::CANDELA);
    for (pname, ty) in &all {
        b = b.param(*pname, ty.clone());
    }
    b.build(move |cx| {
        if let Some(node) = raw(cx.int_arg(0)) {
            let cmd = build(node, cx);
            cx.emit(cmd);
        }
        ScriptValue::Unit
    })
}

/// A list of node ids as a script value.
fn ids(list: Vec<i64>) -> ScriptValue {
    ScriptValue::Array(list.into_iter().map(ScriptValue::I64).collect())
}

/// A list of strings as a script value.
fn strings(list: Vec<String>) -> ScriptValue {
    ScriptValue::Array(list.into_iter().map(ScriptValue::Str).collect())
}

/// A string-keyed map as a script value.
fn map<V: Into<ScriptValue>>(entries: impl IntoIterator<Item = (String, V)>) -> ScriptValue {
    ScriptValue::Map(entries.into_iter().map(|(k, v)| (k, v.into())).collect())
}

/// The rectangle shape every geometry read gives back.
fn rect_map(r: ins::NodeRect) -> ScriptValue {
    map([
        ("x".to_string(), f64::from(r.x)),
        ("y".to_string(), f64::from(r.y)),
        ("width".to_string(), f64::from(r.width)),
        ("height".to_string(), f64::from(r.height)),
        ("client_x".to_string(), f64::from(r.client_x)),
        ("client_y".to_string(), f64::from(r.client_y)),
    ])
}

/// The whole free-function DOM and event surface, in registration order.
pub(crate) fn node_script_fns() -> Vec<ScriptFn> {
    let mut fns = Vec::new();
    fns.extend(query_fns());
    fns.extend(mutator_fns());
    fns.extend(introspection_fns());
    fns.extend(event_fns());
    fns
}

/// Reading the tree: selectors, traversal, and liveness. Every read goes
/// through the snapshot published at the start of the tick.
fn query_fns() -> Vec<ScriptFn> {
    /// A traversal step: one node in, one node out, `0` for none.
    fn step(name: &str, doc: &str, go: fn(u64) -> Option<u64>) -> ScriptFn {
        f(name, doc, &[node_param()], T::Int, move |cx| {
            ScriptValue::I64(packed(cx.int_arg(0)).and_then(go).map(id_of).unwrap_or(0))
        })
    }

    vec![
        f(
            "node_query",
            "Every node matching that CSS selector.",
            &[("selector", T::Str)],
            T::Array(Box::new(T::Int)),
            |cx| {
                ids(node_query::run_query(&cx.str_arg(0))
                    .map(|q| q.nodes.iter().map(|&p| id_of(p)).collect())
                    .unwrap_or_default())
            },
        ),
        f(
            "node_get_by_id",
            "The node with that id, or 0.",
            &[("id", T::Str)],
            T::Int,
            |cx| {
                ScriptValue::I64(
                    node_query::run_get_by_id(&cx.str_arg(0))
                        .map(id_of)
                        .unwrap_or(0),
                )
            },
        ),
        f("node_document", "The document root.", &[], T::Int, |_| {
            ScriptValue::I64(node_query::run_document().map(id_of).unwrap_or(0))
        }),
        step(
            "node_parent",
            "The parent node, or 0.",
            node_query::node_parent,
        ),
        step(
            "node_first_child",
            "The first child, or 0.",
            node_query::node_first_child,
        ),
        step(
            "node_last_child",
            "The last child, or 0.",
            node_query::node_last_child,
        ),
        step(
            "node_next",
            "The next sibling, or 0.",
            node_query::node_next,
        ),
        step(
            "node_prev",
            "The previous sibling, or 0.",
            node_query::node_prev,
        ),
        f(
            "node_children",
            "The children of that node, in document order.",
            &[node_param()],
            T::Array(Box::new(T::Int)),
            |cx| {
                ids(packed(cx.int_arg(0))
                    .map(|p| {
                        node_query::node_children(p)
                            .iter()
                            .map(|&x| id_of(x))
                            .collect()
                    })
                    .unwrap_or_default())
            },
        ),
        f(
            "node_closest",
            "The nearest ancestor matching that selector, or 0.",
            &[node_param(), ("selector", T::Str)],
            T::Int,
            |cx| {
                ScriptValue::I64(
                    packed(cx.int_arg(0))
                        .and_then(|p| node_query::node_closest(p, &cx.str_arg(1)).ok().flatten())
                        .map(id_of)
                        .unwrap_or(0),
                )
            },
        ),
        f(
            "node_valid",
            "Whether that node is still in the tree.",
            &[node_param()],
            T::Bool,
            |cx| {
                ScriptValue::Bool(
                    packed(cx.int_arg(0))
                        .map(node_query::node_valid)
                        .unwrap_or(false),
                )
            },
        ),
    ]
}

/// Writing the tree. Each mutator queues a command the applier runs later in
/// the tick, so a handle a spawn minted is usable before the node exists.
fn mutator_fns() -> Vec<ScriptFn> {
    vec![
        mutate(
            "node_set_attr",
            "Set an attribute on that node.",
            &[("name", T::Str), ("value", T::Str)],
            |node, cx| ScriptCommand::SetAttr {
                node,
                name: cx.str_arg(1),
                value: cx.str_arg(2),
            },
        ),
        mutate(
            "node_remove_attr",
            "Remove an attribute from that node.",
            &[("name", T::Str)],
            |node, cx| ScriptCommand::RemoveAttr {
                node,
                name: cx.str_arg(1),
            },
        ),
        mutate(
            "node_set_id",
            "Set the id of that node.",
            &[("id", T::Str)],
            |node, cx| ScriptCommand::SetAttr {
                node,
                name: "id".to_string(),
                value: cx.str_arg(1),
            },
        ),
        mutate(
            "node_set_text",
            "Replace the text content of that node.",
            &[("text", T::Str)],
            |node, cx| ScriptCommand::SetNodeText {
                node,
                text: cx.str_arg(1),
            },
        ),
        // Guarded markup injection: do not feed it untrusted content.
        mutate(
            "node_set_inner_markup",
            "Replace the children of that node with parsed markup.",
            &[("markup", T::Str)],
            |node, cx| ScriptCommand::SetInnerMarkup {
                node,
                markup: cx.str_arg(1),
            },
        ),
        mutate(
            "node_class_add",
            "Add a class to that node.",
            &[("class", T::Str)],
            |node, cx| ScriptCommand::ClassAdd {
                node,
                class: cx.str_arg(1),
            },
        ),
        mutate(
            "node_class_remove",
            "Remove a class from that node.",
            &[("class", T::Str)],
            |node, cx| ScriptCommand::ClassRemove {
                node,
                class: cx.str_arg(1),
            },
        ),
        mutate(
            "node_class_toggle",
            "Toggle a class on that node.",
            &[("class", T::Str)],
            |node, cx| ScriptCommand::ClassToggle {
                node,
                class: cx.str_arg(1),
            },
        ),
        mutate(
            "node_set_class",
            "Replace the whole class list of that node.",
            &[("classes", T::Str)],
            |node, cx| ScriptCommand::SetAttr {
                node,
                name: "class".to_string(),
                value: cx.str_arg(1),
            },
        ),
        mutate(
            "node_set_style",
            "Set an inline style property on that node.",
            &[("name", T::Str), ("value", T::Str)],
            |node, cx| ScriptCommand::SetStyleProp {
                node,
                name: cx.str_arg(1),
                value: cx.str_arg(2),
            },
        ),
        mutate(
            "node_style_remove",
            "Remove an inline style property from that node.",
            &[("name", T::Str)],
            |node, cx| ScriptCommand::RemoveStyleProp {
                node,
                name: cx.str_arg(1),
            },
        ),
        mutate(
            "node_remove",
            "Remove that node from the tree.",
            &[],
            |node, _| ScriptCommand::RemoveNode { node },
        ),
        pair(
            "node_append",
            "Append the child to the parent.",
            &["parent", "child"],
            |parent, child| ScriptCommand::Insert {
                parent,
                node: child,
                before: 0,
            },
        ),
        f(
            "node_insert_before",
            "Insert the child into the parent, ahead of the reference node.",
            &[("parent", T::Int), ("child", T::Int), ("reference", T::Int)],
            T::Unit,
            |cx| {
                if let (Some(parent), Some(child)) = (raw(cx.int_arg(0)), raw(cx.int_arg(1))) {
                    cx.emit(ScriptCommand::Insert {
                        parent,
                        node: child,
                        before: raw(cx.int_arg(2)).unwrap_or(0),
                    });
                }
                ScriptValue::Unit
            },
        ),
        pair(
            "node_set_parent",
            "Move that node under a new parent.",
            &["node", "parent"],
            |node, parent| ScriptCommand::Insert {
                parent,
                node,
                before: 0,
            },
        ),
        pair(
            "node_move_to",
            "Move that node under a new parent.",
            &["node", "parent"],
            |node, parent| ScriptCommand::Insert {
                parent,
                node,
                before: 0,
            },
        ),
        pair(
            "node_replace_with",
            "Replace the first node with the second.",
            &["old", "new"],
            |old, new| ScriptCommand::ReplaceWith { old, new },
        ),
        f(
            "node_spawn",
            "Create a node with that tag; the id is usable at once.",
            &[("tag", T::Str)],
            T::Int,
            |cx| {
                let (handle, cmd) = node_query::build_spawn(&cx.str_arg(0));
                cx.emit(cmd);
                ScriptValue::I64(intern_raw(handle))
            },
        ),
        f(
            "node_clone_deep",
            "Copy that node and its subtree; the id is usable at once.",
            &[("source", T::Int)],
            T::Int,
            |cx| {
                let Some(source) = raw(cx.int_arg(0)) else {
                    return ScriptValue::I64(0);
                };
                let (handle, cmd) = node_query::build_clone(source);
                cx.emit(cmd);
                ScriptValue::I64(intern_raw(handle))
            },
        ),
        f(
            "node_get_attr",
            "The value of that attribute, or an empty string.",
            &[node_param(), ("name", T::Str)],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .and_then(|h| node_query::node_get_attr(h, &cx.str_arg(1)))
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_text",
            "The text content of that node.",
            &[node_param()],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .and_then(node_query::node_text)
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_id",
            "The id of that node, or an empty string.",
            &[node_param()],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .and_then(node_query::node_id)
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_class_contains",
            "Whether that node carries the class.",
            &[node_param(), ("class", T::Str)],
            T::Bool,
            |cx| {
                ScriptValue::Bool(
                    raw(cx.int_arg(0))
                        .map(|h| node_query::node_class_contains(h, &cx.str_arg(1)))
                        .unwrap_or(false),
                )
            },
        ),
        f(
            "node_style_get",
            "The inline style value for that property.",
            &[node_param(), ("prop", T::Str)],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .and_then(|h| node_query::node_style_get(h, &cx.str_arg(1)))
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_computed_style",
            "The computed value for that property.",
            &[node_param(), ("prop", T::Str)],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .and_then(|h| node_query::node_computed_style(h, &cx.str_arg(1)))
                        .unwrap_or_default(),
                )
            },
        ),
    ]
}

/// A structural op over two node ids: both must resolve or nothing is queued.
fn pair<F>(name: &str, doc: &str, names: &[&str; 2], build: F) -> ScriptFn
where
    F: Fn(u64, u64) -> ScriptCommand + Send + Sync + 'static,
{
    ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .param(names[0], T::Int)
        .param(names[1], T::Int)
        .ret(T::Unit)
        .doc(doc)
        .hosts(HostSet::CANDELA)
        .build(move |cx| {
            if let (Some(a), Some(b)) = (raw(cx.int_arg(0)), raw(cx.int_arg(1))) {
                let cmd = build(a, b);
                cx.emit(cmd);
            }
            ScriptValue::Unit
        })
}

/// Post-layout geometry, resolved style, and the component reads. An absent or
/// unknown read gives back an empty map: a candela host function surfaces no
/// error.
fn introspection_fns() -> Vec<ScriptFn> {
    let map_of_str = T::Map(Box::new(T::Str));
    let map_of_float = T::Map(Box::new(T::Float));
    vec![
        f(
            "node_rect",
            "The border-box rectangle of that node, in logical pixels.",
            &[node_param()],
            map_of_float.clone(),
            |cx| {
                raw(cx.int_arg(0))
                    .and_then(ins::node_rect)
                    .map(rect_map)
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_content_rect",
            "The content-box rectangle of that node.",
            &[node_param()],
            map_of_float.clone(),
            |cx| {
                raw(cx.int_arg(0))
                    .and_then(ins::node_content_rect)
                    .map(rect_map)
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_scroll",
            "The scroll offset and extent of that node.",
            &[node_param()],
            map_of_float,
            |cx| {
                raw(cx.int_arg(0))
                    .and_then(ins::node_scroll)
                    .map(|s| {
                        map([
                            ("x".to_string(), f64::from(s.x)),
                            ("y".to_string(), f64::from(s.y)),
                            ("max_x".to_string(), f64::from(s.max_x)),
                            ("max_y".to_string(), f64::from(s.max_y)),
                        ])
                    })
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_is_visible",
            "Whether that node is laid out and painted.",
            &[node_param()],
            T::Bool,
            |cx| {
                ScriptValue::Bool(
                    raw(cx.int_arg(0))
                        .map(ins::node_is_visible)
                        .unwrap_or(false),
                )
            },
        ),
        f(
            "node_z_index",
            "The paint order of that node.",
            &[node_param()],
            T::Int,
            |cx| {
                ScriptValue::I64(
                    raw(cx.int_arg(0))
                        .map(|h| i64::from(ins::node_z_index(h)))
                        .unwrap_or(0),
                )
            },
        ),
        f(
            "node_computed_style_all",
            "Every resolved style property of that node.",
            &[node_param()],
            map_of_str.clone(),
            |cx| {
                raw(cx.int_arg(0))
                    .map(|h| map(ins::node_computed_style_map(h)))
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_inline_style",
            "The inline style properties of that node.",
            &[node_param()],
            map_of_str.clone(),
            |cx| {
                raw(cx.int_arg(0))
                    .map(|h| map(ins::node_inline_style(h)))
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_attrs",
            "Every attribute of that node.",
            &[node_param()],
            map_of_str.clone(),
            |cx| {
                raw(cx.int_arg(0))
                    .map(|h| map(ins::node_attrs(h)))
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_classes",
            "The class list of that node.",
            &[node_param()],
            T::Array(Box::new(T::Str)),
            |cx| {
                strings(
                    raw(cx.int_arg(0))
                        .map(ins::node_classes)
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_entity_id",
            "The ECS entity behind that node, as index and generation.",
            &[node_param()],
            T::Map(Box::new(T::Int)),
            |cx| match raw(cx.int_arg(0)).and_then(ins::node_entity_id) {
                Some((index, generation)) => map([
                    ("index".to_string(), i64::from(index)),
                    ("generation".to_string(), i64::from(generation)),
                ]),
                None => empty_map(),
            },
        ),
        f(
            "node_components",
            "The names of the components that node carries.",
            &[node_param()],
            T::Array(Box::new(T::Str)),
            |cx| {
                strings(
                    raw(cx.int_arg(0))
                        .map(ins::node_components)
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_component",
            "The fields of that component on that node.",
            &[node_param(), ("name", T::Str)],
            map_of_str,
            |cx| {
                raw(cx.int_arg(0))
                    .and_then(|h| ins::node_component(h, &cx.str_arg(1)).ok().flatten())
                    .map(map)
                    .unwrap_or_else(empty_map)
            },
        ),
        f(
            "node_outer_markup",
            "That node and its subtree, rendered back to markup.",
            &[node_param()],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .map(ins::outer_markup)
                        .unwrap_or_default(),
                )
            },
        ),
        f(
            "node_inner_markup",
            "The children of that node, rendered back to markup.",
            &[node_param()],
            T::Str,
            |cx| {
                ScriptValue::Str(
                    raw(cx.int_arg(0))
                        .map(ins::inner_markup)
                        .unwrap_or_default(),
                )
            },
        ),
    ]
}

/// The empty map an absent read gives back.
fn empty_map() -> ScriptValue {
    ScriptValue::Map(HashMap::new())
}

/// The event object, read through free accessors.
///
/// Each takes the event id the dispatcher passed the handler and reads the
/// process-global current-event cell. The id is accepted for the
/// web-idiomatic `event_target(ev)` shape and to leave room for nested
/// dispatch later. Binding and unbinding stay in the host: they write the
/// handler registry, which is the host's own state.
fn event_fns() -> Vec<ScriptFn> {
    /// One accessor over the current event.
    fn accessor<F>(name: &str, doc: &str, ret: T, read: F) -> ScriptFn
    where
        F: Fn() -> ScriptValue + Send + Sync + 'static,
    {
        f(name, doc, &[("ev", T::Int)], ret, move |_| read())
    }

    vec![
        accessor(
            "event_target",
            "The node the event fired on.",
            T::Int,
            || ScriptValue::I64(id_of(event::event_target())),
        ),
        accessor(
            "event_current_target",
            "The node whose handler is running.",
            T::Int,
            || ScriptValue::I64(id_of(event::event_current_target())),
        ),
        accessor("event_type", "The event type.", T::Str, || {
            ScriptValue::Str(event::event_type())
        }),
        accessor(
            "event_key",
            "The key that produced the event.",
            T::Str,
            || ScriptValue::Str(event::event_key()),
        ),
        accessor(
            "event_value",
            "The value the event carries.",
            T::Str,
            || ScriptValue::Str(event::event_value()),
        ),
        accessor(
            "event_button",
            "The pointer button that produced the event.",
            T::Int,
            || ScriptValue::I64(event::event_button()),
        ),
        accessor(
            "event_x",
            "The pointer x, local to the node.",
            T::Float,
            || ScriptValue::F64(event::event_position_local().0),
        ),
        accessor(
            "event_y",
            "The pointer y, local to the node.",
            T::Float,
            || ScriptValue::F64(event::event_position_local().1),
        ),
        accessor(
            "event_client_x",
            "The pointer x, in window coordinates.",
            T::Float,
            || ScriptValue::F64(event::event_position_client().0),
        ),
        accessor(
            "event_client_y",
            "The pointer y, in window coordinates.",
            T::Float,
            || ScriptValue::F64(event::event_position_client().1),
        ),
        accessor(
            "event_delta_x",
            "The horizontal scroll delta.",
            T::Float,
            || ScriptValue::F64(event::event_delta().0),
        ),
        accessor(
            "event_delta_y",
            "The vertical scroll delta.",
            T::Float,
            || ScriptValue::F64(event::event_delta().1),
        ),
        accessor("event_shift", "Whether shift was held.", T::Bool, || {
            ScriptValue::Bool(event::event_modifiers().0)
        }),
        accessor("event_ctrl", "Whether control was held.", T::Bool, || {
            ScriptValue::Bool(event::event_modifiers().1)
        }),
        accessor("event_alt", "Whether alt was held.", T::Bool, || {
            ScriptValue::Bool(event::event_modifiers().2)
        }),
        accessor(
            "event_super",
            "Whether the super key was held.",
            T::Bool,
            || ScriptValue::Bool(event::event_modifiers().3),
        ),
        accessor(
            "event_prevent_default",
            "Suppress the default action for this event.",
            T::Unit,
            || {
                event::event_prevent_default();
                ScriptValue::Unit
            },
        ),
        accessor(
            "event_stop_propagation",
            "Stop the event after the handlers on this node.",
            T::Unit,
            || {
                event::event_stop_propagation();
                ScriptValue::Unit
            },
        ),
        accessor(
            "event_stop_immediate_propagation",
            "Stop the event after this handler.",
            T::Unit,
            || {
                event::event_stop_immediate_propagation();
                ScriptValue::Unit
            },
        ),
    ]
}
