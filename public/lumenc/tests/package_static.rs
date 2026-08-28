//! `lumenc package --static` end to end: link an app into one executable from
//! a link kit, then run the executable it produced.
//!
//! The kit is the release asset a build leg publishes, so this suite needs one
//! to exist. `LUMEN_LINK_KIT_DIR` names it - the release workflow points the
//! variable at the kit it just packaged, and a local run points it at a kit
//! built the same way (see contributing/building-lumen.md). Without the
//! variable the linking tests say so and pass: the alternative is downloading
//! a kit from the release channel in the middle of a test run.
//!
//! The refusals need no kit at all. They are checked on every platform,
//! because what they are about is the shape of the request rather than
//! anything the link does.
//!
//! One kit is built here rather than published: a manifest naming a single
//! object file this test compiles with `cc`. It links no engine and the
//! executable it produces does nothing, which is the point - what it exercises
//! is the replay itself, on a machine with no release kit on it.

#![cfg(feature = "package")]

use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_modules::link_kit::{
    Artifact, ArtifactKind, Driver, DriverKind, LinkArg, Manifest, SCHEMA_VERSION,
};

/// The kit to link from, or `None` when nothing named one.
fn kit_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LUMEN_LINK_KIT_DIR")?);
    dir.join("manifest.json").is_file().then_some(dir)
}

/// Print why a linking test did nothing, in the one place that decides it.
fn no_kit() -> bool {
    if kit_dir().is_some() {
        return false;
    }
    eprintln!(
        "skipping: set LUMEN_LINK_KIT_DIR to a link kit for this platform to exercise \
         `lumenc package --static`"
    );
    true
}

/// A scratch directory that removes itself when the test ends. A linked
/// executable is large, so a run that leaves them behind fills the temp
/// filesystem.
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

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("lumenc_static_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    Scratch(dir)
}

/// A script that only says it ran, for an app that declares no module.
const PLAIN: &str = "fn on_start() { print(\"alive\"); }\n";

/// A script that writes a file through the `files` namespace, which is what
/// the `lumen-fs` module registers. The file it leaves is the proof that the
/// module was installed rather than merely linked.
const USES_FILES: &str =
    "fn on_start() {\n  print(\"alive\");\n  files::write(\"started.txt\", \"linked\");\n}\n";

/// A one-page app with one script.
fn write_app(dir: &Path, config: &str, script: &str) {
    std::fs::create_dir_all(dir.join("src")).expect("create src dir");
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root>\n  <label id=\"greeting\" text=\"linked\"/>\n  \
         <script src=\"main.rhai\"/>\n</root>\n",
    )
    .expect("write markup");
    std::fs::write(dir.join("src").join("main.rhai"), script).expect("write script");
    std::fs::write(dir.join("lumen.toml"), config).expect("write config");
}

fn run_package(args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lumenc"));
    command.arg("package").args(args);
    if let Some(kit) = kit_dir() {
        command.env("LUMEN_LINK_KIT_DIR", kit);
    }
    command.output().expect("run lumenc package")
}

fn exe_name(app: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{app}.exe")
    } else {
        app.to_string()
    }
}

/// Every file the package holds, one level deep, by name.
fn listing(out: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(out)
        .expect("read the package")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Package `app` statically and return the executable, failing the test with
/// whatever the command said if it did not.
fn link_app(app: &Path, out: &Path, name: &str) -> PathBuf {
    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        name,
        "--static",
    ]);
    assert!(
        result.status.success(),
        "package --static failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let exe = out.join(exe_name(name));
    assert!(exe.is_file(), "no executable at {}", exe.display());
    exe
}

