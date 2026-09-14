//! The registry seam, driven through the `lumenc` binary itself with a stand-in
//! for `lpm` (`tests/fixtures/lpm-stub.rs`).
//!
//! What is under test is the half `lumenc` owns: which requirements it reads
//! out of `lumen.toml`, the command line it builds from them, and what it does
//! with the JSON that comes back. The registry's own half is `lpm`'s.

#![cfg(all(feature = "runtime-parse", feature = "dev-run"))]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::lpm_stub;

/// A private directory for one case, emptied first.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenc-registry-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
    dir
}

/// Write a minimal markup app with the given `lumen.toml`.
fn app(tag: &str, lumen_toml: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label>hi</label></root>\n",
    )
    .expect("markup");
    std::fs::write(dir.join("lumen.toml"), lumen_toml).expect("config");
    dir
}

/// What the stand-in answers with, and where it records what it was asked.
struct Stub {
    json: PathBuf,
    argv: PathBuf,
    fail: Option<(String, i32)>,
}

impl Stub {
    /// Answer with `packages` (the `packages` array of the schema-1 JSON).
    fn answering(dir: &Path, packages: &str) -> Stub {
        Stub {
            json: common::stub_answer(&dir.join("lpm.json"), packages),
            argv: dir.join("lpm.argv"),
            fail: None,
        }
    }

    /// Fail the way the registry client does: one line on stderr, and a code.
    fn failing(dir: &Path, message: &str, code: i32) -> Stub {
        Stub {
            json: dir.join("lpm.json"),
            argv: dir.join("lpm.argv"),
            fail: Some((message.to_string(), code)),
        }
    }

    /// The command line `lumenc` built, one argument per line.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv)
            .expect("the stand-in recorded its arguments")
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Run `lumenc <args>` against the stand-in and hand back what it printed.
fn lumenc(stub: &Stub, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lumenc"));
    command
        .args(args)
        .env("LPM_BIN", lpm_stub())
        .env("LPM_STUB_JSON", &stub.json)
        .env("LPM_STUB_ARGV", &stub.argv)
        // An installed toolchain would look for a newer release; a test
        // never reaches the network.
        .env("LUMEN_NO_UPDATE_CHECK", "1");
    if let Some((message, code)) = &stub.fail {
        command.env("LPM_STUB_FAIL", message);
        command.env("LPM_STUB_EXIT", code.to_string());
    }
    command.output().expect("lumenc runs")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A package the stand-in reports, with a library file written at its root so
/// the by-name probe finds one.
fn lumen_package(dir: &Path, name: &str, version: &str) -> String {
    let root = dir.join("pkg").join(name);
    std::fs::create_dir_all(&root).expect("package root");
    let file = &lumen_modules::library_spellings(name)[0];
    std::fs::write(root.join(file), b"stand-in library bytes").expect("library");
    common::lumen_package(name, version, lumenc::lpm::host_target(), &root, file)
}

/// An app that names nothing in the registry never looks for `lpm`, so a
/// machine without one still runs every app that does not need it.
#[test]
fn nothing_runs_without_a_version_source() {
    let dir = app(
        "no-versions",
        "[dependencies]\nlocal = { path = \"modules/local\" }\n",
    );
    let stub = Stub::failing(&dir, "the stand-in must not be called", 1);
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    // The check itself fails on the missing local module or passes; what
    // matters is that no lpm ran, which the recording file proves.
    assert!(
        !stub.argv.exists(),
        "lpm ran for an app with no version source"
    );
    let err = stderr_of(&out);
    assert!(!err.contains("lpm"), "{err}");
}

/// A `lumen.toml` nobody can read is the error, rather than an app that
/// quietly declares nothing. `fetch` has no later step to report it from.
#[test]
fn a_lumen_toml_that_does_not_parse_is_the_error() {
    let dir = app("bad-toml", "[dependencies]\nshape-tools = 7\n");
    let stub = Stub::failing(&dir, "the stand-in must not be called", 1);
    let out = lumenc(&stub, &["fetch", &dir.display().to_string()]);
    let err = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2));
    assert!(err.contains("lumen.toml"), "{err}");
    assert!(err.contains("shape-tools"), "{err}");
    assert!(!stub.argv.exists(), "nothing was resolved");
}

