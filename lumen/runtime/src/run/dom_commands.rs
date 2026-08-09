//! Applier for the dynamic DOM mutation commands (phases 2 + 3) and the
//! `window` setters (section 4.8).
//!
//! Script-issued mutations arrive as [`ScriptCommandEvent`]s; C-ABI / SDK
//! mutations arrive on the process-global external DOM bus
//! ([`lumen_script::node_query::drain_external_dom_commands`]). Both funnel
//! into one FIFO pass so a fluent chain's `spawn` + queued mutations apply
//! together, keyed by the reserved token the host minted synchronously.
//!
//! Structure edits reuse the same spawn / despawn / hierarchy path the
//! `<for>` reconciler uses, so layout / style / paint stay consistent. A
//! class / attribute / inline-style change bumps [`StyleVersion`] so the
//! cascade re-resolver restyles the affected nodes in place.

use super::*;
use bevy_ecs::hierarchy::{ChildOf, Children};
use lumen_core::components::{InlineStyle, LumenAttributes, LumenClasses, LumenId, TextContent};
use lumen_core::node::is_reserved_token;
use lumen_script::node_query::{
    DomDetails, NodeDetail, drain_external_dom_commands, publish_dom_details,
};
use std::collections::HashMap;

use crate::run::restyle::{LastMediaContext, RuntimeStylesheet, StyleVersion};

/// Commands gathered this tick, in issue order, for [`apply_dom_commands`].
#[derive(Resource, Default)]
pub(crate) struct PendingDomCommands(pub(crate) Vec<ScriptCommand>);

/// Whether a command is one this module applies.
fn is_dom_command(cmd: &ScriptCommand) -> bool {
    matches!(
        cmd,
        ScriptCommand::SetAttr { .. }
            | ScriptCommand::RemoveAttr { .. }
            | ScriptCommand::SetNodeText { .. }
            | ScriptCommand::ClassAdd { .. }
            | ScriptCommand::ClassRemove { .. }
            | ScriptCommand::ClassToggle { .. }
            | ScriptCommand::SetStyleProp { .. }
            | ScriptCommand::RemoveStyleProp { .. }
            | ScriptCommand::Spawn { .. }
            | ScriptCommand::Insert { .. }
            | ScriptCommand::ReplaceWith { .. }
            | ScriptCommand::RemoveNode { .. }
            | ScriptCommand::CloneNode { .. }
            | ScriptCommand::SetInnerMarkup { .. }
            | ScriptCommand::BindEvent { .. }
            | ScriptCommand::UnbindEvent { .. }
            | ScriptCommand::WindowSetTitle { .. }
            | ScriptCommand::WindowSetSize { .. }
    )
}

/// Collect this tick's DOM / window commands from the script event stream
/// and the external bus into [`PendingDomCommands`], preserving order.
pub(crate) fn collect_dom_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut pending: ResMut<PendingDomCommands>,
) {
    for ev in events.read() {
        if is_dom_command(&ev.0) {
            pending.0.push(ev.0.clone());
        }
    }
    pending.0.extend(drain_external_dom_commands());
}

