//! The real verification: build the fixture module and a dylib-linked host
//! runner with `-C prefer-dynamic` (the way `lumenc package` builds a Rust
//! app), then drive the host as a subprocess over generated app directories
//! and assert on what the module printed, what the loader bannered, and
//! whether the app stayed alive.
//!
//! A normal `cargo test` binary links the engine statically and must never
//! load modules itself - that refusal is asserted in
//! `crates/runtime/tests/modules_static.rs`. Everything here therefore runs
//! through the subprocess.
//!
//! Builds go to a dedicated `target/module-fixture` subdirectory (the nested-
//! cargo pattern from `lumenc-plugin`'s testing support) with an explicit
//! `--target`, which is what keeps `prefer-dynamic` off build scripts and
//! proc macros.

#![cfg(not(windows))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

struct Fixtures {
    /// The fixture module cdylib.
    module: PathBuf,
    /// The portable-plugin fixture cdylib (`crates/lumen-plugin/fixture`),
    /// the other `[dependencies]` kind.
    plugin: PathBuf,
    /// The dylib-linked host runner.
    host: PathBuf,
    /// Directories the host and its libraries load from.
    lib_dirs: Vec<PathBuf>,
    /// Scratch root for generated app dirs and stub libraries.
    scratch: PathBuf,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate sits two levels under the workspace root")
        .to_path_buf()
}

fn host_triple() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc).arg("-vV").output().expect("rustc runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("rustc -vV reports a host")
        .to_string()
}

fn target_libdir(triple: &str) -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc)
        .args(["--print", "target-libdir", "--target", triple])
        .output()
        .expect("rustc runs");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// Build the module and the host once, prefer-dynamic, and remember where
/// everything landed.
fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let root = workspace_root();
        let triple = host_triple();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"))
            .join("module-fixture");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let rustflags = {
            let existing = std::env::var("RUSTFLAGS").unwrap_or_default();
            format!("{existing} -C prefer-dynamic").trim().to_string()
        };
        let out = Command::new(cargo)
            .current_dir(&root)
            .args([
                "build",
                "-p",
                "lumen-module-fixture",
                "-p",
                "lumen-module-fixture-host",
                "-p",
                "lumen-plugin-fixture",
                "--message-format=json-render-diagnostics",
            ])
            .arg("--target")
            .arg(&triple)
            .arg("--target-dir")
            .arg(&target_dir)
            .env("RUSTFLAGS", &rustflags)
            .output()
            .expect("cargo runs");
        assert!(
            out.status.success(),
            "fixture build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let mut module = None;
        let mut plugin = None;
        let mut host = None;
        let mut lib_dirs: Vec<PathBuf> = Vec::new();
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if msg["reason"] != "compiler-artifact" {
                continue;
            }
            let name = msg["target"]["name"].as_str().unwrap_or_default();
            if let Some(exe) = msg["executable"].as_str()
                && matches!(
                    name,
                    "lumen-module-fixture-host" | "lumen_module_fixture_host"
                )
            {
                host = Some(PathBuf::from(exe));
            }
            for f in msg["filenames"].as_array().into_iter().flatten() {
                let path = PathBuf::from(f.as_str().unwrap_or_default());
                let is_lib = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("so" | "dylib")
                );
                if !is_lib {
                    continue;
                }
                if matches!(name, "lumen_module_fixture" | "lumen-module-fixture") {
                    module = Some(path.clone());
                }
                if matches!(name, "lumen_plugin_fixture" | "lumen-plugin-fixture") {
                    plugin = Some(path.clone());
                }
                // Every produced shared library's directory joins the load
                // path; `liblumen_engine` and the shared libstd live there.
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let scratch = target_dir.join("apps");
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        Fixtures {
            module: module.expect("fixture module cdylib built"),
            plugin: plugin.expect("fixture plugin cdylib built"),
            host: host.expect("fixture host binary built"),
            lib_dirs,
            scratch,
        }
    })
}