/// Every requirement crosses on the command line, with the table it came from
/// left behind: `lpm` is told what to resolve, never where it was written.
#[test]
fn every_requirement_crosses_on_the_command_line() {
    let dir = app(
        "argv",
        "[dependencies]\nshape-tools = \"1.2\"\nlocal = { path = \"m\" }\n\
         \n[[plugins]]\nname = \"markdown\"\nversion = \"2\"\n",
    );
    let stub = Stub::answering(
        &dir,
        &format!(
            "{},{}",
            lumen_package(&dir, "shape-tools", "1.2.3"),
            lumen_package(&dir, "markdown", "2.0.1")
        ),
    );
    lumenc(&stub, &["check", &dir.display().to_string()]);

    let argv = stub.argv();
    assert_eq!(
        argv.first().map(String::as_str),
        Some("install"),
        "{argv:?}"
    );
    assert!(argv.contains(&"--json".to_string()), "{argv:?}");
    assert!(
        argv.contains(&format!("lumen@{}", env!("CARGO_PKG_VERSION"))),
        "{argv:?}"
    );
    assert!(argv.contains(&"shape-tools@1.2".to_string()), "{argv:?}");
    assert!(argv.contains(&"markdown@2".to_string()), "{argv:?}");
    assert!(
        !argv.iter().any(|a| a.starts_with("local@")),
        "a path source is not a registry requirement: {argv:?}"
    );
    assert!(
        argv.contains(&lumenc::lpm::host_target().to_string()),
        "{argv:?}"
    );
    assert!(
        argv.iter().any(|a| a.ends_with("lumen.lock")),
        "the lock lumenc names is the app's: {argv:?}"
    );
    assert!(!argv.contains(&"--offline".to_string()), "{argv:?}");
}

