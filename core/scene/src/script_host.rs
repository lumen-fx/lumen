//! The per-host half of the script wiring, installed once for each active
//! [`ScriptHost`].
//!
//! A host's own plugin ([`lumen_script::ScriptPlugin`]) installs the systems
//! that drive it every tick. What it leaves to the scene is what needs the
//! scene's tree: `on_ready` once the document is queryable, the component use
//! sites the build left for the script, delivering DOM events into the host,
//! and, in an app with more than one language, the late mirror sync that
//! keeps one host's signals current with another's writes.
//!
//! Every system here joins a [`ScriptSet`], so the host-neutral edges the
//! assembly installs cover it without naming its concrete type. The same call
//! serves a desktop window, a page, and a server render.

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::*;
use lumen_core::prelude::{App, TickStage};
use lumen_script::{ScriptHost, ScriptSet};

use crate::dom::build_dom_index;
use crate::script_commands::apply_scene_script_commands;

/// The systems that publish per-node detail (text, attributes, inline style,
/// the cascade inputs) for a script's reads. A host's lifecycle dispatches run
/// after them, so `on_ready` reads the tree it mounted into. An assembly with
/// no such publisher leaves the set empty and the edge inert.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublishNodeDetails;

/// Install the per-host half of the script wiring for host `H`.
///
/// `multi_host` is true when the app runs more than one script language.
pub fn install<H: ScriptHost + Resource<Mutability = Mutable>>(app: &mut App, multi_host: bool) {
    // Cross-host signal reads. A host keeps its own mirror current as its
    // builtins run, so with one host the early `ScriptSet::SyncSignals` pass
    // is all that is needed and this stays unregistered. With two, a signal
    // written in one language reaches `PropertyStore` only when
    // `apply_scene_script_commands` runs, and its dirty flag is cleared at
    // end of tick - so the other host's mirror must be refreshed here, inside
    // that one-tick window, or the write is invisible to it forever.
    if multi_host {
        app.add_systems(
            TickStage::Systems,
            lumen_script::sync_signals_into_host::<H>
                .in_set(ScriptSet::SyncSignalsLate)
                .after(apply_scene_script_commands)
                .before(ScriptSet::Derivations),
        );
    }
    // Post-mount lifecycle: dispatch `on_ready` once per host, after the first
    // `build_dom_index` publish so a DOM query inside it sees the mounted
    // static tree, and before `collect_dom_commands` so any tree the handler
    // builds is materialized on the same first tick. A missing `on_ready` is a
    // no-op, so `on_start`-only apps are unaffected.
    //
    // `.after(ScriptSet::SyncSignals)`: both write the host's signal mirror on
    // the tick where a value is still dirty, and the sync rewrites entries from
    // the store. Unordered, the sync can run after the dispatch and overwrite
    // the values `on_ready` just wrote with the pre-dispatch store state,
    // leaving the mirror stale for every later handler read.
    app.add_systems(
        TickStage::Systems,
        lumen_script::fire_on_ready::<H>
            .in_set(ScriptSet::Ready)
            .after(build_dom_index)
            .after(PublishNodeDetails)
            .after(ScriptSet::SyncSignals),
    );
    // The use sites the build left for the script to fill, on the same terms
    // as `on_ready`: after the tree is queryable, before the command collector,
    // so what the call builds lands on the tick the call ran. Every tick, not
    // once - a subtree spawned while the app runs can carry a marker too.
    app.add_systems(
        TickStage::Systems,
        lumen_script::fill_components::<H>
            .in_set(ScriptSet::Fill)
            .after(ScriptSet::Ready)
            .after(build_dom_index)
            .after(PublishNodeDetails)
            .after(ScriptSet::SyncSignals),
    );
    // The handlers the script binds with `on(type, handler)`: the one DOM
    // event dispatch reaches this host through its entry here.
    lumen_script::register_event_host::<H>(&mut app.world);
}
