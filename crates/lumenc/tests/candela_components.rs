// Exercises the linked runtime via `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so a
// thin (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Proof that a candela component works end to end: an `lmn!` block compiles
//! to a fragment ahead of time, and instantiating it at run time builds the
//! same tree whether the app runs from source or from an artifact.
//!
//! Both authoring forms reach the one fragment, so both directions are
//! covered: markup writes a candela function as a tag, and a block writes a
//! `<template>` the markup declares.
//!
//! The artifact runs with no parser installed, so an identical tree is the
//! proof that nothing parses markup while the app is running.

use lumen_core::components::{LumenClasses, LumenId, LumenTag, TextContent};
use lumen_core::prelude::App;
use lumenc::{RunOptions, build_headless_app};
use std::path::{Path, PathBuf};

/// The DOM index and the external command bus are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn isolate() -> std::sync::MutexGuard<'static, ()> {
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lumen_core::node::publish_dom_index(lumen_core::node::DomIndex::default());
    guard
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/candela-components")
        .canonicalize()
        .expect("fixtures/candela-components must exist")
}

/// One line per element, indented by depth: `tag#id.class = text`. Read off
/// the world rather than the process-global DOM index, so two runs compare
/// exactly what each built.
fn dump(app: &mut App) -> String {
    let root = app
        .world
        .query_filtered::<bevy_ecs::entity::Entity, bevy_ecs::prelude::Without<bevy_ecs::hierarchy::ChildOf>>()
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

fn write_node(app: &App, entity: bevy_ecs::entity::Entity, depth: usize, out: &mut String) {
    let tag = app
        .world
        .get::<LumenTag>(entity)
        .map_or_else(|| "?".to_string(), |t| t.0.to_string());
    let id = app
        .world
        .get::<LumenId>(entity)
        .map_or_else(String::new, |i| format!("#{}", i.0));
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
        "{:indent$}{tag}{id}{classes}{text}\n",
        "",
        indent = depth * 2
    ));
    if let Some(children) = app.world.get::<bevy_ecs::hierarchy::Children>(entity) {
        for child in children.iter() {
            write_node(app, *child, depth + 1, out);
        }
    }
}

fn settle(app: &mut App) {
    for _ in 0..8 {
        app.tick();
    }
}

/// The tree the fixture builds, whichever way it was loaded.
const EXPECTED: &str = "\
root
  column#stage
    label.home = home for bob
  column#app
    label.home = home for bob
    column.rows
      for
        label.row = Row: Alpha
        label.row = Row: Beta
  column#wrap
    label.card = from a block
";

#[test]
fn a_component_tree_builds_from_source() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);
    assert_eq!(dump(&mut app), EXPECTED);
}

/// The same app, compiled and run from the artifact with no parser installed.
/// An identical tree is the proof that the fragments travelled compiled and
/// nothing parsed markup at run time. That covers the markup use site as well:
/// `<Home name="bob"/>` is already the block's body in the compiled tree.
#[test]
fn the_artifact_builds_the_same_tree_with_no_parser() {
    let _serial = isolate();
    let dir = fixture();
    let compiled = lumenc::compile_app(&dir).expect("the fixture compiles");
    assert!(
        compiled
            .fragments
            .iter()
            .any(|(_, f)| f.kind == lumen_ir::fragment::FragmentKind::Markup),
        "the artifact carries the blocks the script wrote"
    );
    assert!(
        compiled.fragments.by_component("Home").is_some(),
        "the artifact carries the name markup writes the component under"
    );
    let stage = compiled
        .ir
        .root
        .children
        .iter()
        .find(|e| e.attrs.id.as_deref() == Some("stage"))
        .expect("the stage is in the compiled tree");
    assert_eq!(
        stage.children[0].attrs.text.as_deref(),
        Some("home for bob"),
        "the markup use site is already the block's body"
    );
    let bytes = lumen_ir::artifact::serialize(&compiled).expect("artifact serializes");

    // `RunOptions::new` alone installs no parser; the artifact path needs none.
    let opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    let (mut app, _window) = build_headless_app(opts).expect("headless app");
    settle(&mut app);
    assert_eq!(dump(&mut app), EXPECTED);
}

/// An artifact whose table lost the key the script names instantiates
/// nothing, reports it, and keeps running.
#[test]
fn a_missing_key_on_the_artifact_path_instantiates_nothing() {
    let _serial = isolate();
    let dir = fixture();
    let mut compiled = lumenc::compile_app(&dir).expect("the fixture compiles");
    compiled.fragments = lumen_ir::fragment::FragmentTable::new();
    let bytes = lumen_ir::artifact::serialize(&compiled).expect("artifact serializes");

    let opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    let (mut app, _window) = build_headless_app(opts).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(!tree.contains("#app"), "nothing instantiated: {tree}");
    assert!(
        tree.contains("#stage"),
        "the app itself still built: {tree}"
    );
}

/// The block a component element stands in leaves a slot, and the node the
/// component returns takes the slot's place among its siblings.
#[test]
fn a_component_fills_the_slot_it_left_behind() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(
        !tree.contains("slot"),
        "every slot was filled or dropped: {tree}"
    );
    let home = tree.find("label.home").expect("Home instantiated");
    let rows = tree.find("column.rows").expect("Rows instantiated");
    assert!(home < rows, "children keep their source order: {tree}");
}

