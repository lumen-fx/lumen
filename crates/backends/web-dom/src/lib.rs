//! The browser as a Lumen render backend.
//!
//! A Lumen app running in a page is the same app: the same world, the same
//! spawner, the same reconcilers. What changes is where the result goes.
//! Instead of a scene of rects handed to a rasteriser, each entity is bound
//! to a real element, and the page's own CSS engine lays it out and paints
//! it.
//!
//! The document is already there when this starts. `lumenc web` prerendered
//! it from the same IR the runtime loads, so the first thing that happens is
//! not a build but an ADOPTION: the walk finds the element that carries each
//! node's path and binds the entity to it, writing nothing. An element the
//! walk cannot find is built from the entity instead. That second path is not
//! a fallback bolted on the side; it is what mounts a `<for>` row, an `<if>`
//! branch the reconciler just turned on, and every node of a page that was
//! emitted without a prerender at all.
//!
//! Events run the other way and stay in Lumen's hands. The browser is the
//! event SOURCE: one delegated listener per type on the app root turns a DOM
//! event into the typed message the desktop input pipeline would have
//! produced, and everything downstream (the widget primitives, the script
//! event driver) runs unchanged.

#![warn(missing_docs)]

mod events;
mod nodes;
mod project;

use bevy_ecs::prelude::*;
use lumen_core::prelude::{App, Plugin, TickStage};
use lumen_core::property_store::PropertyStore;
use web_sys::Element;

pub use nodes::{HydrationReport, NodeTable};

/// Install the browser backend on an app whose scene has already been
/// spawned.
///
/// `root` is the element the app lives in, which is the one the emitter gave
/// the page-root node path; a page that was emitted without a prerender
/// passes the container the app should fill. `root_entity` is what
/// [`lumen_scene::spawn::SpawnIntoWorld::spawn_into`] returned.
///
/// The plugin carries the two of them because a backend binds one scene to
/// one place in one document; there is nothing to look up and nothing to
/// guess.
pub struct WebDomPlugin {
    /// The element the app's root node is, or is built into.
    pub root: Element,
    /// The entity the app's root node was spawned as.
    pub root_entity: Entity,
}

impl Plugin for WebDomPlugin {
    fn build(self, app: &mut App) {
        let table = NodeTable::adopting(self.root, self.root_entity);
        app.world.insert_non_send(table);
        // A dialog the browser dismisses writes the signal it hangs off, so
        // the store has to be there before the first event is drained.
        app.world.init_resource::<PropertyStore>();

        // Reading the browser comes first, so a click this frame is a message
        // the app's own systems see this frame.
        app.add_systems(
            TickStage::Input,
            (events::drain_dom_events, events::drain_dismissed_dialogs),
        );
        // Projecting it comes last, after every system that could have
        // changed what the page should show.
        app.add_systems(
            TickStage::A11ySync,
            (
                nodes::bind_new_nodes,
                nodes::release_dead_nodes,
                project::project_text,
                project::project_classes,
                project::project_attributes,
                project::project_inline_style,
                project::project_visibility,
                project::project_control_state,
            )
                .chain(),
        );
    }
}

/// Start listening for the DOM events that drive the app.
///
/// Separate from the plugin because the listeners outlive the call that
/// installed them and the world does not own them: they are handed to the
/// browser, and they push onto a queue the app drains each tick.
///
/// `soft_navigation` is `[web] navigation = "soft"`: whether a click on a
/// same-page `<a href>` this crate spawned is kept from reaching the
/// browser's own navigation, so the in-app router swaps the page in place
/// instead. Passed straight through to the click listener, which is the only
/// place a browser event is still in hand to prevent.
///
/// # Errors
///
/// The browser refused a listener.
pub fn listen(root: &Element, soft_navigation: bool) -> Result<(), wasm_bindgen::JsValue> {
    events::listen(root, soft_navigation)
}
