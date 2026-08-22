//! The dynamic DOM: the tree a script reads, and the mutations it issues.
//!
//! A script reaches the scene through two channels, and both are here because
//! neither knows anything about the platform underneath. The read side is
//! [`build_dom_index`], which republishes the live tree each tick so a
//! `query()` from a handler sees this tick's nodes. The write side is a pair:
//! [`collect_dom_commands`] gathers this tick's mutations in issue order, and
//! [`apply_dom_commands`] materializes them against the world.
//!
//! Script-issued mutations arrive as
//! [`ScriptCommandEvent`](lumen_script::runtime::ScriptCommandEvent)s; C-ABI
//! and SDK mutations arrive on the process-global external DOM bus
//! ([`lumen_script::node_query::drain_external_dom_commands`]). Both funnel
//! into one FIFO pass so a fluent chain's `spawn` plus its queued mutations
//! apply together, keyed by the reserved token the host minted synchronously.
//!
//! Structure edits reuse the same spawn / despawn / hierarchy path the
//! `<for>` reconciler uses, so layout, style and paint stay consistent. A
//! class, attribute or inline-style change bumps
//! [`StyleVersion`](lumen_core::components::StyleVersion), which is what a
//! host with a cascade of its own listens on; a host whose platform resolves
//! CSS itself, such as a browser page, has nothing reading it and pays a
//! counter increment.

use std::collections::HashMap;

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use lumen_core::components::{
    DirtyLayout, Disabled, InlineStyle, LumenAttributes, LumenClasses, LumenId, LumenTag,
    StyleVersion, TextContent, TextInput,
};
use lumen_core::node::{DomIndex, DomRecord, NodeHandle, is_reserved_token, publish_dom_index};
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::PropertyStore;
use lumen_core::window_state::{set_size, set_title};
use lumen_ir::fragment::FragmentTable;
use lumen_ir::interpolate::Scope;
use lumen_ir::layout_ir::Element;
use lumen_script::event::{register_host_binding, unregister_binding};
use lumen_script::node_query::drain_external_dom_commands;
use lumen_script::runtime::{ScriptCommandEvent, register_script_commands};
use lumen_script::{ScriptCommand, ScriptSet};

use crate::fragments::{
    FragmentFault, FragmentInstance, FragmentLibrary, SlotPlaceholder, bind_args, instance_body,
    report_once,
};
use crate::source_parser::RuntimeParser;
use crate::spawn::Placeholders;

/// Install the dynamic DOM pipeline on `app`.
///
/// One call for every host, because the pieces only work as a set: the index
/// has to be published before anything reads it, the collector has to run
/// after every producer of commands, and the applier has to run after the
/// collector. A host that installed two of the three would have a `query()`
/// that answers about last tick, or a `mount()` that never lands.
///
/// The read publish runs before the handlers so a query issued from one sees
/// this tick's tree, and before the key dispatch for the same reason.
pub fn install_dom(app: &mut App) {
    // The collector reads `ScriptCommandEvent`. The script plugin registers
    // that message only when a host is installed, so an app with no script
    // must self-register it here or the reader fails parameter validation.
    register_script_commands(&mut app.world);
    app.world.init_resource::<PendingDomCommands>();
    app.add_systems(
        TickStage::Systems,
        build_dom_index
            .before(ScriptSet::Dispatch)
            .before(lumen_input::dispatch_focused_keys),
    );
    app.add_systems(
        TickStage::Systems,
        collect_dom_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::Ready)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::DomState),
    );
    app.add_systems(
        TickStage::Systems,
        apply_dom_commands.after(collect_dom_commands),
    );
}

