//! The compiled-in shape at the seam the loader reads: this test binary links
//! the `lumen-fs` library the way a static app does, so the module's pre-main
//! constructor ran before the first test and the loader has it without a file
//! to find.
//!
//! What `module.rs` proves over a subprocess for the opened shape, this
//! proves in process for the linked one: an app that declares `lumen-fs` gets
//! the `files` namespace either way.

// The link-line anchor, the same line a binary that wants this module
// compiled in writes. Nothing here calls into the crate: what it is here for
// is the constructor its library carries.
use lumen_core::app::App;
use lumen_fs as _;
use lumen_module::registry::registered;
use lumen_runtime::modules::{
    DependenciesCfg, InitEnv, LoadedKind, LoadedModules, ResolvedModules, load_modules,
};
use lumen_script::ScriptFnRegistry;

/// A fresh app directory for the loader to resolve against.
fn app_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-fs-linked-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    dir
}

#[test]
fn the_linked_module_is_on_the_registry_before_any_test_runs() {
    let names: Vec<&str> = registered().iter().map(|m| m.name).collect();
    assert!(names.contains(&"lumen-fs"), "{names:?}");
}

#[test]
fn the_loader_installs_the_linked_module_without_opening_a_file() {
    let dir = app_dir("declared");
    let deps: DependenciesCfg =
        toml::from_str("lumen-fs = { bundled = true }\n").expect("the dependencies table parses");
    let mut app = App::new();
    load_modules(
        &mut app,
        &dir,
        &deps,
        &ResolvedModules::default(),
        &InitEnv {
            app_dir: dir.clone(),
            app_id: "lumen-fs-linked".to_string(),
            headless: true,
            hot_reload: false,
        },
    );

    let state = app.world.resource::<LoadedModules>();
    assert!(state.failed.is_empty(), "{:?}", state.failed);
    assert_eq!(state.loaded.len(), 1);
    assert_eq!(state.loaded[0].name, "lumen-fs");
    assert_eq!(state.loaded[0].kind, LoadedKind::Static);

    // What the module registered reached the app's one script-fn registry,
    // which is what a script would call through.
    let registry = app.world.resource::<ScriptFnRegistry>();
    assert!(
        registry.fns().iter().any(|f| f.name == "read"),
        "the files namespace is bound"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
