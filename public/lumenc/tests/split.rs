// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! `<split>` end to end: the parser's gutters, the user-agent sheet's
//! sizing, layout's measure, and a drag and keys through the simulate queue.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenAttributes, LumenId, Transform};
use lumen_core::input::{CursorKeyword, CursorRequest, FocusTracker, PointerMoved, PointerState};
use lumen_core::prelude::Viewport;
use lumen_mcp::{SimulateKind, SimulateQueue, SimulateRequest};
use lumen_primitives::{Split, SplitGutter, SplitResized, SplitSet, SplitSizes};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_split_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    std::fs::write(src.join("main.css"), css).unwrap();
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

fn rect(app: &mut App, id: &str) -> Transform {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn gutter(app: &mut App) -> (bevy_ecs::entity::Entity, Transform) {
    let mut q = app
        .world
        .query_filtered::<(bevy_ecs::entity::Entity, &Transform), bevy_ecs::query::With<SplitGutter>>();
    q.iter(&app.world)
        .map(|(e, t)| (e, *t))
        .next()
        .expect("the split has a gutter")
}

fn run(app: &mut App, kinds: Vec<SimulateKind>) {
    let queue = app.world.resource::<SimulateQueue>().clone();
    for kind in kinds {
        queue.push(SimulateRequest {
            kind,
            wait_for: None,
        });
    }
    for _ in 0..30 {
        app.tick();
    }
}

/// Rest the pointer on the gutter's middle and read the cursor the window
/// would be asked for.
fn cursor_over_gutter(app: &mut App) -> CursorKeyword {
    let (_, g) = gutter(app);
    let at = g.absolute + g.size / 2.0;
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    for _ in 0..4 {
        app.tick();
    }
    app.world.resource::<CursorRequest>().cursor.keyword
}

fn key(name: &str) -> SimulateKind {
    serde_json::from_value(serde_json::json!({ "kind": "key", "key": name })).expect("a key")
}

const ROW: &str = r#"<root>
  <split id="s" width="400" height="200">
    <column id="left" grow="1" min-width="100"/>
    <column id="right" grow="1"/>
  </split>
</root>"#;

/// The gutter sits between the panes and takes its share of the axis; a
/// drag moves the boundary with the pointer and leaves the split's size
/// alone; the focused gutter's arrow keys step it and Home takes the pane
/// before it to its `min-width`. A split laid out as a column stacks its
/// panes and drags its gutter up and down. One test, so the two apps never
/// run at once in this process.
#[test]
fn gutters_resize_the_panes() {
    row_split();
    column_split();
    script_split();
    rust_split();
}

/// What the split `s` holds, in order: a pane's id, or `|` for a gutter.
fn split_children(app: &mut App) -> Vec<String> {
    let mut q = app.world.query::<(bevy_ecs::entity::Entity, &LumenId)>();
    let split = q
        .iter(&app.world)
        .find(|(_, id)| id.0 == "s")
        .map(|(e, _)| e)
        .expect("the split");
    let children: Vec<bevy_ecs::entity::Entity> = app
        .world
        .get::<bevy_ecs::hierarchy::Children>(split)
        .map(|c| c.to_vec())
        .unwrap_or_default();
    children
        .into_iter()
        .map(|c| match app.world.get::<SplitGutter>(c) {
            Some(_) => "|".to_string(),
            None => app
                .world
                .get::<LumenId>(c)
                .map_or_else(|| "?".to_string(), |id| id.0.clone()),
        })
        .collect()
}

/// A split a script builds gets the gutters markup would, between every two
/// panes, and they resize it the same way; a pane taken out takes its gutter
/// with it.
fn script_split() {
    let mut app = build(
        r#"<root>
  <script>
    import "lumen.cdl";
    fn pane(id: string) -> int {
        let p = lumen::node_spawn("column");
        lumen::node_set_id(p, id);
        lumen::node_set_style(p, "grow", "1");
        return p;
    }
    fn on_ready() {
        let s = lumen::node_spawn("split");
        lumen::node_set_id(s, "s");
        lumen::node_set_style(s, "width", "400");
        lumen::node_set_style(s, "height", "200");
        lumen::node_append(lumen::node_document(), s);
        let a = pane("a");
        lumen::node_set_style(a, "min-width", "100");
        lumen::node_append(s, a);
        lumen::node_append(s, pane("b"));
        lumen::node_append(s, pane("c"));
    }
    fn main() {}
  </script>
</root>"#,
        "",
    );
    assert_eq!(split_children(&mut app), ["a", "|", "b", "|", "c"]);

    let b = lumen_script::node_query::run_get_by_id("b").expect("pane b");
    lumen_script::node_query::push_external_dom_command(lumen_script::ScriptCommand::RemoveNode {
        node: b,
    });
    for _ in 0..4 {
        app.tick();
    }
    assert_eq!(split_children(&mut app), ["a", "|", "c"]);
    let mut gutters = app
        .world
        .query_filtered::<(), bevy_ecs::query::With<SplitGutter>>();
    assert_eq!(
        gutters.iter(&app.world).count(),
        1,
        "the spare gutter is gone"
    );

    let (_, g) = gutter(&mut app);
    assert_eq!(g.size.x, 6.0, "the gutter is styled like a written one");
    assert_eq!(rect(&mut app, "a").size.x, 197.0);
    assert_eq!(rect(&mut app, "c").size.x, 197.0);
    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x + 50.0,
                y: at.y,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x + 50.0,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    assert_eq!(
        rect(&mut app, "a").size.x,
        247.0,
        "a drag resizes the panes"
    );
    assert_eq!(rect(&mut app, "c").size.x, 147.0);
}

fn row_split() {
    let mut app = build(ROW, "");
    let (gutter_e, g) = gutter(&mut app);
    let left = rect(&mut app, "left");
    let right = rect(&mut app, "right");
    assert_eq!(g.size.x, 6.0, "the gutter's share of the axis");
    assert_eq!(g.size.y, 200.0, "the gutter stretches across");
    assert_eq!(
        cursor_over_gutter(&mut app),
        CursorKeyword::ColResize,
        "a row's gutter shows the column-resize cursor"
    );
    assert_eq!(left.size.x, 197.0);
    assert_eq!(right.size.x, 197.0);
    let attrs = app
        .world
        .get::<LumenAttributes>(gutter_e)
        .expect("separator values");
    assert_eq!(attrs.get("aria-valuenow"), Some("50"));
    assert_eq!(attrs.get("aria-orientation"), Some("vertical"));

    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x + 50.0,
                y: at.y + 30.0,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x + 50.0,
                y: at.y + 30.0,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    assert_eq!(rect(&mut app, "left").size.x, 247.0);
    assert_eq!(rect(&mut app, "right").size.x, 147.0);
    assert_eq!(rect(&mut app, "s").size.x, 400.0);
    let attrs = app.world.get::<LumenAttributes>(gutter_e).unwrap();
    assert_eq!(attrs.get("aria-valuenow"), Some("63"));

    assert_eq!(
        app.world.resource::<FocusTracker>().0,
        Some(gutter_e),
        "a press focuses the gutter"
    );
    run(&mut app, vec![key("ArrowLeft"), key("ArrowLeft")]);
    assert_eq!(rect(&mut app, "left").size.x, 227.0);
    assert_eq!(rect(&mut app, "right").size.x, 167.0);
    run(&mut app, vec![key("Home")]);
    assert_eq!(rect(&mut app, "left").size.x, 100.0);
    assert_eq!(rect(&mut app, "right").size.x, 294.0);
}

fn column_split() {
    let mut app = build(
        r#"<root>
  <split id="s" width="300" height="306">
    <column id="top" grow="1"/>
    <column id="bottom" grow="1"/>
  </split>
</root>"#,
        "#s { flex-direction: column; }",
    );
    let (_, g) = gutter(&mut app);
    assert_eq!(g.size.y, 6.0);
    assert_eq!(g.size.x, 300.0);
    assert_eq!(
        cursor_over_gutter(&mut app),
        CursorKeyword::RowResize,
        "a column's gutter shows the row-resize cursor"
    );
    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x,
                y: at.y - 40.0,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x,
                y: at.y - 40.0,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    assert_eq!(rect(&mut app, "top").size.y, 110.0);
    assert_eq!(rect(&mut app, "bottom").size.y, 190.0);
}

/// The resize-end messages a test app saw, in order.
#[derive(bevy_ecs::resource::Resource, Default)]
struct Resizes(Vec<SplitResized>);

fn record_resizes(
    mut seen: bevy_ecs::message::MessageReader<SplitResized>,
    mut out: bevy_ecs::system::ResMut<Resizes>,
) {
    out.0.extend(seen.read().cloned());
}

/// A split a module's Rust builds element by element, with no parser and no
/// script, gets its gutters too; its sizes are read off the split and set
/// through `SplitSizes`, and a drag the user makes ends with a message.
fn rust_split() {
    use bevy_ecs::schedule::IntoScheduleConfigs;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::prelude::TickStage;
    use lumen_ir::layout_ir::Element;
    use lumen_scene::spawn::{Placeholders, spawn_subtree};

    let mut app = build(r#"<root><column id="host"/></root>"#, "");
    app.world.init_resource::<Resizes>();
    app.add_systems(TickStage::Systems, record_resizes.after(SplitSet));
    let host = {
        let mut q = app.world.query::<(bevy_ecs::entity::Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, id)| id.0 == "host")
            .map(|(e, _)| e)
            .expect("the host")
    };
    let element = |tag: &str, id: &str, style: &[(&str, &str)]| {
        let mut el = Element {
            tag: tag.to_string(),
            ..Element::default()
        };
        el.attrs.id = Some(id.to_string());
        for (name, value) in style {
            lumen_ir::css::apply_markup_declaration(name, value, &mut el.attrs)
                .expect("a valid declaration");
        }
        el
    };
    let split = spawn_subtree(
        &mut app.world,
        &element("split", "s", &[("width", "402"), ("height", "100")]),
        Some(host),
        Placeholders::Resolved,
    );
    for id in ["a", "b", "c"] {
        spawn_subtree(
            &mut app.world,
            &element("column", id, &[("grow", "1")]),
            Some(split),
            Placeholders::Resolved,
        );
    }
    for _ in 0..4 {
        app.tick();
    }
    assert_eq!(split_children(&mut app), ["a", "|", "b", "|", "c"]);
    let sizes = app.world.get::<Split>(split).unwrap().sizes();
    assert_eq!(sizes, vec![130.0, 130.0, 130.0]);

    let set = app
        .world
        .run_system_once(move |mut sizes: SplitSizes| sizes.set(split, &[200.0, 100.0, 90.0]))
        .expect("the system runs");
    assert!(set, "a laid-out split takes sizes");
    for _ in 0..4 {
        app.tick();
    }
    assert_eq!(rect(&mut app, "a").size.x, 200.0);
    assert_eq!(rect(&mut app, "b").size.x, 100.0);
    assert_eq!(rect(&mut app, "c").size.x, 90.0);
    assert!(
        app.world.resource::<Resizes>().0.is_empty(),
        "a size the app set is not a resize the user made"
    );

    let first = app.world.get::<Split>(split).unwrap().gutters[0];
    let g = *app.world.get::<Transform>(first).unwrap();
    let at = g.absolute + g.size / 2.0;
    run(
        &mut app,
        vec![
            SimulateKind::PointerDown {
                x: at.x,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
            SimulateKind::PointerMove {
                x: at.x - 50.0,
                y: at.y,
                pointer: Default::default(),
            },
            SimulateKind::PointerUp {
                x: at.x - 50.0,
                y: at.y,
                button: None,
                pointer: Default::default(),
            },
        ],
    );
    let resizes = &app.world.resource::<Resizes>().0;
    assert_eq!(resizes.len(), 1, "one drag, one resize end");
    assert_eq!(resizes[0].split, split);
    assert_eq!(resizes[0].sizes, vec![150.0, 150.0, 90.0]);
}