/// A child's instantiation reaches the applier before its parent's: candela
/// evaluates the call's arguments first, so the command the child pushed is
/// already in the sink when the parent's goes in.
#[test]
fn a_child_is_instantiated_before_its_parent() {
    let _serial = isolate();
    let dir = fixture();
    let source = std::fs::read_to_string(dir.join("main.cdl")).expect("read main.cdl");
    let index = lumen_script_candela::lmn::FnIndex::scan(&source);
    let body = "<column id=\"app\"><Home name=\"bob\"/></column>";
    let expansion = lumen_script_candela::lmn::expand(body, &index).expect("expands");
    let child = expansion.find("Home(").expect("the child call");
    let parent = expansion
        .find("lumen::fragment_spawn")
        .expect("the parent call");
    assert!(
        parent < child,
        "the child sits in the parent's argument list, so it runs first: {expansion}"
    );
}

/// A `<for>` inside a block reads the record from the reconciler and the
/// prefix from the argument the instance was built with.
#[test]
fn a_for_inside_a_block_resolves_rows_and_arguments() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(tree.contains("label.row = Row: Alpha"), "{tree}");
    assert!(tree.contains("label.row = Row: Beta"), "{tree}");
}

/// Markup writes a candela function as a tag, and the argument it passes
/// reaches the block's `$name` slot.
#[test]
fn markup_instantiates_a_component_with_an_argument() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    let stage = tree
        .lines()
        .skip_while(|line| line.trim() != "column#stage")
        .nth(1)
        .expect("the stage has a child");
    assert_eq!(stage.trim(), "label.home = home for bob", "{tree}");
}

/// One component reached both ways builds one subtree. The markup use site
/// inlines the block, the `lmn!` call site instantiates it, and neither is a
/// different entity.
#[test]
fn a_component_builds_the_same_subtree_from_markup_and_from_a_block() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    let built: Vec<&str> = tree
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("label.home"))
        .collect();
    assert_eq!(
        built,
        ["label.home = home for bob", "label.home = home for bob"],
        "{tree}"
    );
}

/// A block instantiates a `<template>` the markup declares, and the argument
/// it passes reaches the template's marker.
#[test]
fn a_block_instantiates_a_markup_template() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(tree.contains("label.card = from a block"), "{tree}");
}

/// Write an app directory under a name of its own and check it.
fn check(name: &str, markup: &str, script: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("lumen_lmn_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[script]\nengine = \"candela\"\n\n[mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    std::fs::write(dir.join("main.lmn"), markup).expect("write main.lmn");
    std::fs::write(dir.join("main.cdl"), script).expect("write main.cdl");
    let result = lumenc::check_app(&dir)
        .map(|_| ())
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Markup inlines the block a component returns, so a function that returns
/// more than one has nothing to inline. The compiler says so instead of
/// picking a branch.
#[test]
fn a_component_with_two_blocks_cannot_be_used_from_markup() {
    let _serial = isolate();
    let err = check(
        "two_blocks",
        "<root><column id=\"app\"><Toggle on=\"yes\"/></column>\
         <script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\n\
         fn Toggle(on) {\n\
             if on == \"yes\" { return lmn!(<label text=\"on\"/>); }\n\
             return lmn!(<label text=\"off\"/>);\n\
         }\n\
         fn main() {}\n",
    )
    .expect_err("Toggle has no single block to inline");
    assert!(err.contains("Toggle"), "{err}");
    assert!(err.contains("single `return lmn!(...)`"), "{err}");
}

/// A `<template>` and a candela component claiming one name leaves a use site
/// with nothing to pick between. Both declarations are named.
#[test]
fn a_component_colliding_with_a_template_is_reported_against_both() {
    let _serial = isolate();
    let err = check(
        "collision",
        "<root><template name=\"Home\"><label text=\"from markup\"/></template>\
         <column id=\"app\"><Home/></column><script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\n\
         fn Home() { return lmn!(<label text=\"from a block\"/>); }\n\
         fn main() {}\n",
    )
    .expect_err("two declarations claim `Home`");
    assert!(err.contains("`Home` is declared twice"), "{err}");
    // Both sites, each with the position that finds it.
    assert!(err.contains("main.lmn:1:"), "{err}");
    assert!(err.contains("main.cdl:2:"), "{err}");
}

/// A capitalized tag naming nothing is still an error, not an empty node.
#[test]
fn a_capital_tag_naming_no_component_still_errors() {
    let _serial = isolate();
    let err = check(
        "unknown",
        "<root><column id=\"app\"><Nowhere/></column><script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\nfn main() {}\n",
    )
    .expect_err("Nowhere names nothing");
    assert!(err.contains("Nowhere"), "{err}");
}

/// Editing the script re-extracts its blocks and re-mounts what they build.
#[test]
fn a_hot_reload_re_extracts_and_re_mounts() {
    let _serial = isolate();
    let dir = std::env::temp_dir().join(format!("lumen_lmn_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    let fixture = fixture();
    for name in ["lumen.toml", "main.lmn", "main.cdl"] {
        std::fs::copy(fixture.join(name), dir.join(name)).expect("copy fixture file");
    }

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = true;
    let (mut app, _window) = build_headless_app(opts).expect("headless app");
    settle(&mut app);
    assert!(dump(&mut app).contains("home for bob"));

    let edited = std::fs::read_to_string(dir.join("main.cdl"))
        .expect("read the script")
        .replace("home for $name", "welcome, $name");
    std::fs::write(dir.join("main.cdl"), edited).expect("write the script");
    // The watcher's fallback driver polls on a wall-clock interval, so give it
    // one before ticking the reload through.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut tree = dump(&mut app);
    while !tree.contains("welcome, bob") && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        settle(&mut app);
        tree = dump(&mut app);
    }

    // Both use sites, the one in the markup and the one in the block, come
    // from the edited fragment.
    assert_eq!(
        tree.matches("welcome, bob").count(),
        2,
        "the edited block re-mounted everywhere it is named: {tree}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
