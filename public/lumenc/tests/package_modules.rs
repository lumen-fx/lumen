//! `lumenc package` and the shipped shared-runtime layout: the engine dylib
//! and libstd travel beside a dynamic `liblumen`, declared modules stage into
//! `modules/`, and the combinations that cannot produce a working folder are
//! refused. Everything runs against stand-in toolchain files through
//! `--lib-dir`, or against a release faked on disk in the download cache -
//! packaging copies files, it never opens them - so the suite asserts
//! layout, not execution; the runnable end-to-end proof lives in
//! `public/lumen-module/tests/end_to_end.rs`.

#![cfg(all(not(windows), feature = "package"))]

mod common;

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

fn package_command(app: &Path, out: &Path, lib_dir: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lumenc"));
    command
        .arg("package")
        .arg(app)
        .arg(out)
        .arg("--lib-dir")
        .arg(lib_dir)
        .arg("--no-hooks")
        .args(extra);
    command
}

fn run_package(app: &Path, out: &Path, lib_dir: &Path, extra: &[&str]) -> std::process::Output {
    package_command(app, out, lib_dir, extra)
        // Nothing on this path declares a registry package, and pinning the
        // client at a path that does not exist is what proves it: a test that
        // grew one would fail rather than reach the developer's own `lpm`.
        .env("LPM_BIN", out.join("no-such-lpm"))
        .output()
        .expect("lumenc runs")
}

