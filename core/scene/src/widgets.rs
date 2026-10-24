//! Keeping a composed widget's parts in step with its children.
//!
//! A `<split>` has a gutter between every two of its panes, and a `<tabs>`
//! a strip with one button per tab. The parser writes those parts for
//! markup, but both widgets are also built by a script through the DOM API
//! and by a module's own systems, and their children change at run time.
//! [`install`] adds the systems that keep the parts right however the
//! children got there, so every way of building a widget builds the same
//! widget. They run when a widget appears or its children change and cost
//! nothing on a tick where neither happened.
//!
//! A tab written in markup also keeps its content unmounted until the tab
//! first shows, as an `<if mode="hide">` body does; [`mount_shown_tabs`]
//! mounts it.

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use lumen_core::components::{
    DirtyLayout, Disabled, LumenAttributes, LumenClasses, StyleDirty, TextContent, Visible,
};
use lumen_core::prelude::{App, TickStage};
use lumen_ir::layout_ir::{
    Element, SPLIT_PANE_CLASS, TabSpec, split_gutter, tab_button, tab_strip,
};
use lumen_primitives::{
    Split, SplitGutter, SplitPane, SplitSet, Tab, TabStrip, TabStripButton, Tabs, TabsSet,
    tab_label, tab_name,
};

use crate::spawn::{BodyChild, Placeholders, spawn_body_child, spawn_subtree};

/// A `<tab>`'s content as the markup wrote it, waiting for the tab to show
/// for the first time.
#[derive(Component, Clone, Debug)]
pub struct PendingTabBody(pub Vec<Element>);

/// Add the systems that keep composed widgets' parts in step with their
/// children, and mount a tab's content when it first shows. The parts are
/// kept after the DOM applier, so a script's edit is in place, and before
/// the widgets' own systems, so a new part takes input on the tick it
/// appears. A host with a cascade of its own orders its restyle after
/// [`keep_widget_parts`] and [`mount_shown_tabs`], which mark what they
/// built.
pub fn install(app: &mut App) {
    app.add_systems(
        TickStage::Systems,
        keep_widget_parts
            .after(crate::dom::apply_dom_commands)
            .before(SplitSet)
            .before(TabsSet)
            .run_if(any_with_component::<Split>.or_else(any_with_component::<Tabs>)),
    );
    app.add_systems(
        TickStage::Systems,
        mount_shown_tabs
            .after(TabsSet)
            .run_if(any_with_component::<PendingTabBody>),
    );
}

