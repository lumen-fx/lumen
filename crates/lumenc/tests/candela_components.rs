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
///
/// Everything a use site names is here, including the three the build could
/// not stand in for: `settle` runs enough ticks for the script to fill them.
const EXPECTED: &str = "\
root
  column#stage
    label.home = home for bob
    column.outer
      label.inner = in x
    label.inner = in y
    label.shout = hey ann!
    label.arm = on
    label.arm = off
  column#app
    label.home = home for bob
    column.rows
      for
        label.row = Row: Alpha
        label.row = Row: Beta
  column#wrap
    label.card = from a block
";

/// [`EXPECTED`] with the root line a headless run produces: the OS theme
/// follow writes the effective theme class onto the root on the first tick,
/// which the windowless prerender assembly (no theme follow) does not.
fn themed(expected: &str) -> String {
    expected.replacen("root\n", "root.theme-light\n", 1)
}

#[test]
fn a_component_tree_builds_from_source() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);
    assert_eq!(dump(&mut app), themed(EXPECTED));
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
    assert_eq!(dump(&mut app), themed(EXPECTED));
}

/// The same artifact through the assembly that has no window.
///
/// A browser page, a prehydration run and a server render all build the app
/// from `lumen-portable` rather than from the runtime a window uses. Nothing
/// about a fragment is a windowing question, so the tree that comes out is the
/// tree above or the two halves have drifted.
#[cfg(feature = "web")]
#[test]
fn the_windowless_assembly_builds_the_same_tree() {
    use std::sync::Arc;

    let _serial = isolate();
    let compiled = lumenc::compile_app(&fixture()).expect("the fixture compiles");
    let mut booted = lumen_prerender::boot(
        &compiled,
        "main",
        &lumen_html::contract::Seed::new(),
        Arc::new(lumen_prerender::DenyDispatch::default()),
    );
    lumen_prerender::settle(&mut booted.app, lumen_prerender::Budget::default());

    assert_eq!(dump(&mut booted.app), EXPECTED);
}

