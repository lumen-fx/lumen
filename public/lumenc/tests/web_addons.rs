//! Browser add-ons through `lumenc web`, `lumenc check` and a build-time run.
//!
//! The app under test is `web/tests/fixtures/addon-echo`, which depends on the
//! `echo` add-on beside it for its web build. A browser loading the site is
//! `.github/scripts/web-page-smoke.sh`'s subject; this reads what a build put
//! on disk and what it said.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::Engine as _;
use sha2::{Digest, Sha384};

const APP: &str = "web/tests/fixtures/addon-echo";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("public/lumenc sits two levels under the repository")
        .to_path_buf()
}

/// A fresh directory of its own for one case.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-web-addons-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// A stand-in for the prebuilt browser runtime.
fn runtime_dir(scratch: &Path) -> PathBuf {
    let dir = scratch.join("lib");
    std::fs::create_dir_all(&dir).expect("create the runtime directory");
    std::fs::write(dir.join("lumen-web.wasm"), b"\0asm\x01\0\0\0").expect("write the wasm stub");
    std::fs::write(dir.join("lumen-web.js"), b"export function boot() {}\n")
        .expect("write the module stub");
    dir
}

/// Run `lumenc web app --out <scratch>/site` with `extra`.
fn web(app: &Path, scratch: &Path, extra: &[&str]) -> (Output, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(app)
        .arg("--out")
        .arg(scratch.join("site"))
        .arg("--lib-dir")
        .arg(runtime_dir(scratch))
        .args(extra)
        .output()
        .expect("running lumenc web");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output, text)
}

fn manifest(site: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(site.join("lumen.web.json")).expect("read the manifest");
    serde_json::from_str(&text).expect("the manifest is JSON")
}

#[test]
fn an_addon_ships_with_the_site_checked_and_named_for_its_contents() {
    let scratch = scratch("ships");
    let (output, text) = web(&repo().join(APP), &scratch, &[]);
    assert!(output.status.success(), "{text}");
    // The program was compiled against the add-on, and reads back with it.
    assert!(!text.contains("does not load"), "{text}");

    let site = scratch.join("site");
    let manifest = manifest(&site);
    let addon = &manifest["addons"][0];
    assert_eq!(addon["name"], "echo");
    let module = addon["module"].as_str().expect("a module path");
    let (root, file) = module.rsplit_once('/').expect("a directory and a file");
    assert_eq!(file, "echo.js");
    assert!(root.starts_with("addons/echo."), "{module}");
    assert_eq!(addon["styles"][0], format!("{root}/echo.css"));
    assert_eq!(manifest["foreign"]["echo-view"]["html"], "div");

    // The files the descriptor names are copied, and the descriptor is not.
    assert!(site.join(module).is_file());
    assert!(site.join(root).join("echo.css").is_file());
    assert!(!site.join(root).join("lumen-addon.toml").exists());

    // The page holds the module to the hash of the bytes it was shipped with.
    let bytes = std::fs::read(site.join(module)).expect("read the module");
    let integrity = format!(
        "sha384-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha384::digest(&bytes))
    );
    let page = std::fs::read_to_string(site.join("index.html")).expect("read the page");
    assert!(
        page.contains(&format!(r#""/{module}":"{integrity}""#)),
        "{page}"
    );
    assert!(
        page.contains(&format!(r#"import * as a0 from "/{module}";"#)),
        "{page}"
    );
    assert!(page.contains(r#"data-lm-foreign="echo-view""#), "{page}");
    assert!(page.contains(">the fallback</div>"), "{page}");
}

#[test]
fn a_build_time_run_warns_about_an_addon_call_and_writes_the_fallback() {
    let scratch = scratch("prerender");
    let (output, text) = web(&repo().join(APP), &scratch, &["--prerender", "run"]);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("called `echo::shout`, which runs only in a browser"),
        "{text}"
    );
    let page =
        std::fs::read_to_string(scratch.join("site").join("index.html")).expect("read the page");
    assert!(
        page.contains(r#"id="sync" data-lm="0.0">waiting<"#),
        "{page}"
    );
}

#[test]
fn check_accepts_an_app_that_calls_a_web_only_addon() {
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("check")
        .arg(repo().join(APP))
        .output()
        .expect("running lumenc check");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An app of one page and no script, with `toml` as its `lumen.toml`.
fn app_with(scratch: &Path, toml: &str) -> PathBuf {
    let app = scratch.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create the app");
    std::fs::write(app.join("lumen.toml"), toml).expect("write lumen.toml");
    std::fs::write(
        app.join("src/main.lmn"),
        "<root><label text=\"hi\" /></root>\n",
    )
    .expect("write the markup");
    app
}

#[test]
fn a_native_library_in_the_web_build_is_refused_and_a_desktop_one_is_not() {
    let scratch = scratch("native");
    let app = app_with(
        &scratch,
        "[dependencies]\nshapes = { path = \"lib/shapes\" }\n",
    );
    let (output, text) = web(&app, &scratch, &[]);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("'shapes'"), "{text}");
    assert!(text.contains("[target.desktop.dependencies]"), "{text}");

    let scratch = self::scratch("desktop-only");
    let app = app_with(
        &scratch,
        "[target.desktop.dependencies]\nshapes = { path = \"lib/shapes\" }\n",
    );
    let (output, text) = web(&app, &scratch, &[]);
    assert!(output.status.success(), "{text}");
}

#[test]
fn a_broken_descriptor_fails_the_build_naming_the_addon() {
    let scratch = scratch("broken");
    let app = app_with(&scratch, "[dependencies]\nbad = { path = \"bad\" }\n");
    std::fs::create_dir_all(app.join("bad")).expect("create the add-on");
    std::fs::write(
        app.join("bad/lumen-addon.toml"),
        "[addon]\nnamespace = \"bad\"\nmodule = \"missing.js\"\n",
    )
    .expect("write the descriptor");
    let (output, text) = web(&app, &scratch, &[]);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("add-on 'bad': module `missing.js` is not in the package"),
        "{text}"
    );
}