/// Apply the collected DOM / window commands against the live world.
pub(crate) fn apply_dom_commands(world: &mut World) {
    let commands = {
        let Some(mut pending) = world.get_resource_mut::<PendingDomCommands>() else {
            return;
        };
        if pending.0.is_empty() {
            return;
        }
        std::mem::take(&mut pending.0)
    };

    // Reserved token -> spawned entity, filled as `Spawn` / `CloneNode`
    // materialize; subsequent commands in the same tick resolve through it.
    let mut reserved: HashMap<u64, Entity> = HashMap::new();
    let mut style_dirty = false;

    for cmd in commands {
        match cmd {
            ScriptCommand::Spawn { tag, reserved: tok } => {
                let el = Element {
                    tag,
                    ..Default::default()
                };
                let entity = crate::spawn::spawn_subtree(world, &el, None);
                reserved.insert(tok, entity);
                style_dirty = true;
            }
            ScriptCommand::CloneNode {
                source,
                reserved: tok,
            } => {
                if let Some(src) = resolve(world, &reserved, source) {
                    if let Some(el) = element_from_entity(world, src) {
                        let entity = crate::spawn::spawn_subtree(world, &el, None);
                        reserved.insert(tok, entity);
                        style_dirty = true;
                    }
                }
            }
            ScriptCommand::Insert {
                parent,
                node,
                before,
            } => {
                if let (Some(parent), Some(node)) = (
                    resolve(world, &reserved, parent),
                    resolve(world, &reserved, node),
                ) {
                    attach(world, parent, node, resolve(world, &reserved, before));
                    style_dirty = true;
                }
            }
            ScriptCommand::ReplaceWith { old, new } => {
                if let (Some(old), Some(new)) = (
                    resolve(world, &reserved, old),
                    resolve(world, &reserved, new),
                ) {
                    if let Some(parent) = world.get::<ChildOf>(old).map(|c| c.parent()) {
                        let index = child_index(world, parent, old);
                        attach(world, parent, new, None);
                        if let Some(i) = index {
                            world.entity_mut(parent).insert_child(i, new);
                        }
                    }
                    world.entity_mut(old).despawn();
                    style_dirty = true;
                }
            }
            ScriptCommand::RemoveNode { node } => {
                if let Some(node) = resolve(world, &reserved, node) {
                    world.entity_mut(node).despawn();
                    style_dirty = true;
                }
            }
            ScriptCommand::SetInnerMarkup { node, markup } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    style_dirty |= apply_inner_markup(world, entity, &markup);
                }
            }
            ScriptCommand::SetNodeText { node, text } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    set_text(world, entity, &text);
                }
            }
            ScriptCommand::SetAttr { node, name, value } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    style_dirty |= set_attr(world, entity, &name, &value);
                }
            }
            ScriptCommand::RemoveAttr { node, name } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    style_dirty |= remove_attr(world, entity, &name);
                }
            }
            ScriptCommand::ClassAdd { node, class } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    class_edit(world, entity, &class, ClassOp::Add);
                    style_dirty = true;
                }
            }
            ScriptCommand::ClassRemove { node, class } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    class_edit(world, entity, &class, ClassOp::Remove);
                    style_dirty = true;
                }
            }
            ScriptCommand::ClassToggle { node, class } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    class_edit(world, entity, &class, ClassOp::Toggle);
                    style_dirty = true;
                }
            }
            ScriptCommand::SetStyleProp { node, name, value } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    let mut e = world.entity_mut(entity);
                    let mut style = e.take::<InlineStyle>().unwrap_or_default();
                    style.set(&name, value);
                    e.insert(style);
                    style_dirty = true;
                }
            }
            ScriptCommand::RemoveStyleProp { node, name } => {
                if let Some(entity) = resolve(world, &reserved, node) {
                    let mut e = world.entity_mut(entity);
                    if let Some(mut style) = e.take::<InlineStyle>() {
                        style.remove(&name);
                        e.insert(style);
                    }
                    style_dirty = true;
                }
            }
            ScriptCommand::BindEvent {
                node,
                event_type,
                capture,
                token,
            } => {
                // Resolve a reserved spawn token to the real handle so the
                // dispatcher (which sees live entities) matches the binding.
                let handle = resolve(world, &reserved, node)
                    .map(|e| lumen_core::node::NodeHandle::new(e).pack())
                    .unwrap_or(node);
                lumen_script::event::register_host_binding(token, handle, event_type, capture);
            }
            ScriptCommand::UnbindEvent { token } => {
                lumen_script::event::unregister_binding(token);
            }
            ScriptCommand::WindowSetTitle { title } => {
                lumen_core::window_state::set_title(&title);
            }
            ScriptCommand::WindowSetSize { width, height } => {
                lumen_core::window_state::set_size(width, height);
            }
            _ => {}
        }
    }

    if style_dirty {
        bump_style_version(world);
    }
}

/// Resolve a packed handle or reserved token to a live entity. Reserved
/// tokens map through this tick's spawn table; real handles must still be
/// present in the world.
fn resolve(world: &World, reserved: &HashMap<u64, Entity>, handle: u64) -> Option<Entity> {
    if handle == 0 {
        return None;
    }
    if is_reserved_token(handle) {
        return reserved.get(&handle).copied();
    }
    let entity = lumen_core::node::NodeHandle::unpack(handle)?.entity;
    world.entities().contains(entity).then_some(entity)
}

