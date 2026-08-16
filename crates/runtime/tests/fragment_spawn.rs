//! Headless proof that a compiled fragment instantiates at run time.
//!
//! Each test builds a window-free app from a hand-assembled artifact that
//! carries a fragment table, pushes `SpawnFragment` on the host-neutral DOM
//! command bus (the seam the C-ABI and the SDKs use), and reads the tree
//! back. No script host is involved: the command is the whole surface this
//! wave adds, and the script builtin that will issue it lands later.

use lumen_core::components::{Fill, Visuals};
use lumen_core::signals::{ArrayItem, push_external_array};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::css::{Declaration, Origin, Rule, Stylesheet};
use lumen_ir::fragment::{Fragment, FragmentKind, FragmentParam, FragmentTable, SLOT_TAG};
use lumen_ir::layout_ir::{Attributes, Element, InterpolationSlot, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::ScriptCommand;
use lumen_script::node_query::{self, build_spawn_fragment, push_external_dom_command};

/// The DOM snapshot, the external command bus, and the signal channel are
/// process-global, so the headless apps that read and write them run one at
/// a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn el(tag: &str, attrs: Attributes, children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs,
        children,
        ..Default::default()
    }
}

/// An element whose placeholders the compiler classified for it.
fn interpolated(tag: &str, attrs: Attributes, slots: Vec<InterpolationSlot>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs,
        interpolations: slots,
        ..Default::default()
    }
}

fn text(value: &str) -> Attributes {
    Attributes {
        text: Some(value.to_string()),
        ..Attributes::default()
    }
}

fn param(name: &str, default: Option<&str>) -> FragmentParam {
    FragmentParam {
        name: name.to_string(),
        default: default.map(str::to_string),
    }
}

fn fragment(key: &str, params: Vec<FragmentParam>, body: Vec<Element>) -> Fragment {
    Fragment {
        key: key.to_string(),
        params,
        body,
        origins: Vec::new(),
        kind: FragmentKind::Template,
        components: Vec::new(),
    }
}

fn table(fragments: Vec<Fragment>) -> FragmentTable {
    let mut table = FragmentTable::new();
    for f in fragments {
        table.insert(f).expect("distinct keys");
    }
    table
}

/// One rule from a selector string and `(property, value)` pairs.
fn rule(selector: &str, decls: &[(&str, &str)]) -> Rule {
    Rule {
        selectors: lumen_ir::css::parse_selector_list(selector).expect("selector parses"),
        declarations: decls
            .iter()
            .map(|(name, value)| Declaration {
                name: (*name).to_string(),
                value: (*value).to_string(),
                important: false,
            })
            .collect(),
        origin: Origin::Author,
        source_order: 0,
        media: None,
        selector: Default::default(),
    }
}

struct Harness {
    app: lumen_core::app::App,
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(fragments: FragmentTable) -> Self {
        Self::styled(fragments, Vec::new())
    }

    /// A `root > column.list` app carrying `fragments` and `rules`.
    fn styled(fragments: FragmentTable, rules: Vec<Rule>) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _ = node_query::drain_external_dom_commands();
        let dir = std::env::temp_dir().join(format!("lumen_frag_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
        std::fs::create_dir_all(&dir).expect("temp app dir");
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("lumen.toml");
        let list = Element {
            tag: "column".to_string(),
            attrs: Attributes {
                classes: vec!["list".to_string()],
                ..Attributes::default()
            },
            ..Default::default()
        };
        let ir = LayoutIR {
            root: el("root", Attributes::default(), vec![list]),
            combined_stylesheet: (!rules.is_empty()).then_some(Stylesheet { rules }),
            ..Default::default()
        };
        let bytes = artifact::serialize(&CompiledApp {
            ir,
            fragments,
            ..Default::default()
        })
        .expect("artifact serializes");
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _window) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness {
            app,
            dir,
            _guard: guard,
        };
        h.settle();
        h
    }

    /// Tick enough for a pushed batch to apply and for the next snapshot to
    /// reflect it (apply runs mid-tick, publish runs at the start).
    fn settle(&mut self) {
        for _ in 0..4 {
            self.app.tick();
        }
    }

    fn list(&self) -> u64 {
        node_query::run_query(".list")
            .expect("selector parses")
            .single()
            .expect("the app has a list column")
    }

    fn entity(&self, handle: u64) -> bevy_ecs::entity::Entity {
        lumen_core::node::NodeHandle::unpack(handle)
            .expect("live handle")
            .entity
    }

