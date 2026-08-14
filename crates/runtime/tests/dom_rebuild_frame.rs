//! A subtree a script rebuilds is styled in the frame it appears.
//!
//! Typing in a field whose handler rebuilds part of the tree used to paint
//! one frame of unstyled, wrongly-measured nodes for every keystroke: the
//! cascade that turns a freshly spawned element into real `TextStyle` /
//! `Visuals` / box `Style` could be scheduled ahead of the system that
//! materializes the spawn, so it only caught up on the next tick. The
//! result was a per-keystroke flash of default-styled content that then
//! reverted.
//!
//! The app here is the smallest shape that reproduces it: an `<input>`
//! whose `on_text_input` clears a list and respawns one styled row.

use bevy_ecs::prelude::*;
use lumen_core::components::{LumenId, TextStyle};
use lumen_core::input::{FocusTracker, Focused, Key, KeyPressed, Modifiers};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::css::{Declaration, Origin, Rule, Stylesheet};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query;

/// The DOM snapshot and the external command bus are process-global, so the
/// headless apps that read and write them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Font size the stylesheet gives every rebuilt row. Deliberately not the
/// `TextStyle` default, so a row that reaches the painter uncascaded is
/// distinguishable from one that was styled.
const ROW_SIZE_PX: f32 = 30.0;

fn el(tag: &str, id: Option<&str>, children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Attributes {
            id: id.map(str::to_string),
            // An editable only gets its `TextBuffer` once it carries
            // `TextContent`, which the authored `text` attribute is what
            // supplies; without it the field swallows every keystroke.
            text: (tag == "input").then(String::new),
            ..Default::default()
        },
        children,
        interpolations: Vec::new(),
    }
}

const SCRIPT: &str = r#"
fn on_text_input(id, text) {
    if id != "field" { return; }
    let list = get_by_id("list");
    if !list.exists() { return; }
    for k in list.children() { k.remove(); }
    let row = create("label");
    row.set_attr("class", "row");
    row.set_text(text);
    list.append(row);
}
"#;

struct Harness {
    app: lumen_core::app::App,
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new() -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _ = node_query::drain_external_dom_commands();
        let dir = std::env::temp_dir().join(format!("lumen_rebuild_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Pin the engine: the baked source is Rhai, and the default is
        // candela. Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();

        let rules = vec![Rule {
            selectors: lumen_ir::css::parse_selector_list(".row").expect("selector parses"),
            declarations: vec![Declaration {
                name: "font-size".to_string(),
                value: format!("{ROW_SIZE_PX}"),
                important: false,
            }],
            origin: Origin::Author,
            source_order: 0,
            media: None,
            selector: Default::default(),
        }];

        let mut ir = LayoutIR {
            root: el(
                "root",
                Some("app"),
                vec![
                    el("input", Some("field"), vec![]),
                    el("column", Some("list"), vec![]),
                ],
            ),
            script_source: SCRIPT.to_string(),
            combined_stylesheet: Some(Stylesheet { rules }),
            ..Default::default()
        };
        if let Some(sheet) = ir.combined_stylesheet.clone() {
            lumen_ir::css::apply_css(&mut ir, &sheet).expect("cascade");
        }
        let bytes = artifact::serialize(&CompiledApp {
            ir,
            script_source: SCRIPT.to_string(),
            ..Default::default()
        })
        .unwrap();
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _winit) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness {
            app,
            dir,
            _guard: guard,
        };
        for _ in 0..8 {
            h.app.tick();
        }
        h
    }

    fn by_id(&mut self, want: &str) -> Entity {
        let mut q = self.app.world.query::<(Entity, &LumenId)>();
        q.iter(&self.app.world)
            .find(|(_, l)| l.0 == want)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("#{want} is in the tree"))
    }

    /// Font sizes of every row currently under `#list`, as the painter
    /// would read them: the row's own `TextStyle`, or the default when the
    /// cascade has not reached it.
    fn row_sizes(&mut self, list: Entity) -> Vec<f32> {
        let kids: Vec<Entity> = self
            .app
            .world
            .get::<bevy_ecs::hierarchy::Children>(list)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        kids.into_iter()
            .map(|k| {
                self.app
                    .world
                    .get::<TextStyle>(k)
                    .cloned()
                    .unwrap_or_default()
                    .size_px
            })
            .collect()
    }

    fn press(&mut self, ch: &str) {
        let mut msgs = self
            .app
            .world
            .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>();
        msgs.write(KeyPressed {
            key: Key::Character(ch.to_string()),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every row the handler spawns carries its cascaded font size on the very
/// tick it joins the tree. A single tick showing the default size is one
/// painted frame of unstyled content, which is what the user sees as a
/// flash.
#[test]
fn a_rebuilt_row_is_styled_in_the_frame_it_appears() {
    let mut h = Harness::new();
    let field = h.by_id("field");
    let list = h.by_id("list");
    h.app.world.entity_mut(field).insert(Focused);
    h.app.world.insert_resource(FocusTracker(Some(field)));
    for _ in 0..4 {
        h.app.tick();
    }

    let mut seen_rows = false;
    for step in 0..12 {
        h.press("x");
        h.app.tick();
        let sizes = h.row_sizes(list);
        seen_rows |= !sizes.is_empty();
        for size in sizes {
            assert_eq!(
                size, ROW_SIZE_PX,
                "tick {step}: a row is in the tree without its cascaded font size, \
                 so this frame paints default-styled content the next frame undoes"
            );
        }
    }
    assert!(
        seen_rows,
        "the handler never rebuilt the list, so nothing was asserted"
    );
}