/// Attach `node` under `parent`, before `before` when given (else append).
/// bevy's `ChildOf` relationship reparents automatically.
fn attach(world: &mut World, parent: Entity, node: Entity, before: Option<Entity>) {
    match before.and_then(|b| child_index(world, parent, b)) {
        Some(index) => {
            world.entity_mut(parent).insert_child(index, node);
        }
        None => {
            world.entity_mut(parent).add_child(node);
        }
    }
    world.entity_mut(node).insert(DirtyLayout);
}

/// Index of `child` in `parent`'s ordered children, if present.
fn child_index(world: &World, parent: Entity, child: Entity) -> Option<usize> {
    world
        .get::<Children>(parent)
        .and_then(|c| c.iter().position(|e| e == child))
}

fn set_text(world: &mut World, entity: Entity, text: &str) {
    let mut e = world.entity_mut(entity);
    e.insert(TextContent(text.to_string()));
    if let Some(mut input) = e.get_mut::<lumen_core::components::TextInput>() {
        input.cursor = text.len();
    }
}

/// Set an attribute, routing known names to typed components. Returns
/// whether the change can affect the cascade (needs a restyle).
fn set_attr(world: &mut World, entity: Entity, name: &str, value: &str) -> bool {
    match name {
        "id" => {
            world.entity_mut(entity).insert(LumenId(value.to_string()));
            true
        }
        "class" => {
            let classes: Vec<String> = value.split_whitespace().map(str::to_string).collect();
            world.entity_mut(entity).insert(LumenClasses::from(classes));
            true
        }
        "text" => {
            set_text(world, entity, value);
            false
        }
        "disabled" => {
            let on = matches!(value, "true" | "" | "disabled");
            if on {
                world
                    .entity_mut(entity)
                    .insert(lumen_core::components::Disabled);
            } else {
                world
                    .entity_mut(entity)
                    .remove::<lumen_core::components::Disabled>();
            }
            true
        }
        _ => {
            let mut e = world.entity_mut(entity);
            let mut attrs = e.take::<LumenAttributes>().unwrap_or_default();
            attrs.set(name, value);
            e.insert(attrs);
            false
        }
    }
}

fn remove_attr(world: &mut World, entity: Entity, name: &str) -> bool {
    match name {
        "id" => {
            world.entity_mut(entity).remove::<LumenId>();
            true
        }
        "class" => {
            world.entity_mut(entity).remove::<LumenClasses>();
            true
        }
        "disabled" => {
            world
                .entity_mut(entity)
                .remove::<lumen_core::components::Disabled>();
            true
        }
        _ => {
            let mut e = world.entity_mut(entity);
            if let Some(mut attrs) = e.take::<LumenAttributes>() {
                attrs.remove(name);
                e.insert(attrs);
            }
            false
        }
    }
}

enum ClassOp {
    Add,
    Remove,
    Toggle,
}

fn class_edit(world: &mut World, entity: Entity, class: &str, op: ClassOp) {
    let mut e = world.entity_mut(entity);
    let mut classes = e.take::<LumenClasses>().unwrap_or_default();
    let present = classes.0.iter().any(|c| c.as_ref() == class);
    let want = match op {
        ClassOp::Add => true,
        ClassOp::Remove => false,
        ClassOp::Toggle => !present,
    };
    if want && !present {
        classes.0.push(class.into());
    } else if !want && present {
        classes.0.retain(|c| c.as_ref() != class);
    }
    e.insert(classes);
}

/// Reconstruct a minimal [`Element`] subtree from a live entity: tag,
/// identity, text, generic attrs, inline style, and children (deep). Used
/// by `clone_deep`. Runtime-only components (interaction / layout state)
/// are re-derived by the spawn path, not copied.
fn element_from_entity(world: &World, entity: Entity) -> Option<Element> {
    use lumen_core::components::{LumenClasses as Classes, LumenTag};
    let tag = world.get::<LumenTag>(entity)?.0.to_string();
    let mut el = Element {
        tag,
        ..Default::default()
    };
    if let Some(c) = world.get::<Classes>(entity) {
        el.attrs.classes = c.0.iter().map(|s| s.to_string()).collect();
    }
    if let Some(i) = world.get::<LumenId>(entity) {
        el.attrs.id = Some(i.0.clone());
    }
    if let Some(t) = world.get::<TextContent>(entity) {
        el.attrs.text = Some(t.0.clone());
    }
    if let Some(children) = world.get::<Children>(entity) {
        for child in children.iter() {
            if let Some(child_el) = element_from_entity(world, child) {
                el.children.push(child_el);
            }
        }
    }
    Some(el)
}