/// [`run_package`] with the registry answering from `answer`.
fn run_package_with_registry(
    app: &Path,
    out: &Path,
    lib_dir: &Path,
    extra: &[&str],
    answer: &Path,
) -> std::process::Output {
    package_command(app, out, lib_dir, extra)
        .env("LPM_BIN", common::lpm_stub())
        .env("LPM_STUB_JSON", answer)
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
    assert!(stderr.contains("runtime module"), "{stderr}");
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
    assert!(stderr.contains("registry version"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A `version` module cross-packages. The registry resolves for the platform
/// being packaged, not for this one, so what stages into `modules/` is that
/// platform's build.
#[test]
fn a_version_module_cross_packages_from_the_registry() {
    let cross = cross();
    let root = scratch("cross-version");
    let app = write_app(&root, "demo-mod = \"1.0\"\n");
    let toolchain = write_cross_toolchain(&root, &cross);
    let out = root.join("dist");

    // The package the registry resolved for the target, holding that
    // platform's library under the name the loader probes for.
    let package = root.join("pkg");
    let file = format!("libdemo-mod.{}", cross.ext);
    write(&package, &file, "the target's build");
    let answer = common::stub_answer(
        &root.join("lpm.json"),
        &common::lumen_package("demo-mod", "1.0.4", cross.target, &package, &file),
    );

    let output =
        run_package_with_registry(&app, &out, &toolchain, &["--target", cross.target], &answer);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(out.join("modules").join(&file)).expect("the module staged"),
        "the target's build"
    );

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

/// A stand-in Windows toolchain: the launcher and the C library, which is
/// all a Windows folder carries.
fn write_windows_toolchain(root: &Path) -> PathBuf {
    let toolchain = root.join("win-toolchain");
    std::fs::create_dir_all(&toolchain).expect("toolchain dir");
    write(&toolchain, "lumen-launcher.exe", "stub");
    write(&toolchain, "lumen.dll", "library");
    toolchain
}

/// A minimal x86-64 PE DLL whose export table names `exports`: the file a
/// Windows build of a library is, down to the table a package reads to tell
/// a portable plugin from a runtime module. It holds no code; every export
/// points at the same empty bytes, which is all the table has to say.
fn pe_dll(exports: &[&str]) -> Vec<u8> {
    const FILE_ALIGN: usize = 0x200;
    const SECTION_RVA: u32 = 0x1000;
    let put16 = |buf: &mut Vec<u8>, at: usize, v: u16| {
        buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
    };
    let put32 = |buf: &mut Vec<u8>, at: usize, v: u32| {
        buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    };

    // The section: the export directory, its three arrays, then the strings.
    let n = exports.len() as u32;
    let functions = 40u32;
    let names = functions + 4 * n;
    let ordinals = names + 4 * n;
    let mut strings = ordinals + 2 * n;
    let mut section = vec![0u8; FILE_ALIGN];
    let dll_name = strings;
    section[strings as usize..strings as usize + 10].copy_from_slice(b"plugin.dll");
    strings += 11;
    for (i, name) in exports.iter().enumerate() {
        let i = i as u32;
        put32(
            &mut section,
            (names + 4 * i) as usize,
            SECTION_RVA + strings,
        );
        put16(&mut section, (ordinals + 2 * i) as usize, i as u16);
        let at = strings as usize;
        section[at..at + name.len()].copy_from_slice(name.as_bytes());
        strings += name.len() as u32 + 1;
    }
    let directory_size = strings;
    // Past the directory, so no export reads as forwarded to another DLL.
    let body = (FILE_ALIGN - 16) as u32;
    assert!(directory_size < body, "the export names fit the section");
    for i in 0..n {
        put32(
            &mut section,
            (functions + 4 * i) as usize,
            SECTION_RVA + body,
        );
    }
    put32(&mut section, 12, SECTION_RVA + dll_name);
    put32(&mut section, 16, 1);
    put32(&mut section, 20, n);
    put32(&mut section, 24, n);
    put32(&mut section, 28, SECTION_RVA + functions);
    put32(&mut section, 32, SECTION_RVA + names);
    put32(&mut section, 36, SECTION_RVA + ordinals);

    // The headers: DOS stub, PE signature, COFF header, PE32+ optional header
    // with sixteen data directories, one section header.
    let mut image = vec![0u8; FILE_ALIGN];
    image[0..2].copy_from_slice(b"MZ");
    put32(&mut image, 0x3c, 0x40);
    image[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    put16(&mut image, coff, 0x8664);
    put16(&mut image, coff + 2, 1);
    put16(&mut image, coff + 16, 240);
    put16(&mut image, coff + 18, 0x2022);
    let opt = coff + 20;
    put16(&mut image, opt, 0x20b);
    image[opt + 24..opt + 32].copy_from_slice(&0x1_8000_0000u64.to_le_bytes());
    put32(&mut image, opt + 32, SECTION_RVA);
    put32(&mut image, opt + 36, FILE_ALIGN as u32);
    put16(&mut image, opt + 48, 6);
    put32(&mut image, opt + 56, SECTION_RVA * 2);
    put32(&mut image, opt + 60, FILE_ALIGN as u32);
    put16(&mut image, opt + 68, 2);
    put32(&mut image, opt + 108, 16);
    put32(&mut image, opt + 112, SECTION_RVA);
    put32(&mut image, opt + 116, directory_size);
    let header = opt + 240;
    image[header..header + 6].copy_from_slice(b".edata");
    put32(&mut image, header + 8, FILE_ALIGN as u32);
    put32(&mut image, header + 12, SECTION_RVA);
    put32(&mut image, header + 16, FILE_ALIGN as u32);
    put32(&mut image, header + 20, FILE_ALIGN as u32);
    put32(&mut image, header + 36, 0x4000_0040);

    image.extend_from_slice(&section);
    image
}

/// The resolution for a registry package `name` holding one Windows library
/// built with `exports`, laid out where the registry would have unpacked it.
fn windows_library_package(root: &Path, name: &str, exports: &[&str]) -> PathBuf {
    let package = root.join("pkg").join(name);
    std::fs::create_dir_all(&package).expect("package root");
    let file = format!("{name}.dll");
    std::fs::write(package.join(&file), pe_dll(exports)).expect("write the library");
    common::stub_answer(
        &root.join("lpm.json"),
        &common::lumen_package(name, "1.0.0", "windows-x86_64", &package, &file),
    )
}

/// The table a package reads is the one Windows reads: a built DLL names its
/// entries there, and a file exporting something else is not a plugin.
#[test]
fn a_windows_library_is_told_apart_by_its_export_table() {
    use lumenc::package::library::exports;
    let plugin = pe_dll(&["lumen_plugin_v1"]);
    assert!(exports(&plugin, "lumen_plugin_v1"));
    let module = pe_dll(&["lumen_module_install_demo", "lumen_module_probe_demo"]);
    assert!(exports(&module, "lumen_module_probe_demo"));
    assert!(!exports(&module, "lumen_plugin_v1"));
}

/// A bundled module is a runtime module, and a folder-shaped Windows package
/// has no shared engine for one to load into, so an app declaring one is
/// refused and pointed at the shape that answers there: the executable with
/// the module linked in.
#[test]
fn a_windows_target_refuses_a_bundled_module_and_names_the_static_package() {
    let root = scratch("windows-target");
    let app = write_app(&root, "demo-mod = { bundled = true }\n");
    let toolchain = write_windows_toolchain(&root);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", "windows-x86_64"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("dependency 'demo-mod'"), "{stderr}");
    assert!(stderr.contains("lumenc package --static"), "{stderr}");
    assert!(!out.exists(), "nothing was written");

    let _ = std::fs::remove_dir_all(&root);
}

/// A registry library that is a runtime module has no Windows package in
/// either shape, and the refusal says so rather than pointing at `--static`,
/// which cannot link it either.
#[test]
fn a_windows_target_refuses_a_runtime_module_from_the_registry() {
    let root = scratch("windows-runtime-module");
    let app = write_app(&root, "demo-mod = \"1.0\"\n");
    let toolchain = write_windows_toolchain(&root);
    let answer = windows_library_package(
        &root,
        "demo-mod",
        &[
            "lumen_module_install_demo_mod",
            "lumen_module_probe_demo_mod",
        ],
    );
    let out = root.join("dist");

    let output = run_package_with_registry(
        &app,
        &out,
        &toolchain,
        &["--target", "windows-x86_64"],
        &answer,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("dependency 'demo-mod'"), "{stderr}");
    assert!(stderr.contains("no Windows package"), "{stderr}");
    assert!(!stderr.contains("staged beside"), "{stderr}");
    assert!(!out.exists(), "nothing was written");

    let _ = std::fs::remove_dir_all(&root);
}

/// A portable plugin loads into any host, a Windows executable included, so
/// a Windows package carries it in `modules/` under the name the loader
/// probes for.
#[test]
fn a_windows_target_ships_a_portable_plugin() {
    let root = scratch("windows-portable");
    let app = write_app(&root, "demo-plugin = \"1.0\"\n");
    let toolchain = write_windows_toolchain(&root);
    let answer = windows_library_package(&root, "demo-plugin", &["lumen_plugin_v1"]);
    let out = root.join("dist");

    let output = run_package_with_registry(
        &app,
        &out,
        &toolchain,
        &["--target", "windows-x86_64"],
        &answer,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(out.join("app.exe").is_file(), "the launcher copy");
    assert_eq!(
        std::fs::read(out.join("modules").join("demo-plugin.dll")).expect("the plugin staged"),
        pe_dll(&["lumen_plugin_v1"])
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A candela package is script source the app's own scripts compile
/// against, so it ships wherever the app does: the Windows package compiles
/// the import in and carries nothing for it.
#[test]
fn a_windows_target_ships_an_app_using_a_candela_package() {
    let root = scratch("windows-candela");
    let app = write_app(&root, "strutil = \"0.1\"\n");
    write(
        &app,
        "src/main.lmn",
        "<root><label>hi</label><script src=\"main.cdl\"/></root>\n",
    );
    write(
        &app,
        "src/main.cdl",
        "import \"strutil\";\n\nfn main() {\n    let n = twice(3);\n}\n",
    );
    let package = root.join("pkg").join("strutil");
    write(
        &package,
        "candela.toml",
        "[package]\nname = \"strutil\"\nversion = \"0.1.0\"\nentry = \"strutil.cdl\"\n",
    );
    write(
        &package,
        "strutil.cdl",
        "fn twice(n: int) -> int {\n    return n * 2;\n}\n",
    );
    let answer = common::stub_answer(
        &root.join("lpm.json"),
        &format!(
            "{{\"name\":\"strutil\",\"version\":\"0.1.0\",\"platform\":\"candela\",\
             \"target\":\"any\",\"dir\":{},\"files\":[\"candela.toml\",\"strutil.cdl\"]}}",
            serde_json::to_string(&package.display().to_string()).expect("a path encodes"),
        ),
    );
    let toolchain = write_windows_toolchain(&root);
    let out = root.join("dist");

    let output = run_package_with_registry(
        &app,
        &out,
        &toolchain,
        &["--target", "windows-x86_64"],
        &answer,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(out.join("app.exe").is_file(), "the launcher copy");
    assert!(
        !out.join("modules").exists(),
        "nothing staged for a script library"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A `path` library on the way to Windows is this machine's build, whatever
/// kind it is, and the refusal says that rather than anything about modules.
#[test]
fn a_windows_target_refuses_a_path_library_built_here() {
    let root = scratch("windows-path");
    let app = write_app(&root, "shape-tools = { path = \"modules/shape-tools\" }\n");
    write(
        &app,
        &format!("modules/libshape-tools.{}", dll_ext()),
        "module bytes",
    );
    let toolchain = write_windows_toolchain(&root);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", "windows-x86_64"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("dependency 'shape-tools'"), "{stderr}");
    assert!(stderr.contains("built for one platform"), "{stderr}");
    assert!(!out.exists(), "nothing was written");

    let _ = std::fs::remove_dir_all(&root);
}

/// The same app without `[dependencies]` still packages for Windows: the
/// refusal is about the modules, not about the target.
#[test]
fn a_windows_target_without_modules_still_packages() {
    let root = scratch("windows-no-modules");
    let app = write_app(&root, "");
    let toolchain = write_windows_toolchain(&root);
    let out = root.join("dist");

    let output = run_package(&app, &out, &toolchain, &["--target", "windows-x86_64"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
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
