//! Headless proof that the root element's class list is writable and that
//! it tracks the effective color scheme.
//!
//! `set_root_class` used to resolve the root out of the hot-reload state,
//! which exists only while a file watcher is running, so the call silently
//! did nothing in a compiled app. The theme classes had the mirror problem:
//! the spawn pass gives an element a class list only when its markup
//! declared one, and a root that declared none had nothing to write onto.

use lumen_core::components::{ColorScheme, StyleManager};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptCommand, ScriptCommandEvent, introspect, node_query};

/// The DOM snapshot is process-global, so the headless apps that read it run
/// one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: lumen_core::app::App,
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    /// A `root#app.shell > tile#box` app with `script` baked in as its Rhai
    /// source.
    fn new(script: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumen_root_class_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        // Pin the engine: the baked source below is Rhai, and the default is
        // candela. Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();

        let ir = LayoutIR {
            root: Element {
                tag: "root".to_string(),
                attrs: Attributes {
                    id: Some("app".to_string()),
                    classes: vec!["shell".to_string()],
                    ..Default::default()
                },
                children: vec![Element {
                    tag: "tile".to_string(),
                    attrs: Attributes {
                        id: Some("box".to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            },
            script_source: script.to_string(),
            ..Default::default()
        };
        let bytes = artifact::serialize(&CompiledApp {
            ir,
            script_source: script.to_string(),
            ..Default::default()
        })
        .unwrap();
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

    fn settle(&mut self) {
        for _ in 0..6 {
            self.app.tick();
        }
    }

    /// The root's class list as scripts read it.
    fn root_classes(&self) -> Vec<String> {
        let handle = node_query::run_document().expect("the document root is in the DOM index");
        introspect::node_classes(handle)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `set_root_class` replaces the whole list, and `node_classes` on the root
/// reads back what was written. Driven through the command stream so the
/// write lands mid-run rather than during startup.
#[test]
fn set_root_class_replaces_the_root_class_list() {
    // An app with no script at all installs no command applier, so the
    // harness carries an empty handler.
    let mut h = Harness::new("fn on_start() {}");
    assert!(
        h.root_classes().contains(&"shell".to_string()),
        "the markup class is there to start with: {:?}",
        h.root_classes()
    );

    h.app
        .world
        .write_message(ScriptCommandEvent(ScriptCommand::SetClasses {
            target_id: "<root>".to_string(),
            classes: "app compact".to_string(),
        }));
    h.settle();

    let classes = h.root_classes();
    assert!(classes.contains(&"app".to_string()), "{classes:?}");
    assert!(classes.contains(&"compact".to_string()), "{classes:?}");
    assert!(
        !classes.contains(&"shell".to_string()),
        "the previous list is replaced, not extended: {classes:?}"
    );
}

/// The same call through a script, which is how an app makes it.
#[test]
fn a_script_can_set_the_root_class() {
    let h = Harness::new(r#"fn on_start() { set_root_class("app compact"); }"#);
    let classes = h.root_classes();
    assert!(classes.contains(&"app".to_string()), "{classes:?}");
    assert!(classes.contains(&"compact".to_string()), "{classes:?}");
    assert!(!classes.contains(&"shell".to_string()), "{classes:?}");
}

/// The root carries `theme-dark` or `theme-light` in step with the effective
/// color scheme, so a stylesheet can key a token scope off it without a
/// script.
#[test]
fn the_root_carries_the_effective_theme_class() {
    let mut h = Harness::new("");
    let classes = h.root_classes();
    assert!(
        classes.contains(&"theme-light".to_string()),
        "the default scheme resolves light: {classes:?}"
    );

    h.app
        .world
        .resource_mut::<StyleManager>()
        .set_scheme(ColorScheme::ForceDark);
    h.settle();

    let classes = h.root_classes();
    assert!(classes.contains(&"theme-dark".to_string()), "{classes:?}");
    assert!(!classes.contains(&"theme-light".to_string()), "{classes:?}");
}
