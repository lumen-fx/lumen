//! `lumenc package` and the shipped shared-runtime layout: the engine dylib
//! and libstd travel beside a dynamic `liblumen`, declared modules stage into
//! `modules/`, and the combinations that cannot produce a working folder are
//! refused. Everything runs against stand-in toolchain files through
//! `--lib-dir`, or against a release faked on disk in the download cache -
//! packaging copies files, it never opens them - so the suite asserts
//! layout, not execution; the runnable end-to-end proof lives in
//! `public/lumen-module/tests/end_to_end.rs`.

#![cfg(all(not(windows), feature = "package"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumenc-package-modules-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(dir: &Path, name: &str, body: &str) {
    if let Some(parent) = dir.join(name).parent() {
        std::fs::create_dir_all(parent).expect("parent dir");
    }
    std::fs::write(dir.join(name), body).expect("write file");
}

/// A minimal markup app declaring one path-source module, with the module
/// library present as a stand-in file.
fn write_app(root: &Path, dependencies: &str) -> PathBuf {
    let app = root.join("app");
    std::fs::create_dir_all(&app).expect("app dir");
    write(&app, "src/main.lmn", "<root><label>hi</label></root>\n");
    write(
        &app,
        "lumen.toml",
        &format!("[dependencies]\n{dependencies}"),
    );
    app
}

/// A stand-in toolchain directory. `dynamic` adds the engine dylib and a
/// hashed libstd beside the launcher and liblumen, the shape a current Unix
/// release archive has.
fn write_toolchain(root: &Path, dynamic: bool) -> PathBuf {
    let dir = root.join("toolchain");
    std::fs::create_dir_all(&dir).expect("toolchain dir");
    write(&dir, "lumen-launcher", "stub");
    write(&dir, lib_name(), "library");
    if dynamic {
        write(&dir, engine_name(), "engine");
        write(&dir, &format!("libstd-abc123.{}", dll_ext()), "std");
    }
    dir
}

fn dll_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

fn lib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "liblumen.dylib"
    } else {
        "liblumen.so"
    }
}

fn engine_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "liblumen_engine.dylib"
    } else {
        "liblumen_engine.so"
    }
}

fn run_package(app: &Path, out: &Path, lib_dir: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("package")
        .arg(app)
        .arg(out)
        .arg("--lib-dir")
        .arg(lib_dir)
        .arg("--no-hooks")
        .args(extra)
        .output()
        .expect("lumenc runs")
}