/// Rebuild the per-tick [`DomIndex`] snapshot from the live tree and publish
/// it for cross-thread readers (script hosts, the C-ABI).
///
/// Every spawned element carries a [`LumenTag`], so the query walks all
/// selector-reachable entities; a parent or child that is not itself tagged,
/// such as the window root, is dropped from the element tree.
#[allow(clippy::type_complexity)]
pub fn build_dom_index(
    query: Query<
        (
            Entity,
            &LumenTag,
            Option<&LumenClasses>,
            Option<&LumenId>,
            Option<&ChildOf>,
            Option<&Children>,
        ),
        Without<lumen_core::components::DomHidden>,
    >,
) {
    use std::collections::HashSet;
    let indexed: HashSet<u64> = query.iter().map(|(e, ..)| e.to_bits()).collect();
    let mut records: Vec<DomRecord> = Vec::with_capacity(indexed.len());
    for (entity, tag, classes, id, child_of, children) in query.iter() {
        let parent = child_of
            .map(|c| c.parent())
            .filter(|p| indexed.contains(&p.to_bits()));
        let kids: Vec<Entity> = children
            .map(|c| {
                c.iter()
                    .filter(|e| indexed.contains(&e.to_bits()))
                    .collect()
            })
            .unwrap_or_default();
        records.push(DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: tag.0.to_string(),
            id: id.map(|i| i.0.clone()),
            classes: classes
                .map(|c| c.0.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            parent,
            children: kids,
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        });
    }
    publish_dom_index(DomIndex::build(records));
}

/// Commands gathered this tick, in issue order, for [`apply_dom_commands`].
#[derive(Resource, Default)]
pub struct PendingDomCommands(Vec<ScriptCommand>);

/// Collect this tick's DOM / window commands from the script event stream
/// and the external bus into [`PendingDomCommands`], preserving order.
pub fn collect_dom_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut pending: ResMut<PendingDomCommands>,
) {
    for ev in events.read() {
        if ev.0.mutates_dom() {
            pending.0.push(ev.0.clone());
        }
    }
    pending.0.extend(drain_external_dom_commands());
}

/// Apply the collected DOM / window commands against the live world.
pub fn apply_dom_commands(world: &mut World) {
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
                let entity =
                    crate::spawn::spawn_subtree(world, &el, None, Placeholders::Unresolved);
                reserved.insert(tok, entity);
                style_dirty = true;
            }
            ScriptCommand::CloneNode {
                source,
                reserved: tok,
            } => {
                if let Some(src) = resolve(world, &reserved, source) {
                    if let Some(el) = element_from_entity(world, src) {
                        // Read back out of a live entity, so its strings are
                        // the resolved ones the original was built with.
                        let entity =
                            crate::spawn::spawn_subtree(world, &el, None, Placeholders::Resolved);
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
                    replace_node(world, old, new);
                    style_dirty = true;
                }
            }
            ScriptCommand::SpawnFragment {
                key,
                args,
                children,
                reserved: tok,
            } => {
                let children: Vec<(String, Entity)> = children
                    .into_iter()
                    .filter_map(|(slot, handle)| Some((slot, resolve(world, &reserved, handle)?)))
                    .collect();
                if let Some(entity) = spawn_fragment(world, &key, &args, &children) {
                    reserved.insert(tok, entity);
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
                // dispatcher, which sees live entities, matches the binding.
                let handle = resolve(world, &reserved, node)
                    .map(|e| NodeHandle::new(e).pack())
                    .unwrap_or(node);
                register_host_binding(token, handle, event_type, capture);
            }
            ScriptCommand::UnbindEvent { token } => {
                unregister_binding(token);
            }
            ScriptCommand::WindowSetTitle { title } => {
                set_title(&title);
            }
            ScriptCommand::WindowSetSize { width, height } => {
                set_size(width, height);
            }
            _ => {}
        }
    }

    if style_dirty {
        StyleVersion::bump(world);
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
    let entity = NodeHandle::unpack(handle)?.entity;
    world.entities().contains(entity).then_some(entity)
}

/// Move `new` into `old`'s place among its siblings, then despawn `old`
/// and its subtree. A detached `old` has no place to take, so `new` stays
/// where it is and only the despawn happens.
fn replace_node(world: &mut World, old: Entity, new: Entity) {
    if let Some(parent) = world.get::<ChildOf>(old).map(|c| c.parent()) {
        let index = child_index(world, parent, old);
        attach(world, parent, new, None);
        if let Some(i) = index {
            world.entity_mut(parent).insert_child(i, new);
        }
    }
    world.entity_mut(old).despawn();
}

