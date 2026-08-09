//! Dev-only mount for the in-window devtools overlay (`lumen-devtools`).
//!
//! Compiled in only behind lumenc's `devtools` feature (off by default,
//! enabled by `lumenc run` in the dev loop). Everything of substance lives in
//! the `lumen-devtools` crate; this module is the thin bridge that owns the
//! two things that crate cannot: the markup/CSS parser and the ECS spawner.
//!
//! It parses the crate's embedded overlay assets, spawns them as a second
//! root, lifts that root into the top paint band, tags the subtree so the
//! Elements tab excludes it, and installs `DevtoolsPlugin`.

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::{Entity, World};
use lumen_core::app::App;

/// Parse the embedded overlay assets, spawn the overlay, and install the
/// devtools systems. Failures are logged, never fatal - a broken dev overlay
/// must not take the app down.
pub fn install(app: &mut App, parser: &dyn crate::source_parser::SourceParser) {
    // Register state, the network-capture ring + sink, the snapshot-schedule
    // tweak, and the per-tick systems first.
    app.add_plugin(lumen_devtools::DevtoolsPlugin);

    // Parse the embedded `.lmn` + `.css` with the injected front-end.
    let mut ir = match parser.parse_html(lumen_devtools::OVERLAY_LMN) {
        Ok(ir) => ir,
        Err(e) => {
            tracing::warn!("devtools: overlay markup failed to parse: {e}");
            return;
        }
    };
    match parser.parse_css(lumen_devtools::OVERLAY_CSS) {
        Ok(sheet) => {
            let media = lumen_ir::css::MediaContext::default();
            if let Err(e) = lumen_ir::css::apply_css_with_media(&mut ir, &sheet, &media) {
                tracing::warn!("devtools: overlay CSS failed to apply: {e}");
            }
        }
        Err(e) => tracing::warn!("devtools: overlay CSS failed to parse: {e}"),
    }

    // Spawn as an isolated root (does not clobber the app's LumenStylesheet).
    let root = crate::spawn::spawn_subtree(&mut app.world, &ir.root, None);

    // Collect the whole spawned subtree so lumen-devtools can tag it.
    let descendants = collect_subtree(&mut app.world, root);

    // Stamp DevtoolsMarker across the subtree, DevtoolsRoot + Visible on the
    // root. Starts hidden (until F12) unless LUMEN_DEVTOOLS_OPEN requests
    // startup-open.
    lumen_devtools::mount_marks(
        &mut app.world,
        root,
        &descendants,
        lumen_devtools::env_open(),
    );

    tracing::info!(
        "devtools: overlay mounted ({} entities); press F12 to toggle",
        descendants.len()
    );
}

/// Breadth-first collect `root` and every descendant via the `Children`
/// relationship. `RelationshipTarget` is not in scope here, so `iter()`
/// resolves through `Children`'s slice deref and yields `&Entity`; the
/// `.copied()` below is what turns that into owned ids.
fn collect_subtree(world: &mut World, root: Entity) -> Vec<Entity> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let e = out[i];
        i += 1;
        let kids: Vec<Entity> = world
            .get::<Children>(e)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();
        out.extend(kids);
    }
    out
}