/// Queue the parts of every split and every `<tabs>` that appeared or whose
/// children changed, and of every `<tabs>` one of whose tabs was renamed,
/// relabelled, or enabled or disabled.
#[allow(clippy::type_complexity)]
pub fn keep_widget_parts(
    splits: Query<Entity, (With<Split>, Or<(Added<Split>, Changed<Children>)>)>,
    sets: Query<Entity, (With<Tabs>, Or<(Added<Tabs>, Changed<Children>)>)>,
    changed_tabs: Query<&ChildOf, (With<Tab>, Or<(Changed<LumenAttributes>, Added<Disabled>)>)>,
    mut enabled: RemovedComponents<Disabled>,
    parents: Query<&ChildOf, With<Tab>>,
    mut commands: Commands,
) {
    for split in &splits {
        commands.queue(move |world: &mut World| keep_split_gutters(world, split));
    }
    let mut touched: Vec<Entity> = Vec::new();
    let renamed = changed_tabs.iter().map(ChildOf::parent);
    let enabled = enabled
        .read()
        .filter_map(|e| parents.get(e).ok().map(ChildOf::parent));
    for set in sets.iter().chain(renamed).chain(enabled) {
        if !touched.contains(&set) {
            touched.push(set);
        }
    }
    for set in touched {
        commands.queue(move |world: &mut World| keep_tab_strip(world, set));
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
    let children = children_of(world, split);
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
    if order != children_of(world, split) {
        world.entity_mut(split).replace_children(&order);
        for &gutter in &gutters {
            world.entity_mut(gutter).insert(DirtyLayout);
        }
    }
}

/// When `set` is a `<tabs>`, give it a strip as its first child holding one
/// button per `<tab>` child, in the tabs' order, each captioned with its
/// tab's label and disabled with it. A button the parser wrote is paired
/// with the tab in its place; a button whose tab left goes, and one is built
/// for a tab that has none. Anything else a strip holds stays after the
/// buttons. What was built is marked to restyle.
pub fn keep_tab_strip(world: &mut World, set: Entity) {
    if world.get::<Tabs>(set).is_none() {
        return;
    }
    let children = children_of(world, set);
    let tabs: Vec<Entity> = children
        .iter()
        .copied()
        .filter(|&e| world.get::<Tab>(e).is_some())
        .collect();
    let strip = match children
        .iter()
        .copied()
        .find(|&e| world.get::<TabStrip>(e).is_some())
    {
        Some(strip) => strip,
        None => {
            let strip = spawn_subtree(world, &tab_strip(), Some(set), Placeholders::Resolved);
            world.entity_mut(strip).insert(StyleDirty);
            strip
        }
    };
    if children_of(world, set).first() != Some(&strip) {
        world.entity_mut(set).insert_children(0, &[strip]);
    }

    let strip_children = children_of(world, strip);
    let (buttons, others): (Vec<Entity>, Vec<Entity>) = strip_children
        .iter()
        .partition(|&&e| world.get::<TabStripButton>(e).is_some());
    let mut paired: Vec<Option<Entity>> = vec![None; tabs.len()];
    let mut loose: Vec<Entity> = Vec::new();
    for &button in &buttons {
        let tab = world.get::<TabStripButton>(button).map(|b| b.tab);
        match tab.and_then(|t| tabs.iter().position(|&x| x == t)) {
            Some(i) if paired[i].is_none() => paired[i] = Some(button),
            _ => loose.push(button),
        }
    }
    // A button the parser wrote names no tab yet; it stands for the tab in
    // its place, which is where the parser put it.
    let mut loose = loose.into_iter();
    for (i, slot) in paired.iter_mut().enumerate() {
        if slot.is_some() {
            continue;
        }
        if let Some(button) = loose.next() {
            *slot = Some(button);
        } else {
            let attrs = world.get::<LumenAttributes>(tabs[i]);
            let spec = TabSpec::new(
                tab_name(attrs).unwrap_or_default(),
                Some(tab_label(attrs).to_string()),
            );
            let disabled = world.get::<Disabled>(tabs[i]).is_some();
            let button = spawn_subtree(
                world,
                &tab_button(&spec, disabled),
                Some(strip),
                Placeholders::Resolved,
            );
            world.entity_mut(button).insert(StyleDirty);
            *slot = Some(button);
        }
    }
    for button in loose {
        world.entity_mut(button).despawn();
    }
    let mut order: Vec<Entity> = Vec::with_capacity(tabs.len() + others.len());
    for (&tab, button) in tabs.iter().zip(&paired) {
        let Some(button) = *button else { continue };
        let label = tab_label(world.get::<LumenAttributes>(tab)).to_string();
        let disabled = world.get::<Disabled>(tab).is_some();
        let mut e = world.entity_mut(button);
        if e.get::<TabStripButton>().map(|b| b.tab) != Some(tab) {
            e.insert(TabStripButton { tab });
        }
        if e.get::<TextContent>().map(|t| t.0.as_str()) != Some(label.as_str()) {
            e.insert(TextContent(label));
            e.insert(DirtyLayout);
        }
        if disabled != e.contains::<Disabled>() {
            if disabled {
                e.insert(Disabled);
            } else {
                e.remove::<Disabled>();
            }
            e.insert(StyleDirty);
        }
        order.push(button);
    }
    order.extend(others);
    if order != children_of(world, strip) {
        world.entity_mut(strip).replace_children(&order);
        world.entity_mut(strip).insert(DirtyLayout);
    }
}

/// Mount the content of every markup `<tab>` that shows for the first time.
/// The content mounts the way an `<if>` body does: marked to restyle, so
/// it takes the color scheme and media in force rather than the ones the
/// app started with, and fading in when its styles ask for a transition.
pub fn mount_shown_tabs(
    pending: Query<(Entity, &PendingTabBody, &Visible), Changed<Visible>>,
    mut commands: Commands,
) {
    for (tab, body, visible) in &pending {
        if !visible.0 {
            continue;
        }
        for el in &body.0 {
            spawn_body_child(
                &mut commands,
                el,
                tab,
                Placeholders::Resolved,
                BodyChild::Restyled,
            );
        }
        commands.entity(tab).remove::<PendingTabBody>();
    }
}

fn children_of(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.to_vec())
        .unwrap_or_default()
}