/// The tree a site is emitted from, once the build has filled the components
/// that have to run.
#[cfg(feature = "web")]
fn filled() -> lumen_ir::artifact::CompiledApp {
    let mut compiled = lumenc::compile_app(&fixture()).expect("the fixture compiles");
    let mut warnings = Vec::new();
    lumenc::component_fill::fill(&mut compiled, "main", &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    compiled
}

/// Filling a component while the site is built leaves the tree the runtime
/// would have built anyway.
///
/// This is the join between the two halves. The emitter writes this tree into
/// the document, and the browser spawns this tree out of the artifact beside
/// it; if filling produced anything other than what the call produces at run
/// time, the page and the app it hydrates into would disagree from the first
/// frame.
#[cfg(feature = "web")]
#[test]
fn a_filled_tree_still_builds_what_the_runtime_builds() {
    use std::sync::Arc;

    let _serial = isolate();
    let compiled = filled();
    assert!(
        !holds_marker(&compiled.ir.root),
        "every component the build can run is its body by now"
    );

    let mut booted = lumen_prerender::boot(
        &compiled,
        "main",
        &lumen_html::contract::Seed::new(),
        Arc::new(lumen_prerender::DenyDispatch::default()),
    );
    lumen_prerender::settle(&mut booted.app, lumen_prerender::Budget::default());

    assert_eq!(dump(&mut booted.app), EXPECTED);
}

/// Whether anything under `element` still stands in for a component.
#[cfg(feature = "web")]
fn holds_marker(element: &lumen_ir::layout_ir::Element) -> bool {
    element.frag_use.is_some() || element.children.iter().any(holds_marker)
}

/// What a crawler is served: the body of every component, in the document
/// itself, with no box left for a browser to fill.
///
/// Written from the same tree the test above spawns, so the paths the emitter
/// numbers and the paths the runtime derives come from one source.
#[cfg(feature = "web")]
#[test]
fn the_emitted_page_carries_every_component_body() {
    let _serial = isolate();
    let compiled = filled();
    let spec = lumen_web::SiteSpec {
        pages: vec![lumen_web::PageSpec::new("main", compiled.ir.clone())],
        web: lumen_web::WebSpec {
            runtime: false,
            ..lumen_web::WebSpec::default()
        },
        ..lumen_web::SiteSpec::default()
    };
    let mut warnings = Vec::new();
    let html = lumen_web::html::emit_tree(&spec.pages[0], &spec, &mut warnings)
        .expect("the filled tree emits");

    // The three the build had to run for, each with the value its call worked
    // out rather than the box it used to leave.
    assert!(html.contains("hey ann!"), "{html}");
    assert!(html.contains(">on<"), "{html}");
    assert!(html.contains(">off<"), "{html}");
    // And the ones it could stand in for, which were never in doubt.
    assert!(html.contains("home for bob"), "{html}");
    assert!(html.contains("in x"), "{html}");
    assert!(
        !html.contains("lm-fragment"),
        "no component is left as a box: {html}"
    );
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

/// A marker the loaded program cannot fill leaves an empty element, which is
/// the failure a reader would not notice. It is reported and the marker
/// dropped, rather than retried every tick against a program that will never
/// have it.
#[test]
fn a_marker_no_script_can_fill_is_reported_and_dropped() {
    let _serial = isolate();
    let dir = fixture();
    let mut compiled = lumenc::compile_app(&dir).expect("the fixture compiles");
    // Ship the artifact with a program that lost the functions its tree names,
    // which is what a tampered or half-built artifact looks like.
    let stripped = "import \"lumen.cdl\";\nfn main() {}\n";
    compiled.script_source = stripped.to_string();
    for script in &mut compiled.scripts {
        script.source = stripped.to_string();
        script.bytecode = None;
    }
    let bytes = lumen_ir::artifact::serialize(&compiled).expect("artifact serializes");

    let opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    let (mut app, _window) = build_headless_app(opts).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(
        !tree.contains("hey ann!"),
        "nothing filled the marker: {tree}"
    );
    assert!(
        tree.contains("label.home = home for bob"),
        "what the build baked is still there: {tree}"
    );
    let left = app
        .world
        .query::<&lumen_core::components::PendingFill>()
        .iter(&app.world)
        .count();
    assert_eq!(left, 0, "the marker was dropped rather than retried");
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

/// A component element is a use site in the fragment, not a call in the
/// expansion. The build resolves it, so a block naming a component the build
/// can stand in for expands to nothing but its own instantiation.
#[test]
fn a_block_naming_a_component_expands_to_no_call() {
    let _serial = isolate();
    let dir = fixture();
    let source = std::fs::read_to_string(dir.join("src").join("main.cdl")).expect("read main.cdl");
    let index = lumen_script_candela::lmn::FnIndex::scan(&source);
    let body = "<column id=\"app\"><Home name=\"bob\"/></column>";
    let expansion = lumen_script_candela::lmn::expand(body, &index).expect("expands");
    assert!(
        !expansion.contains("Home("),
        "the component is a use site, not a call: {expansion}"
    );
    assert!(
        expansion.contains("lumen::fragment_spawn"),
        "the block still instantiates itself: {expansion}"
    );
}

/// Two levels deep with the prop forwarded down, and nothing to work out at
/// either level, so the whole subtree is in the compiled tree.
#[test]
fn a_forwarded_prop_reaches_two_levels_down_at_build_time() {
    let _serial = isolate();
    let compiled = lumenc::compile_app(&fixture()).expect("the fixture compiles");
    let stage = stage_of(&compiled.ir.root);
    let outer = stage
        .iter()
        .find(|e| e.attrs.classes.iter().any(|c| c == "outer"))
        .expect("the outer column is baked");
    assert_eq!(
        outer.children[0].attrs.text.as_deref(),
        Some("in x"),
        "the inner label is baked with the forwarded prop"
    );
    assert!(
        outer.children[0].frag_use.is_none(),
        "nothing is left for the script to fill"
    );
}

/// A body that is one component element is a fragment whose root is that use
/// site, so naming the enclosing function from markup reaches through it.
#[test]
fn a_pass_through_component_is_usable_from_markup() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);

    let tree = dump(&mut app);
    assert!(tree.contains("label.inner = in y"), "{tree}");
}

/// A component that works a value out is not baked whole: the build leaves a
/// marker naming it, and the runtime fills that by calling the function.
#[test]
fn a_computing_component_is_filled_by_calling_it() {
    let _serial = isolate();
    let compiled = lumenc::compile_app(&fixture()).expect("the fixture compiles");
    let marker = stage_of(&compiled.ir.root)
        .iter()
        .find_map(|e| e.frag_use.as_ref().filter(|u| u.key == "Shout"))
        .expect("Shout stays in the tree as a marker");
    assert_eq!(
        marker.args,
        [("who".to_string(), "ann".to_string())],
        "the marker carries the argument in parameter order"
    );

    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);
    assert!(
        dump(&mut app).contains("label.shout = hey ann!"),
        "the call worked the value out and filled the marker"
    );
}

/// The whole tree is there on the first tick, filled parts included: the fill
/// runs before the command applier, so what it builds lands on the tick it was
/// mounted. Timing a reader can see, so it is pinned rather than described.
#[test]
fn the_tree_is_whole_on_the_first_tick() {
    let _serial = isolate();
    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    app.tick();
    let stage: String = dump(&mut app)
        .lines()
        .skip_while(|line| line.trim() != "column#stage")
        .take_while(|line| line.starts_with("    ") || line.trim() == "column#stage")
        .map(|line| format!("{}\n", line.trim()))
        .collect();
    assert_eq!(
        stage,
        "column#stage\n\
         label.home = home for bob\n\
         column.outer\n\
         label.inner = in x\n\
         label.inner = in y\n\
         label.shout = hey ann!\n\
         label.arm = on\n\
         label.arm = off\n"
    );
}