/// The whole point: one executable, with a declared module inside it, that
/// runs from a directory that is neither the app nor the package.
#[test]
fn a_static_package_is_one_executable_carrying_its_declared_module() {
    if no_kit() {
        return;
    }
    let root = scratch("declared");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(
        &app,
        "[dependencies]\nlumen-fs = { bundled = true }\n",
        USES_FILES,
    );
    let out = root.join("out");

    let exe = link_app(&app, &out, "Demo");

    let names = listing(&out);
    assert!(
        !names.iter().any(|name| name.contains("liblumen")
            || name == "lumen.dll"
            || name.starts_with("libstd-")
            || name == "modules"),
        "a static package carries no library beside the executable: {names:?}"
    );
    assert!(names.contains(&"lumen.toml".to_string()), "{names:?}");

    let run = Command::new(&exe)
        .args(["--headless", "--ticks", "3"])
        .current_dir(&*root)
        .output()
        .expect("start the linked app");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success(), "the linked app failed: {output}");
    assert!(output.contains("alive"), "the script did not run: {output}");
    assert!(
        !output.contains("skipped"),
        "the module was linked in, so nothing is skipped: {output}"
    );
    // The script wrote it through the module's own namespace, against the
    // executable's directory.
    assert!(
        out.join("started.txt").is_file(),
        "the module's `files` namespace did not answer: {output}"
    );
}

/// An app that declares no module links without one, and the module it did
/// not declare is not in the file: its objects never reached the line.
#[test]
fn an_undeclared_module_is_left_out_of_the_executable() {
    if no_kit() {
        return;
    }
    let root = scratch("undeclared");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app, "", PLAIN);
    let bare = link_app(&app, &root.join("bare"), "Bare");

    // The same app, declaring the module, linked from the same kit. The only
    // difference between the two lines is that module.
    write_app(
        &app,
        "[dependencies]\nlumen-fs = { bundled = true }\n",
        PLAIN,
    );
    let with_fs = link_app(&app, &root.join("with-fs"), "WithFs");

    let bare_size = std::fs::metadata(&bare).expect("stat").len();
    let with_size = std::fs::metadata(&with_fs).expect("stat").len();
    assert!(
        bare_size < with_size,
        "the module's objects are only in the executable that declared it: \
         {bare_size} vs {with_size}"
    );

    let run = Command::new(&bare)
        .args(["--headless", "--ticks", "3"])
        .current_dir(&*root)
        .output()
        .expect("start the linked app");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success(), "the linked app failed: {output}");
    assert!(output.contains("alive"), "the script did not run: {output}");
}

/// Every request `--static` cannot answer, and what it says instead. None of
/// them reaches a link, so none of them needs a kit.
#[test]
fn the_requests_static_packaging_cannot_answer_are_refused() {
    let root = scratch("refusals");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");

    let refusal = |config: &str, args: &[&str], expect: &str| {
        write_app(&app, config, PLAIN);
        let out = root.join("out");
        let _ = std::fs::remove_dir_all(&out);
        let mut all = vec![
            app.to_str().expect("utf-8 path"),
            out.to_str().expect("utf-8 path"),
            "--static",
        ];
        all.extend_from_slice(args);
        let result = run_package(&all);
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        assert_eq!(result.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains(expect), "expected {expect:?} in: {stderr}");
        assert!(!out.exists(), "nothing was written: {stderr}");
    };

    // A trimmed engine is a from-source build, which this path is not.
    refusal(
        "[capabilities]\nhttp-fetch = false\n",
        &[],
        "[capabilities]",
    );
    // Only the modules the kit was built with can be linked in.
    refusal(
        "[dependencies]\nshape-tools = { path = \"modules/shape-tools\" }\n",
        &[],
        "dependency 'shape-tools'",
    );
    refusal(
        "[dependencies]\nmarkdown-widgets = \"1.2\"\n",
        &[],
        "dependency 'markdown-widgets'",
    );
    // The link runs through the tools installed here.
    let other = if cfg!(target_os = "linux") {
        "macos-aarch64"
    } else {
        "linux-x86_64"
    };
    refusal("", &["--target", other], other);

    // An SDK app brings its own executable.
    write_app(&app, "[app]\nkind = \"rust\"\n", PLAIN);
    let out = root.join("out");
    let result = run_package(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--static",
    ]);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("Rust"), "{stderr}");
    assert!(stderr.contains("without --static"), "{stderr}");
}

/// This machine's platform, named the way the release assets are.
fn host_target() -> (&'static str, &'static str) {
    let aarch64 = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "windows") {
        ("windows-x86_64", "x86_64-pc-windows-msvc")
    } else if cfg!(target_os = "macos") && aarch64 {
        ("macos-aarch64", "aarch64-apple-darwin")
    } else if cfg!(target_os = "macos") {
        ("macos-x86_64", "x86_64-apple-darwin")
    } else if aarch64 {
        ("linux-aarch64", "aarch64-unknown-linux-gnu")
    } else {
        ("linux-x86_64", "x86_64-unknown-linux-gnu")
    }
}

