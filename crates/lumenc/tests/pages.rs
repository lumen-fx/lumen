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
    let dir = std::env::temp_dir().join(format!("lumen_pages_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
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
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");

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
    let dir = std::env::temp_dir().join(format!("lumen_pages_auto_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    // No `[pages]` block: routing is entirely default-driven.
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .unwrap();
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
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");

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

/// A compiled multi-page app: the same routing, out of an artifact, in a
/// directory holding no `.lmn` files at all.
///
/// This is what a packaged app is. Every page is compiled into the artifact
/// behind its own gate, and the page set travels with it, so navigation
/// resolves without the directory scan a from-source run uses. The app is
/// built in a second directory that holds only `lumen.toml`, which is what
/// makes the point: nothing here can be coming off the page files.
fn artifact_scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_pages_lmna_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("layout.lmn"),
        r#"<root>
  <template name="layout">
    <column>
      <a href="index" text="Home"/>
      <a href="settings" text="Settings"/>
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
    <button id="go" text="Open settings"/>
  </use>
  <script>
fn on_click(id) {
    if id == "go" { page("settings"); }
}
  </script>
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

/// Find a spawned entity by its markup id.
fn entity_by_id(app: &mut App, id: &str) -> bevy_ecs::entity::Entity {
    let mut q = app
        .world
        .query::<(bevy_ecs::entity::Entity, &lumen_core::components::LumenId)>();
    q.iter(&app.world)
        .find(|(_, lid)| lid.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no spawned element with id \"{id}\""))
}

#[test]
fn multi_page_navigation_from_an_artifact() {
    let _guard = nav_test_guard();
    let source = artifact_scratch_dir();

    // Compile the whole app, then throw the source away: the run below gets a
    // directory with `lumen.toml` and nothing else, the shape a packaged app
    // has once its markup is compiled in.
    let compiled = lumenc::compile_app(&source).expect("compile the page set");
    let pages = compiled
        .pages
        .as_ref()
        .expect("a multi-page app compiles with its page set");
    assert_eq!(pages.entry, "index");
    assert!(pages.keys.iter().any(|k| k == "settings"));
    assert!(
        !pages.keys.iter().any(|k| k == "layout"),
        "the shared layout is a template, not a navigable page"
    );
    let bytes = lumen_ir::artifact::serialize(&compiled).expect("serialize");

    let packaged = std::env::temp_dir().join(format!(
        "lumen_pages_lmna_run_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

    let mut opts = RunOptions::new(&packaged).with_artifact_bytes(bytes);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build from the artifact");
    tick_n(&mut app, 4);

    // The entry page mounted, and only it.
    assert_eq!(route_signal(&mut app, "route.path"), "index");
    assert!(
        texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "the entry page should be mounted: {:?}",
        texts(&mut app)
    );
    assert!(
        !texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"),
        "a page that is not active must stay unmounted"
    );
    // The shared layout template reached the compiled pages too.
    assert_eq!(anchor_count(&mut app), 2, "the layout nav should render");

    // Navigate with the script `page()` builtin, from the compiled script.
    let button = entity_by_id(&mut app, "go");
    app.world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: button,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    tick_n(&mut app, 6);

    assert_eq!(route_signal(&mut app, "route.path"), "settings");
    assert!(
        texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"),
        "the second page should have spawned from the artifact: {:?}",
        texts(&mut app)
    );
    assert!(
        !texts(&mut app).iter().any(|t| t == "INDEX_PAGE"),
        "the entry page should unmount after navigating away"
    );

    // A declarative anchor from the compiled layout navigates back.
    let home = anchor_entity(&mut app, "index");
    app.world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: home,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    tick_n(&mut app, 6);
    assert_eq!(route_signal(&mut app, "route.path"), "index");

    let _ = std::fs::remove_dir_all(&source);
    let _ = std::fs::remove_dir_all(&packaged);
}

/// A single-page app compiles with no page set at all, so nothing about
/// routing is installed for it and the load path stays as it was.
#[test]
fn a_single_page_app_compiles_without_a_page_set() {
    let dir = std::env::temp_dir().join(format!("lumen_pages_single_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(
        dir.join("main.lmn"),
        "<root><label id=\"only\" text=\"ONE_PAGE\"/></root>",
    )
    .unwrap();

    let compiled = lumenc::compile_app(&dir).expect("compile");
    assert!(
        compiled.pages.is_none(),
        "a single-page app carries no routing data"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `page_current()` reads the active page key, on every host and under the
/// same name. The no-argument reader is spelled apart from `page(path)`
/// because a candela host function takes one arity per name; rhai and lua get
/// the same spelling as a runtime extension beside their `page` overloads.
///
/// Each app writes what it read into the `seen` signal, which a label binds,
/// so the value travels the same path a real app's would.
#[test]
fn page_current_reads_the_active_page_on_every_host() {
    let scripts = [
        (
            "rhai",
            "main.rhai",
            "fn on_ready() { signal(\"seen\", \"\").set(page_current()); }".to_string(),
        ),
        (
            "lua",
            "main.lua",
            "function on_ready() signal(\"seen\", \"\"):set(page_current()) end".to_string(),
        ),
        (
            "candela",
            "main.cdl",
            "import \"lumen.cdl\";\n\
             fn on_ready() { let key = lumen::page_current(); \
             lumen::signal_set(\"seen\", key); }\n\
             fn main() {}\n"
                .to_string(),
        ),
    ];

    for (engine, script_name, script) in scripts {
        let _guard = nav_test_guard();
        let dir = std::env::temp_dir().join(format!(
            "lumen_page_current_{engine}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        std::fs::write(
            dir.join("index.lmn"),
            format!(
                "<root>\n  <label id=\"seen\" bind-text=\"seen\" text=\"(none)\"/>\n  \
                 <script src=\"{script_name}\"/>\n</root>"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("settings.lmn"), "<root><label text=\"S\"/></root>").unwrap();
        std::fs::write(dir.join(script_name), script).unwrap();

        let mut opts = RunOptions::new(&dir);
        opts.hot_reload = false;
        let (mut app, _window) =
            build_headless_app(opts).unwrap_or_else(|e| panic!("build {engine} app: {e}"));
        tick_n(&mut app, 6);

        assert_eq!(
            route_signal(&mut app, "seen"),
            "index",
            "{engine}: page_current() should read the active page key"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `page_back()` reports whether the step reached the navigation bus, so a
/// script can branch on it. Rhai and Lua see a boolean; candela's prelude
/// entry returns nothing, which its reference page states.
#[test]
fn page_back_returns_whether_the_step_was_queued() {
    let scripts = [
        (
            "rhai",
            "main.rhai",
            "fn on_ready() { signal(\"went\", \"\").set(if page_back() { \"yes\" } else { \"no\" }); }",
        ),
        (
            "lua",
            "main.lua",
            "function on_ready() signal(\"went\", \"\"):set(page_back() and \"yes\" or \"no\") end",
        ),
    ];

    for (engine, script_name, script) in scripts {
        let _guard = nav_test_guard();
        let dir =
            std::env::temp_dir().join(format!("lumen_page_back_{engine}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        std::fs::write(
            dir.join("index.lmn"),
            format!(
                "<root>\n  <label id=\"went\" bind-text=\"went\" text=\"(none)\"/>\n  \
                 <script src=\"{script_name}\"/>\n</root>"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("settings.lmn"), "<root><label text=\"S\"/></root>").unwrap();
        std::fs::write(dir.join(script_name), script).unwrap();

        let mut opts = RunOptions::new(&dir);
        opts.hot_reload = false;
        let (mut app, _winit) =
            build_headless_app(opts).unwrap_or_else(|e| panic!("build {engine} app: {e}"));
        tick_n(&mut app, 6);

        assert_eq!(
            route_signal(&mut app, "went"),
            "yes",
            "{engine}: page_back() should hand the script a boolean"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The no-argument `page()` reader resolves as its own call, beside the
/// one-argument `page(path)` that navigates. Both are registered over a single
/// body and told apart by how many arguments arrive, so this checks the host
/// actually dispatches the empty call rather than falling through to the
/// navigating one. candela is absent because a candela host function takes one
/// arity per name; `page_current()` is its spelling, covered above.
#[test]
fn page_with_no_arguments_reads_the_active_page() {
    let scripts = [
        (
            "rhai",
            "main.rhai",
            "fn on_ready() { signal(\"seen\", \"\").set(page()); }",
        ),
        (
            "lua",
            "main.lua",
            "function on_ready() signal(\"seen\", \"\"):set(page()) end",
        ),
    ];

    for (engine, script_name, script) in scripts {
        let _guard = nav_test_guard();
        let dir =
            std::env::temp_dir().join(format!("lumen_page_noarg_{engine}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        std::fs::write(
            dir.join("index.lmn"),
            format!(
                "<root>\n  <label id=\"seen\" bind-text=\"seen\" text=\"(none)\"/>\n  \
                 <script src=\"{script_name}\"/>\n</root>"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("settings.lmn"), "<root><label text=\"S\"/></root>").unwrap();
        std::fs::write(dir.join(script_name), script).unwrap();

        let mut opts = RunOptions::new(&dir);
        opts.hot_reload = false;
        let (mut app, _winit) =
            build_headless_app(opts).unwrap_or_else(|e| panic!("build {engine} app: {e}"));
        tick_n(&mut app, 6);

        assert_eq!(
            route_signal(&mut app, "seen"),
            "index",
            "{engine}: page() with no argument should read the active page key"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The fragment table is app-wide, not per-file: a `<template>` declared in
/// one page is instantiable from another, the same way `layout.lmn`'s is.
#[test]
fn a_fragment_declared_in_one_page_is_usable_from_another() {
    let _guard = nav_test_guard();
    let dir = std::env::temp_dir().join(format!("lumen_pages_shared_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(
        dir.join("index.lmn"),
        r#"<root>
  <template name="chip"><label class="chip" text="{label}"/></template>
  <label text="INDEX_PAGE"/>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("settings.lmn"),
        r#"<root>
  <label text="SETTINGS_PAGE"/>
  <chip label="FROM_INDEX"/>
</root>"#,
    )
    .unwrap();

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    tick_n(&mut app, 4);

    lumen_core::nav::navigate("settings");
    tick_n(&mut app, 5);
    assert!(
        texts(&mut app).iter().any(|t| t == "FROM_INDEX"),
        "settings.lmn instantiates the fragment index.lmn declares: {:?}",
        texts(&mut app)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A fragment file is read on every load, so editing the shared layout and
/// reloading renders the new body. Caching the table at boot is what this
/// guards against: hot reload reuses the boot-time page plan.
#[test]
fn editing_a_fragment_file_renders_the_new_body() {
    let _guard = nav_test_guard();
    let dir = std::env::temp_dir().join(format!("lumen_pages_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let layout = dir.join("layout.lmn");
    std::fs::write(
        &layout,
        r#"<root>
  <template name="layout"><column><label text="LAYOUT_V1"/><slot/></column></template>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("index.lmn"),
        r#"<root><use template="layout"><label text="INDEX_PAGE"/></use></root>"#,
    )
    .unwrap();

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    tick_n(&mut app, 4);
    assert!(texts(&mut app).iter().any(|t| t == "LAYOUT_V1"));

    std::fs::write(
        &layout,
        r#"<root>
  <template name="layout"><column><label text="LAYOUT_V2"/><slot/></column></template>
</root>"#,
    )
    .unwrap();

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("rebuild after the edit");
    tick_n(&mut app, 4);
    let rendered = texts(&mut app);
    assert!(
        rendered.iter().any(|t| t == "LAYOUT_V2"),
        "the edited layout body should render: {rendered:?}"
    );
    assert!(rendered.iter().any(|t| t == "INDEX_PAGE"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A page shell that asks for the full window height gets it.
///
/// Every page mounts inside its own host box under `<root>`. That box is the
/// page's containing block, so a `height: 100%` shell resolves its percentage
/// against it; a host box sized to its content would resolve the percentage
/// against the shell's own content height and push the footer past the bottom
/// of the window. No `position: absolute` anywhere in the fixture.
fn shell_scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_pages_shell_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let page = |marker: &str| {
        format!(
            r#"<root>
  <column id="shell" width="100%" height="100%">
    <row id="head" height="48"/>
    <column id="body" grow="1">
      <label text="{marker}"/>
    </column>
    <row id="foot" height="40"/>
  </column>
</root>"#
        )
    };
    std::fs::write(dir.join("index.lmn"), page("INDEX_PAGE")).unwrap();
    std::fs::write(dir.join("settings.lmn"), page("SETTINGS_PAGE")).unwrap();
    dir
}

fn transform_of(app: &mut App, id: &str) -> lumen_core::components::Transform {
    let e = entity_by_id(app, id);
    *app.world
        .get::<lumen_core::components::Transform>(e)
        .expect("laid-out element")
}

#[test]
fn a_full_height_page_shell_fits_the_window() {
    let _guard = nav_test_guard();
    let dir = shell_scratch_dir();
    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    tick_n(&mut app, 4);

    let window_h = app
        .world
        .resource::<lumen_core::render_world::Viewport>()
        .size
        .y;
    assert!(window_h > 0.0, "the headless window has a height");

    let check = |app: &mut App, page: &str| {
        let shell = transform_of(app, "shell");
        let foot = transform_of(app, "foot");
        assert_eq!(
            shell.size.y, window_h,
            "{page}: the shell should fill the window height"
        );
        assert_eq!(
            foot.absolute.y + foot.size.y,
            window_h,
            "{page}: the footer should end at the bottom of the window"
        );
        assert_eq!(foot.size.y, 40.0, "{page}: the footer keeps its height");
    };
    check(&mut app, "index");

    // The same holds for a page reached by navigating, not just the entry.
    lumen_core::nav::navigate("settings");
    tick_n(&mut app, 5);
    assert_eq!(route_signal(&mut app, "route.path"), "settings");
    check(&mut app, "settings");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `<root>` on a page other than the home page styles that page.
///
/// A page mounts inside its own box, and that box is what the page's `<root>`
/// describes, so a background and a padding written there reach the screen
/// instead of having to move to a wrapper inside the page. The home page is
/// the exception: its `<root>` is the app's root element, so its attributes
/// apply once, on the root, and not a second time on the box.
#[test]
fn a_page_root_keeps_its_attributes() {
    let _guard = nav_test_guard();
    let dir = std::env::temp_dir().join(format!("lumen_pages_rootattrs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    std::fs::write(
        dir.join("index.lmn"),
        r#"<root id="home-root" padding="17">
  <label text="INDEX_PAGE"/>
</root>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("settings.lmn"),
        r##"<root id="settings-root" bg="#102030" padding="24">
  <label text="SETTINGS_PAGE"/>
</root>"##,
    )
    .unwrap();

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    tick_n(&mut app, 4);

    // The home page's padding lands on the root element and nowhere else.
    let home = entity_by_id(&mut app, "home-root");
    let home_style = app
        .world
        .get::<lumen_core::components::Style>(home)
        .expect("the root element carries a style");
    assert_eq!(home_style.padding.top, 17.0);
    let mut styles = app.world.query::<&lumen_core::components::Style>();
    let padded = styles
        .iter(&app.world)
        .filter(|s| s.padding.top == 17.0)
        .count();
    assert_eq!(padded, 1, "the home page's padding should apply once");

    lumen_core::nav::navigate("settings");
    tick_n(&mut app, 5);
    assert!(texts(&mut app).iter().any(|t| t == "SETTINGS_PAGE"));

    let page = entity_by_id(&mut app, "settings-root");
    let style = app
        .world
        .get::<lumen_core::components::Style>(page)
        .expect("the page box carries a style");
    assert_eq!(
        style.padding.left, 24.0,
        "padding on a page's <root> should inset that page"
    );
    let visuals = app
        .world
        .get::<lumen_core::components::Visuals>(page)
        .expect("the page box carries visuals");
    assert!(
        visuals.fill.is_some(),
        "bg on a page's <root> should paint that page"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