/// Write a minimal app dir: one text element (the host component the module
/// queries) and the given `[dependencies]` block.
fn write_app(f: &Fixtures, case: &str, dependencies: &str) -> PathBuf {
    let dir = f.scratch.join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root><label>hello</label></root>\n",
    )
    .expect("markup");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[dependencies]\n{dependencies}"),
    )
    .expect("config");
    dir
}

/// Run the host over an app dir and return (stdout, stderr). `envs` are
/// extra environment overrides (the plugin-cache root, mostly); every run
/// pins `LUMEN_PLUGIN_CACHE` into the scratch tree so no test can touch the
/// developer's real cache.
fn run_host(f: &Fixtures, app_dir: &Path, ticks: u32, envs: &[(&str, &str)]) -> (String, String) {
    let joined = std::env::join_paths(&f.lib_dirs).expect("lib dirs join");
    let mut command = Command::new(&f.host);
    command
        .arg(app_dir)
        .arg(ticks.to_string())
        .env("LD_LIBRARY_PATH", &joined)
        .env("DYLD_LIBRARY_PATH", &joined)
        .env("DYLD_FALLBACK_LIBRARY_PATH", &joined)
        .env("LUMEN_PLUGIN_CACHE", f.scratch.join("no-such-cache"));
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("host runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "host exited non-zero.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("HOST done"), "{stdout}");
    (stdout, stderr)
}