/// `--offline` is read once, before dispatch, and reaches the resolution the
/// command runs several calls further down.
#[test]
fn offline_reaches_the_resolution() {
    let dir = app("offline", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::answering(&dir, &lumen_package(&dir, "shape-tools", "1.0.0"));
    lumenc(&stub, &["check", &dir.display().to_string(), "--offline"]);
    assert!(stub.argv().contains(&"--offline".to_string()));
}

/// `lumenc fetch` resolves for the platform it was asked about, which is what
/// a cross-target package needs.
#[test]
fn fetch_resolves_for_another_platform() {
    let dir = app("fetch-target", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::answering(&dir, &lumen_package(&dir, "shape-tools", "1.0.0"));
    let out = lumenc(
        &stub,
        &[
            "fetch",
            &dir.display().to_string(),
            "--target",
            "macos-aarch64",
            "--locked",
        ],
    );
    let argv = stub.argv();
    assert!(argv.contains(&"macos-aarch64".to_string()), "{argv:?}");
    assert!(argv.contains(&"--locked".to_string()), "{argv:?}");
    assert!(out.status.success(), "{}", stderr_of(&out));
}

/// An unknown `--target` is a usage error naming nothing it would have
/// downloaded.
#[test]
fn fetch_refuses_a_platform_that_does_not_exist() {
    let dir = app("fetch-bad-target", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::answering(&dir, &lumen_package(&dir, "shape-tools", "1.0.0"));
    let out = lumenc(
        &stub,
        &[
            "fetch",
            &dir.display().to_string(),
            "--target",
            "plan9-x86_64",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("plan9-x86_64"));
    assert!(!stub.argv.exists(), "nothing was resolved");
}

/// What `lpm` says when it fails is what the user reads, and the exit code it
/// used says which kind of failure it was.
#[test]
fn the_clients_own_words_reach_the_user() {
    let dir = app("client-error", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::failing(&dir, "no version of shape-tools satisfies ^1", 1);
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(
        err.contains("no version of shape-tools satisfies ^1"),
        "{err}"
    );
}

/// A lock that would move under `--locked` exits 3, and the message says what
/// to run instead of leaving the code to be looked up.
#[test]
fn a_lock_that_would_move_says_what_to_run() {
    let dir = app("locked", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::failing(&dir, "lumen.lock would change", 3);
    let out = lumenc(&stub, &["fetch", &dir.display().to_string(), "--locked"]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(err.contains("lumenc update"), "{err}");
}

/// A miss under `--offline` exits 4, and the message says the download has
/// not happened rather than that the package does not exist.
#[test]
fn an_offline_miss_says_it_is_a_missing_download() {
    let dir = app("offline-miss", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::failing(&dir, "shape-tools 1.0.0 is not downloaded", 4);
    let out = lumenc(&stub, &["check", &dir.display().to_string(), "--offline"]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(err.contains("not been downloaded"), "{err}");
}

/// A package for a platform `lumenc` has nothing to do with is named, with
/// its platform, rather than being passed to a loader that would refuse it.
#[test]
fn a_platform_lumenc_has_no_use_for_is_named() {
    let dir = app("platform", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::answering(
        &dir,
        "{\"name\":\"shape-tools\",\"version\":\"1.0.0\",\"platform\":\"zig\",\
         \"target\":\"any\",\"dir\":\"/tmp\"}",
    );
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(err.contains("shape-tools"), "{err}");
    assert!(err.contains("zig platform"), "{err}");
}

/// A `lumenc` that reads schema 1 says so when it meets another, rather than
/// reading the fields it recognizes out of a shape that changed.
#[test]
fn a_newer_resolution_schema_is_refused() {
    let dir = app("schema", "[dependencies]\nshape-tools = \"1\"\n");
    let json = dir.join("lpm.json");
    std::fs::write(&json, "{\"schema\":7,\"packages\":[]}").expect("answer");
    let stub = Stub {
        json,
        argv: dir.join("lpm.argv"),
        fail: None,
    };
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(err.contains("schema 7"), "{err}");
    assert!(err.contains("update lumenc"), "{err}");
}

/// A resolved package that carries no library for this platform names what it
/// does carry, so the answer is the package rather than the lookup.
#[test]
fn a_package_without_a_library_names_what_it_holds() {
    let dir = app("no-library", "[dependencies]\nshape-tools = \"1\"\n");
    let empty = dir.join("pkg").join("empty");
    std::fs::create_dir_all(&empty).expect("package root");
    let stub = Stub::answering(
        &dir,
        &format!(
            "{{\"name\":\"shape-tools\",\"version\":\"1.0.0\",\"platform\":\"lumen\",\
             \"target\":\"{}\",\"dir\":{},\"files\":[\"README.md\"]}}",
            lumenc::lpm::host_target(),
            serde_json::to_string(&empty.display().to_string()).expect("a path encodes"),
        ),
    );
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    let err = stderr_of(&out);
    assert!(!out.status.success());
    assert!(err.contains("README.md"), "{err}");
}

/// `lumenc add` writes the declaration, resolves it, and writes back the
/// version the registry settled on; `lumenc remove` takes it out again and
/// leaves the author's own file as it was.
#[test]
fn add_and_remove_round_trip_through_lumen_toml() {
    let before = "# my app\n\
                  [window]\n\
                  title = \"demo\"   # the window title\n\
                  \n\
                  [dependencies]\n\
                  # the filesystem module\n\
                  lumen-fs = { bundled = true }\n";
    let dir = app("add-remove", before);
    let stub = Stub::answering(&dir, &lumen_package(&dir, "shape-tools", "1.4.2"));

    let out = lumenc(&stub, &["add", "shape-tools", &dir.display().to_string()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let after = std::fs::read_to_string(dir.join("lumen.toml")).expect("read back");
    assert!(after.contains("shape-tools = \"1.4.2\""), "{after}");
    assert!(after.contains("# the filesystem module"), "{after}");
    assert!(
        after.contains("title = \"demo\"   # the window title"),
        "{after}"
    );
    assert!(after.starts_with("# my app\n"), "{after}");

    let out = lumenc(
        &stub,
        &["remove", "shape-tools", &dir.display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("lumen.toml")).expect("read back"),
        before
    );
}

/// `--plugin` puts the package in the other table, and `--config` rides along
/// with the types the author typed.
#[test]
fn add_declares_a_compiler_plugin_with_its_config() {
    let dir = app("add-plugin", "[window]\ntitle = \"demo\"\n");
    let stub = Stub::answering(&dir, &lumen_package(&dir, "markdown", "3.1.0"));
    let out = lumenc(
        &stub,
        &[
            "add",
            "markdown@3",
            &dir.display().to_string(),
            "--plugin",
            "--config",
            "flavor=gfm",
            "--config",
            "strict=true",
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let after = std::fs::read_to_string(dir.join("lumen.toml")).expect("read back");
    assert!(after.contains("[[plugins]]"), "{after}");
    assert!(after.contains("name = \"markdown\""), "{after}");
    assert!(after.contains("version = \"3\""), "{after}");
    assert!(after.contains("flavor = \"gfm\""), "{after}");
    assert!(after.contains("strict = true"), "{after}");
}

/// A `candela` package becomes an import root: the app's script imports it by
/// the name it was declared under, and the compile reads the package's own
/// `.cdl` sources.
#[test]
fn a_candela_package_is_an_import_root() {
    let dir = app("candela-root", "[dependencies]\nshapes = \"1\"\n");
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label>hi</label><script src=\"main.cdl\"/></root>\n",
    )
    .expect("markup");
    std::fs::write(
        dir.join("src").join("main.cdl"),
        "import \"shapes\";\n\nfn main() {\n    let n = area(3, 4);\n}\n",
    )
    .expect("script");

    // The package the registry resolved, carrying the module the script
    // imports under the package's own name.
    let package = dir.join("pkg").join("shapes");
    std::fs::create_dir_all(&package).expect("package root");
    std::fs::write(
        package.join("shapes.cdl"),
        "fn area(w: int, h: int) -> int {\n    return w * h;\n}\n",
    )
    .expect("package source");

    let stub = Stub::answering(
        &dir,
        &format!(
            "{{\"name\":\"shapes\",\"version\":\"1.0.0\",\"platform\":\"candela\",\
             \"target\":\"any\",\"dir\":{},\"files\":[\"shapes.cdl\"]}}",
            serde_json::to_string(&package.display().to_string()).expect("a path encodes"),
        ),
    );
    let out = lumenc(&stub, &["check", &dir.display().to_string()]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // The ahead-of-time build reads the same root, which is what makes a
    // packaged app carry the package: the bytecode in the artifact is
    // compiled with the import resolved, so nothing has to travel beside it.
    let artifact = dir.join("app.lmna");
    let out = lumenc(
        &stub,
        &[
            "build",
            &dir.display().to_string(),
            &artifact.display().to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(artifact.is_file(), "the artifact was written");

    // Without the root the same import has nowhere to read from, which is
    // what makes the passes above statements about the root rather than about
    // candela ignoring the import.
    let bare = Stub::answering(&dir, "");
    let out = lumenc(&bare, &["check", &dir.display().to_string()]);
    assert!(
        !out.status.success(),
        "the import resolved without the package: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `lumenc update` names the packages it was given and nothing else.
#[test]
fn update_asks_for_the_named_packages() {
    let dir = app(
        "update",
        "[dependencies]\nshape-tools = \"1\"\ngeom = \"0.3\"\n",
    );
    let stub = Stub::answering(
        &dir,
        &format!(
            "{},{}",
            lumen_package(&dir, "shape-tools", "1.5.0"),
            lumen_package(&dir, "geom", "0.3.9")
        ),
    );
    let out = lumenc(
        &stub,
        &["update", "shape-tools", "--dir", &dir.display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let argv = stub.argv();
    assert_eq!(argv.first().map(String::as_str), Some("update"), "{argv:?}");
    assert_eq!(
        argv.last().map(String::as_str),
        Some("shape-tools"),
        "{argv:?}"
    );
}

/// Updating a name the app never declared is a usage error, not a request the
/// registry gets to answer.
#[test]
fn update_refuses_a_name_the_app_does_not_declare() {
    let dir = app("update-unknown", "[dependencies]\nshape-tools = \"1\"\n");
    let stub = Stub::answering(&dir, &lumen_package(&dir, "shape-tools", "1.0.0"));
    let out = lumenc(
        &stub,
        &["update", "ghost", "--dir", &dir.display().to_string()],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("ghost"));
}
