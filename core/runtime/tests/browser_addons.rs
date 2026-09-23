//! A desktop run of an app compiled against a browser add-on.
//!
//! The add-on is a module for a page, so a desktop run cannot load it. The
//! program still calls its functions, so they have to resolve: each is bound
//! to a body that raises `<namespace>::<function> runs only in a browser` in
//! the script that called it, and the app keeps running. The dependency that
//! names the add-on is not handed to the module loader, which would look for a
//! library that does not exist.

#![cfg(all(feature = "modules", feature = "host-candela"))]

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::addon::{Addon, AddonFunction, AddonParam};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::modules::LoadedModules;
use lumen_runtime::{RunOptions, build_headless_app};

/// The property store and the DOM snapshot are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn echo() -> Addon {
    Addon {
        name: "echo".to_string(),
        namespace: "echo".to_string(),
        functions: vec![AddonFunction {
            name: "shout".to_string(),
            params: vec![AddonParam {
                name: "text".to_string(),
                ty: "string".to_string(),
            }],
            returns: "string".to_string(),
            event: None,
            doc: String::new(),
        }],
        elements: Vec::new(),
    }
}

/// An app directory whose `lumen.toml` declares the add-on at `echo/`.
fn app_dir() -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumen_browser_addons_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    let addon = dir.join("echo");
    std::fs::create_dir_all(&addon).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[dependencies]\necho = { path = \"echo\" }\n",
    )
    .expect("write lumen.toml");
    std::fs::write(
        addon.join("lumen-addon.toml"),
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n",
    )
    .expect("write the descriptor");
    std::fs::write(addon.join("echo.js"), "").expect("write the module");
    dir
}

const SOURCE: &str = r#"
import "lumen.cdl";

fn on_start() {
    let loud = echo::shout("hello");
    lumen::signal_set("after", loud);
}

fn on_ready() {
    lumen::signal_set("ready", "yes");
}

fn main() {}
"#;

/// Build the app the way `lumenc run` builds a compiled one.
fn app(addons: Vec<Addon>, opts_addons: Vec<Addon>) -> EcsApp {
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        script_source: SOURCE.to_string(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: SOURCE.to_string(),
            bytecode: None,
        }],
        addons,
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(app_dir()).with_artifact_bytes(bytes);
    opts.bounded = true;
    opts.addons = opts_addons;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app.tick();
    app
}

fn signal(app: &EcsApp, name: &str) -> Option<String> {
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

/// The call raises where it was made, the program it was made from loaded,
/// and the app runs on past it.
#[test]
fn an_addon_call_on_the_desktop_raises_and_the_app_runs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let app = app(vec![echo()], Vec::new());
    assert_eq!(signal(&app, "ready").as_deref(), Some("yes"));
    assert_eq!(
        signal(&app, "after"),
        None,
        "the call raised, so nothing after it in on_start ran"
    );
    assert!(
        app.world
            .get_resource::<lumen_script::ScriptLoadFailure>()
            .is_none(),
        "the program loaded: its call to the add-on resolved"
    );
}

/// What `lumenc run` hands in binds the same way a compiled app's own list
/// does, and one add-on named by both binds once.
#[test]
fn an_addon_the_compiler_handed_in_binds_the_same_way() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (compiled, handed_in) in [(Vec::new(), vec![echo()]), (vec![echo()], vec![echo()])] {
        let app = app(compiled, handed_in);
        assert_eq!(signal(&app, "ready").as_deref(), Some("yes"));
        assert_eq!(signal(&app, "after"), None);
        assert!(
            app.world
                .get_resource::<lumen_script::ScriptLoadFailure>()
                .is_none()
        );
    }
}

/// The dependency that names the add-on never reaches the module loader.
#[test]
fn an_addon_dependency_is_not_opened_as_a_library() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let app = app(vec![echo()], Vec::new());
    if let Some(loaded) = app.world.get_resource::<LoadedModules>() {
        assert!(
            loaded.failed.iter().all(|failure| failure.name != "echo"),
            "the loader was handed the add-on: {:?}",
            loaded.failed.iter().map(|f| &f.reason).collect::<Vec<_>>()
        );
    }
}
