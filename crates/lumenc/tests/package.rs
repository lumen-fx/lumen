//! `lumenc package` end to end: assemble an app folder, then run the app it
//! produced.
//!
//! The packaged executable is started headless, never windowed, so this runs
//! anywhere the rest of the suite does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory holding the workspace's built binaries, which is where the test
/// finds the launcher stub and the shared runtime library.
fn build_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lumenc"))
        .parent()
        .expect("lumenc lives in a directory")
        .to_path_buf()
}

fn stub_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "lumen-launcher.exe"
    } else {
        "lumen-launcher"
    }
}

fn lib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "lumen.dll"
    } else if cfg!(target_os = "macos") {
        "liblumen.dylib"
    } else {
        "liblumen.so"
    }
}

/// Whether the two toolchain files a package is assembled from are present.
/// A whole-workspace test run builds both; a run of this crate alone does not,
/// and the test reports that rather than failing on a missing input.
fn toolchain_present() -> bool {
    build_dir().join(stub_name()).is_file() && build_dir().join(lib_name()).is_file()
}

/// A scratch directory that removes itself when the test ends. A packaged app
/// is a few hundred megabytes, and the name carries the process id, so a run
/// that leaves its directories behind leaves a fresh set every time and fills
/// the temp filesystem.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "lumenc_package_{name}_{}_{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    Scratch(dir)
}

/// A one-page app with an external Rhai script and a file the markup names.
fn write_app(dir: &Path) {
    std::fs::create_dir_all(dir.join("assets")).expect("create assets dir");
    std::fs::write(
        dir.join("main.lmn"),
        "<root>\n  <label id=\"greeting\" text=\"packaged\"/>\n  \
         <image id=\"logo\" src=\"assets/logo.png\"/>\n  \
         <script src=\"main.rhai\"/>\n</root>\n",
    )
    .expect("write markup");
    std::fs::write(
        dir.join("main.rhai"),
        "fn on_start() { print(\"alive\"); }\n",
    )
    .expect("write script");
    std::fs::write(dir.join("assets/logo.png"), RED_DOT_PNG).expect("write asset");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write config");
}

/// A 1x1 red PNG - small enough to inline, valid enough to decode.
const RED_DOT_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn run_package(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("package")
        .args(args)
        .env("LUMEN_LIB_DIR", build_dir())
        .output()
        .expect("run lumenc package")
}