/// Compile a tiny dependency-free stub cdylib from source text.
fn build_stub(f: &Fixtures, name: &str, source: &str) -> PathBuf {
    let src = f.scratch.join(format!("{name}.rs"));
    std::fs::write(&src, source).expect("stub source");
    let out_path = f.scratch.join(format!(
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

#[test]
fn a_module_installs_and_runs_real_ecs() {
    let f = fixtures();
    let dir = write_app(
        f,
        "success",
        &format!(
            "fixture = {{ path = \"{}\", config = {{ units = \"mm\" }} }}\n",
            f.module.display()
        ),
    );
    let (stdout, stderr) = run_host(f, &dir, 5, &[]);

    // Probe passed and install ran, with the config value in hand.
    assert!(
        stdout.contains("module-install units=mm"),
        "{stdout}\n{stderr}"
    );
    // The loader recorded the module under the engine's own build id.
    assert!(
        stdout.contains("HOST loaded name=fixture kind=EngineModule build_id=lumen-engine "),
        "{stdout}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}");
    // The module's system ran every tick and mutated its own resource.
    for n in 1..=5 {
        assert!(stdout.contains(&format!("module-tick n={n} ")), "{stdout}");
    }
    // Host-spawned components (the markup's labels; the skin contributes its
    // own) are visible to the module's query - shared `lumen-core`, shared
    // `TypeId`s - and so is the entity the module spawned itself.
    assert!(!stdout.contains("texts=0 "), "{stdout}");
    assert!(stdout.contains("marks=1"), "{stdout}");
}

#[test]
fn the_module_records_its_engine_dependency() {
    // The macro's linkage anchor must force a NEEDED entry on the engine
    // dylib, so the dynamic linker itself refuses a module in an engine-less
    // process. ELF-only check; the macOS equivalent is covered by the
    // end-to-end run (the module could not load at all without the binding).
    if !cfg!(target_os = "linux") {
        return;
    }
    let f = fixtures();
    let out = Command::new("readelf").arg("-d").arg(&f.module).output();
    let Ok(out) = out else {
        eprintln!("readelf not available; skipping the NEEDED assertion");
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("liblumen_engine"),
        "no NEEDED entry on liblumen_engine:\n{text}"
    );
}

#[test]
fn a_panicking_module_system_leaves_the_app_alive() {
    let f = fixtures();
    let dir = write_app(
        f,
        "panic-tick",
        &format!(
            "fixture = {{ path = \"{}\", config = {{ panic_at_tick = 2 }} }}\n",
            f.module.display()
        ),
    );
    let (stdout, _stderr) = run_host(f, &dir, 5, &[]);
    // The panic unwound out of the module's system, across the dylib
    // boundary (shared libstd), into the host's catch - not an abort.
    assert!(stdout.contains("HOST tick-panic-caught"), "{stdout}");
    assert!(stdout.contains("panics at tick 2"), "{stdout}");
    // The tick before the panic ran normally.
    assert!(stdout.contains("module-tick n=1"), "{stdout}");
    // Ticking resumes after the caught panic: `App::tick` restores the Tick
    // schedule on unwind (see lumen-core's `panic_recovery` tests), so the
    // module's system runs again on the ticks that follow, state intact.
    assert!(stdout.contains("module-tick n=3"), "{stdout}");
    assert!(stdout.contains("module-tick n=5"), "{stdout}");
    assert!(stdout.contains("HOST loaded name=fixture"), "{stdout}");
    assert!(stdout.contains("HOST done"), "{stdout}");
}

#[test]
fn a_panicking_constructor_is_a_failed_install_not_a_dead_app() {
    let f = fixtures();
    let dir = write_app(
        f,
        "panic-ctor",
        &format!(
            "fixture = {{ path = \"{}\", config = {{ panic_in_ctor = true }} }}\n",
            f.module.display()
        ),
    );
    let (stdout, stderr) = run_host(f, &dir, 2, &[]);
    assert!(stderr.contains("MODULE LOAD FAILED: fixture"), "{stderr}");
    assert!(stderr.contains("constructor panicked"), "{stderr}");
    assert!(stdout.contains("HOST failed name=fixture"), "{stdout}");
    assert!(!stdout.contains("module-install"), "{stdout}");
}

#[test]
fn a_missing_file_banners_every_probed_path() {
    let f = fixtures();
    let dir = write_app(f, "missing", "ghost = { path = \"modules/ghost\" }\n");
    let (stdout, stderr) = run_host(f, &dir, 1, &[]);
    assert!(stderr.contains("MODULE LOAD FAILED: ghost"), "{stderr}");
    assert!(stderr.contains("no module library found"), "{stderr}");
    // The probed list names the declared path's platform spellings and the
    // modules/ fallbacks.
    assert!(stderr.contains("libghost.so"), "{stderr}");
    assert!(stderr.contains("modules"), "{stderr}");
    assert!(stdout.contains("HOST failed name=ghost"), "{stdout}");
}

#[test]
fn a_library_without_the_probe_is_refused_as_not_a_module() {
    let f = fixtures();
    let stub = build_stub(
        f,
        "wrongkind",
        "#[no_mangle]\npub extern \"C\" fn unrelated() {}\n",
    );
    let dir = write_app(
        f,
        "wrong-kind",
        &format!("wrongkind = {{ path = \"{}\" }}\n", stub.display()),
    );
    let (stdout, stderr) = run_host(f, &dir, 1, &[]);
    assert!(stderr.contains("MODULE LOAD FAILED: wrongkind"), "{stderr}");
    // The refusal names both entry symbols, one per kind.
    assert!(
        stderr.contains("exports neither lumen_module_probe nor lumen_plugin_v1"),
        "{stderr}"
    );
    assert!(stdout.contains("HOST failed name=wrongkind"), "{stdout}");
}

#[test]
fn both_kinds_load_side_by_side_and_the_script_reaches_both() {
    let f = fixtures();
    let dir = write_app(
        f,
        "side-by-side",
        &format!(
            "fixture = {{ path = \"{}\", config = {{ units = \"mm\" }} }}\n\
             lumen-plugin-fixture = {{ path = \"{}\" }}\n",
            f.module.display(),
            f.plugin.display()
        ),
    );
    // The script calls one function from each kind; both prints land on the
    // same warn stream through the ordinary command applier.
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root><label>hello</label><script src=\"main.rhai\" /></root>\n",
    )
    .expect("markup");
    std::fs::write(
        dir.join("src/main.rhai"),
        r#"fn on_start() {
    print("module says " + module_double(21));
    print("plugin says " + fixture_echo("hi"));
}
"#,
    )
    .expect("script");
    let (stdout, stderr) = run_host(f, &dir, 5, &[]);

    // Both kinds loaded, each recorded under its kind.
    assert!(stdout.contains("module-install units=mm"), "{stdout}");
    assert!(
        stdout.contains("HOST loaded name=fixture kind=EngineModule build_id=lumen-engine "),
        "{stdout}"
    );
    assert!(
        stdout.contains("HOST loaded name=lumen-plugin-fixture"),
        "{stdout}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    // The script reached the module's function and the plugin's.
    assert!(stderr.contains("module says 42"), "{stdout}\n{stderr}");
    assert!(stderr.contains("plugin says hi"), "{stdout}\n{stderr}");
}

#[test]
fn a_mismatched_build_id_banners_both_strings() {
    let f = fixtures();
    let stub = build_stub(
        f,
        "skewed",
        "#[no_mangle]\n\
         pub extern \"C\" fn lumen_module_probe() -> *const u8 {\n\
             b\"lumen-engine 9.9.9 git:feedface rustc:0000000000000000 features:none\\0\"\n\
                 .as_ptr()\n\
         }\n\
         #[no_mangle]\n\
         pub extern \"C\" fn lumen_module_install() {}\n",
    );
    let dir = write_app(
        f,
        "mismatch",
        &format!("skewed = {{ path = \"{}\" }}\n", stub.display()),
    );
    let (stdout, stderr) = run_host(f, &dir, 1, &[]);
    assert!(stderr.contains("MODULE LOAD FAILED: skewed"), "{stderr}");
    assert!(stderr.contains("different engine build"), "{stderr}");
    // Both sides of the comparison are in the banner.
    assert!(stderr.contains("git:feedface"), "{stderr}");
    assert!(stderr.contains("engine is:"), "{stderr}");
    assert!(stderr.contains("Rebuild the module"), "{stderr}");
    assert!(stdout.contains("HOST failed name=skewed"), "{stdout}");
}

#[test]
fn a_version_source_resolves_through_the_shared_cache() {
    let f = fixtures();
    // Install the fixture module into a scratch plugin cache the way the
    // registry client would: <cache>/<name>/<version>/<platform library>.
    let cache = f.scratch.join("cache-hit");
    let version_dir = cache.join("fixture").join("1.4.0");
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&version_dir).expect("cache dir");
    let spelling = format!(
        "{}fixture{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    std::fs::copy(&f.module, version_dir.join(&spelling)).expect("install into the cache");

    let dir = write_app(
        f,
        "version-hit",
        "fixture = { version = \"1.4\", config = { units = \"cm\" } }\n",
    );
    let (stdout, stderr) = run_host(
        f,
        &dir,
        3,
        &[("LUMEN_PLUGIN_CACHE", &cache.display().to_string())],
    );

    // lumenc's injection resolved the version and the loader installed the
    // resolved copy - the whole `lumenc run` path, headless.
    assert!(
        stdout.contains("module-install units=cm"),
        "{stdout}\n{stderr}"
    );
    assert!(stdout.contains("HOST loaded name=fixture"), "{stdout}");
    assert!(stdout.contains("module-tick n=3"), "{stdout}");
    // The resolution pinned itself: lumen.lock appeared beside lumen.toml.
    let lock = std::fs::read_to_string(dir.join("lumen.lock")).expect("lock written");
    assert!(lock.contains("version = \"1.4.0\""), "{lock}");
}

#[test]
fn an_unresolvable_version_banners_the_resolvers_reason() {
    let f = fixtures();
    let dir = write_app(f, "version", "md = \"1.2\"\n");
    // The harness pins LUMEN_PLUGIN_CACHE at a directory that does not
    // exist, so resolution fails and the loader banners the resolver's own
    // reason instead of loading anything.
    let (stdout, stderr) = run_host(f, &dir, 1, &[]);
    assert!(stderr.contains("MODULE LOAD FAILED: md"), "{stderr}");
    assert!(stderr.contains("no cached version matches"), "{stderr}");
    assert!(stdout.contains("HOST failed name=md"), "{stdout}");
}
