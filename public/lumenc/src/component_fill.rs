//! Resolving a component that has to run, while the site is being built.
//!
//! A component whose body the build can stand in for is already its body by
//! the time the tree reaches the emitter. One that has to run, because it works
//! a value out or picks between blocks, is left in the tree as a marker for the
//! runtime to fill.
//!
//! On the web that marker is not good enough. A component's shape is tree
//! structure, not app state, so it belongs in the document like the rest of the
//! markup: a crawler reads the page it is served, and a page whose components
//! are empty boxes is a page missing whatever those components render. So the
//! build fills them here, in every `render` and `prerender` combination, and
//! what the browser gets is a body it adopts like any other.
//!
//! The call itself is not run here. The app is booted and settled by
//! [`lumen_prerender`], which already answers the network without leaving the
//! machine and already stops when the app stops changing; its world is then
//! read for what each marker turned into. What comes back is a fragment key
//! and the arguments it was built with, which is all the compiler's own
//! fragment inliner needs to put the body in the tree.
//!
//! Reading the key rather than the built subtree is what keeps the result
//! exactly what a window would have built: the body comes from the artifact's
//! fragment table, through the same instantiation a `<template>` goes through,
//! rather than from a walk back out of the ECS that would have to reconstruct
//! every attribute the spawner consumed.
//!
//! A component inside a `<for>` is not filled into the tree: the tree holds
//! the row template, and what the component renders for one row is not what it
//! renders for another. Those bodies come back from the same world as
//! [`RowFills`], one per row, and the emitter writes each one into the row it
//! belongs to.

use std::collections::BTreeSet;
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use lumen_core::components::LumenTag;
use lumen_html::contract::Seed;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Element, FragmentUse};
use lumen_prerender::{Budget, DenyDispatch, Location, boot, root_entity, row_fills, settle};
use lumen_runtime::fragments::FragmentInstance;
use lumen_web::RowFills;

/// How many times the tree is filled and re-inlined.
///
/// One round resolves the markers standing in the tree; a body spliced in by
/// that round can name a component of its own, which the next round resolves.
/// The bound is the deepest chain of components-inside-components a build
/// follows, and reaching it means the app is asking for something other than
/// what it looks like.
const MAX_ROUNDS: u32 = 16;

/// Replace every component marker in `compiled`'s tree with the body its call
/// produces, and hand back what the components inside its `<for>` rows built
/// along with the names this pass left standing on purpose.
///
/// The tree is left as it was where a marker cannot be resolved, which is a
/// component the loaded program cannot be called by name; the export check in
/// `web_cli` reports it, because it holds the export list. The names that come
/// back are the ones it must not report: a marker this pass never called, so
/// nothing about it says the call came back empty.
///
/// A component inside a `<for>` row renders a body per row, and the tree holds
/// the template rather than the rows, so those bodies come back separately for
/// the emitter to write into the rows it writes. They are the last round's:
/// the tree the last boot ran is the tree the pages are written from.
///
/// `seed` is the state the boot starts from, which is the state the pages are
/// written with.
pub fn fill(
    compiled: &mut CompiledApp,
    page: &str,
    seed: &Seed,
    warnings: &mut Vec<String>,
) -> (RowFills, BTreeSet<String>) {
    // An app with no marker in it has nothing to run and nothing to wait for,
    // which is most apps; booting one to learn that is a cost with no answer
    // attached.
    if !holds_marker(&compiled.ir.root) {
        return (RowFills::default(), BTreeSet::new());
    }

    let mut fills = RowFills::default();
    let mut exhausted = true;
    for _ in 0..MAX_ROUNDS {
        let (filled, round_fills) = round(compiled, page, seed, warnings);
        fills = round_fills;
        if !filled {
            exhausted = false;
            break;
        }
    }

    let mut left_standing = BTreeSet::new();
    if exhausted {
        warnings.push(format!(
            "components are nested deeper than {MAX_ROUNDS} levels; the ones left are emitted as \
             the empty box the browser fills"
        ));
        // The depth is why they are still markers, and it is reported once
        // above. Nothing called them, so the export check has nothing to add.
        markers(&compiled.ir.root, &mut left_standing);
    } else {
        markers_in_rows(&compiled.ir.root, false, &mut left_standing);
    }
    (fills, left_standing)
}

