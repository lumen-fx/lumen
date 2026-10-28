//! The loader's refusal arms, driven in-process where the profiler can see
//! them. Nothing here is a working module on purpose: the end-to-end proof
//! that a real one loads lives in `public/lumen-module/tests/end_to_end.rs`,
//! and these are the arms a real module never reaches.

#![cfg(all(feature = "loader", not(windows)))]

use std::path::{Path, PathBuf};

use lumen_core::app::App;
use lumen_modules::{
    DepCfg, DependenciesCfg, InitEnv, LoadedModules, ModuleSource, PortablePlugins,
    ResolvedModules, load_modules,
};

fn env(dir: &Path) -> InitEnv {
    InitEnv {
        app_dir: dir.to_path_buf(),
        app_id: "loader-arms".to_string(),
        headless: true,
        hot_reload: false,
    }
}

fn dep(name: &str, source: ModuleSource) -> DepCfg {
    DepCfg {
        name: name.to_string(),
        source,
        config: toml::Table::new(),
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-loader-arms-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
    dir
}

fn run(dir: &Path, deps: Vec<DepCfg>, resolved: ResolvedModules) -> App {
    let mut app = App::new();
    load_modules(&mut app, dir, &DependenciesCfg(deps), &resolved, &env(dir));
    app
}

fn failure_reason(app: &App, name: &str) -> String {
    let state = app.world.resource::<LoadedModules>();
    assert!(state.loaded.is_empty());
    state
        .failed
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("'{name}' must be a recorded failure"))
        .reason
        .clone()
}

#[test]
fn no_dependencies_still_leaves_the_resources_behind() {
    let dir = scratch("empty");
    let app = run(&dir, Vec::new(), ResolvedModules::default());
    let state = app.world.resource::<LoadedModules>();
    assert!(state.loaded.is_empty() && state.failed.is_empty());
    assert!(app.world.contains_resource::<PortablePlugins>());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_that_resolves_to_nothing_names_what_was_probed() {
    let dir = scratch("no-file");
    let app = run(
        &dir,
        vec![dep("ghost", ModuleSource::Path("modules/ghost".into()))],
        ResolvedModules::default(),
    );
    let reason = failure_reason(&app, "ghost");
    assert!(reason.contains("no module library found"), "{reason}");
    assert!(reason.contains("modules"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_not_a_library_reports_the_open_failure() {
    let dir = scratch("not-a-library");
    std::fs::write(dir.join("fake.so"), b"not elf").expect("write the stand-in");
    let app = run(
        &dir,
        vec![dep("fake", ModuleSource::Path("fake.so".into()))],
        ResolvedModules::default(),
    );
    let reason = failure_reason(&app, "fake");
    assert!(reason.contains("could not open"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bundled_module_missing_from_a_static_host_is_a_quiet_skip() {
    // This test binary compiles the engine in, which is exactly the shape
    // the loader treats as a build property rather than a defect.
    let dir = scratch("bundled");
    let app = run(
        &dir,
        vec![dep("lumen-shapes", ModuleSource::Bundled)],
        ResolvedModules::default(),
    );
    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.failed.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_compiler_resolution_failure_is_bannered_verbatim() {
    let dir = scratch("resolved-err");
    let mut resolved = ResolvedModules::default();
    resolved.0.insert(
        "versioned".to_string(),
        Err("no cached version matches '9.9'".to_string()),
    );
    let app = run(
        &dir,
        vec![dep("versioned", ModuleSource::Version("9.9".into()))],
        resolved,
    );
    let reason = failure_reason(&app, "versioned");
    assert!(reason.contains("no cached version matches"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_resolved_copy_that_disappeared_says_so() {
    let dir = scratch("resolved-gone");
    let mut resolved = ResolvedModules::default();
    resolved.0.insert(
        "versioned".to_string(),
        Ok(dir.join("gone").join("libversioned.so")),
    );
    let app = run(
        &dir,
        vec![dep("versioned", ModuleSource::Version("1.0".into()))],
        resolved,
    );
    let reason = failure_reason(&app, "versioned");
    assert!(reason.contains("is gone"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unresolved_version_tells_the_user_to_go_through_lumenc() {
    let dir = scratch("version-probe");
    let app = run(
        &dir,
        vec![dep("versioned", ModuleSource::Version("1.0".into()))],
        ResolvedModules::default(),
    );
    let reason = failure_reason(&app, "versioned");
    assert!(reason.contains("not resolved at runtime"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_compiler_plugin_is_pointed_at_its_own_table() {
    let dir = scratch("wrong-kind");
    let lib = lumenc_plugin::testing::fixture_cdylib();
    let app = run(
        &dir,
        vec![dep(
            "wrong-kind",
            ModuleSource::Path(lib.to_string_lossy().into_owned()),
        )],
        ResolvedModules::default(),
    );
    let reason = failure_reason(&app, "wrong-kind");
    assert!(reason.contains("[[plugins]]"), "{reason}");
    let _ = std::fs::remove_dir_all(&dir);
}