/// Instantiate the fragment `key` into a fresh detached subtree and return
/// its root, or report why it could not and return `None`.
///
/// The body is cloned and its placeholders resolved against the bound
/// arguments before anything spawns, so the tree that reaches the world is
/// already the instance's own. Each `<slot>` the caller passed a child for
/// is replaced by that child, in place; a slot nothing filled keeps the
/// fallback content the body wrote inside it.
///
/// The result is detached, the way `Spawn` is: a following `Insert` puts it
/// in the tree.
fn spawn_fragment(
    world: &mut World,
    key: &str,
    args: &[(String, String)],
    children: &[(String, Entity)],
) -> Option<Entity> {
    // An app built without a library declares nothing, which the key lookup
    // below reports the same way as a key nobody wrote.
    let library = world
        .get_resource::<FragmentLibrary>()
        .cloned()
        .unwrap_or_default();
    let Some(fragment) = library.get(key) else {
        report_once(key, &FragmentFault::UnknownKey);
        return None;
    };
    let body = match instance_body(fragment) {
        Ok(body) => body,
        Err(fault) => {
            report_once(key, &fault);
            return None;
        }
    };
    let bound = bind_args(fragment, args);

    let instance = {
        let empty = PropertyStore::default();
        let store = world.get_resource::<PropertyStore>().unwrap_or(&empty);
        let scope = Scope::new(store).with_args(&bound);
        crate::spawn::substitute_in_element_with_css(body, &scope, None)
    };
    let root = crate::spawn::spawn_subtree(world, &instance, None, Placeholders::Resolved);

    if !children.is_empty() {
        let slots: Vec<(String, Entity)> = descendants(world, root)
            .into_iter()
            .filter_map(|e| Some((world.get::<SlotPlaceholder>(e)?.0.clone(), e)))
            .collect();
        for (name, child) in children {
            match slots.iter().find(|(slot, _)| slot == name) {
                Some((_, slot)) => replace_node(world, *slot, *child),
                None => report_once(key, &FragmentFault::UnknownSlot(name.clone())),
            }
        }
    }

    world.entity_mut(root).insert(FragmentInstance {
        key: key.to_string(),
        args: bound,
    });
    Some(root)
}

/// Every entity under `root`, `root` itself included, parents before
/// children.
fn descendants(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        if let Some(kids) = world.get::<Children>(out[i]) {
            out.extend(kids.iter());
        }
        i += 1;
    }
    out
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
    if let Some(mut input) = e.get_mut::<TextInput>() {
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
                world.entity_mut(entity).insert(Disabled);
            } else {
                world.entity_mut(entity).remove::<Disabled>();
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
            world.entity_mut(entity).remove::<Disabled>();
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
    let tag = world.get::<LumenTag>(entity)?.0.to_string();
    let mut el = Element {
        tag,
        ..Default::default()
    };
    if let Some(c) = world.get::<LumenClasses>(entity) {
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
/// ([`RuntimeParser`](crate::source_parser::RuntimeParser)), which the
/// from-source run path installs and the precompiled-artifact path does not.
/// Absent, this is a no-op with a one-time warning, the documented
/// limitation. The markup is live and unsanitized: callers must not feed it
/// untrusted content.
fn apply_inner_markup(world: &mut World, entity: Entity, markup: &str) -> bool {
    let Some(parser) = world.get_resource::<RuntimeParser>().map(|p| p.0.clone()) else {
        warn_inner_markup_unavailable();
        return false;
    };
    // Wrap the fragment in a throwaway root so multiple top-level nodes (or a
    // bare text run) parse under one element the front-end accepts; only the
    // wrapper's children are spawned.
    let wrapped = format!("<div>{markup}</div>");
    let ir = match parser.parse_html(&wrapped, &FragmentTable::new()) {
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
    // `<for>` reconciler and `Spawn` use.
    let root = &ir.root;
    if root.children.is_empty() {
        if let Some(text) = &root.attrs.text {
            set_text(world, entity, text);
        }
    } else {
        for child_el in &root.children {
            crate::spawn::spawn_subtree(world, child_el, Some(entity), Placeholders::Unresolved);
        }
    }
    world.entity_mut(entity).insert(DirtyLayout);
    true
}

/// One-time warning when `set_inner_markup` runs without an injected parser,
/// which is every app running from a precompiled artifact. Keeps the log from
/// flooding on a per-tick caller.
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
