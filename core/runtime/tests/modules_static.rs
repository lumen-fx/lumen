//! The `[dependencies]` loader inside a static host. This test binary
//! compiles the engine in statically, which is exactly the process shape
//! that must never *open* an engine-locked module (the module would map
//! `liblumen_engine` as a second engine instance sharing no worlds or
//! statics with this one) - and exactly the shape a portable plugin loads
//! into all the same. What a static host does carry is a module compiled
//! into it, which the loader takes from the registry without opening
//! anything.
//!
//! The dispatch is what these tests exercise: a declared name is answered
//! from the registry, or by the arm the resolved file's exported symbols
//! name.

#![cfg(all(feature = "modules", not(windows)))]

use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_core::app::App;
use lumen_module_registry::{StaticModule, register};
use lumen_runtime::modules::{
    DependenciesCfg, InitEnv, LoadedKind, LoadedModules, ResolvedModules, load_modules,
};
use lumen_script::{ScriptFn, ScriptFnAppExt, ScriptFnRegistry, ScriptTy, ScriptValue};

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumen-modules-static-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn env(dir: &Path) -> InitEnv {
    InitEnv {
        app_dir: dir.to_path_buf(),
        app_id: "static-test".to_string(),
        headless: true,
        hot_reload: false,
    }
}

/// Load one `[dependencies]` table into a fresh app.
fn load(dir: &Path, dependencies: &str) -> App {
    let deps: DependenciesCfg = toml::from_str(dependencies).expect("the test table parses");
    let mut app = App::new();
    load_modules(&mut app, dir, &deps, &ResolvedModules::default(), &env(dir));
    app
}

/// Compile a tiny dependency-free stub cdylib from source text.
fn build_stub(dir: &Path, name: &str, source: &str) -> PathBuf {
    let src = dir.join(format!("{name}.rs"));
    std::fs::write(&src, source).expect("stub source");
    let out_path = dir.join(format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc)
        .args(["--crate-type", "cdylib", "--edition", "2021", "-o"])
        .arg(&out_path)
        .arg(&src)
        .output()
        .expect("rustc runs");
    assert!(
        out.status.success(),
        "stub build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out_path
}

/// A module compiled into a static host installs, which is the shape a
/// static build carries its capabilities in. It takes the registry arm, so
/// nothing on disk is looked for and no engine dylib is needed.
#[test]
fn a_static_host_installs_a_module_compiled_into_it() {
    fn install(app: &mut App, config_toml: &str) -> u32 {
        assert!(config_toml.contains("units = \"mm\""), "{config_toml}");
        app.add_script_fn(
            ScriptFn::new("compiled_in_double")
                .param("n", ScriptTy::Int)
                .build(|cx| Ok(ScriptValue::I64(cx.int_arg(0) * 2))),
        );
        0
    }
    // The call a real module's pre-main constructor makes.
    register(StaticModule {
        name: "compiled-in",
        install,
    });

    let dir = scratch("compiled-in");
    let app = load(
        &dir,
        "compiled-in = { bundled = true, config = { units = \"mm\" } }\n",
    );
    let state = app.world.resource::<LoadedModules>();
    assert!(state.failed.is_empty(), "{:?}", state.failed);
    assert_eq!(state.loaded.len(), 1);
    assert_eq!(state.loaded[0].kind, LoadedKind::Static);
    assert_eq!(state.loaded[0].build_id, "");

    let registry = app.world.resource::<ScriptFnRegistry>();
    let double = registry
        .fns()
        .iter()
        .find(|f| f.name == "compiled_in_double")
        .expect("the module's function is bound");
    let (result, _commands) = double.invoke(&[ScriptValue::I64(21)]);
    assert_eq!(result, Ok(ScriptValue::I64(42)));
}

#[test]
fn a_static_host_refuses_an_engine_locked_module() {
    let dir = scratch("engine-locked");
    let stub = build_stub(
        &dir,
        "locked",
        "#[export_name = \"lumen_module_probe_locked\"]\n\
         pub extern \"C\" fn probe() -> *const u8 {\n\
             b\"lumen-engine 0.0.0 nogit rustc:0 features:none\\0\".as_ptr()\n\
         }\n\
         #[export_name = \"lumen_module_install_locked\"]\n\
         pub extern \"C\" fn install() {}\n",
    );
    let app = load(
        &dir,
        &format!("locked = {{ path = \"{}\" }}\n", stub.display()),
    );
    let state = app.world.resource::<LoadedModules>();
    assert!(state.loaded.is_empty());
    assert_eq!(state.failed.len(), 1);
    let reason = &state.failed[0].reason;
    assert!(
        reason.contains("does not compile the module in"),
        "{reason}"
    );
    // The refusal points at the kind that does load here.
    assert!(reason.contains("portable plugin"), "{reason}");
}

#[test]
fn a_static_host_loads_a_portable_plugin() {
    let dir = scratch("portable");
    let plugin = lumen_plugin::testing::fixture_copy("static-host");
    let app = load(
        &dir,
        &format!(
            "lumen-plugin-fixture = {{ path = \"{}\", config = {{ ns = \"extension\" }} }}\n",
            plugin.display()
        ),
    );
    let state = app.world.resource::<LoadedModules>();
    assert!(state.failed.is_empty(), "{:?}", state.failed);
    assert_eq!(state.loaded.len(), 1);
    assert_eq!(state.loaded[0].kind, LoadedKind::PortablePlugin);
    assert_eq!(state.loaded[0].build_id, "");

    // What it registered reached the app's one script-fn registry, and the
    // body calls back into the loaded library.
    let registry = app.world.resource::<ScriptFnRegistry>();
    let echo = registry
        .fns()
        .iter()
        .find(|f| f.name == "fixture_echo")
        .expect("the plugin's function is bound");
    let (result, _commands) = echo.invoke(&[ScriptValue::I64(41)]);
    assert_eq!(result, Ok(ScriptValue::I64(41)));
}

#[test]
fn both_kinds_dispatch_side_by_side_in_a_static_host() {
    let dir = scratch("side-by-side");
    let stub = build_stub(
        &dir,
        "locked2",
        "#[export_name = \"lumen_module_probe_zz_locked\"]\n\
         pub extern \"C\" fn probe() -> *const u8 {\n\
             b\"lumen-engine 0.0.0 nogit rustc:0 features:none\\0\".as_ptr()\n\
         }\n",
    );
    let plugin = lumen_plugin::testing::fixture_copy("static-side-by-side");
    let app = load(
        &dir,
        &format!(
            "lumen-plugin-fixture = {{ path = \"{}\" }}\nzz-locked = {{ path = \"{}\" }}\n",
            plugin.display(),
            stub.display()
        ),
    );
    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.loaded.len(), 1, "{:?}", state.failed);
    assert_eq!(state.loaded[0].kind, LoadedKind::PortablePlugin);
    assert_eq!(state.failed.len(), 1);
    assert_eq!(state.failed[0].name, "zz-locked");
    assert!(
        state.failed[0]
            .reason
            .contains("does not compile the module in"),
        "{}",
        state.failed[0].reason
    );
}