/// `lumenc package`, with the kit variable cleared: these tests say which kit
/// they mean, and the environment running them may name another.
fn package_with(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lumenc"));
    command.env_remove("LUMEN_LINK_KIT_DIR");
    command.arg("package").args(args).arg("--static");
    command
}

/// A directory a caller named is used as it is, so one that is not a kit is
/// answered rather than quietly fetched over.
#[test]
fn a_named_directory_that_is_not_a_kit_stops_the_package() {
    let root = scratch("not-a-kit");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app, "", PLAIN);
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).expect("create the empty directory");
    let out = root.join("out");

    let result = package_with(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
    ])
    .env("LUMEN_LINK_KIT_DIR", &empty)
    .output()
    .expect("run lumenc package");

    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("LUMEN_LINK_KIT_DIR"), "{stderr}");
    assert!(stderr.contains("manifest.json"), "{stderr}");
}

/// A kit and the lumenc that replays it come from one release, so a manifest
/// this build does not read is refused before a linker is started.
#[test]
fn a_kit_from_another_release_is_refused_before_anything_is_linked() {
    let root = scratch("other-schema");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app, "", PLAIN);

    let kit = root.join("kit");
    std::fs::create_dir_all(&kit).expect("create the kit directory");
    let mut manifest = synthetic_manifest(Vec::new());
    manifest.schema = SCHEMA_VERSION + 1;
    write_manifest(&kit, &manifest);
    let out = root.join("out");

    // Found through the toolchain search rather than by name, which is the
    // order a machine with an installed kit takes.
    let result = package_with(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--lib-dir",
        kit.to_str().expect("utf-8 path"),
    ])
    .output()
    .expect("run lumenc package");

    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("schema"), "{stderr}");
}

/// The manifest of a kit whose whole link line is `args`.
fn synthetic_manifest(args: Vec<LinkArg>) -> Manifest {
    let (target, triple) = host_target();
    Manifest {
        schema: SCHEMA_VERSION,
        target: target.to_string(),
        rust_triple: triple.to_string(),
        rustc: "rustc 1.97.0".to_string(),
        lumen_version: "0.0.0".to_string(),
        driver: Driver {
            kind: DriverKind::Cc,
            flavor: if cfg!(target_os = "macos") {
                "darwin".to_string()
            } else {
                "gnu".to_string()
            },
            path: None,
        },
        args,
        modules: Vec::new(),
        artifact: Artifact {
            kind: ArtifactKind::Append,
        },
    }
}

fn write_manifest(kit: &Path, manifest: &Manifest) {
    std::fs::write(
        kit.join("manifest.json"),
        serde_json::to_string(manifest).expect("the manifest encodes"),
    )
    .expect("write the manifest");
}

/// Whether there is a C compiler here to drive the link.
fn cc_present() -> bool {
    if Command::new("cc").arg("--version").output().is_ok() {
        return true;
    }
    eprintln!("skipping: no cc on this machine to replay a link kit through");
    false
}