    fn set_global(&mut self, name: &str, value: &str) {
        self.app
            .world
            .get_resource_mut::<lumen_core::property_store::PropertyStore>()
            .expect("the property store is installed")
            .set_global_str(name, value);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Push a `SpawnFragment` for `key` and return the handle it reserved.
fn spawn_fragment(key: &str, args: &[(&str, &str)], children: &[(&str, u64)]) -> u64 {
    let (handle, cmd) = build_spawn_fragment(
        key,
        args.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        children
            .iter()
            .map(|(slot, node)| ((*slot).to_string(), *node))
            .collect(),
    );
    push_external_dom_command(cmd);
    handle
}

/// Whether a fill matches an `#rrggbb` literal, with the tolerance the
/// cascade's byte-to-f32 conversion needs.
fn near(a: lumen_core::components::Color, hex: &str) -> bool {
    let byte = |i: usize| {
        u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).expect("#rrggbb literal") as f32 / 255.0
    };
    (a.r - byte(0)).abs() < 0.01 && (a.g - byte(1)).abs() < 0.01 && (a.b - byte(2)).abs() < 0.01
}

/// An instance is born detached, the way `spawn(tag)` is, and joins the tree
/// only when something inserts it.
#[test]
fn an_instance_spawns_detached_and_a_later_insert_attaches_it() {
    let card = fragment(
        "card",
        Vec::new(),
        vec![el(
            "column",
            Attributes {
                id: Some("card-root".to_string()),
                ..Attributes::default()
            },
            vec![el("label", text("Recent"), Vec::new())],
        )],
    );
    let mut h = Harness::new(table(vec![card]));
    let list = h.list();

    spawn_fragment("card", &[], &[]);
    h.settle();
    let root = node_query::run_get_by_id("card-root").expect("the instance is in the tree");
    assert_eq!(node_query::node_parent(root), None, "born detached");
    assert_eq!(
        node_query::node_text(node_query::node_first_child(root).expect("the body child"))
            .as_deref(),
        Some("Recent"),
        "the whole body spawned, not just the root"
    );

    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: root,
        before: 0,
    });
    h.settle();
    assert_eq!(node_query::node_parent(root), Some(list));
}

/// The handle the command reserved addresses the instance in the same tick,
/// so a chain can insert it without waiting for a round trip.
#[test]
fn the_reserved_handle_addresses_the_instance_in_the_same_tick() {
    let card = fragment(
        "card",
        Vec::new(),
        vec![el("column", Attributes::default(), Vec::new())],
    );
    let mut h = Harness::new(table(vec![card]));
    let list = h.list();

    let instance = spawn_fragment("card", &[], &[]);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: instance,
        name: "id".into(),
        value: "same-tick".into(),
    });
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    let root = node_query::run_get_by_id("same-tick").expect("the reserved handle resolved");
    assert_eq!(node_query::node_parent(root), Some(list));
}

/// Arguments reach every string-valued attribute the substitution walk
/// covers, not only text.
#[test]
fn arguments_land_in_text_id_class_and_src() {
    let body = el(
        "column",
        Attributes::default(),
        vec![
            interpolated(
                "label",
                Attributes {
                    text: Some("{$title}".to_string()),
                    id: Some("{$slug}-label".to_string()),
                    classes: vec!["chip-{$tone}".to_string()],
                    ..Attributes::default()
                },
                vec![
                    InterpolationSlot::Arg("title".to_string()),
                    InterpolationSlot::Arg("slug".to_string()),
                    InterpolationSlot::Arg("tone".to_string()),
                ],
            ),
            interpolated(
                "image",
                Attributes {
                    src: Some("icons/{$icon}.png".to_string()),
                    id: Some("art".to_string()),
                    ..Attributes::default()
                },
                vec![InterpolationSlot::Arg("icon".to_string())],
            ),
        ],
    );
    let card = fragment(
        "card",
        vec![
            param("title", None),
            param("slug", None),
            param("tone", None),
            param("icon", None),
        ],
        vec![body],
    );
    let mut h = Harness::new(table(vec![card]));

    spawn_fragment(
        "card",
        &[
            ("title", "Recent"),
            ("slug", "recent"),
            ("tone", "warm"),
            ("icon", "sun"),
        ],
        &[],
    );
    h.settle();

    let label = node_query::run_get_by_id("recent-label").expect("the id argument landed");
    assert_eq!(node_query::node_text(label).as_deref(), Some("Recent"));
    assert!(node_query::node_class_contains(label, "chip-warm"));

    let art = h.entity(node_query::run_get_by_id("art").expect("the image spawned"));
    let source = h
        .app
        .world
        .get::<lumen_assets::ImageSource>(art)
        .expect("the image carries its source");
    assert_eq!(source.0, std::path::PathBuf::from("icons/sun.png"));
}

