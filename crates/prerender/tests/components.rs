//! The fragments a compiled app carries, built by a run that has no window.
//!
//! A fragment reaches a running app two ways, and a build runs the same
//! assembly a browser and a server do, so both have to work here or they work
//! nowhere off the desktop. One is a component the build could not stand in
//! for, which the tree carries as a marker and the run fills by calling the
//! function it names. The other is a key a script instantiates and mounts.
//!
//! The tree is read off the world rather than off the document: a run's
//! product is the state a page is written with, and what these check is that
//! the subtrees exist to have state read out of them at all.

use std::sync::{Arc, Mutex, MutexGuard};

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::Without;
use lumen_core::components::{LumenClasses, LumenTag, TextContent};
use lumen_core::prelude::App;
use lumen_html::contract::Seed;
use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_ir::fragment::{Fragment, FragmentKind, FragmentParam, FragmentTable};
use lumen_ir::layout_ir::{Attributes, Element, FragmentUse, InterpolationSlot, LayoutIR};
use lumen_prerender::{Budget, DenyDispatch, boot, row_fills, settle};

/// The program the build script compiled: a component that has to run, and an
/// `on_ready` that mounts a fragment by key.
const COMPONENTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/components.cdlb"));

/// The external buses belong to the process, and a run empties them on the way
/// in, so runs take this in turn.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn in_turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// A body of one label, whose text is the fragment's `name` parameter.
fn body(class: &str, param: &str) -> Vec<Element> {
    vec![Element {
        tag: "label".to_string(),
        attrs: Attributes {
            classes: vec![class.to_string()],
            text: Some(format!("{{{param}}}")),
            ..Attributes::default()
        },
        interpolations: vec![InterpolationSlot::Arg(param.to_string())],
        ..Element::default()
    }]
}

/// The table the artifact carries: the block `Shout` returns, and the one the
/// app mounts.
fn fragments() -> FragmentTable {
    let mut table = FragmentTable::new();
    for (key, class, param) in [("shout", "shout", "who"), ("card", "card", "title")] {
        table
            .insert(Fragment {
                key: key.to_string(),
                params: vec![FragmentParam {
                    name: param.to_string(),
                    default: None,
                }],
                body: body(class, param),
                origins: Vec::new(),
                kind: FragmentKind::Markup,
                components: Vec::new(),
            })
            .expect("distinct keys");
    }
    table
}

/// The marker `Shout` leaves behind, calling the component with `who`.
fn marker(who: &str) -> Element {
    let mut marker = Element {
        tag: "Shout".to_string(),
        ..Element::default()
    };
    marker.frag_use = Some(Box::new(FragmentUse {
        key: "Shout".to_string(),
        args: vec![("who".to_string(), who.to_string())],
        slot_children: false,
    }));
    marker
}

/// The tree the build emits: a stage holding the marker `Shout` left behind,
/// and a `<for>` whose row template holds one of its own.
fn tree() -> LayoutIR {
    let stage = Element {
        tag: "column".to_string(),
        attrs: Attributes {
            id: Some("stage".to_string()),
            ..Attributes::default()
        },
        children: vec![marker("ann")],
        ..Element::default()
    };
    let rows = Element {
        tag: "for".to_string(),
        attrs: Attributes {
            each: Some("rows".to_string()),
            ..Attributes::default()
        },
        children: vec![marker("{title}")],
        ..Element::default()
    };
    LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![stage, rows],
            ..Element::default()
        },
        ..LayoutIR::default()
    }
}

/// A seed holding two rows, which is what a page written from `[web.seed]`
/// starts from.
fn seeded() -> Seed {
    let mut seed = Seed::new();
    seed.arrays.insert(
        "rows".to_string(),
        ["Alpha", "Beta"]
            .into_iter()
            .map(|title| {
                [("title".to_string(), title.to_string())]
                    .into_iter()
                    .collect()
            })
            .collect(),
    );
    seed
}