/// The whole point: package an app, then start the result and let it run.
///
/// The app is started from a directory that is not the package and not the
/// source, so anything that resolved against the packaging machine's layout
/// fails here.
#[test]
fn a_packaged_app_runs_from_anywhere() {
    if !toolchain_present() {
        eprintln!(
            "skipping: no {} / {} in {} (build the workspace to get them)",
            stub_name(),
            lib_name(),
            build_dir().display()
        );
        return;
    }
    // A macOS package is linked rather than appended, so it needs a compiler.
    if cfg!(target_os = "macos") && Command::new("cc").arg("--version").output().is_err() {
        eprintln!("skipping: cc is not installed, and a macOS package is built with it");
        return;
    }

    let root = scratch("run");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app);
    let out = root.join("out");

    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        "Demo",
    ]);
    assert!(
        result.status.success(),
        "package failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let exe = out.join(if cfg!(target_os = "windows") {
        "Demo.exe"
    } else {
        "Demo"
    });
    assert!(exe.is_file(), "no app executable at {}", exe.display());
    assert!(
        out.join(lib_name()).is_file(),
        "the runtime library travels"
    );
    assert!(out.join("lumen.toml").is_file(), "lumen.toml travels");
    assert!(
        out.join("assets/logo.png").is_file(),
        "the app's files keep their relative paths"
    );
    assert!(
        !out.join("main.lmn").exists(),
        "the markup is compiled in, not copied"
    );

    let run = Command::new(&exe)
        .args(["--headless", "--ticks", "3"])
        .current_dir(&root)
        .output()
        .expect("start the packaged app");
    assert!(
        run.status.success(),
        "the packaged app failed: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    // The script travels inside the artifact, under the engine its file
    // extension named at compile time.
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        output.contains("alive"),
        "the app's script did not run: {output}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Packaging for another platform is file assembly, so it works from any host.
/// Stand-in toolchain files keep this off the network: what is under test is
/// the shape of the output, not the download.
#[test]
fn cross_packaging_assembles_each_platform() {
    let root = scratch("cross");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app);

    // One directory holding a stand-in stub and library for every target.
    let libs = root.join("libs");
    std::fs::create_dir_all(&libs).expect("create lib dir");
    for name in [
        "lumen-launcher",
        "lumen-launcher.exe",
        "liblumen.so",
        "liblumen.dylib",
        "lumen.dll",
    ] {
        std::fs::write(libs.join(name), b"stand-in toolchain file").expect("write stand-in");
    }
    let libs_arg = libs.to_str().expect("utf-8 path");

    // Windows and Linux carry the app inside the executable.
    for (target, exe_name) in [("windows-x86_64", "Demo.exe"), ("linux-aarch64", "Demo")] {
        let out = root.join(target);
        let result = run_package(&[
            app.to_str().expect("utf-8 path"),
            out.to_str().expect("utf-8 path"),
            "--name",
            "Demo",
            "--target",
            target,
            "--lib-dir",
            libs_arg,
        ]);
        assert!(
            result.status.success(),
            "packaging for {target} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let image = std::fs::read(out.join(exe_name)).expect("read the packaged executable");
        assert_eq!(
            &image[image.len() - 16..image.len() - 8],
            b"LMNAPACK",
            "{target} carries the app inside its executable"
        );
        assert!(
            !out.join("Demo.lmna").exists(),
            "{target} needs no sidecar app file"
        );
    }

    // macOS from another platform ships the app beside the executable, since
    // linking it in needs a Mach-O linker.
    let out = root.join("macos-aarch64");
    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        "Demo",
        "--target",
        "macos-aarch64",
        "--lib-dir",
        libs_arg,
    ]);
    if cfg!(target_os = "macos") {
        // On a macOS host this goes through the compiler instead; the sidecar
        // shape under test here is the cross-packaging one.
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    assert!(
        result.status.success(),
        "packaging for macos-aarch64 failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        out.join("Demo.lmna").is_file(),
        "a cross-built macOS package ships the app beside the executable"
    );
    assert!(out.join("liblumen.dylib").is_file());
    let image = std::fs::read(out.join("Demo")).expect("read the packaged executable");
    assert_eq!(
        image, b"stand-in toolchain file",
        "the signed stub is copied unchanged"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--zip` writes the folder into one file, rooted at the folder itself so
/// unpacking it gives the directory back rather than loose files.
#[test]
fn the_zip_holds_the_folder() {
    let root = scratch("zip");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app);

    let libs = root.join("libs");
    std::fs::create_dir_all(&libs).expect("create lib dir");
    for name in [
        "lumen-launcher",
        "lumen-launcher.exe",
        "liblumen.so",
        "liblumen.dylib",
        "lumen.dll",
    ] {
        std::fs::write(libs.join(name), b"stand-in toolchain file").expect("write stand-in");
    }

    let other = if cfg!(target_os = "windows") {
        "linux-x86_64"
    } else {
        "windows-x86_64"
    };
    let out = root.join("Demo");
    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        "Demo",
        "--target",
        other,
        "--lib-dir",
        libs.to_str().expect("utf-8 path"),
        "--zip",
    ]);
    assert!(
        result.status.success(),
        "packaging failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let archive = root.join("Demo.zip");
    assert!(archive.is_file(), "the archive was not written");
    let bytes = std::fs::read(&archive).expect("read the archive");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("read as a zip");
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).expect("member").name().to_string())
        .collect();
    assert!(
        names.iter().all(|n| n.starts_with("Demo/")),
        "every member sits under the folder: {names:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A name no release covers is rejected before anything is compiled.
#[test]
fn an_unknown_target_is_refused() {
    let root = scratch("target");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app);

    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        "--target",
        "plan9-x86_64",
    ]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("linux-x86_64"),
        "the error lists the targets that do exist"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A multi-page app packages whole: every page is compiled into the executable
/// and the routing travels with it, so the folder runs with no `.lmn` files
/// anywhere near it.
///
/// That navigation mounts the second page is asserted in-process in
/// `pages.rs`, where the world can be inspected; what this covers is that the
/// packaged folder builds and starts.
#[test]
fn a_packaged_multi_page_app_runs() {
    if !toolchain_present() {
        eprintln!("skipping: the launcher stub and runtime library are not built");
        return;
    }
    if cfg!(target_os = "macos") && Command::new("cc").arg("--version").output().is_err() {
        eprintln!("skipping: cc is not installed, and a macOS package is built with it");
        return;
    }

    let root = scratch("pages");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(
        app.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .expect("write config");
    std::fs::write(
        app.join("index.lmn"),
        "<root>\n  <label id=\"home\" text=\"HOME\"/>\n  <a href=\"about\" text=\"About\"/>\n  \
         <script>\nfn on_start() { print(\"pages alive\"); }\n</script>\n</root>\n",
    )
    .expect("write entry page");
    std::fs::write(
        app.join("about.lmn"),
        "<root>\n  <label id=\"about\" text=\"ABOUT\"/>\n</root>\n",
    )
    .expect("write second page");

    let out = root.join("out");
    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        "Pages",
    ]);
    assert!(
        result.status.success(),
        "packaging a multi-page app failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let exe = out.join(if cfg!(target_os = "windows") {
        "Pages.exe"
    } else {
        "Pages"
    });
    assert!(exe.is_file(), "no app executable at {}", exe.display());
    assert!(
        !out.join("index.lmn").exists() && !out.join("about.lmn").exists(),
        "the pages are compiled in, not copied"
    );

    let run = Command::new(&exe)
        .args(["--headless", "--ticks", "3"])
        .current_dir(&root)
        .output()
        .expect("start the packaged app");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        run.status.success(),
        "the packaged multi-page app failed: {output}"
    );
    assert!(
        output.contains("pages alive"),
        "the compiled page scripts did not run: {output}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A Python app is frozen against the interpreter doing the freezing, so it
/// packages for this machine only. Asking for another platform says so rather
/// than producing this machine's executable under that platform's name.
#[test]
fn a_python_app_packages_for_this_machine_only() {
    let root = scratch("python");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(app.join("main.py"), "import lumen\n").expect("write entry");
    std::fs::write(app.join("main.lmn"), "<root/>").expect("write markup");

    let other = if cfg!(target_os = "windows") {
        "linux-x86_64"
    } else {
        "windows-x86_64"
    };
    let result = run_package(&[app.to_str().expect("utf-8 path"), "--target", other]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("frozen") && message.contains(other),
        "the refusal should say why and name the platform: {message}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Cross-compiling a C++ app is CMake's job and needs a toolchain file for the
/// other platform, which nothing here can stand in for. Without one, say so
/// instead of building this machine's binary.
#[test]
fn cross_packaging_a_cpp_app_needs_a_toolchain_file() {
    let root = scratch("sdk-target");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(app.join("CMakeLists.txt"), "project(demo)\n").expect("write manifest");
    std::fs::write(app.join("main.lmn"), "<root/>").expect("write markup");

    let other = if cfg!(target_os = "windows") {
        "linux-x86_64"
    } else {
        "windows-x86_64"
    };
    let result = run_package(&[app.to_str().expect("utf-8 path"), "--target", other]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("CMAKE_TOOLCHAIN_FILE") && message.contains(other),
        "the refusal should name what is missing and the platform: {message}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