/// Collect the name of every marker still standing under `element`.
fn markers(element: &Element, out: &mut BTreeSet<String>) {
    if let Some(use_site) = &element.frag_use {
        out.insert(use_site.key.clone());
    }
    for child in &element.children {
        markers(child, out);
    }
}

/// Collect the name of every marker under `element` that stands inside a
/// `<for>` row template.
///
/// Those are the ones this pass leaves for the browser by design: a row's body
/// is read off the run and written into the row, and the template keeps the
/// marker however well the call worked.
fn markers_in_rows(element: &Element, in_a_row: bool, out: &mut BTreeSet<String>) {
    if in_a_row && let Some(use_site) = &element.frag_use {
        out.insert(use_site.key.clone());
    }
    let rows = in_a_row || element.tag == "for";
    for child in &element.children {
        markers_in_rows(child, rows, out);
    }
}

/// Boot the app, read what its markers became, and put those bodies in the
/// tree. Answers whether anything was filled, and what the rows built.
fn round(
    compiled: &mut CompiledApp,
    page: &str,
    seed: &Seed,
    warnings: &mut Vec<String>,
) -> (bool, RowFills) {
    let mut booted = boot(
        compiled,
        &Location::page(page),
        seed,
        Arc::new(DenyDispatch::default()),
    );
    settle(&mut booted.app, Budget::default());

    let root = match root_entity(&booted.app) {
        Some(root) => root,
        None => {
            warnings.push(
                "the app built no tree to read its components out of, so they are emitted as the \
                 empty box the browser fills"
                    .to_string(),
            );
            return (false, RowFills::default());
        }
    };

    let mut found = Found::default();
    resolve(&mut compiled.ir.root, root, &booted.app.world, &mut found);
    let fills = row_fills(&mut booted.app);
    drop(booted);

    if !found.filled {
        return (false, fills);
    }

    // The keys are the table's own now, so the inliner treats each one the way
    // it treats a `<template>` a use site names, and a body that names another
    // component keeps its marker for the next round.
    let mut lint = Vec::new();
    if let Err(error) =
        crate::fragments::inline(&mut compiled.ir.root, &compiled.fragments, &mut lint)
    {
        warnings.push(format!(
            "a component's body could not be put in the tree: {error}"
        ));
        return (false, fills);
    }
    (true, fills)
}

/// What one round ran into.
#[derive(Default)]
struct Found {
    /// At least one marker took a body, so another round is worth running.
    filled: bool,
}

/// Walk the tree beside the world that was spawned from it, pointing each
/// marker at the fragment its call built.
///
/// A `<for>` template is not its rows, so the walk stops at one: what a
/// component renders for one row is not what it renders for another, and the
/// bodies are read off the world by [`row_fills`] instead.
///
/// The two walks stay in step because the world is this tree, spawned: a
/// marker's replacement takes the marker's own place among its siblings, so it
/// is the entity at that position. Where they can no longer be in step the walk
/// stops rather than guessing, because a wrong pairing writes one component's
/// body where another's belongs.
fn resolve(
    element: &mut Element,
    entity: Entity,
    world: &bevy_ecs::world::World,
    found: &mut Found,
) {
    if element.frag_use.is_some() {
        // No instance means the marker is still standing in the world too,
        // which is what a call that built nothing leaves behind. It stays in
        // the tree, and the export check downstream reads it there.
        if let Some(instance) = world.get::<FragmentInstance>(entity) {
            element.frag_use = Some(Box::new(FragmentUse {
                key: instance.key.clone(),
                args: instance
                    .args
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
                slot_children: false,
            }));
            found.filled = true;
        }
        return;
    }

    // A `<for>` block's children are its row template, and the world's are the
    // rows built from it, so there is nothing to pair one to one.
    if element.tag == "for" {
        return;
    }

    let kids: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();

    for (child, child_entity) in element.children.iter_mut().zip(kids) {
        // A tag that disagrees means the walks have parted: an `<if>` branch
        // the app dropped, or a subtree a script rebuilt. Everything below is
        // then unpairable, so it is left alone.
        let matches = world
            .get::<LumenTag>(child_entity)
            .is_some_and(|tag| *tag.0 == child.tag);
        if !matches && child.frag_use.is_none() {
            continue;
        }
        resolve(child, child_entity, world, found);
    }
}

/// Whether anything under `element` stands in for a component.
fn holds_marker(element: &Element) -> bool {
    element.frag_use.is_some() || element.children.iter().any(holds_marker)
}
