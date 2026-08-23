//! Tab traversal follows the document, not the spawn clock.
//!
//! An `<if>` body mounts on the tick its signal turns truthy, long after
//! the initial walk stamped everything else. When the ordering key is a
//! spawn counter, the gated button lands behind every element that was
//! already there, so Tab jumps straight over it into whatever follows -
//! and the longer that list, the further the button is thrown. This app
//! puts a 500-button list right after the gated one, which is the shape
//! the defect was reported against.

use bevy_ecs::prelude::*;
use lumen_core::components::LumenId;
use lumen_core::input::{FocusTracker, Focused, Key, KeyPressed, Modifiers, NamedKey};
use lumen_core::property_store::PropertyStore;
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// The DOM snapshot and the external command bus are process-global, so
/// headless apps that touch them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Focusables between the gated button and the end of the document. Large
/// enough that a mis-ordered gated button is unmistakable rather than an
/// off-by-one.
const LIST_ROWS: usize = 500;

fn button(id: &str) -> Element {
    Element {
        tag: "button".to_string(),
        attrs: Attributes {
            id: Some(id.to_string()),
            tab_index: Some(0),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn container(tag: &str, children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        children,
        ..Default::default()
    }
}

/// `<root>` = head button, an `<if>` holding one button, a long list, a
/// tail button.
fn app_ir() -> LayoutIR {
    let mut gate = container("if", vec![button("gated")]);
    gate.attrs.if_signal = Some("open".to_string());
    let rows: Vec<Element> = (0..LIST_ROWS).map(|i| button(&format!("row{i}"))).collect();
    LayoutIR {
        root: container(
            "root",
            vec![
                button("head"),
                gate,
                container("column", rows),
                button("tail"),
            ],
        ),
        ..Default::default()
    }
}

struct Harness {
    app: lumen_core::app::App,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new() -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lumen_taborder_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        let bytes = artifact::serialize(&CompiledApp {
            ir: app_ir(),
            ..Default::default()
        })
        .unwrap();
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _window) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness { app, _guard: guard };
        // Open the gate, then let the mount and the reconcilers settle.
        for _ in 0..4 {
            h.app.tick();
        }
        h.app
            .world
            .resource_mut::<PropertyStore>()
            .set_global_str("open", "1");
        for _ in 0..8 {
            h.app.tick();
        }
        h
    }

    fn press_tab(&mut self) {
        self.app
            .world
            .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Tab),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        self.app.tick();
    }

    /// `id` of the entity holding focus, or `None` when nothing is focused
    /// and `"<unnamed>"` when the focused entity carries no id.
    fn focused_id(&mut self) -> Option<String> {
        let mut q = self.app.world.query_filtered::<Entity, With<Focused>>();
        let e = q.iter(&self.app.world).next()?;
        Some(
            self.app
                .world
                .get::<LumenId>(e)
                .map(|l| l.0.clone())
                .unwrap_or_else(|| "<unnamed>".to_string()),
        )
    }
}

/// The gated button sits second in the document, so it is the second Tab
/// stop - the 500 rows after it change nothing.
#[test]
fn tab_reaches_an_if_body_before_the_list_that_follows_it() {
    let mut h = Harness::new();
    assert!(
        h.app
            .world
            .query::<(&LumenId, &lumen_core::components::TabIndex)>()
            .iter(&h.app.world)
            .any(|(l, _)| l.0 == "gated"),
        "the if body must have mounted for the test to mean anything"
    );
    assert!(
        h.app.world.resource::<FocusTracker>().0.is_none(),
        "nothing is focused before the first Tab"
    );

    h.press_tab();
    assert_eq!(h.focused_id().as_deref(), Some("head"));
    h.press_tab();
    assert_eq!(
        h.focused_id().as_deref(),
        Some("gated"),
        "Tab must enter the if body before the list that follows it"
    );
    h.press_tab();
    assert_eq!(
        h.focused_id().as_deref(),
        Some("row0"),
        "the list follows the if body"
    );
}

/// Walking the whole chain lands on the tail button last, so nothing was
/// merely swapped with its neighbour.
#[test]
fn the_whole_tab_chain_is_in_document_order() {
    let mut h = Harness::new();
    let mut seen = Vec::new();
    for _ in 0..(LIST_ROWS + 3) {
        h.press_tab();
        seen.push(h.focused_id().unwrap_or_default());
    }
    let mut want = vec!["head".to_string(), "gated".to_string()];
    want.extend((0..LIST_ROWS).map(|i| format!("row{i}")));
    want.push("tail".to_string());
    assert_eq!(seen, want, "Tab chain must follow the document");
}