/// The replay itself, against a kit built here: the recorded line runs, the
/// executable it names is written, and the app's artifact is inside it.
///
/// Unix only, because the driver is `cc`. The Windows kit is replayed through
/// the LLD it ships, and nothing here can produce one.
#[cfg(unix)]
#[test]
fn a_kit_links_the_app_into_the_executable_it_names() {
    if !cc_present() {
        return;
    }
    let root = scratch("replay");
    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    write_app(&app, "", PLAIN);

    let kit = root.join("kit");
    let stage = kit.join("stage");
    std::fs::create_dir_all(&stage).expect("create the stage directory");
    let source = root.join("launcher.c");
    std::fs::write(&source, "int main(void) { return 0; }\n").expect("write the C file");
    let object = stage.join("aa-launcher.o");
    let compiled = Command::new("cc")
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status()
        .expect("run cc");
    assert!(compiled.success(), "cc did not compile the object");

    write_manifest(
        &kit,
        &synthetic_manifest(vec![
            LinkArg::File {
                path: "aa-launcher.o".to_string(),
                module: None,
            },
            LinkArg::Lit {
                value: "-o".to_string(),
            },
            LinkArg::Out {
                prefix: String::new(),
            },
        ]),
    );

    let out = root.join("out");
    let result = package_with(&[
        app.to_str().expect("utf-8 path"),
        out.to_str().expect("utf-8 path"),
        "--name",
        "Replayed",
        "--lib-dir",
        kit.to_str().expect("utf-8 path"),
    ])
    .output()
    .expect("run lumenc package");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("linked"), "{stdout}");

    let exe = out.join("Replayed");
    let image = std::fs::read(&exe).expect("the link wrote the executable");
    // The launcher reads the artifact off the end of its own file: the length
    // last, the magic before it, and the artifact before that.
    assert_eq!(&image[image.len() - 16..image.len() - 8], b"LMNAPACK");
    let mut len = [0u8; 8];
    len.copy_from_slice(&image[image.len() - 8..]);
    let len = u64::from_le_bytes(len) as usize;
    assert!(
        len > 0 && len + 16 < image.len(),
        "{len} of {}",
        image.len()
    );

    assert!(
        listing(&out).contains(&"lumen.toml".to_string()),
        "the app's own files travel beside the executable"
    );
    assert!(
        !out.join("Replayed.lmna-staging").exists(),
        "the artifact the link was handed is not left behind"
    );
    assert!(is_executable(&exe), "the executable is one");
}

/// Selection, against a kit built here: one object stands in for a module,
/// and it reaches the executable when the app declares the module and stays
/// out of it when the app does not.
#[cfg(unix)]
#[test]
fn a_declared_module_reaches_the_executable_and_an_undeclared_one_does_not() {
    if !cc_present() {
        return;
    }
    let root = scratch("selection");
    let kit = root.join("kit");
    let stage = kit.join("stage");
    std::fs::create_dir_all(&stage).expect("create the stage directory");

    // The symbol a module's rlib is pulled in by. Its own object is what a
    // replay adds or drops.
    let symbol = "lumen_module_register_lumen_fs";
    let object = |name: &str, body: &str| {
        let source = root.join(format!("{name}.c"));
        std::fs::write(&source, body).expect("write the C file");
        let object = stage.join(format!("{name}.o"));
        let compiled = Command::new("cc")
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .status()
            .expect("run cc");
        assert!(compiled.success(), "cc did not compile {name}");
    };
    object("aa-launcher", "int main(void) { return 0; }\n");
    object("bb-module", &format!("void {symbol}(void) {{}}\n"));

    let mut manifest = synthetic_manifest(vec![
        LinkArg::File {
            path: "aa-launcher.o".to_string(),
            module: None,
        },
        LinkArg::File {
            path: "bb-module.o".to_string(),
            module: Some("lumen-fs".to_string()),
        },
        LinkArg::Lit {
            value: "-o".to_string(),
        },
        LinkArg::Out {
            prefix: String::new(),
        },
    ]);
    manifest.modules = vec![lumen_modules::link_kit::KitModule::new("lumen-fs")];
    write_manifest(&kit, &manifest);

    let app = root.join("demo");
    std::fs::create_dir_all(&app).expect("create app dir");
    let link = |config: &str, name: &str| {
        write_app(&app, config, PLAIN);
        let out = root.join(name);
        let result = package_with(&[
            app.to_str().expect("utf-8 path"),
            out.to_str().expect("utf-8 path"),
            "--name",
            name,
            "--lib-dir",
            kit.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("run lumenc package");
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let image = std::fs::read(out.join(name)).expect("the link wrote the executable");
        let carries = image.windows(symbol.len()).any(|w| w == symbol.as_bytes());
        (
            String::from_utf8_lossy(&result.stdout).into_owned(),
            carries,
        )
    };

    let (said, carries) = link(
        "[dependencies]\nlumen-fs = { bundled = true }\n",
        "Declared",
    );
    assert!(said.contains("1 module compiled in: lumen-fs"), "{said}");
    assert!(carries, "the declared module's object is in the executable");

    let (said, carries) = link("", "Bare");
    assert!(!said.contains("compiled in"), "{said}");
    assert!(
        !carries,
        "an app that declared nothing links neither the object nor the symbol"
    );
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .expect("stat the executable")
        .permissions()
        .mode()
        & 0o111
        != 0
}