/// Both arms of a conditional component are compiled; the call picks which one
/// is used, so the same component renders differently per use site.
#[test]
fn a_conditional_component_picks_each_arm_from_markup() {
    let _serial = isolate();
    let compiled = lumenc::compile_app(&fixture()).expect("the fixture compiles");
    let arms = compiled
        .fragments
        .iter()
        .filter(|(_, f)| f.components.iter().any(|c| c.name == "Pick"))
        .count();
    assert_eq!(arms, 2, "both arms travel in the artifact");

    let (mut app, _window) = build_headless_app(RunOptions::new(fixture())).expect("headless app");
    settle(&mut app);
    let tree = dump(&mut app);
    assert!(tree.contains("label.arm = on"), "{tree}");
    assert!(tree.contains("label.arm = off"), "{tree}");
}

/// The children of the fixture's `#stage`, which is where its markup use
/// sites are written.
fn stage_of(root: &lumen_ir::layout_ir::Element) -> &[lumen_ir::layout_ir::Element] {
    &root
        .children
        .iter()
        .find(|e| e.attrs.id.as_deref() == Some("stage"))
        .expect("the fixture has a stage")
        .children
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
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[script]\nengine = \"candela\"\n\n[mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    std::fs::write(src.join("main.lmn"), markup).expect("write main.lmn");
    std::fs::write(src.join("main.cdl"), script).expect("write main.cdl");
    let result = lumenc::check_app(&dir)
        .map(|_| ())
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A component naming a prop the function does not declare reaches nothing,
/// so the build says which parameters it has instead.
#[test]
fn a_prop_naming_no_parameter_is_refused_at_a_markup_use_site() {
    let _serial = isolate();
    let err = check(
        "bad_prop",
        "<root><column id=\"app\"><Shout title=\"x\"/></column>\
         <script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\n\
         fn Shout(who) {\n\
             let loud = who;\n\
             return lmn!(<label text=\"$loud\"/>);\n\
         }\n\
         fn main() {}\n",
    )
    .expect_err("Shout has no parameter `title`");
    assert!(err.contains("Shout"), "{err}");
    assert!(err.contains("title"), "{err}");
    assert!(err.contains("who"), "{err}");
}

/// A component that reaches itself, directly or through another, would build
/// forever. Rejected when the table is built, not discovered at run time, and
/// named the way the author wrote it rather than by content key.
#[test]
fn a_component_cycle_is_rejected_at_build_time() {
    let _serial = isolate();
    let err = check(
        "cycle",
        "<root><column id=\"app\"><Outer/></column><script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\n\
         fn Inner() { return lmn!(<column><Outer/></column>); }\n\
         fn Outer() { return lmn!(<column><Inner/></column>); }\n\
         fn main() {}\n",
    )
    .expect_err("Outer reaches itself");
    assert!(err.contains("instantiates itself"), "{err}");
    assert!(err.contains("Outer -> Inner -> Outer"), "{err}");
}

/// The same cycle through a component that has to run would recurse while the
/// app is running rather than while it is building. Rejected just the same.
#[test]
fn a_cycle_through_a_component_that_runs_is_rejected_too() {
    let _serial = isolate();
    let err = check(
        "cycle_running",
        "<root><column id=\"app\"><Outer/></column><script src=\"main.cdl\"/></root>",
        "import \"lumen.cdl\";\n\
         fn Inner() { let x = 1; return lmn!(<column><Outer/></column>); }\n\
         fn Outer() { return lmn!(<column><Inner/></column>); }\n\
         fn main() {}\n",
    )
    .expect_err("Outer reaches itself");
    assert!(err.contains("instantiates itself"), "{err}");
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
    std::fs::create_dir_all(dir.join("src")).expect("temp app dir");
    let fixture = fixture();
    std::fs::copy(fixture.join("lumen.toml"), dir.join("lumen.toml")).expect("copy the config");
    for name in ["main.lmn", "main.cdl"] {
        std::fs::copy(fixture.join("src").join(name), dir.join("src").join(name))
            .expect("copy fixture file");
    }

    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = true;
    let (mut app, _window) = build_headless_app(opts).expect("headless app");
    settle(&mut app);
    assert!(dump(&mut app).contains("home for bob"));

    let script = dir.join("src").join("main.cdl");
    let edited = std::fs::read_to_string(&script)
        .expect("read the script")
        .replace("home for $name", "welcome, $name");
    std::fs::write(&script, edited).expect("write the script");
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