/// Replace `entity`'s children with the subtree parsed from `markup`
/// (`set_inner_markup` / `element.innerHTML`). Returns whether the tree
/// changed (needs a restyle).
///
/// Guarded: parsing needs the injected markup front-end
/// ([`RuntimeParser`](crate::source_parser)), which the from-source run path
/// installs but the precompiled-artifact path does not. Absent, this is a
/// no-op with a one-time warning, the documented limitation. The markup is
/// live and unsanitized: callers must not feed it untrusted content.
fn apply_inner_markup(world: &mut World, entity: Entity, markup: &str) -> bool {
    let Some(parser) = world
        .get_resource::<crate::source_parser::RuntimeParser>()
        .map(|p| p.0.clone())
    else {
        warn_inner_markup_unavailable();
        return false;
    };
    // Wrap the fragment in a throwaway root so multiple top-level nodes (or a
    // bare text run) parse under one element the front-end accepts; only the
    // wrapper's children are spawned.
    let wrapped = format!("<div>{markup}</div>");
    let ir = match parser.parse_html(&wrapped) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("set_inner_markup: parse error: {e}");
            return false;
        }
    };
    // Despawn the current children (each with its whole subtree).
    let existing: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    for child in existing {
        world.entity_mut(child).despawn();
    }
    // Spawn the parsed children under `entity` through the same path the
    // `<for>` reconciler + phase-3 Spawn use.
    let root = &ir.root;
    if root.children.is_empty() {
        if let Some(text) = &root.attrs.text {
            set_text(world, entity, text);
        }
    } else {
        for child_el in &root.children {
            crate::spawn::spawn_subtree(world, child_el, Some(entity));
        }
    }
    world.entity_mut(entity).insert(DirtyLayout);
    true
}

/// One-time warning when `set_inner_markup` runs without an injected parser
/// (the precompiled-artifact path). Keeps the log from flooding on a
/// per-tick caller.
fn warn_inner_markup_unavailable() {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "set_inner_markup: no markup parser is linked on this run path \
             (precompiled artifact); the call is a no-op. It works on the \
             dev / from-source run path."
        );
    });
}

fn bump_style_version(world: &mut World) {
    if let Some(mut v) = world.get_resource_mut::<StyleVersion>() {
        v.0 = v.0.wrapping_add(1);
    } else {
        world.insert_resource(StyleVersion(1));
    }
}

/// Monotonic frame counter + previous-tick timestamp backing
/// `frame_info()`. Advanced by [`publish_introspection`].
#[derive(Resource)]
pub(crate) struct FrameClock {
    frame: u64,
    last: std::time::Instant,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self {
            frame: 0,
            last: std::time::Instant::now(),
        }
    }
}

