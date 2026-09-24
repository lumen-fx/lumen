//! The first-party browser add-ons under `std/addons`, as a build finds them.
//!
//! What they do in a page is `.github/scripts/web-addons-smoke.py`'s subject,
//! run against `web/tests/fixtures/std-addons`. This checks what a build reads
//! off disk: every package's descriptor, where `bundled = true` finds it, and
//! which target takes the canvas as an add-on rather than as its module.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_modules::Target;
use lumen_modules::addon::read_addon;
use lumenc::addons::{BUNDLED_ADDON_DIR, bundled_addon, libraries_of, target_deps};

const APP: &str = "web/tests/fixtures/std-addons";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("public/lumenc sits two levels under the repository")
        .to_path_buf()
}

/// Every package under `std/addons`, by directory name.
fn packages() -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> =
        std::fs::read_dir(repo().join("std").join(BUNDLED_ADDON_DIR))
            .expect("std/addons is readable")
            .map(|entry| entry.expect("a readable directory entry").path())
            .filter(|path| path.is_dir())
            .map(|path| {
                let name = path
                    .file_name()
                    .expect("a named directory")
                    .to_string_lossy()
                    .into_owned();
                (name, path)
            })
            .collect();
    found.sort();
    found
}

#[test]
fn every_first_party_addon_reads_and_names_its_own_namespace() {
    let packages = packages();
    assert!(
        !packages.is_empty(),
        "std/addons holds the first-party add-ons"
    );
    let mut namespaces = BTreeSet::new();
    let mut tags = BTreeSet::new();
    for (name, dir) in &packages {
        assert!(
            name.starts_with("lumen-"),
            "{name}: a first-party dependency name starts with lumen-"
        );
        let package = read_addon(name, dir).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            namespaces.insert(package.addon.namespace.clone()),
            "{name}: namespace `{}` is taken by another add-on",
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
}

#[test]
fn a_bundled_declaration_finds_the_checkout_copy() {
    for (name, dir) in packages() {
        let found = bundled_addon(&name, None).unwrap_or_else(|| panic!("{name} is not found"));
        assert_eq!(
            found.canonicalize().expect("the found copy exists"),
            dir.canonicalize().expect("the package exists"),
            "{name}"
        );
    }
    assert!(bundled_addon("lumen-no-such-addon", None).is_none());
}

#[test]
fn the_canvas_is_an_addon_on_the_web_and_a_module_elsewhere() {
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
    assert_eq!(
        js.config
            .get("allow")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(1),
        "the dependency's config table travels with the add-on"
    );

    let desktop = target_deps(&app, Target::Desktop, None).expect("the desktop build resolves");
    assert!(
        desktop.packages.is_empty(),
        "a desktop build takes no add-on here: {:?}",
        desktop
            .packages
            .iter()
            .map(|p| &p.addon.name)
            .collect::<Vec<_>>()
    );
    let cfg = lumenc::LumenToml::load_or_default(&app).expect("lumen.toml");
    let libraries = libraries_of(
        &app,
        &cfg.dependencies_for(Target::Desktop),
        Target::Desktop,
        &Default::default(),
        None,
    );
    assert_eq!(
        libraries
            .0
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["lumen-canvas"],
        "a desktop build loads the canvas module"
    );
}

#[test]
fn a_site_carries_the_config_and_writes_the_canvas_as_a_canvas() {
    let scratch = std::env::temp_dir().join(format!("lumen-std-addons-{}", std::process::id()));
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
