// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions` / `lumenc::pages`, which lumenc only exposes under the
// `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! File-based pages - end-to-end headless proof.
//!
//! Builds a real multi-page app from disk (multiple `.lmn` files + a global
//! `layout.lmn` template) via the same `build_headless_app` the golden suite
//! uses, then drives navigation through every surface and asserts the active
//! page signal + the mounted subtree track it. Covers, in one run:
//!
//! * multiple `.lmn` pages load; `index.lmn` is the entry;
//! * `page("settings")` (the shared nav command, here reached through
//!   `lumen_core::nav`) switches the active-page signal AND swaps the
//!   rendered subtree;
//! * a real `<a href>` anchor click does the same;
//! * a global `<template>` (from `layout.lmn`) is usable from more than one
//!   page;
//! * a non-file path (`/user/42`) resolves to the nearest page file
//!   (`user.lmn`) with the leftover (`/42`) exposed on `route.segment`.

#![cfg(feature = "runtime-parse")]

use lumen_core::app::App;
use lumen_core::components::TextContent;
use lumen_core::input::{ClickEvent, PointerButton};
use lumen_core::property_store::PropertyStore;
use lumenc::pages::Anchor;
use lumenc::{RunOptions, build_headless_app};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Navigation rides a process-global bus (`lumen_core::nav`), so two apps in
/// one test binary share it. Serialise the nav-driving tests behind one lock;
/// each app drains its own queued requests every tick, so back-to-back runs
/// stay isolated.
fn nav_test_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_pages_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[pages]\nentry = \"index\"\n",
    )
    .unwrap();
    // Shared, global layout template (a template-only file).
    std::fs::write(
        dir.join("layout.lmn"),
        r#"<root>
  <template name="layout">
    <column>
      <row>
        <a href="index" text="Home"/>
        <a href="settings" text="Settings"/>
        <a href="user/42" text="User"/>
      </row>
      <column>
        <slot/>
      </column>
    </column>
  </template>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("index.lmn"),
        r#"<root>
  <use template="layout">
    <label id="page-marker" text="INDEX_PAGE"/>
  </use>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("settings.lmn"),
        r#"<root>
  <use template="layout">
    <label id="page-marker" text="SETTINGS_PAGE"/>
  </use>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("user.lmn"),
        r#"<root>
  <use template="layout">
    <label id="page-marker" text="USER_PAGE"/>
    <label id="seg" bind-text="route.segment" text="(none)"/>
  </use>
</root>"#,
    )
    .unwrap();
    dir
}

fn route_signal(app: &mut App, name: &str) -> String {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|a| a.to_string())
        .unwrap_or_default()
}

fn texts(app: &mut App) -> Vec<String> {
    let mut q = app.world.query::<&TextContent>();
    q.iter(&app.world).map(|t| t.0.clone()).collect()
}

fn anchor_count(app: &mut App) -> usize {
    let mut q = app.world.query::<&Anchor>();
    q.iter(&app.world).count()
}

/// Find the currently-spawned anchor entity whose href equals `href`.
fn anchor_entity(app: &mut App, href: &str) -> bevy_ecs::entity::Entity {
    let mut q = app.world.query::<(bevy_ecs::entity::Entity, &Anchor)>();
    q.iter(&app.world)
        .find(|(_, a)| a.0 == href)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no spawned <a href=\"{href}\">"))
}

fn tick_n(app: &mut App, n: usize) {
    for _ in 0..n {
        app.tick();
    }
}

