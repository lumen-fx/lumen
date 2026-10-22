//! Keeping a composed widget's parts in step with its children.
//!
//! A `<split>` has a gutter between every two of its panes. The parser
//! writes them for markup, but a split is also built by a script through the
//! DOM API and by a module's own systems, and its children change at run
//! time. [`install`] adds the system that keeps the parts right however the
//! children got there, so every way of building the widget builds the same
//! widget. It runs when a widget appears or its children change and costs
//! nothing on a tick where neither happened.

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::*;
use lumen_core::components::{DirtyLayout, LumenClasses, StyleDirty};
use lumen_core::prelude::{App, TickStage};
use lumen_ir::layout_ir::{SPLIT_PANE_CLASS, split_gutter};
use lumen_primitives::{Split, SplitGutter, SplitPane, SplitSet};

use crate::spawn::{Placeholders, spawn_subtree};

/// Add the systems that keep composed widgets' parts in step with their
/// children. They run after the DOM applier, so a script's edit is in place,
/// and before the widgets' own systems, so a new part takes input on the
/// tick it appears. A host with a cascade of its own orders its restyle
/// after [`keep_widget_parts`], which marks what it built.
pub fn install(app: &mut App) {
    app.add_systems(
        TickStage::Systems,
        keep_widget_parts
            .after(crate::dom::apply_dom_commands)
            .before(SplitSet)
            .run_if(any_with_component::<Split>),
    );
}

/// Queue the parts of every split that appeared or whose children changed.
#[allow(clippy::type_complexity)]
pub fn keep_widget_parts(
    splits: Query<Entity, (With<Split>, Or<(Added<Split>, Changed<Children>)>)>,
    mut commands: Commands,
) {
    for split in &splits {
        commands.queue(move |world: &mut World| keep_split_gutters(world, split));
    }
}

/// When `split` is a `<split>`, put one gutter between every two of its
/// children and none anywhere else, as the parser does for one read from
/// markup: a gutter it lacks is built, one it no longer needs goes, and the
/// rest move into place. Each child that is not a gutter is a pane, marked
/// [`SplitPane`] and classed `split-pane`. What was built or reclassed is
/// marked to restyle.
pub fn keep_split_gutters(world: &mut World, split: Entity) {
    if world.get::<Split>(split).is_none() {
        return;
    }
    let children: Vec<Entity> = world
        .get::<Children>(split)
        .map(|c| c.to_vec())
        .unwrap_or_default();
    let (mut gutters, panes): (Vec<Entity>, Vec<Entity>) = children
        .iter()
        .partition(|&&e| world.get::<SplitGutter>(e).is_some());
    for &pane in &panes {
        if world.get::<SplitPane>(pane).is_some() {
            continue;
        }
        let mut e = world.entity_mut(pane);
        let mut classes = e.take::<LumenClasses>().unwrap_or_default();
        let reclassed = !classes.has(SPLIT_PANE_CLASS);
        if reclassed {
            classes.0.push(SPLIT_PANE_CLASS.into());
        }
        e.insert((classes, SplitPane));
        if reclassed {
            e.insert(StyleDirty);
        }
    }
    let wanted = panes.len().saturating_sub(1);
    for extra in gutters.split_off(wanted.min(gutters.len())) {
        world.entity_mut(extra).despawn();
    }
    while gutters.len() < wanted {
        let gutter = spawn_subtree(world, &split_gutter(), Some(split), Placeholders::Resolved);
        world.entity_mut(gutter).insert(StyleDirty);
        gutters.push(gutter);
    }
    let mut order = Vec::with_capacity(panes.len() + wanted);
    for (i, &pane) in panes.iter().enumerate() {
        order.push(pane);
        if let Some(&gutter) = gutters.get(i) {
            order.push(gutter);
        }
    }
    let now: Vec<Entity> = world
        .get::<Children>(split)
        .map(|c| c.to_vec())
        .unwrap_or_default();
    if order != now {
        world.entity_mut(split).replace_children(&order);
        for &gutter in &gutters {
            world.entity_mut(gutter).insert(DirtyLayout);
        }
    }
}