/// Publish the phase-5 low-level introspection snapshot: post-layout
/// geometry (rect / content_rect / scroll / visibility / z-index), typed
/// component field maps, pointer / frame state, and the signal set. Runs
/// each tick alongside the [`DomIndex`] publish so a read issued from a
/// handler sees this tick's tree. Geometry reflects the last committed
/// layout (this tick's `sync_layout` runs later in `LayoutSync`), matching
/// `computed_style`'s one-tick lag. An inspection pass, not a render path.
#[allow(clippy::type_complexity)]
pub(crate) fn publish_introspection(world: &mut World) {
    use lumen_core::components::{
        DirtyLayout, Display, LumenTag, Style, Transform, Visible, Visuals, ZIndex,
    };
    use lumen_core::input::{ModifiersState, PointerState, ScrollOffset};
    use lumen_core::introspect::ComponentIntrospection;
    use lumen_script::introspect::{
        FrameInfo, IntrospectSnapshot, NodeGeometry, NodeRect, NodeScroll, PointerSnapshot,
        publish_introspection as publish,
    };
    use std::collections::HashMap;

    let registry = ComponentIntrospection::with_defaults();
    let known = registry.names().iter().map(|s| s.to_string()).collect();

    // Absolute transform + parent lookups, computed once.
    let transforms: HashMap<Entity, (glam::Vec2, glam::Vec2)> = {
        let mut q = world.query::<(Entity, &Transform)>();
        q.iter(world)
            .map(|(e, t)| (e, (t.absolute, t.size)))
            .collect()
    };
    let parents: HashMap<Entity, Entity> = {
        let mut q = world.query::<(Entity, &ChildOf)>();
        q.iter(world).map(|(e, c)| (e, c.parent())).collect()
    };
    let mut children_of: HashMap<Entity, Vec<Entity>> = HashMap::new();
    for (child, parent) in &parents {
        children_of.entry(*parent).or_default().push(*child);
    }

    // Raw per-element geometry inputs, for indexed (tagged) elements only.
    struct Raw {
        entity: Entity,
        abs: glam::Vec2,
        size: glam::Vec2,
        pad: (f32, f32, f32, f32),
        border: (f32, f32, f32, f32),
        display_none: bool,
        visible: bool,
        z: i32,
        scroll_off: Option<glam::Vec2>,
    }
    let raws: Vec<Raw> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Transform,
            Option<&Style>,
            Option<&Visuals>,
            Option<&ZIndex>,
            Option<&Visible>,
            Option<&ScrollOffset>,
        ), With<LumenTag>>();
        q.iter(world)
            .map(|(e, t, style, visuals, z, vis, scroll)| {
                let pad = style
                    .map(|s| {
                        (
                            s.padding.left,
                            s.padding.right,
                            s.padding.top,
                            s.padding.bottom,
                        )
                    })
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                let border = visuals
                    .and_then(|v| v.border.as_ref())
                    .map(|b| (b.widths.left, b.widths.right, b.widths.top, b.widths.bottom))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                Raw {
                    entity: e,
                    abs: t.absolute,
                    size: t.size,
                    pad,
                    border,
                    display_none: style.map(|s| s.display == Display::None).unwrap_or(false),
                    visible: vis.map(|v| v.0).unwrap_or(true),
                    z: z.map(|z| z.0).unwrap_or(0),
                    scroll_off: scroll.map(|s| s.0),
                }
            })
            .collect()
    };

    let mut geometry: HashMap<u64, NodeGeometry> = HashMap::with_capacity(raws.len());
    for r in &raws {
        let handle = lumen_core::node::NodeHandle::new(r.entity).pack();
        let parent_abs = parents
            .get(&r.entity)
            .and_then(|p| transforms.get(p))
            .map(|(a, _)| *a)
            .unwrap_or(glam::Vec2::ZERO);
        let local = r.abs - parent_abs;
        let rect = NodeRect {
            x: local.x,
            y: local.y,
            width: r.size.x,
            height: r.size.y,
            client_x: r.abs.x,
            client_y: r.abs.y,
        };
        let (pl, pr, pt, pb) = r.pad;
        let (bl, br, bt, bb) = r.border;
        let inset_x = pl + bl;
        let inset_y = pt + bt;
        let content_rect = NodeRect {
            x: local.x + inset_x,
            y: local.y + inset_y,
            width: (r.size.x - pl - pr - bl - br).max(0.0),
            height: (r.size.y - pt - pb - bt - bb).max(0.0),
            client_x: r.abs.x + inset_x,
            client_y: r.abs.y + inset_y,
        };
        // Scroll extent from the bbox of direct children relative to self
        // (the same rule `clamp_scroll_offsets` applies).
        let scroll = match r.scroll_off {
            Some(off) => {
                let mut ext = glam::Vec2::ZERO;
                if let Some(kids) = children_of.get(&r.entity) {
                    for kid in kids {
                        if let Some((kabs, ksize)) = transforms.get(kid) {
                            let rel = (*kabs - r.abs) + *ksize;
                            ext = ext.max(rel);
                        }
                    }
                }
                NodeScroll {
                    x: off.x,
                    y: off.y,
                    max_x: (ext.x - r.size.x).max(0.0),
                    max_y: (ext.y - r.size.y).max(0.0),
                }
            }
            None => NodeScroll::default(),
        };
        geometry.insert(
            handle,
            NodeGeometry {
                rect,
                content_rect,
                scroll,
                visible: r.visible && !r.display_none,
                z_index: r.z,
            },
        );
    }

    // Component field maps via the whitelist registry.
    let mut components: HashMap<u64, Vec<(String, Vec<(String, String)>)>> =
        HashMap::with_capacity(raws.len());
    for r in &raws {
        let handle = lumen_core::node::NodeHandle::new(r.entity).pack();
        let maps = registry.read_all(world.entity(r.entity));
        if !maps.is_empty() {
            components.insert(handle, maps);
        }
    }

    // Pointer state.
    let pointer = {
        let ps = world
            .get_resource::<PointerState>()
            .copied()
            .unwrap_or_default();
        let m = world
            .get_resource::<ModifiersState>()
            .map(|m| m.0)
            .unwrap_or_default();
        let pos = ps.position.unwrap_or(glam::Vec2::ZERO);
        PointerSnapshot {
            x: pos.x,
            y: pos.y,
            inside: ps.position.is_some(),
            buttons: u32::from(ps.primary_down),
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            super_: m.super_,
        }
    };

    // Signal set (global scalar cells).
    let signals: Vec<(String, String)> = world
        .get_resource::<lumen_core::property_store::PropertyStore>()
        .map(|store| {
            let mut out: Vec<(String, String)> = store
                .iter()
                .filter_map(|(k, v)| match k {
                    lumen_core::property_store::PropertyKey::Global(name) => {
                        Some((name.to_string(), property_value_string(v)))
                    }
                    _ => None,
                })
                .collect();
            out.sort();
            out
        })
        .unwrap_or_default();

    // Frame counters.
    let dirty_count = {
        let mut q = world.query_filtered::<Entity, With<DirtyLayout>>();
        q.iter(world).count() as u64
    };
    let frame = {
        let now = std::time::Instant::now();
        let mut clock = world.get_resource_or_insert_with(FrameClock::default);
        let dt_ms = now.duration_since(clock.last).as_secs_f64() * 1000.0;
        clock.frame = clock.frame.wrapping_add(1);
        clock.last = now;
        FrameInfo {
            frame: clock.frame,
            dt_ms,
            dirty_count,
        }
    };

    publish(IntrospectSnapshot::new(
        geometry, components, known, pointer, frame, signals,
    ));
}

