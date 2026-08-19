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

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use lumen_core::components::LumenTag;
use lumen_html::contract::Seed;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::{Element, FragmentUse};
use lumen_prerender::{Budget, DenyDispatch, boot, settle};
use lumen_runtime::fragments::FragmentInstance;

/// How many times the tree is filled and re-inlined.
///
/// One round resolves the markers standing in the tree; a body spliced in by
/// that round can name a component of its own, which the next round resolves.
/// The bound is the deepest chain of components-inside-components a build
/// follows, and reaching it means the app is asking for something other than
/// what it looks like.
const MAX_ROUNDS: u32 = 16;

/// Replace every component marker in `compiled`'s tree with the body its call
/// produces.
///
/// The tree is left as it was where a marker cannot be resolved: a component
/// the loaded program cannot be called by name (see the export check in
/// `web_cli`), and a component inside a `<for>` row, whose body depends on the
/// row it is rendered for. Both are reported.
pub fn fill(compiled: &mut CompiledApp, page: &str, warnings: &mut Vec<String>) {
    // An app with no marker in it has nothing to run and nothing to wait for,
    // which is most apps; booting one to learn that is a cost with no answer
    // attached.
    if !holds_marker(&compiled.ir.root) {
        return;
    }

    for _ in 0..MAX_ROUNDS {
        let filled = round(compiled, page, warnings);
        if !filled {
            return;
        }
    }
    warnings.push(format!(
        "components are nested deeper than {MAX_ROUNDS} levels; the ones left are emitted as the \
         empty box the browser fills"
    ));
}

/// Boot the app, read what its markers became, and put those bodies in the
/// tree. Answers whether anything was filled.
fn round(compiled: &mut CompiledApp, page: &str, warnings: &mut Vec<String>) -> bool {
    let mut booted = boot(
        compiled,
        page,
        &Seed::new(),
        Arc::new(DenyDispatch::default()),
    );
    settle(&mut booted.app, Budget::default());

    let root = match root_entity(&mut booted.app) {
        Some(root) => root,
        None => {
            warnings.push(
                "the app built no tree to read its components out of, so they are emitted as the \
                 empty box the browser fills"
                    .to_string(),
            );
            return false;
        }
    };

    let mut found = Found::default();
    resolve(
        &mut compiled.ir.root,
        root,
        &booted.app.world,
        false,
        &mut found,
    );
    drop(booted);

    // A component that built nothing is left in the tree as its marker, which
    // is what the export check downstream reads: it holds the export list, so
    // it can say whether the name is callable at all and what to do about it.
    // Saying it here as well would say it twice.
    for name in &found.in_a_row {
        warnings.push(format!(
            "`{name}` is written inside a `<for>`, and what a component renders for one row is \
             not what it renders for another, so the browser fills it rather than the build"
        ));
    }
    if !found.filled {
        return false;
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
        return false;
    }
    true
}

/// What one round ran into.
#[derive(Default)]
struct Found {
    /// At least one marker took a body, so another round is worth running.
    filled: bool,
    /// Components written inside a `<for>`, by name, once each.
    in_a_row: Vec<String>,
}

impl Found {
    fn note(list: &mut Vec<String>, name: &str) {
        if !list.iter().any(|seen| seen == name) {
            list.push(name.to_string());
        }
    }
}

/// Walk the tree beside the world that was spawned from it, pointing each
/// marker at the fragment its call built.
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
    in_a_row: bool,
    found: &mut Found,
) {
    if let Some(use_site) = &element.frag_use {
        if in_a_row {
            let name = use_site.key.clone();
            Found::note(&mut found.in_a_row, &name);
            return;
        }
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
    let rows = element.tag == "for";
    let kids: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();
    if rows {
        for child in &mut element.children {
            mark_row_components(child, found);
        }
        return;
    }

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
        resolve(child, child_entity, world, in_a_row || rows, found);
    }
}

/// Report every component written inside a `<for>` row template.
fn mark_row_components(element: &Element, found: &mut Found) {
    if let Some(use_site) = &element.frag_use {
        Found::note(&mut found.in_a_row, &use_site.key);
    }
    for child in &element.children {
        mark_row_components(child, found);
    }
}

/// Whether anything under `element` stands in for a component.
fn holds_marker(element: &Element) -> bool {
    element.frag_use.is_some() || element.children.iter().any(holds_marker)
}

/// The app's root element, which is where both walks start.
fn root_entity(app: &mut lumen_core::app::App) -> Option<Entity> {
    let mut query = app
        .world
        .query_filtered::<Entity, bevy_ecs::prelude::Without<bevy_ecs::hierarchy::ChildOf>>();
    let roots: Vec<Entity> = query.iter(&app.world).collect();
    roots.into_iter().find(|entity| {
        app.world
            .get::<LumenTag>(*entity)
            .is_some_and(|tag| &*tag.0 == "root")
    })
}