#[test]
fn a_package_ships_the_shared_runtime_and_the_staged_module() {
    let root = scratch("layout");
    let module_file = format!("libdemo-mod.{}", dll_ext());
    let app = write_app(&root, "demo-mod = { path = \"modules/demo-mod\" }\n");
    write(&app, &format!("modules/{module_file}"), "module bytes");
    let toolchain = write_toolchain(&root, true);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");

    // The executable, the C library, and the shared runtime beside them.
    assert!(out.join("app").is_file(), "the launcher copy");
    assert!(out.join(lib_name()).is_file(), "liblumen travels");
    assert!(out.join(engine_name()).is_file(), "the engine travels");
    assert!(
        out.join(format!("libstd-abc123.{}", dll_ext())).is_file(),
        "libstd travels under its hashed name"
    );
    // The module, staged under the file name the loader probes modules/ for.
    assert_eq!(
        std::fs::read(out.join("modules").join(&module_file)).expect("staged module"),
        b"module bytes"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_static_toolchain_still_packages_a_module_free_app() {
    let root = scratch("static");
    let app = root.join("app");
    std::fs::create_dir_all(&app).expect("app dir");
    write(&app, "src/main.lmn", "<root><label>hi</label></root>\n");
    let toolchain = write_toolchain(&root, false);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(out.join(lib_name()).is_file());
    assert!(
        !out.join(engine_name()).exists(),
        "a static toolchain has no engine to ship"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dependencies_against_a_static_toolchain_fail_the_package() {
    let root = scratch("static-deps");
    let module_file = format!("libdemo-mod.{}", dll_ext());
    let app = write_app(&root, "demo-mod = { path = \"modules/demo-mod\" }\n");
    write(&app, &format!("modules/{module_file}"), "module bytes");
    let toolchain = write_toolchain(&root, false);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &[]);
    assert!(!output.status.success(), "a broken package must not ship");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[dependencies]"), "{stderr}");
    assert!(stderr.contains("liblumen_engine"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}

/// The cross target the suite packages for, with the library extension that
/// platform spells its files with: always the other Unix platform, so the
/// spelling assertions are real on either host.
struct Cross {
    target: &'static str,
    ext: &'static str,
}

fn cross() -> Cross {
    if cfg!(target_os = "macos") {
        Cross {
            target: "linux-x86_64",
            ext: "so",
        }
    } else {
        Cross {
            target: "macos-aarch64",
            ext: "dylib",
        }
    }
}

/// A stand-in toolchain for the cross target, shaped like its release
/// archive: the launcher, liblumen, the engine dylib, and a hashed libstd,
/// all under the target platform's spellings.
fn write_cross_toolchain(root: &Path, cross: &Cross) -> PathBuf {
    let dir = root.join("cross-toolchain");
    std::fs::create_dir_all(&dir).expect("toolchain dir");
    write(&dir, "lumen-launcher", "stub");
    write(&dir, &format!("liblumen.{}", cross.ext), "library");
    write(&dir, &format!("liblumen_engine.{}", cross.ext), "engine");
    write(&dir, &format!("libstd-abc123.{}", cross.ext), "std");
    dir
}

/// A repository address that can never answer: GitHub does not issue an
/// owner name with two hyphens in a row. Tests that must not download point
/// the fetch here, so code that reaches for the network anyway fails them.
const UNREACHABLE_REPO: &str = "lumen--fx/lumen";

/// A cross-target package with a `path` module is refused: the declared file
/// is this machine's build, and no release can supply another platform's
/// copy of a local library.
#[test]
fn a_path_module_refuses_a_cross_target_package() {
    let root = scratch("cross-path");
    let app = write_app(&root, "demo-mod = { path = \"modules/demo-mod\" }\n");
    let toolchain = write_toolchain(&root, true);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", cross().target]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency 'demo-mod'"), "{stderr}");
    assert!(stderr.contains("built for one platform"), "{stderr}");
    assert!(stderr.contains("bundled"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A cross-target package with a `version` module is refused until the
/// registry exists, and the message says so.
#[test]
fn a_version_module_refuses_a_cross_target_package() {
    let root = scratch("cross-version");
    let app = write_app(&root, "demo-mod = \"1.0\"\n");
    let toolchain = write_toolchain(&root, true);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", cross().target]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency 'demo-mod'"), "{stderr}");
    assert!(stderr.contains("registry"), "{stderr}");
    assert!(stderr.contains("does not exist yet"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A `bundled` module crosses platforms: `--lib-dir` names the directory the
/// target's files come from, and a module library beside them - under
/// cargo's underscored spelling, as the release archives carry it - stages
/// into `modules/` under the target platform's spelling of the declared
/// name.
#[test]
fn a_cross_target_package_stages_a_bundled_module_from_the_lib_dir() {
    let root = scratch("cross-bundled-libdir");
    let cross = cross();
    let app = write_app(&root, "demo-mod = { bundled = true }\n");
    let toolchain = write_cross_toolchain(&root, &cross);
    write(
        &toolchain,
        &format!("libdemo_mod.{}", cross.ext),
        "module bytes",
    );
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", cross.target]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        std::fs::read(
            out.join("modules")
                .join(format!("libdemo-mod.{}", cross.ext))
        )
        .expect("staged module"),
        b"module bytes",
        "staged under the target's spelling of the declared name"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A `bundled` module the `--lib-dir` does not hold comes from the release's
/// modules archive. The release is faked on disk: the resolver's remembered
/// answer names a version, and the archive's contents are already unpacked
/// in the download cache for it, exactly where a real fetch would have put
/// them. The repository address cannot answer, so a package that reached for
/// the network anyway would fail the test.
#[test]
fn a_cross_target_package_stages_a_bundled_module_from_the_release_cache() {
    let root = scratch("cross-bundled-cache");
    let cross = cross();
    let app = write_app(&root, "demo-mod = { bundled = true }\n");
    let toolchain = write_cross_toolchain(&root, &cross);
    let out = root.join("dist");

    let cache = root.join("cache");
    let state_dir = cache.join("lumen");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    std::fs::write(
        state_dir.join("update-check"),
        format!("checked {now}\nlatest 9.9.9\n"),
    )
    .expect("state file");
    // The per-release, per-target download cache, at the path the packing
    // host computes it (macOS ignores XDG for it).
    let module_cache = if cfg!(target_os = "macos") {
        cache.join("Library").join("Caches")
    } else {
        cache.clone()
    }
    .join("lumen")
    .join("toolchain")
    .join("9.9.9")
    .join(cross.target);
    std::fs::create_dir_all(&module_cache).expect("cache dir");
    std::fs::write(
        module_cache.join(format!("libdemo_mod.{}", cross.ext)),
        "cached module bytes",
    )
    .expect("cached module");

    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("package")
        .arg(&app)
        .arg(&out)
        .arg("--lib-dir")
        .arg(&toolchain)
        .arg("--no-hooks")
        .args(["--target", cross.target])
        .env("LUMEN_GH_REPO", UNREACHABLE_REPO)
        .env("HOME", &cache)
        .env("XDG_CACHE_HOME", &cache)
        .env("LOCALAPPDATA", &cache)
        .env_remove("LUMEN_LIB_DIR")
        .output()
        .expect("lumenc runs");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.status.success(), "{printed}");
    assert!(
        !printed.contains("fetching"),
        "a cached module is not downloaded again: {printed}"
    );
    assert_eq!(
        std::fs::read(
            out.join("modules")
                .join(format!("libdemo-mod.{}", cross.ext))
        )
        .expect("staged module"),
        b"cached module bytes"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A `bundled` module that is neither beside the `--lib-dir` files nor in
/// the cache has to come from the release, and when the release cannot
/// answer the package fails rather than shipping a folder without its
/// modules.
#[test]
fn a_cross_target_package_never_ships_silently_without_its_modules() {
    let root = scratch("cross-bundled-missing");
    let cross = cross();
    let app = write_app(&root, "demo-mod = { bundled = true }\n");
    let toolchain = write_cross_toolchain(&root, &cross);
    let out = root.join("dist");

    let cache = root.join("cache");
    let state_dir = cache.join("lumen");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    std::fs::write(
        state_dir.join("update-check"),
        format!("checked {now}\nlatest 9.9.9\n"),
    )
    .expect("state file");

    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("package")
        .arg(&app)
        .arg(&out)
        .arg("--lib-dir")
        .arg(&toolchain)
        .arg("--no-hooks")
        .args(["--target", cross.target])
        .env("LUMEN_GH_REPO", UNREACHABLE_REPO)
        .env("HOME", &cache)
        .env("XDG_CACHE_HOME", &cache)
        .env("LOCALAPPDATA", &cache)
        .env_remove("LUMEN_LIB_DIR")
        .output()
        .expect("lumenc runs");
    assert!(
        !output.status.success(),
        "a broken package must not ship: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !out.join("modules").exists(),
        "nothing was staged for a package that failed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A Windows target stages no modules whoever packages it - no shared engine
/// exists there - and says so with a warning rather than an error.
#[test]
fn a_windows_target_warns_and_stages_no_modules() {
    let root = scratch("windows-target");
    let app = write_app(&root, "demo-mod = { bundled = true }\n");
    let toolchain = root.join("win-toolchain");
    std::fs::create_dir_all(&toolchain).expect("toolchain dir");
    write(&toolchain, "lumen-launcher.exe", "stub");
    write(&toolchain, "lumen.dll", "library");
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", "windows-x86_64"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("not supported on Windows"), "{stderr}");
    assert!(out.join("app.exe").is_file(), "the launcher copy");
    assert!(!out.join("modules").exists(), "nothing staged");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_module_library_fails_the_package_naming_the_probes() {
    let root = scratch("missing-module");
    let app = write_app(&root, "ghost = { path = \"modules/ghost\" }\n");
    let toolchain = write_toolchain(&root, true);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency 'ghost'"), "{stderr}");
    assert!(stderr.contains("no module library found"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}
