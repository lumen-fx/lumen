//! The state an app is in, read out as the state a page is rendered with.
//!
//! A document is produced from the state an app holds, wherever that state
//! came from: values an author declared, or values the app itself wrote while
//! it ran. This turns a running app into the two forms a document needs, the
//! text the markup is rendered with and the typed seed the browser adopts it
//! with, so that a build and a server produce a page the same way.
//!
//! State is not all in the stores. `set_class`, `set_attr` and `set_style`
//! write onto one node rather than onto a signal, so the settled scene is
//! read too, along the walk that names a node the way the document does.

use std::collections::BTreeMap;

use lumen_core::components::{InlineStyle, LumenAttributes, LumenClasses, LumenTag, TextContent};
use lumen_core::prelude::{Children, Entity, World};
use lumen_core::property_store::{PropertyKey, PropertyStore};
use lumen_core::signals::ArraySignals;
use lumen_html::contract::{NodePath, Seed, SeedValue};
use lumen_html::paths::walk_nodes;
use lumen_scene::spawn::{DocumentRoot, ForMarker};

use crate::spec::SignalEnv;

/// One app's signal state, in both the forms a page needs.
///
/// [`Self::signals`] is what the markup is rendered against and
/// [`Self::seed`] is what the browser restores before its first reconcile.
/// They come out of one read so the two cannot disagree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The state the document is rendered with.
    pub signals: SignalEnv,
    /// The same state, typed, for the runtime that adopts the document.
    pub seed: Seed,
    /// What the app wrote onto single nodes, by node path. Kept whole here;
    /// what a document has to say about a node is the difference from what
    /// its markup already says, and only the emitter holds both.
    pub nodes: BTreeMap<String, NodeState>,
    /// Values that have no place in a document, by name. A caller reports
    /// these: the page renders without them.
    pub skipped: Vec<String>,
}

/// What one node of the settled scene says about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeState {
    /// The node's IR tag, which is how the emitter tells whether the path
    /// still names the same node on both sides.
    pub tag: String,
    /// Its class list, in order.
    pub classes: Vec<String>,
    /// Its attributes, sorted: the component behind them is a hash map, and
    /// a build has to write the same bytes twice.
    pub attrs: BTreeMap<String, String>,
    /// Its inline style, in the order it was written.
    pub style: Vec<(String, String)>,
    /// Its text, when it has any.
    pub text: Option<String>,
}

/// Read the signal state out of a running app's stores.
///
/// Globals are ordered by name and rows keep the order the app put them in,
/// so the same state always produces the same document.
///
/// A global's text is the text the app itself reads out of the store, which
/// is what keeps a rendered page and the app that adopts it showing the same
/// thing. Its seed value is the typed one, because the text of a color is
/// empty and a page that seeded that would hand the browser an empty color.
/// The two values a store can hold that a document cannot carry, a vector and
/// a live Rust value, are named in [`State::skipped`], and so is a property
/// written on one node: a document names a node by its path, and nothing says
/// which entity a path will be on the next run.
pub fn state_of(world: &World) -> State {
    let store = world.resource::<PropertyStore>();
    let arrays = world.resource::<ArraySignals>();
    let mut names: Vec<&str> = Vec::new();
    let mut entity_cells: Vec<String> = Vec::new();
    for (key, _) in store.iter() {
        match key {
            PropertyKey::Global(name) => names.push(&**name),
            PropertyKey::Entity(_, name) => {
                entity_cells.push(format!("`{name}`: a property written on one node"));
            }
        }
    }
    names.sort_unstable();
    entity_cells.sort();
    entity_cells.dedup();

    let mut state = State {
        skipped: entity_cells,
        ..State::default()
    };
    for name in names {
        let Some(value) = store.get(&PropertyKey::global(name)) else {
            continue;
        };
        let text = store.get_global_str(name).unwrap_or_default();
        state.signals = state.signals.with_global(name, &*text);
        match SeedValue::try_from(value) {
            Ok(seeded) => {
                state.seed.globals.insert(name.to_string(), seeded);
            }
            Err(unsupported) => state.skipped.push(format!("`{name}`: {unsupported}")),
        }
    }

    let mut array_names: Vec<&str> = arrays.0.keys().map(String::as_str).collect();
    array_names.sort_unstable();
    for name in array_names {
        let Some(rows) = arrays.get(name) else {
            continue;
        };
        state.signals = state.signals.with_array(name, rows.to_vec());
        state.seed.arrays.insert(
            name.to_string(),
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|(field, value)| (field.clone(), value.clone()))
                        .collect()
                })
                .collect(),
        );
    }

    state.nodes = nodes_of(world);
    state
}

/// Read what the app wrote onto each node of the settled scene.
///
/// The walk is the one the browser runtime binds by, so a path here names the
/// element that path names in the document. A world with no scene in it - a
/// store read on its own - has no nodes to report.
fn nodes_of(world: &World) -> BTreeMap<String, NodeState> {
    let mut nodes = BTreeMap::new();
    let Some(root) = world.get_resource::<DocumentRoot>().map(|r| r.0) else {
        return nodes;
    };
    // The root is the one node the walk does not visit: it is where the walk
    // starts from, and `set_root_class` writes onto it.
    if let Some(state) = node_state(world, root) {
        nodes.insert(NodePath::root().to_string(), state);
    }
    walk_nodes(
        root,
        (),
        |entity| {
            world
                .get::<Children>(entity)
                .map(|kids| &**kids)
                .unwrap_or(&[])
        },
        |entity| world.get::<ForMarker>(entity).is_some(),
        |entity| world.get::<LumenTag>(entity).is_some(),
        |visit| {
            nodes.insert(visit.path.to_string(), node_state(world, visit.entity)?);
            Some(())
        },
    );
    nodes
}