/// Inside a body, a parameter wins over a global signal spelled the same.
#[test]
fn an_argument_shadows_a_global_signal_of_the_same_name() {
    let card = fragment(
        "card",
        vec![param("title", None)],
        vec![interpolated(
            "label",
            Attributes {
                text: Some("{$title}".to_string()),
                id: Some("shadowed".to_string()),
                ..Attributes::default()
            },
            vec![InterpolationSlot::Arg("title".to_string())],
        )],
    );
    let mut h = Harness::new(table(vec![card]));
    h.set_global("title", "from the signal");

    spawn_fragment("card", &[("title", "from the argument")], &[]);
    h.settle();

    let label = node_query::run_get_by_id("shadowed").expect("the instance spawned");
    assert_eq!(
        node_query::node_text(label).as_deref(),
        Some("from the argument")
    );
}

/// A declared default stands in for an argument the use site omits.
#[test]
fn a_declared_default_fills_an_omitted_argument() {
    let card = fragment(
        "card",
        vec![param("title", Some("Untitled"))],
        vec![interpolated(
            "label",
            Attributes {
                text: Some("{$title}".to_string()),
                id: Some("defaulted".to_string()),
                ..Attributes::default()
            },
            vec![InterpolationSlot::Arg("title".to_string())],
        )],
    );
    let mut h = Harness::new(table(vec![card]));

    spawn_fragment("card", &[], &[]);
    h.settle();

    let label = node_query::run_get_by_id("defaulted").expect("the instance spawned");
    assert_eq!(node_query::node_text(label).as_deref(), Some("Untitled"));
}

/// A parameter with neither an argument nor a default resolves empty rather
/// than leaving the placeholder to render as literal text.
#[test]
fn a_parameter_with_no_argument_and_no_default_resolves_empty() {
    let card = fragment(
        "card",
        vec![param("title", None)],
        vec![interpolated(
            "label",
            Attributes {
                text: Some("{$title}".to_string()),
                id: Some("empty".to_string()),
                ..Attributes::default()
            },
            vec![InterpolationSlot::Arg("title".to_string())],
        )],
    );
    let mut h = Harness::new(table(vec![card]));

    spawn_fragment("card", &[], &[]);
    h.settle();

    let label = node_query::run_get_by_id("empty").expect("the instance spawned");
    assert_eq!(node_query::node_text(label).as_deref(), Some(""));
}

