//! What the components inside a `<for>` row rendered, read off a settled app.
//!
//! A component written outside a `<for>` renders one body, so the build puts
//! that body in the tree and every document carries it. One written inside a
//! `<for>` renders a body per row, and the tree holds the row template rather
//! than the rows, so there is nowhere in it for those bodies to go. They are
//! read out here instead, keyed by the node path of the element that stands
//! where each one belongs, and the emitter writes them into the rows it is
//! already writing.
//!
//! The walk numbers nodes the way [`NodePath`] does and the way the browser's
//! DOM binder does: children counted under an ordinary element, rows counted
//! under a `<for>` block, and an entity standing for no element counted but
//! passed over. A path that disagreed with the binder's would be a document
//! the runtime rebuilds rather than adopts, which is the thing this is for.
//!
//! The body itself comes from the app's fragment table, instantiated with the
//! arguments the call was made with, rather than from a walk back out of the
//! spawned entities. Same rule the compiler's own component fill follows, and
//! the same reason: an instantiation is what the browser builds from the same
//! table, and a walk back out would have to reconstruct every attribute the
//! spawner consumed.

use lumen_core::app::App;
use lumen_core::components::LumenTag;
use lumen_core::prelude::{ChildOf, Children, Entity, Without, World};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::NodePath;
use lumen_ir::interpolate::{Scope, substitute_element};
use lumen_ir::layout_ir::Element;
use lumen_scene::fragments::{FragmentInstance, FragmentLibrary, instance_body};
use lumen_scene::spawn::ForMarker;
use lumen_web::RowFills;

/// Read every `<for>` row's filled components off `app`.
///
/// Empty for an app with no component inside a `<for>`, which is most of
/// them, and for one whose components have not been called yet: this reads a
/// settled world, not a booted one.
pub fn row_fills(app: &mut App) -> RowFills {
    let mut fills = RowFills::default();
    let Some(root) = root_entity(app) else {
        return fills;
    };
    // The table is a handle on the artifact's own, so holding it leaves the
    // world free for the reads below.
    let library = app
        .world
        .get_resource::<FragmentLibrary>()
        .cloned()
        .unwrap_or_default();
    read(
        &app.world,
        root,
        &NodePath::root(),
        None,
        false,
        &library,
        &mut fills,
    );
    fills
}

/// The app's root element, which is where a walk over the world starts.
pub fn root_entity(app: &mut App) -> Option<Entity> {
    let mut query = app.world.query_filtered::<Entity, Without<ChildOf>>();
    let roots: Vec<Entity> = query.iter(&app.world).collect();
    roots.into_iter().find(|entity| {
        app.world
            .get::<LumenTag>(*entity)
            .is_some_and(|tag| &*tag.0 == "root")
    })
}

/// Walk `entity` and everything under it, recording each block it meets and
/// each component body a row of one was filled with.
///
/// `template` is the row-template element `entity` was spawned from, while
/// the walk is inside a row and still in step with it. It is what says which
/// component stands at a position, because a filled marker is gone from the
/// world by the time this reads it: what stands there is the body the call
/// built.
fn read(
    world: &World,
    entity: Entity,
    path: &NodePath,
    template: Option<&Element>,
    in_a_row: bool,
    library: &FragmentLibrary,
    fills: &mut RowFills,
) {
    // Outside a row the tree already carries the body: the build inlined it
    // there, and a document disagreeing with the artifact is a subtree the
    // browser rebuilds on load.
    let mut body_here = false;
    if in_a_row
        && let Some(instance) = world.get::<FragmentInstance>(entity)
        && let Some(body) = library.get(&instance.key).and_then(|fragment| {
            instance_body(fragment).ok().map(|body| {
                let empty = PropertyStore::default();
                let store = world.get_resource::<PropertyStore>().unwrap_or(&empty);
                substitute_element(body, &Scope::new(store).with_args(&instance.args))
            })
        })
    {
        fills.with_body(path.to_string(), body);
        body_here = true;
    }
    // A marker in the template is a call the run made for this row, and what
    // stands here says how it went: the body it returned, or the marker
    // itself where it returned nothing.
    if let Some(use_site) = template.and_then(|el| el.frag_use.as_ref()) {
        fills.with_component(use_site.key.clone(), body_here);
    }

    let rows = world.get::<ForMarker>(entity);
    let kids: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();
    if let Some(marker) = rows {
        // The count is the list's, not the child list's: a block whose
        // template is two elements holds two children per row.
        fills.with_block(
            path.to_string(),
            marker.array_name.clone(),
            marker.cached_keys.len(),
        );
    }
    for (index, child) in kids.into_iter().enumerate() {
        let slot = index as u32;
        let (child_path, child_template) = match rows {
            // A block's children are its rows laid end to end, so which
            // template element a child came from is its place within the row.
            // The same numbering the emitter writes the rows back at.
            Some(marker) if !marker.body.is_empty() => {
                (path.row(slot), marker.body.get(index % marker.body.len()))
            }
            Some(_) => (path.row(slot), None),
            // Below a body root the walk is inside what the call built, which
            // the template says nothing about.
            None if body_here => (path.child(slot), None),
            None => (
                path.child(slot),
                template.and_then(|el| el.children.get(index)),
            ),
        };
        // An entity standing for no element still counts, the way it does in
        // the browser: dropping it would renumber every sibling after it.
        if world.get::<LumenTag>(child).is_none() {
            continue;
        }
        // A tag that disagrees means the walks have parted: an `<if>` branch
        // the app dropped, or a subtree a script rebuilt. A marker is the one
        // place they disagree by design, because what stands there is the body
        // the call returned.
        let child_template = child_template.filter(|el| {
            el.frag_use.is_some()
                || world
                    .get::<LumenTag>(child)
                    .is_some_and(|tag| *tag.0 == el.tag)
        });
        read(
            world,
            child,
            &child_path,
            child_template,
            in_a_row || rows.is_some(),
            library,
            fills,
        );
    }
}