/// The whole app, as a build reads it out of an artifact.
fn compiled() -> CompiledApp {
    CompiledApp {
        ir: tree(),
        fragments: fragments(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(COMPONENTS.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

/// Run the app until it settles, and hand back the app to read the tree off.
fn run() -> App {
    run_with(&seeded())
}

/// The same, from a seed of the caller's choosing.
fn run_with(seed: &Seed) -> App {
    run_tree(tree(), seed)
}

/// The same, from a tree of the caller's choosing.
fn run_tree(ir: LayoutIR, seed: &Seed) -> App {
    let mut compiled = compiled();
    compiled.ir = ir;
    let mut booted = boot(&compiled, "index", seed, Arc::new(DenyDispatch::default()));
    settle(&mut booted.app, Budget::default());
    booted.app
}

/// One line per element, indented by depth: `tag.class = text`. Read off the
/// world, so what it shows is what the run built.
fn dump(app: &mut App) -> String {
    let root = app
        .world
        .query_filtered::<Entity, Without<ChildOf>>()
        .iter(&app.world)
        .find(|e| {
            app.world
                .get::<LumenTag>(*e)
                .is_some_and(|t| &*t.0 == "root")
        })
        .expect("the app has a root element");
    let mut out = String::new();
    write_node(app, root, 0, &mut out);
    out
}

fn write_node(app: &App, entity: Entity, depth: usize, out: &mut String) {
    let tag = app
        .world
        .get::<LumenTag>(entity)
        .map_or_else(|| "?".to_string(), |t| t.0.to_string());
    let classes = app
        .world
        .get::<LumenClasses>(entity)
        .map_or_else(String::new, |c| {
            c.0.iter()
                .map(|class| format!(".{class}"))
                .collect::<String>()
        });
    let text = app
        .world
        .get::<TextContent>(entity)
        .map_or_else(String::new, |t| format!(" = {}", t.0));
    out.push_str(&format!(
        "{:indent$}{tag}{classes}{text}\n",
        "",
        indent = depth * 2
    ));
    if let Some(children) = app.world.get::<Children>(entity) {
        for child in children.iter() {
            write_node(app, *child, depth + 1, out);
        }
    }
}

/// The tree a run builds, which is the tree a window builds from the same
/// artifact: the marker gone, its body in its place, and the mounted block at
/// the root.
#[test]
fn a_run_builds_every_fragment_the_artifact_carries() {
    let _turn = in_turn();
    let mut app = run();

    assert_eq!(
        dump(&mut app),
        "root\n  column\n    label.shout = ann!\n  for\n    label.shout = Alpha!\n    \
         label.shout = Beta!\n  label.card = mounted\n"
    );
}

/// What a `<for>` row's component rendered, keyed by the node path of the
/// element the document writes it at.
///
/// The tree carries the row template, so nothing in it says what the call
/// produced for row one and what it produced for row two. This is where a
/// build reads that, and the paths are the ones the emitter and the browser
/// both number.
#[test]
fn a_row_component_body_is_read_off_the_world_per_row() {
    let _turn = in_turn();
    let mut app = run();

    let fills = row_fills(&mut app);

    assert_eq!(fills.block("0.1"), Some(("rows", 2)));
    assert_eq!(
        fills.body("0.1::0").and_then(|el| el.attrs.text.as_deref()),
        Some("Alpha!")
    );
    assert_eq!(
        fills.body("0.1::1").and_then(|el| el.attrs.text.as_deref()),
        Some("Beta!")
    );
    // The marker outside the block is the compiler's to inline into the tree,
    // so nothing here speaks for it.
    assert!(fills.body("0.0.0").is_none());
}

/// A block with no rows fills nothing and still records itself, so a page
/// written from a longer list is caught rather than written with bodies from
/// a list it does not show.
#[test]
fn an_empty_list_records_the_block_and_no_bodies() {
    let _turn = in_turn();
    let mut app = run_with(&Seed::new());

    let fills = row_fills(&mut app);

    assert_eq!(fills.block("0.1"), Some(("rows", 0)));
    assert!(fills.is_empty());
}

/// A row template naming two components, one of which the program does not
/// declare.
///
/// Each row position answers for itself: the call that built a body is
/// recorded at its own node path however the other went, and the one that
/// built nothing leaves its path empty, which is where the emitter reports it.
#[test]
fn a_row_records_a_body_only_where_the_call_built_one() {
    let _turn = in_turn();
    let mut ir = tree();
    let mut absent = marker("{title}");
    absent.tag = "Missing".to_string();
    absent
        .frag_use
        .as_mut()
        .expect("the marker carries a use site")
        .key = "Missing".to_string();
    ir.root.children[1].children.push(absent);
    let mut app = run_tree(ir, &seeded());

    let fills = row_fills(&mut app);

    // The rows are laid end to end, so a two-element template puts the second
    // row's first element two places along.
    assert!(fills.body("0.1::0").is_some(), "{}", dump(&mut app));
    assert!(fills.body("0.1::2").is_some(), "{}", dump(&mut app));
    assert!(fills.body("0.1::1").is_none(), "{}", dump(&mut app));
    assert!(fills.body("0.1::3").is_none(), "{}", dump(&mut app));
}

/// A component the build left a marker for is filled by calling the function,
/// so nothing carrying an unfilled marker survives the run.
#[test]
fn a_marker_is_filled_rather_than_left_standing() {
    let _turn = in_turn();
    let mut app = run();

    let left = app
        .world
        .query::<&lumen_core::components::PendingFill>()
        .iter(&app.world)
        .count();
    assert_eq!(left, 0, "{}", dump(&mut app));
}

/// A key the table does not hold builds nothing and does not stop the run:
/// what the rest of the app declares is still there.
#[test]
fn a_key_the_table_lost_builds_nothing_and_the_run_goes_on() {
    let _turn = in_turn();
    let mut app = {
        let mut compiled = compiled();
        compiled.fragments = FragmentTable::new();
        let mut booted = boot(
            &compiled,
            "index",
            &Seed::new(),
            Arc::new(DenyDispatch::default()),
        );
        settle(&mut booted.app, Budget::default());
        booted.app
    };

    let tree = dump(&mut app);
    assert!(!tree.contains("label"), "{tree}");
    assert!(
        tree.contains("column"),
        "the app itself still built: {tree}"
    );
}
