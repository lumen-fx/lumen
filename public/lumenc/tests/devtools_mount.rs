//! End-to-end mount test for the dev-only devtools overlay. Only compiled
//! with `--features devtools`; an empty file otherwise (so the default
//! `cargo test` still passes without the crate).
//!
//! Exercises the full lumenc bridge: parse the embedded overlay assets,
//! spawn them as a second root, lift + tag the subtree, then tick the
//! headless app and confirm the body refreshes from an injected snapshot.

#![cfg(feature = "devtools")]
// Snapshot has ~30 fields; assigning the couple that matter after default()
// is clearer than a full literal.
#![allow(clippy::field_reassign_with_default)]

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use bevy_ecs::prelude::*;
use lumen_core::components::{Color, LumenId, TextContent, Visible};
use lumen_devtools::{
    BODY_ID, DevtoolsMarker, DevtoolsRoot, DevtoolsState, OverlayPalette, RowTarget, Tab,
};
use lumen_mcp::{EntityInspect, EntityView, Snapshot, SnapshotHandle};
use lumenc::{RunOptions, build_headless_app};

#[test]
fn devtools_overlay_mounts_and_refreshes() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/devtools_app");
    let (mut app, _window) = build_headless_app(RunOptions::new(dir)).expect("build headless app");

    // The overlay root spawned, starts hidden, and is tagged. It must sit in
    // the top-layer paint band: the overlay is a second root spawned before
    // the app's, so without OverlayLayer the app's background paints over it.
    let mut roots = app.world.query_filtered::<(
        &Visible,
        Option<&lumen_core::render_world::OverlayLayer>,
    ), With<DevtoolsRoot>>();
    let (vis, overlay) = roots.iter(&app.world).next().expect("DevtoolsRoot spawned");
    assert!(!vis.0, "overlay starts hidden until F12");
    assert!(
        overlay.is_some(),
        "overlay root lifted into the top paint band"
    );

    // The dynamic-state palette the mount resolved matches the value
    // `overlay.css` declares for `--dt-tag-color` (the fallback/override
    // cases live in `lumen_runtime::devtools_mount`'s own unit tests).
    let palette = *app.world.resource::<OverlayPalette>();
    assert_eq!(
        palette.tag_color,
        Color::from_rgba8([0x5d, 0xb0, 0xd7, 0xff]),
        "tag_color resolved from overlay.css's --dt-tag-color"
    );

    // The data-driven body entity exists and carries DevtoolsMarker (so the
    // Elements tab excludes the overlay's own entities).
    let mut bodies = app
        .world
        .query_filtered::<(&LumenId, Option<&DevtoolsMarker>), With<TextContent>>();
    let tagged_body = bodies
        .iter(&app.world)
        .any(|(id, marker)| id.0 == BODY_ID && marker.is_some());
    assert!(tagged_body, "dt-body present and tagged DevtoolsMarker");

    // Inject a populated snapshot (MCP is disabled in the fixture), open the
    // overlay, tick once, and confirm the body reflects the element data.
    let mut snap = Snapshot::default();
    snap.entities = vec![EntityView {
        id: 100,
        components: vec![],
    }];
    snap.inspect.insert(
        100,
        EntityInspect {
            tag: Some("column".into()),
            lumen_id: Some("app-hello".into()),
            ..Default::default()
        },
    );
    app.world
        .insert_resource(SnapshotHandle(Arc::new(RwLock::new(snap))));
    {
        let mut state = app.world.resource_mut::<DevtoolsState>();
        state.visible = true;
        state.tab = Tab::Elements;
    }

    app.tick();

    // The Elements tab spawns one row entity per element (label parts are
    // its children), all tagged so the next snapshot excludes them from
    // the tree.
    let mut rows = app.world.query::<&RowTarget>();
    assert!(
        rows.iter(&app.world).next().is_some(),
        "rows spawned for the injected snapshot"
    );
    let mut labels = app
        .world
        .query_filtered::<&TextContent, With<DevtoolsMarker>>();
    assert!(
        labels.iter(&app.world).any(|t| t.0.contains("app-hello")),
        "a label part for the injected element"
    );
}
