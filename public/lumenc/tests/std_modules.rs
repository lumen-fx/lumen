//! The first-party modules under `std/`, as a build finds their web halves.
//!
//! What the web halves do in a page is `.github/scripts/web-addons-smoke.py`'s
//! subject, run against `web/tests/fixtures/std-modules`; what the desktop
//! halves do is each crate's own tests. This checks what a build reads off
//! disk: every web half's descriptor, where `bundled = true` finds a module's
//! root, and that a web build takes web halves where every other build takes
//! none.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_modules::Target;
use lumen_modules::addon::{read_web_half, web_half};
use lumenc::addons::{bundled_module, target_deps};

const APP: &str = "web/tests/fixtures/std-modules";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("public/lumenc sits two levels under the repository")
        .to_path_buf()
}

/// Every crate under `std/`, by package name, with its root.
fn modules() -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(repo().join("std"))
        .expect("std/ is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .map(|path| {
            let manifest: toml::Table = toml::from_str(
                &std::fs::read_to_string(path.join("Cargo.toml")).expect("Cargo.toml reads"),
            )
            .expect("Cargo.toml parses");
            let name = manifest["package"]["name"]
                .as_str()
                .expect("a package name")
                .to_string();
            (name, path)
        })
        .collect();
    found.sort();
    found
}

#[test]
fn every_web_half_reads_and_names_its_own_namespace() {
    let mut namespaces = BTreeSet::new();
    let mut tags = BTreeSet::new();
    let mut with_web_half = 0;
    for (name, root) in modules() {
        assert!(
            name.starts_with("lumen-"),
            "{name}: a first-party module's name starts with lumen-"
        );
        if web_half(&root).is_none() {
            continue;
        }
        with_web_half += 1;
        let package = read_web_half(&name, &root).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            namespaces.insert(package.addon.namespace.clone()),
            "{name}: namespace `{}` is taken by another module",
            package.addon.namespace
        );
        for element in &package.addon.elements {
            assert!(
                tags.insert(element.tag.clone()),
                "{name}: <{}> is answered for twice",
                element.tag
            );
        }
        // Every event an async function answers with follows one scheme:
        // `on_<namespace>_<function>`.
        for function in &package.addon.functions {
            if let Some(event) = &function.event {
                assert_eq!(
                    event,
                    &format!("on_{}_{}", package.addon.namespace, function.name),
                    "{name}: {}",
                    function.name
                );
            }
        }
    }
    assert!(with_web_half > 0, "some module under std/ has a web half");
}

#[test]
fn a_bundled_declaration_finds_the_checkout_crate() {
    for (name, root) in modules() {
        let found = bundled_module(&name, None).unwrap_or_else(|| panic!("{name} is not found"));
        assert_eq!(
            found.canonicalize().expect("the found root exists"),
            root.canonicalize().expect("the crate exists"),
            "{name}"
        );
    }
    assert!(bundled_module("lumen-no-such-module", None).is_none());
}

#[test]
fn an_installed_toolchain_s_modules_directory_answers_first() {
    let scratch =
        std::env::temp_dir().join(format!("lumen-std-modules-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let root = scratch.join("modules").join("lumen-storage");
    std::fs::create_dir_all(root.join("web")).expect("create the module root");
    assert_eq!(bundled_module("lumen-storage", Some(&scratch)), Some(root));
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_web_build_takes_every_web_half_and_a_desktop_build_none() {
    let app = repo().join(APP);

    let web = target_deps(&app, Target::Web, None).expect("the web build resolves");
    let names: BTreeSet<&str> = web.packages.iter().map(|p| p.addon.name.as_str()).collect();
    for name in [
        "lumen-browser",
        "lumen-canvas",
        "lumen-cookie",
        "lumen-js",
        "lumen-storage",
        "lumen-svg",
        "lumen-websocket",
    ] {
        assert!(names.contains(name), "the web build takes {name}");
    }
    let js = web
        .packages
        .iter()
        .find(|p| p.addon.name == "lumen-js")
        .expect("lumen-js");
    assert!(js.dir.ends_with("js/web"), "{}", js.dir.display());
    assert_eq!(
        js.config
            .get("allow")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(1),
        "the dependency's config table travels with the web half"
    );

    let desktop = target_deps(&app, Target::Desktop, None).expect("the desktop build resolves");
    assert!(
        desktop.packages.is_empty() && desktop.compile.addons.is_empty(),
        "a desktop build loads libraries and reads no web half"
    );
}

#[test]
fn a_site_carries_the_config_and_writes_the_canvas_as_a_canvas() {
    let scratch = std::env::temp_dir().join(format!("lumen-std-modules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let lib = scratch.join("lib");
    std::fs::create_dir_all(&lib).expect("create the runtime directory");
    std::fs::write(lib.join("lumen-web.wasm"), b"\0asm\x01\0\0\0").expect("write the wasm stub");
    std::fs::write(lib.join("lumen-web.js"), b"export function boot() {}\n")
        .expect("write the module stub");
    let site = scratch.join("site");
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(repo().join(APP))
        .arg("--out")
        .arg(&site)
        .arg("--lib-dir")
        .arg(&lib)
        .output()
        .expect("running lumenc web");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(site.join("lumen.web.json")).expect("the manifest"),
    )
    .expect("the manifest parses");
    let js = manifest["addons"]
        .as_array()
        .expect("addons")
        .iter()
        .find(|a| a["name"] == "lumen-js")
        .expect("lumen-js is in the manifest");
    assert_eq!(js["config"], r#"{"allow":["data:"]}"#);
    let websocket = manifest["addons"]
        .as_array()
        .expect("addons")
        .iter()
        .find(|a| a["name"] == "lumen-websocket")
        .expect("lumen-websocket is in the manifest");
    assert!(websocket.get("config").is_none(), "no config, no key");
    assert_eq!(manifest["foreign"]["canvas"]["html"], "canvas");

    let page = std::fs::read_to_string(site.join("index.html")).expect("the page");
    assert!(
        page.contains(r#"id="paint" data-lm="#) && page.contains(r#"data-lm-foreign="canvas""#),
        "{page}"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
