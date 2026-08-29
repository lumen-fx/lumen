// Exercises the linked runtime through `build_headless_app`, which lumenc
// only exposes under `dev-run`.
#![cfg(feature = "dev-run")]

//! Where the module phase sits in the app build.
//!
//! Two claims, and both are about order rather than about any one module.
//! Modules install before the app's markup is parsed, so a module that brings
//! an element can register its tag and have the parse accept it. And they
//! still install after the runtime's own registrations, so an app that
//! replaces a service the runtime installed keeps winning.

use std::sync::atomic::{AtomicBool, Ordering};

use lumen_core::app::App;
use lumen_module_registry::{StaticModule, register};
use lumen_script::FetchRegistry;
use lumenc::{RunOptions, build_headless_app};

/// Whether the runtime's HTTP client was already installed when the module
/// phase ran. Written by the module below, read by the test.
static FETCH_REGISTRY_PRESENT: AtomicBool = AtomicBool::new(false);

/// The tag the module brings. Nothing in the language knows it, so a parse
/// that ran before the module installed would refuse the element.
const TAG: &str = "phase-order-probe";

/// A module's install entry: what `lumen_module!` generates, hand-written so
/// the test needs no library on disk.
fn install(app: &mut App, _config_toml: &str) -> u32 {
    FETCH_REGISTRY_PRESENT.store(
        app.world.contains_resource::<FetchRegistry>(),
        Ordering::SeqCst,
    );
    lumen_widget::register_widget_tag_owned(TAG);
    0
}

/// An app whose markup uses the module's tag and whose config declares the
/// module without naming the tag: the run path has to accept it on the
/// strength of the module alone.
fn app_dir() -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumenc-module-phase-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::write(
        dir.join("src/main.lmn"),
        format!("<root>\n  <{TAG} />\n</root>\n"),
    )
    .expect("markup");
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n\n\
         [dependencies]\nphase-order = { bundled = true }\n",
    )
    .expect("lumen.toml");
    dir
}

#[test]
fn a_module_installs_before_the_parse_and_after_the_runtime() {
    register(StaticModule {
        name: "phase-order",
        install,
    });
    let dir = app_dir();
    let (mut app, _window) =
        build_headless_app(RunOptions::new(dir.clone())).expect("the app builds");
    app.tick();

    assert!(
        FETCH_REGISTRY_PRESENT.load(Ordering::SeqCst),
        "the runtime's HTTP client must already be installed when a module builds, or a \
         module that installs its own would stop winning"
    );
    // The element the module's tag names is in the tree, which it could only
    // be if the tag was registered before the markup was parsed.
    let mut tags = app
        .world
        .query::<&lumen_core::components::LumenTag>()
        .iter(&app.world)
        .map(|t| t.0.to_string())
        .collect::<Vec<_>>();
    tags.sort();
    assert!(tags.iter().any(|t| t == TAG), "{tags:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