/// Everything one entity says about the node it stands for.
fn node_state(world: &World, entity: Entity) -> Option<NodeState> {
    let tag = world.get::<LumenTag>(entity)?;
    Some(NodeState {
        tag: tag.0.to_string(),
        classes: world
            .get::<LumenClasses>(entity)
            .map(|c| c.0.iter().map(|class| class.to_string()).collect())
            .unwrap_or_default(),
        attrs: world
            .get::<LumenAttributes>(entity)
            .map(|a| {
                a.0.iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        style: world
            .get::<InlineStyle>(entity)
            .map(|style| style.0.clone())
            .unwrap_or_default(),
        text: world.get::<TextContent>(entity).map(|text| text.0.clone()),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lumen_core::components::Color;
    use lumen_core::property_store::PropertyValue;
    use lumen_core::signals::ArrayItem;

    use super::*;

    fn world_with(store: PropertyStore, arrays: ArraySignals) -> World {
        let mut world = World::new();
        world.insert_resource(store);
        world.insert_resource(arrays);
        world
    }

    fn store_with(values: &[(&str, PropertyValue)]) -> PropertyStore {
        let mut store = PropertyStore::default();
        for (name, value) in values {
            store.set(PropertyKey::global(*name), value.clone());
        }
        store
    }

    fn globals(values: &[(&str, PropertyValue)]) -> World {
        world_with(store_with(values), ArraySignals::default())
    }

    #[test]
    fn a_global_reaches_both_the_markup_and_the_seed() {
        let world = globals(&[
            ("count", PropertyValue::I64(3)),
            ("open", PropertyValue::Bool(true)),
        ]);
        let state = state_of(&world);
        assert_eq!(state.signals.global("count"), Some("3"));
        assert!(state.signals.is_truthy("open"));
        assert_eq!(state.seed.globals["count"], SeedValue::I64(3));
        assert_eq!(state.seed.globals["open"], SeedValue::Bool(true));
        assert!(state.skipped.is_empty());
    }

    #[test]
    fn a_color_keeps_its_type_in_the_seed() {
        let world = globals(&[(
            "accent",
            PropertyValue::Color(Color::rgba(1.0, 0.0, 0.0, 1.0)),
        )]);
        let state = state_of(&world);
        assert_eq!(
            state.seed.globals["accent"],
            SeedValue::Color([1.0, 0.0, 0.0, 1.0])
        );
        assert!(state.skipped.is_empty());
    }

    #[test]
    fn a_value_a_document_cannot_carry_is_named() {
        let world = globals(&[("handle", PropertyValue::Custom(Arc::new(7u32)))]);
        let state = state_of(&world);
        assert!(state.seed.globals.is_empty());
        assert_eq!(state.skipped.len(), 1);
        assert!(state.skipped[0].starts_with("`handle`: "));
    }

    #[test]
    fn a_property_written_on_a_node_is_named() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut store = PropertyStore::default();
        store.set(
            PropertyKey::entity(entity, "text"),
            PropertyValue::Str(Arc::from("hello")),
        );
        world.insert_resource(store);
        world.init_resource::<ArraySignals>();
        let state = state_of(&world);
        assert_eq!(state.skipped, ["`text`: a property written on one node"]);
    }

    #[test]
    fn rows_reach_both_forms_in_order() {
        let mut arrays = ArraySignals::default();
        let row = |id: &str| ArrayItem::from([("id".to_string(), id.to_string())]);
        arrays.set("todos", vec![row("1"), row("2")]);
        let world = world_with(PropertyStore::default(), arrays);
        let state = state_of(&world);
        assert_eq!(state.signals.rows("todos").map(<[_]>::len), Some(2));
        assert_eq!(state.seed.arrays["todos"][0]["id"], "1");
        assert_eq!(state.seed.arrays["todos"][1]["id"], "2");
    }

    #[test]
    fn the_same_state_reads_the_same_way_twice() {
        let mut arrays = ArraySignals::default();
        arrays.set("rows", vec![ArrayItem::new()]);
        let world = world_with(
            store_with(&[
                ("b", PropertyValue::Str(Arc::from("two"))),
                ("a", PropertyValue::F64(1.5)),
            ]),
            arrays,
        );
        assert_eq!(state_of(&world), state_of(&world));
    }

    #[test]
    fn what_a_node_wears_is_read_off_the_scene() {
        let mut world = World::new();
        world.init_resource::<PropertyStore>();
        world.init_resource::<ArraySignals>();
        let root = world.spawn(LumenTag("root".into())).id();
        let mut style = InlineStyle::default();
        style.set("bg", "#ff0000");
        let tile = world
            .spawn((
                LumenTag("label".into()),
                LumenClasses::from(vec!["lit".to_string()]),
                style,
                TextContent("on".to_string()),
            ))
            .id();
        world.entity_mut(root).add_child(tile);
        world.insert_resource(DocumentRoot(root));

        let state = state_of(&world);
        assert_eq!(state.nodes["0"].tag, "root");
        let tile = &state.nodes["0.0"];
        assert_eq!(tile.classes, ["lit"]);
        assert_eq!(tile.style, [("bg".to_string(), "#ff0000".to_string())]);
        assert_eq!(tile.text.as_deref(), Some("on"));
    }
}