#[test]
fn multi_page_navigation_end_to_end() {
    let _guard = nav_test_guard();
    let dir = scratch_dir();
    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false; // deterministic: no fs watcher in the test
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    // Settle the first mount.
    tick_n(&mut app, 4);

    // 1. index.lmn is the entry; its subtree is mounted, others are not.
    assert_eq!(route_signal(&mut app, "route.path"), "index");
    assert!(
        texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "index page content should be mounted: {:?}",
        texts(&mut app)
    );
    assert!(
        !texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"),
        "settings page must NOT be mounted on the index route"
    );

    // Global template used from page 1: the layout nav (3 anchors) rendered.
    assert_eq!(
        anchor_count(&mut app),
        3,
        "layout template's 3 anchors should render on index"
    );

    // 2. Programmatic `page("settings")` (shared nav command) swaps the page.
    lumen_core::nav::navigate("settings");
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "settings");
    assert!(texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"));
    assert!(
        !texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "index subtree must be unmounted after navigating away (Render mode)"
    );
    // Global template used from page 2 as well.
    assert_eq!(
        anchor_count(&mut app),
        3,
        "layout template's anchors should render on settings too"
    );

    // 3. Declarative `<a href="index">` click navigates back.
    let home_anchor = anchor_entity(&mut app, "index");
    app.world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: home_anchor,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "index");
    assert!(texts(&mut app).iter().any(|t| t == "INDEX_PAGE"));

    // 4. Non-file path resolves to the nearest page + exposes the leftover
    //    on route.segment.
    lumen_core::nav::navigate("/user/42");
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "user");
    assert_eq!(route_signal(&mut app, "route.segment"), "/42");
    assert!(texts(&mut app).iter().any(|t| t == "USER_PAGE"));
    // The page's own binding surfaced the segment.
    assert!(
        texts(&mut app).iter().any(|t| t == "/42"),
        "route.segment should be bound into the page: {:?}",
        texts(&mut app)
    );

    // 5. History back returns to index.
    lumen_core::nav::back();
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "index");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Same app shape with ZERO page config: no `[pages]` block at all. File-based
/// routing must come up purely from auto-discovery (multiple `.lmn` files,
/// `index.lmn` as home, `layout.lmn` as the shared non-page template).
/// `layout.lmn` here carries a comment that mentions the literal `<template>`;
/// a scanner that does not skip comments swallows the whole template and every
/// `<use template="layout">` then fails to resolve, which is the exact bug this
/// guards against.
fn auto_scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_pages_auto_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // No `[pages]` block: routing is entirely default-driven.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(
        dir.join("layout.lmn"),
        r#"<root>
  <!-- Shared layout. This `<template>` is hoisted into the global preamble. -->
  <template name="layout">
    <column>
      <row>
        <a href="index" text="Home"/>
        <a href="settings" text="Settings"/>
      </row>
      <column>
        <slot/>
      </column>
    </column>
  </template>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("index.lmn"),
        r#"<root>
  <use template="layout">
    <label id="page-marker" text="INDEX_PAGE"/>
  </use>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("settings.lmn"),
        r#"<root>
  <use template="layout">
    <label id="page-marker" text="SETTINGS_PAGE"/>
  </use>
</root>"#,
    )
    .unwrap();
    dir
}

#[test]
fn auto_discovered_pages_navigate_with_no_config() {
    let _guard = nav_test_guard();
    let dir = auto_scratch_dir();
    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _winit) = build_headless_app(opts).expect("build_headless_app");

    tick_n(&mut app, 4);

    // Auto-discovery picked index.lmn as the home page and hoisted the shared
    // layout template despite its comment mentioning `<template>`. The layout
    // slot holds the index content and the nav bar (2 anchors) rendered.
    assert_eq!(route_signal(&mut app, "route.path"), "index");
    assert!(
        texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "index content should sit in the layout slot: {:?}",
        texts(&mut app)
    );
    assert_eq!(
        anchor_count(&mut app),
        2,
        "shared layout nav bar should render"
    );

    // `layout` is not a navigable page: it never gets a route key or a gate.
    lumen_core::nav::navigate("layout");
    tick_n(&mut app, 5);
    assert_ne!(
        route_signal(&mut app, "route.path"),
        "layout",
        "layout.lmn must not be routable"
    );

    // Navigate between the two real pages via the shared command bus.
    lumen_core::nav::navigate("settings");
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "settings");
    assert!(texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"));
    assert!(
        !texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "index subtree must unmount after navigating away"
    );

    // Declarative anchor click navigates home.
    let home = anchor_entity(&mut app, "index");
    app.world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: home,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "index");
    assert!(texts(&mut app).iter().any(|t| t == "INDEX_PAGE"));

    let _ = std::fs::remove_dir_all(&dir);
}
