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
//! The paths come from [`walk_nodes`], the walk the document, the DOM binder
//! and the snapshot are all numbered by. A path that disagreed with the
//! binder's would be a document the runtime rebuilds rather than adopts,
//! which is the thing this is for.
//!
//! The body itself comes from the app's fragment table, instantiated with the
//! arguments the call was made with, rather than from a walk back out of the
//! spawned entities. Same rule the compiler's own component fill follows, and
//! the same reason: an instantiation is what the browser builds from the same
//! table, and a walk back out would have to reconstruct every attribute the
//! spawner consumed.

use lumen_core::app::App;
use lumen_core::components::LumenTag;
use lumen_core::prelude::{Children, Entity};
use lumen_core::property_store::PropertyStore;
use lumen_html::paths::walk_nodes;
use lumen_ir::interpolate::{Scope, substitute_element};
use lumen_ir::layout_ir::Element;
use lumen_scene::fragments::{FragmentInstance, FragmentLibrary, instance_body};
use lumen_scene::spawn::{DocumentRoot, ForMarker};
use lumen_web::RowFills;

/// What the walk carries down to a node's children.
///
/// `template` is the row-template element the node was spawned from, while
/// the walk is inside a row and still in step with it. It is what says which
/// component stands at a position, because a filled marker is gone from the
/// world by the time this reads it: what stands there is the body the call
/// built.
#[derive(Clone, Copy, Default)]
struct Row<'a> {
    /// The template element this node was spawned from.
    template: Option<&'a Element>,
    /// The row template of the `<for>` block this node is, when it is one.
    /// Its children are its rows, laid end to end.
    rows: Option<&'a [Element]>,
    /// Whether the node stands inside a `<for>` row.
    in_a_row: bool,
    /// Whether the node is the root of a body a call built, below which the
    /// template says nothing.
    body: bool,
}

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
    let world = &app.world;
    let empty = PropertyStore::default();
    let store = world.get_resource::<PropertyStore>().unwrap_or(&empty);
    walk_nodes(
        root,
        Row::default(),
        |entity| {
            world
                .get::<Children>(entity)
                .map(|kids| &**kids)
                .unwrap_or(&[])
        },
        |entity| world.get::<ForMarker>(entity).is_some(),
        |entity| world.get::<LumenTag>(entity).is_some(),
        |visit| {
            let parent = *visit.parent;
            let index = visit.index as usize;
            let template = match parent.rows {
                // A block's children are its rows laid end to end, so which
                // template element a child came from is its place within the
                // row.
                Some(body) if !body.is_empty() => body.get(index % body.len()),
                Some(_) => None,
                // Below a body root the walk is inside what the call built,
                // which the template says nothing about.
                None if parent.body => None,
                None => parent.template.and_then(|el| el.children.get(index)),
            };
            // A tag that disagrees means the walks have parted: an `<if>`
            // branch the app dropped, or a subtree a script rebuilt. A marker
            // is the one place they disagree by design, because what stands
            // there is the body the call returned.
            let template = template.filter(|el| {
                el.frag_use.is_some()
                    || world
                        .get::<LumenTag>(visit.entity)
                        .is_some_and(|tag| *tag.0 == el.tag)
            });
            let in_a_row = parent.in_a_row || parent.rows.is_some();

            // Outside a row the tree already carries the body: the build
            // inlined it there, and a document disagreeing with the artifact
            // is a subtree the browser rebuilds on load.
            let mut body = false;
            if in_a_row
                && let Some(instance) = world.get::<FragmentInstance>(visit.entity)
                && let Some(built) = library.get(&instance.key).and_then(|fragment| {
                    instance_body(fragment).ok().map(|body| {
                        substitute_element(body, &Scope::new(store).with_args(&instance.args))
                    })
                })
            {
                fills.with_body(visit.path.to_string(), built);
                body = true;
            }
            // A marker in the template is a call the run made for this row,
            // and what stands here says how it went: the body it returned, or
            // the marker itself where it returned nothing.
            if let Some(use_site) = template.and_then(|el| el.frag_use.as_ref()) {
                fills.with_component(use_site.key.clone(), body);
            }
            let rows = world.get::<ForMarker>(visit.entity).map(|marker| {
                // The count is the list's, not the child list's: a block whose
                // template is two elements holds two children per row.
                fills.with_block(
                    visit.path.to_string(),
                    marker.array_name.clone(),
                    marker.cached_keys.len(),
                );
                &*marker.body
            });
            Some(Row {
                template,
                rows,
                in_a_row,
                body,
            })
        },
    );
    fills
}

/// The app's root element, which is where a walk over the world starts.
pub fn root_entity(app: &App) -> Option<Entity> {
    app.world.get_resource::<DocumentRoot>().map(|root| root.0)
}