/// Render a scalar [`PropertyValue`](lumen_core::property_store::PropertyValue)
/// as a display string for `signals_all()`. Colors render as hex, vectors
/// as `x,y`; the enumerated scalars use their natural form.
fn property_value_string(v: &lumen_core::property_store::PropertyValue) -> String {
    use lumen_core::property_store::PropertyValue as V;
    match v {
        V::Bool(b) => b.to_string(),
        V::I64(n) => n.to_string(),
        V::F64(n) => n.to_string(),
        V::Str(s) => s.to_string(),
        V::Color(c) => {
            let [r, g, b, a] = c.to_rgba8();
            if a == 0xff {
                format!("#{r:02x}{g:02x}{b:02x}")
            } else {
                format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
            }
        }
        V::Vec2(p) => format!("{},{}", p.x, p.y),
        V::Custom(_) => String::new(),
    }
}

/// Publish the per-node detail snapshot (text / generic attrs / inline
/// style) plus the cascade inputs `computed_style` needs. Runs each tick
/// alongside the [`DomIndex`] publish so a read issued from a handler sees
/// this tick's state.
#[allow(clippy::type_complexity)]
pub(crate) fn publish_node_details(
    nodes: Query<(
        Entity,
        Option<&TextContent>,
        Option<&LumenAttributes>,
        Option<&InlineStyle>,
    )>,
    focused: Query<Entity, With<lumen_core::input::Focused>>,
    hovered: Query<Entity, With<lumen_core::input::Hovered>>,
    sheet: Option<Res<RuntimeStylesheet>>,
    media: Option<Res<LastMediaContext>>,
) {
    let pack = |e: Entity| lumen_core::node::NodeHandle::new(e).pack();
    lumen_script::node_query::publish_focus(
        focused.iter().next().map(pack),
        hovered.iter().next().map(pack),
    );
    let mut map: HashMap<u64, NodeDetail> = HashMap::new();
    for (entity, text, attrs, inline) in nodes.iter() {
        let detail = NodeDetail {
            text: text.map(|t| t.0.clone()),
            attributes: attrs
                .map(|a| a.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            inline_style: inline.map(|s| s.0.clone()).unwrap_or_default(),
        };
        map.insert(entity.to_bits(), detail);
    }
    let sheet = sheet.map(|s| std::sync::Arc::new(s.0.clone()));
    let media = media.map(|m| m.0).unwrap_or_default();
    publish_dom_details(DomDetails::new(map, sheet, media));
}
