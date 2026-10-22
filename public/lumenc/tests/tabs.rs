// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input ->
// script), which needs `RunOptions` / `build_headless_app`; lumenc only
// exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! `<tabs>` end to end: the strip the parser writes and the one the runtime
//! builds, picking a tab by click, by script and from Rust, the `change`
//! event, a tab's content mounting when it first shows, and the strip
//! following tabs that move.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use bevy_ecs::query::With;
use bevy_ecs::system::RunSystemOnce;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent, Transform, Visible};
use lumen_core::prelude::Viewport;
use lumen_core::property_store::PropertyStore;
use lumen_mcp::{SimulateKind, SimulateQueue, SimulateRequest};
use lumen_primitives::{TabPicks, TabStrip, TabStripButton, Tabs};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_tabs_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.css"), "").unwrap();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        probe.local_addr().unwrap().port()
    };
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[mcp]\nport = {port}\nsimulate = true\n\n[render]\nbackend = \"cpu\"\n"),
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.resource_mut::<Viewport>().size = Vec2::new(600.0, 400.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn find(app: &mut App, id: &str) -> Option<Entity> {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
}

fn entity(app: &mut App, id: &str) -> Entity {
    find(app, id).unwrap_or_else(|| panic!("no element `{id}`"))
}

/// Whether the tab `id` shows.
fn shows(app: &mut App, id: &str) -> bool {
    let e = entity(app, id);
    app.world.get::<Visible>(e).is_none_or(|v| v.0)
}

/// The text of each strip button of the `<tabs>` `set`, in order.
fn strip(app: &mut App, set: Entity) -> Vec<String> {
    let children: Vec<Entity> = app.world.get::<Children>(set).unwrap().to_vec();
    let strip = children
        .iter()
        .copied()
        .find(|&e| app.world.get::<TabStrip>(e).is_some())
        .expect("a strip");
    assert_eq!(children[0], strip, "the strip comes first");
    app.world
        .get::<Children>(strip)
        .map(|c| c.to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter(|&b| app.world.get::<TabStripButton>(b).is_some())
        .map(|b| {
            app.world
                .get::<TextContent>(b)
                .map(|t| t.0.clone())
                .unwrap_or_default()
        })
        .collect()
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

fn tick(app: &mut App, n: usize) {
    for _ in 0..n {
        app.tick();
    }
}

/// Click the strip button that says `label`.
fn click_button(app: &mut App, label: &str) {
    let mut q = app
        .world
        .query_filtered::<(&TextContent, &Transform), With<TabStripButton>>();
    let at = q
        .iter(&app.world)
        .find(|(t, _)| t.0 == label)
        .map(|(_, t)| t.absolute + t.size / 2.0)
        .unwrap_or_else(|| panic!("no tab button `{label}`"));
    let queue = app.world.resource::<SimulateQueue>().clone();
    queue.push(SimulateRequest {
        kind: SimulateKind::Click {
            x: at.x,
            y: at.y,
            button: None,
            pointer: None,
        },
        wait_for: None,
    });
    tick(app, 10);
}

/// One app, so two never run at once in this process.
#[test]
fn tabs_show_one_tab_however_they_were_built() {
    markup_tabs();
}

/// A bound `<tabs>` opens on its first tab, mounts a tab's content the
/// first time it shows, and a click on a strip button switches tabs, writes
/// the signal and raises `change` on the `<tabs>`. An unbound one keeps its
/// choice in its `value` attribute, which a script sets. A `<tabs>` a
/// script builds gets its strip, which follows its tabs when they move, and
/// a module picks a tab through `TabPicks`.
fn markup_tabs() {
    let mut app = build(
        r#"<root>
  <tabs id="bound" bind-value="active">
    <tab id="t-a" name="a" label="Alpha"><label id="in-a" text="A"/></tab>
    <tab id="t-b" name="b" label="Beta"><label id="in-b" text="B"/></tab>
  </tabs>
  <tabs id="free">
    <tab id="f-x" name="x"><label text="X"/></tab>
    <tab id="f-y" name="y"><label text="Y"/></tab>
  </tabs>
  <column id="host"/>
  <script>
    import "lumen.cdl";
    fn on_change(ev: int) {
        lumen::signal_set("changed", lumen::event_value(ev));
    }
    fn tab(name: string, label: string) -> int {
        let t = lumen::node_spawn("tab");
        lumen::node_set_id(t, "s-" + name);
        lumen::node_set_attr(t, "name", name);
        lumen::node_set_attr(t, "label", label);
        return t;
    }
    fn on_ready() {
        lumen::event_on(lumen::node_get_by_id("bound"), "change", "on_change");
        let s = lumen::node_spawn("tabs");
        lumen::node_set_id(s, "built");
        lumen::node_append(lumen::node_get_by_id("host"), s);
        lumen::node_append(s, tab("one", "One"));
        lumen::node_append(s, tab("two", "Two"));
        lumen::node_append(s, tab("three", "Three"));
    }
    fn main() {}
  </script>
</root>"#,
    );

    // Markup, bound.
    assert_eq!(signal(&app, "active").as_deref(), Some("a"), "seeded");
    assert!(shows(&mut app, "t-a"));
    assert!(!shows(&mut app, "t-b"));
    assert!(find(&mut app, "in-a").is_some());
    assert!(
        find(&mut app, "in-b").is_none(),
        "a tab's content waits for the tab to show"
    );
    let bound = entity(&mut app, "bound");
    assert_eq!(strip(&mut app, bound), ["Alpha", "Beta"]);
    click_button(&mut app, "Beta");
    assert_eq!(signal(&app, "active").as_deref(), Some("b"));
    assert_eq!(
        signal(&app, "changed").as_deref(),
        Some("b"),
        "`change` ran"
    );
    assert!(shows(&mut app, "t-b"));
    assert!(!shows(&mut app, "t-a"));
    assert!(find(&mut app, "in-b").is_some(), "mounted on first show");
    let t_b = entity(&mut app, "t-b");
    assert_eq!(app.world.get::<Tabs>(bound).unwrap().shown, Some(t_b));

    // Markup, unbound: a script picks through the `value` attribute.
    assert!(shows(&mut app, "f-x"));
    let free = lumen_script::node_query::run_get_by_id("free").expect("free");
    lumen_script::node_query::push_external_dom_command(lumen_script::ScriptCommand::SetAttr {
        node: free,
        name: "value".into(),
        value: "y".into(),
    });
    tick(&mut app, 4);
    assert!(shows(&mut app, "f-y"));
    assert!(!shows(&mut app, "f-x"));

    // Built by a script: the runtime builds the strip.
    let built = entity(&mut app, "built");
    assert_eq!(strip(&mut app, built), ["One", "Two", "Three"]);
    assert!(shows(&mut app, "s-one"));
    assert!(!shows(&mut app, "s-two"));

    // Moving a tab moves its button; relabelling one renames its button.
    let three = lumen_script::node_query::run_get_by_id("s-three").expect("three");
    let one = lumen_script::node_query::run_get_by_id("s-one").expect("one");
    lumen_script::node_query::push_external_dom_command(lumen_script::ScriptCommand::Insert {
        parent: lumen_script::node_query::run_get_by_id("built").expect("built"),
        node: three,
        before: one,
    });
    lumen_script::node_query::push_external_dom_command(lumen_script::ScriptCommand::SetAttr {
        node: one,
        name: "label".into(),
        value: "First".into(),
    });
    tick(&mut app, 4);
    assert_eq!(strip(&mut app, built), ["Three", "First", "Two"]);

    // A module picks a tab.
    let two = entity(&mut app, "s-two");
    app.world
        .run_system_once(move |mut picks: TabPicks| {
            picks.pick(two);
        })
        .expect("the system runs");
    tick(&mut app, 3);
    assert!(shows(&mut app, "s-two"));
    assert!(!shows(&mut app, "s-one"));
    assert_eq!(
        signal(&app, "changed").as_deref(),
        Some("b"),
        "a pick that is not the user's raises no `change`"
    );
}