/// A child passed for a slot takes the slot's place among its siblings, and
/// the slot element itself is gone.
#[test]
fn slot_children_land_where_the_slot_sat() {
    let body = el(
        "column",
        Attributes {
            id: Some("frame".to_string()),
            ..Attributes::default()
        },
        vec![
            el(
                "label",
                Attributes {
                    text: Some("head".to_string()),
                    id: Some("head".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            ),
            el(SLOT_TAG, Attributes::default(), Vec::new()),
            el(
                "label",
                Attributes {
                    text: Some("tail".to_string()),
                    id: Some("tail".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            ),
        ],
    );
    let mut h = Harness::new(table(vec![fragment("card", Vec::new(), vec![body])]));

    let (child, spawn) = node_query::build_spawn("button");
    push_external_dom_command(spawn);
    push_external_dom_command(ScriptCommand::SetAttr {
        node: child,
        name: "id".into(),
        value: "passed".into(),
    });
    let instance = spawn_fragment("card", &[], &[("default", child)]);
    let list = h.list();
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    let frame = node_query::run_get_by_id("frame").expect("the instance spawned");
    let passed = node_query::run_get_by_id("passed").expect("the child survived");
    let head = node_query::run_get_by_id("head").expect("head");
    let tail = node_query::run_get_by_id("tail").expect("tail");
    assert_eq!(
        node_query::node_children(frame),
        vec![head, passed, tail],
        "the child sits where the slot did"
    );
}

/// A slot nothing fills keeps the fallback content the body wrote inside it.
#[test]
fn an_unfilled_slot_keeps_its_fallback_content() {
    let body = el(
        "column",
        Attributes {
            id: Some("frame".to_string()),
            ..Attributes::default()
        },
        vec![el(
            SLOT_TAG,
            Attributes::default(),
            vec![el(
                "label",
                Attributes {
                    text: Some("Empty".to_string()),
                    id: Some("fallback".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            )],
        )],
    );
    let mut h = Harness::new(table(vec![fragment("card", Vec::new(), vec![body])]));

    spawn_fragment("card", &[], &[]);
    h.settle();

    let fallback = node_query::run_get_by_id("fallback").expect("the fallback spawned");
    assert_eq!(node_query::node_text(fallback).as_deref(), Some("Empty"));
}

/// A key nothing declares reports and spawns nothing; the reserved handle
/// never resolves, so the commands chained onto it are inert and the app
/// keeps running.
#[test]
fn an_unknown_key_leaves_the_world_intact() {
    let card = fragment(
        "card",
        Vec::new(),
        vec![el("column", Attributes::default(), Vec::new())],
    );
    let mut h = Harness::new(table(vec![card]));
    let list = h.list();

    let instance = spawn_fragment("nope", &[], &[]);
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    assert!(node_query::node_children(list).is_empty());
    assert!(node_query::node_valid(list), "the world is still sound");
}

/// Instantiation returns one node, so a body with several roots does not
/// instantiate at all rather than inventing a wrapper for them.
#[test]
fn a_body_with_several_roots_does_not_instantiate() {
    let card = fragment(
        "card",
        Vec::new(),
        vec![
            el(
                "label",
                Attributes {
                    id: Some("first".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            ),
            el(
                "label",
                Attributes {
                    id: Some("second".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            ),
        ],
    );
    let mut h = Harness::new(table(vec![card]));
    let list = h.list();

    let instance = spawn_fragment("card", &[], &[]);
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    assert!(node_query::run_get_by_id("first").is_none());
    assert!(node_query::run_get_by_id("second").is_none());
    assert!(node_query::node_children(list).is_empty());
}

/// A class written in the body cascades once the instance is in the tree:
/// the applier marks styling dirty and the re-resolver does the rest.
#[test]
fn a_class_in_the_body_cascades_after_instantiation() {
    let card = fragment(
        "card",
        Vec::new(),
        vec![el(
            "tile",
            Attributes {
                id: Some("chip".to_string()),
                classes: vec!["chip".to_string()],
                ..Attributes::default()
            },
            Vec::new(),
        )],
    );
    let mut h = Harness::styled(table(vec![card]), vec![rule(".chip", &[("bg", "#112233")])]);
    let list = h.list();

    let instance = spawn_fragment("card", &[], &[]);
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    let chip = h.entity(node_query::run_get_by_id("chip").expect("the instance spawned"));
    let fill = h
        .app
        .world
        .get::<Visuals>(chip)
        .and_then(|v| v.fill.as_ref())
        .and_then(Fill::as_solid)
        .expect("the cascade painted the chip");
    assert!(
        near(fill, "#112233"),
        "the stylesheet rule reached the instance"
    );
}

/// A `<for>` inside a body meets two scopes: the argument walk resolves what
/// the instance was built with and leaves the row placeholders standing, and
/// the reconciler resolves those per row.
#[test]
fn a_for_inside_a_body_resolves_row_and_argument_placeholders() {
    let row_label = interpolated(
        "label",
        Attributes {
            text: Some("{$prefix}: {row.name}".to_string()),
            classes: vec!["row".to_string()],
            ..Attributes::default()
        },
        vec![
            InterpolationSlot::Arg("prefix".to_string()),
            InterpolationSlot::Row("name".to_string()),
        ],
    );
    let body = el(
        "column",
        Attributes::default(),
        vec![el(
            "for",
            Attributes {
                each: Some("rows".to_string()),
                ..Attributes::default()
            },
            vec![row_label],
        )],
    );
    let card = fragment("card", vec![param("prefix", None)], vec![body]);
    let mut h = Harness::new(table(vec![card]));

    let mut alpha = ArrayItem::new();
    alpha.insert("name".to_string(), "alpha".to_string());
    let mut beta = ArrayItem::new();
    beta.insert("name".to_string(), "beta".to_string());
    push_external_array("rows", vec![alpha, beta]);
    h.settle();

    let instance = spawn_fragment("card", &[("prefix", "Row")], &[]);
    let list = h.list();
    push_external_dom_command(ScriptCommand::Insert {
        parent: list,
        node: instance,
        before: 0,
    });
    h.settle();

    let rows = node_query::run_query(".row")
        .expect("selector parses")
        .collect();
    let texts: Vec<String> = rows
        .iter()
        .filter_map(|r| node_query::node_text(*r))
        .collect();
    assert_eq!(
        texts,
        vec!["Row: alpha".to_string(), "Row: beta".to_string()]
    );
}
