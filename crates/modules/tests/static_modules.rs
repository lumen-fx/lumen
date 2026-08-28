//! The registry arm: a module compiled into the running binary, declared by
//! name and installed without a file being resolved or opened.
//!
//! No platform gate. This is the one arm that needs neither a shared engine
//! nor dlopen, so it is the arm a Windows build has, and the test runs
//! wherever the loader does.

#![cfg(feature = "loader")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bevy_ecs::resource::Resource;
use lumen_core::app::App;
use lumen_module_registry::{StaticModule, register};
use lumen_modules::{
    DependenciesCfg, InitEnv, LoadedKind, LoadedModules, ResolvedModules, load_modules,
};

/// The `config` table the loader serialized for the fixture, as the fixture
/// received it. A `Mutex` because the install entry is a plain `fn` with
/// nowhere else to leave what it saw.
static SEEN_CONFIG: Mutex<Option<String>> = Mutex::new(None);

/// Proof that the install entry was handed the app the loader is filling,
/// not some app of its own.
#[derive(Resource)]
struct FixtureInstalled;

/// The fixture's install entry, the same signature and status codes a real
/// module's generated one has.
fn install(app: &mut App, config_toml: &str) -> u32 {
    *SEEN_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(config_toml.to_string());
    app.world.insert_resource(FixtureInstalled);
    0
}

fn env(dir: &Path) -> InitEnv {
    InitEnv {
        app_dir: dir.to_path_buf(),
        app_id: "static-modules".to_string(),
        headless: true,
        hot_reload: false,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumen-static-modules-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
    dir
}

/// One test, because the registry is process-wide: registering the fixture
/// once and asserting every outcome of that one load keeps the two arms from
/// racing over `SEEN_CONFIG`.
#[test]
fn a_registered_module_installs_and_an_unknown_one_only_fails() {
    // A real module registers from its pre-main constructor. Calling it here
    // is the same call that constructor makes, and it has to happen before
    // the load below rather than before `main`.
    register(StaticModule {
        name: "static-fixture",
        install,
    });

    let dir = scratch("declared");
    let deps: DependenciesCfg = toml::from_str(
        "static-fixture = { bundled = true, config = { units = \"mm\" } }\n\
         zz-absent = { bundled = true }\n",
    )
    .expect("the test table parses");
    let mut app = App::new();
    load_modules(
        &mut app,
        &dir,
        &deps,
        &ResolvedModules::default(),
        &env(&dir),
    );

    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.loaded.len(), 1, "{:?}", state.loaded);
    let loaded = &state.loaded[0];
    assert_eq!(loaded.name, "static-fixture");
    assert_eq!(loaded.kind, LoadedKind::Static);
    // Nothing was opened, so the record points at the binary the module was
    // compiled into.
    assert_eq!(
        loaded.path,
        std::env::current_exe().expect("the test binary knows its own path")
    );
    // A compiled-in module is this build; there is no id to compare.
    assert_eq!(loaded.build_id, "");

    // The declared `config` table reached the install entry, in the same wire
    // form the opened arm hands over.
    let seen = SEEN_CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("the install entry ran");
    assert!(seen.contains("units = \"mm\""), "{seen}");
    // And the app it was handed is the one the loader filled.
    assert!(app.world.contains_resource::<FixtureInstalled>());

    // A name nothing registered and nothing on disk answers is a recorded
    // failure, and the load of the other module was unaffected by it.
    assert_eq!(state.failed.len(), 1, "{:?}", state.failed);
    assert_eq!(state.failed[0].name, "zz-absent");

    let _ = std::fs::remove_dir_all(&dir);
}
