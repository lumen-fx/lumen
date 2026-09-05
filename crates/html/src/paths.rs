//! Walking an entity tree into the node paths a document names.
//!
//! The emitter numbers a page's elements as it writes them, and the spawner
//! builds entities from the same IR in the same order, so the same walk over
//! the entity tree recovers the same paths. Two callers need that walk: the
//! browser runtime, which binds each entity to the element carrying its path,
//! and the snapshot a prerender run reads, which records what the app wrote
//! onto each node. One walk, so the two cannot disagree about what a path
//! names.
//!
//! The tree is reached through closures rather than queries: this crate is
//! the vocabulary both halves share and knows nothing about the scene layer
//! that owns `<for>` blocks and hierarchy.

use lumen_core::prelude::Entity;

use crate::contract::NodePath;

/// One node the walk reached.
pub struct Visit<'a, C> {
    /// The entity the node stands for.
    pub entity: Entity,
    /// Its path from the page root.
    pub path: &'a NodePath,
    /// What the caller threaded down from the node's parent.
    pub parent: &'a C,
    /// The last sibling that produced a value of its own, which is where a
    /// caller that inserts something puts it.
    pub previous: Option<&'a C>,
    /// True when this node's own children are `<for>` rows.
    pub is_for: bool,
    /// How many children it has, rows included.
    pub children: usize,
}

/// Walk the tree under `root`, naming every node the way the document does.
///
/// `root_ctx` is what the root node's children see as their parent value;
/// the root itself is not visited, because a caller that has a root to walk
/// from already has whatever the root stands for.
///
/// A `<for>` block's children are its rows and a row's path says so;
/// everything else counts children. Every child consumes an index whether or
/// not it stands for an element, so an untagged entity does not renumber the
/// siblings after it. `visit` returns what its node passes down to its own
/// children, or `None` when there is nothing to descend with.
pub fn walk_nodes<'a, C>(
    root: Entity,
    root_ctx: C,
    children: impl Fn(Entity) -> &'a [Entity],
    is_for: impl Fn(Entity) -> bool,
    stands_for_element: impl Fn(Entity) -> bool,
    mut visit: impl FnMut(Visit<'_, C>) -> Option<C>,
) {
    walk(
        root,
        &NodePath::root(),
        &root_ctx,
        &children,
        &is_for,
        &stands_for_element,
        &mut visit,
    );
}

fn walk<'a, C, Kids, IsFor, Stands, V>(
    entity: Entity,
    path: &NodePath,
    ctx: &C,
    children: &Kids,
    is_for: &IsFor,
    stands_for_element: &Stands,
    visit: &mut V,
) where
    Kids: Fn(Entity) -> &'a [Entity],
    IsFor: Fn(Entity) -> bool,
    Stands: Fn(Entity) -> bool,
    V: FnMut(Visit<'_, C>) -> Option<C>,
{
    let rows = is_for(entity);
    let mut previous: Option<C> = None;
    for (index, child) in children(entity).iter().copied().enumerate() {
        let index = index as u32;
        let child_path = if rows {
            path.row(index)
        } else {
            path.child(index)
        };
        // An entity that stands for no element still counts: dropping it
        // would renumber every sibling after it.
        if !stands_for_element(child) {
            continue;
        }
        let Some(child_ctx) = visit(Visit {
            entity: child,
            path: &child_path,
            parent: ctx,
            previous: previous.as_ref(),
            is_for: is_for(child),
            children: children(child).len(),
        }) else {
            continue;
        };
        walk(
            child,
            &child_path,
            &child_ctx,
            children,
            is_for,
            stands_for_element,
            visit,
        );
        previous = Some(child_ctx);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lumen_core::prelude::World;

    use super::*;

    /// A tree of spawned entities, so the walk can be exercised without a
    /// scene to build one.
    struct Tree {
        ids: Vec<Entity>,
        kids: HashMap<Entity, Vec<Entity>>,
        rows: Vec<Entity>,
        untagged: Vec<Entity>,
    }

    fn tree(kids: &[(usize, &[usize])], rows: &[usize], untagged: &[usize]) -> Tree {
        let mut world = World::new();
        let ids: Vec<Entity> = (0..8).map(|_| world.spawn_empty().id()).collect();
        Tree {
            kids: kids
                .iter()
                .map(|(parent, children)| {
                    (ids[*parent], children.iter().map(|c| ids[*c]).collect())
                })
                .collect(),
            rows: rows.iter().map(|r| ids[*r]).collect(),
            untagged: untagged.iter().map(|u| ids[*u]).collect(),
            ids,
        }
    }

    impl Tree {
        fn walk(&self) -> Vec<String> {
            let mut seen = Vec::new();
            walk_nodes(
                self.ids[0],
                (),
                |e| self.kids.get(&e).map(Vec::as_slice).unwrap_or(&[]),
                |e| self.rows.contains(&e),
                |e| !self.untagged.contains(&e),
                |visit| {
                    seen.push(visit.path.to_string());
                    Some(())
                },
            );
            seen
        }
    }

    #[test]
    fn children_are_numbered_by_position() {
        let tree = tree(&[(0, &[1, 2]), (2, &[3])], &[], &[]);
        assert_eq!(tree.walk(), ["0.0", "0.1", "0.1.0"]);
    }

    #[test]
    fn a_for_block_numbers_its_children_as_rows() {
        let tree = tree(&[(0, &[1]), (1, &[2, 3]), (2, &[4])], &[1], &[]);
        assert_eq!(tree.walk(), ["0.0", "0.0::0", "0.0::0.0", "0.0::1"]);
    }

    #[test]
    fn an_entity_standing_for_no_element_still_takes_its_index() {
        let tree = tree(&[(0, &[1, 2, 3])], &[], &[2]);
        assert_eq!(tree.walk(), ["0.0", "0.2"]);
    }
}