#[test]
fn a_library_exporting_neither_symbol_names_both() {
    let dir = scratch("neither");
    let stub = build_stub(
        &dir,
        "neither",
        "#[no_mangle]\npub extern \"C\" fn unrelated() {}\n",
    );
    let app = load(
        &dir,
        &format!("neither = {{ path = \"{}\" }}\n", stub.display()),
    );
    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.failed.len(), 1);
    let reason = &state.failed[0].reason;
    assert!(reason.contains("lumen_module_probe_neither"), "{reason}");
    assert!(reason.contains("lumen_plugin_v1"), "{reason}");
}

#[test]
fn a_compiler_plugin_is_named_as_the_wrong_kind() {
    let dir = scratch("compiler");
    let compiler = lumenc_plugin::testing::fixture_cdylib();
    let app = load(
        &dir,
        &format!("markdown = {{ path = \"{}\" }}\n", compiler.display()),
    );
    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.failed.len(), 1);
    let reason = &state.failed[0].reason;
    assert!(reason.contains("compiler plugin"), "{reason}");
    assert!(reason.contains("[[plugins]]"), "{reason}");
}

#[test]
fn a_missing_file_banners_every_probed_path() {
    let dir = scratch("missing");
    let app = load(&dir, "ghost = { path = \"modules/ghost\" }\n");
    let state = app.world.resource::<LoadedModules>();
    assert_eq!(state.failed.len(), 1);
    let reason = &state.failed[0].reason;
    assert!(reason.contains("no module library found"), "{reason}");
    assert!(reason.contains("libghost.so"), "{reason}");
}

#[test]
fn no_dependencies_is_an_empty_resource() {
    let dir = scratch("empty");
    let app = load(&dir, "");
    let state = app.world.resource::<LoadedModules>();
    assert!(state.loaded.is_empty() && state.failed.is_empty());
}
