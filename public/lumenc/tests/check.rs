// Exercises `lumenc::check_app`, which lumenc only exposes under the
// `dev-run` feature (it loads `lumen.toml` config via the linked runtime).
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Smoke tests for `lumenc check_app`. Validates that the example apps
//! and the test fixtures parse cleanly - guards against directive drift
//! on the markup grammar.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/public/lumenc`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("public/lumenc sits two levels below the workspace root")
        .to_path_buf()
}

#[test]
fn scroll_tiles_app_parses() {
    let dir = workspace_root().join("apps").join("scroll-tiles");
    let report = lumenc::check_app(&dir).expect("apps/scroll-tiles parses");
    assert!(report.element_count > 5, "scroll-tiles has multiple tiles");
    assert!(report.has_script, "scroll-tiles has a <script> block");
}

#[test]
fn blank_no_css_app_parses() {
    let dir = workspace_root().join("fixtures").join("blank-no-css");
    let report = lumenc::check_app(&dir).expect("fixtures/blank-no-css parses");
    assert_eq!(report.element_count, 1, "blank app is just <root />");
    assert!(!report.has_script);
}

#[test]
fn missing_main_lumen_errors() {
    let dir = workspace_root().join("apps").join("does-not-exist");
    let err = lumenc::check_app(&dir).expect_err("missing dir errors");
    let msg = err.to_string();
    assert!(msg.contains("main.lmn"), "error mentions main.lmn: {msg}");
}

/// `check` dispatches the script compile by `[script] engine`. The candela
/// fixture pins `engine = "candela"` and its `main.cdl` uses candela syntax
/// (a prelude import + `fn on_start()`), which the Rhai checker cannot parse.
/// Before the dispatch fix `check` always ran the Rhai checker and this app
/// false-failed with a bogus Rhai parse error even though it loads and runs.
#[test]
fn candela_app_checks_clean() {
    let dir = workspace_root().join("fixtures").join("candela-smoke");
    let report =
        lumenc::check_app(&dir).expect("fixtures/candela-smoke checks clean under candela");
    assert!(report.has_script, "candela-smoke has a <script> block");
}

/// The body of a handler, as an author writes it: the two lines from #191,
/// which call a string method on an int.
const BROKEN_BODY: &str = "    let n = 1;\n    n.uppercase();";

/// The candela source of a one-button app whose click handler carries `body`.
fn handler_source(body: &str) -> String {
    format!(
        "import \"lumen.cdl\";\n\nfn on_start() {{\n    \
         lumen::on(\"click\", \"bump\", \"handle_bump\");\n}}\n\n\
         fn handle_bump(id: any) {{\n{body}\n}}\n\nfn main() {{}}\n"
    )
}

/// The 1-based line `BROKEN_BODY`'s failing call sits on in `src`.
fn line_of_uppercase(src: &str) -> u32 {
    src.lines()
        .position(|l| l.contains("n.uppercase()"))
        .map(|i| i as u32 + 1)
        .expect("the program carries the failing call")
}

/// Write a one-button candela app whose click handler carries `body`.
fn write_handler_app(dir: &std::path::Path, body: &str) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("src")).expect("create app dir");
    // No introspection server, so a check in CI binds no port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><button id=\"bump\" text=\"bump\"/><script src=\"main.cdl\"/></root>\n",
    )
    .expect("write main.lmn");
    std::fs::write(dir.join("src").join("main.cdl"), handler_source(body)).expect("write main.cdl");
}

/// #191: nothing in the script calls a handler, so its body used to reach the
/// compiler for the first time on the click that ran it; `check` accepted the
/// app and the click killed the script. The script compile `check` runs covers
/// every function in the app's own source that annotates its parameters, so the
/// app fails here instead, naming the call the author got wrong.
#[test]
fn a_broken_handler_body_fails_the_check() {
    let dir = std::env::temp_dir().join(format!("lumenc-broken-handler-{}", std::process::id()));
    write_handler_app(&dir, BROKEN_BODY);
    let broken = lumenc::check_app(&dir).err();

    // The same app with a handler that compiles is accepted, so what fails is
    // the body and not the shape of a function only the runtime calls.
    write_handler_app(&dir, "    lumen::signal_set(\"greeting\", \"clicked\");");
    let clean = lumenc::check_app(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let message = broken
        .expect("a handler that cannot compile fails the check")
        .to_string();
    clean.expect("a handler whose body compiles checks clean");
    assert!(
        message.contains("uppercase"),
        "the error names the failing call: {message}"
    );
    let line = line_of_uppercase(&handler_source(BROKEN_BODY));
    assert!(
        message.contains(&format!(":{line}:")),
        "the error points at the author's own line ({line}): {message}"
    );
}

/// Whether the templates the toolchain ships are on this machine.
///
/// They are downloaded rather than kept in the repository
/// (`tools/fetch-templates.sh`), so a checkout that has not run the script has
/// nothing for these cases to read. CI fetches before it tests.
fn templates_present() -> bool {
    match lumenc::cli::scaffold::payload_dir() {
        Ok(_) => true,
        Err(why) => {
            eprintln!("skipping: {why}");
            false
        }
    }
}

/// Every scaffold template checks clean as written. `check` compiles the
/// markup, the CSS, and the script under the host the script's extension
/// selects, so this is what proves a template a user scaffolds runs: the
/// candela ones type-check against the real host surface, the Lua and Rhai
/// ones parse on theirs.
#[test]
fn every_template_checks_clean() {
    if !templates_present() {
        return;
    }
    for template in lumenc::cli::scaffold::TEMPLATES {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_template_check_{}_{}",
            template.name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        lumenc::cli::scaffold::write_template(template.name, &dir)
            .unwrap_or_else(|e| panic!("scaffolding `{}`: {e}", template.name));

        let report = lumenc::check_app(&dir)
            .unwrap_or_else(|e| panic!("template `{}` should check clean: {e}", template.name));
        assert!(
            report.element_count > 0,
            "template `{}` should render at least one element",
            template.name
        );
        assert_eq!(
            report.has_script,
            template.name != "blank",
            "every template but `blank` ships a script"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Every candela template also compiles ahead of time, to an image a runtime
/// without the compiler can decode.
///
/// `check` proves a template compiles; this proves the compiled form is one
/// the VM accepts. The registry here is empty, so the load stops at the host
/// functions the prelude declares, which is the one failure this cannot avoid
/// and the one that says nothing about the image. Anything else means the
/// image itself is wrong.
#[test]
fn every_candela_template_builds_an_image_the_vm_accepts() {
    use lumen_script_candela::CandelaHost;
    use lumen_script_candela::candela::{HostRegistry, LoadError, load_program};

    if !templates_present() {
        return;
    }
    for template in lumenc::cli::scaffold::TEMPLATES {
        let script = lumenc::cli::scaffold::template_dir(template.name)
            .unwrap_or_else(|e| panic!("reading `{}`: {e}", template.name))
            .join("src")
            .join("main.cdl");
        if !script.is_file() {
            continue;
        }
        let source = std::fs::read_to_string(&script)
            .unwrap_or_else(|e| panic!("reading {}: {e}", script.display()));
        let bytes = CandelaHost::new()
            .compile_bytecode(&source, "main.cdl")
            .unwrap_or_else(|e| panic!("template `{}` compiles: {e}", template.name));
        match load_program(&bytes, &HostRegistry::new()) {
            Ok(_) | Err(LoadError::HostBinding(_)) => {}
            Err(e) => panic!("template `{}` image is not loadable: {e}", template.name),
        }
    }
}

/// An unknown attribute reaches the terminal, at its own severity. The
/// severity word was hardcoded `info` for every finding, which read as a
/// style nudge for something that drops what the author wrote.
#[test]
fn unknown_attribute_prints_a_warning_on_check() {
    let dir = std::env::temp_dir().join(format!("lumenc-unknown-attr-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).expect("create app dir");
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label tect=\"typo\" text=\"hi\"/></root>\n",
    )
    .expect("write main.lmn");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("run lumenc check");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    std::fs::remove_dir_all(&dir).ok();

    assert!(out.status.success(), "a lint finding does not fail check");
    assert!(
        stderr.contains("warn") && stderr.contains("[unknown-attribute]"),
        "expected a warn-level unknown-attribute line, got: {stderr}"
    );
    assert!(
        stderr.contains("tect"),
        "the finding names the attribute: {stderr}"
    );
    assert!(
        !stderr.contains("info  "),
        "an unknown attribute is not an info nudge: {stderr}"
    );
}

/// A `[pages] include` may be written ahead of the pages it names: the entry
/// that has no file yet is skipped, and the app checks on the pages that
/// exist instead of failing on the one the author has not written.
#[test]
fn include_naming_an_unwritten_page_checks_clean() {
    let dir = std::env::temp_dir().join(format!("lumenc-pages-unwritten-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("create app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[pages]\ninclude = [\"index.lmn\", \"settings.lmn\"]\n",
    )
    .expect("write lumen.toml");
    std::fs::write(
        dir.join("src").join("index.lmn"),
        "<root><label text=\"home\"/></root>\n",
    )
    .expect("write index.lmn");

    let report = lumenc::check_app(&dir).expect("an unwritten include entry does not fail check");
    assert!(report.element_count > 0, "the home page still compiles");

    std::fs::remove_dir_all(&dir).ok();
}

/// Walk `el` and its descendants, collecting every `id`.
fn ids(el: &lumenc::Element, out: &mut Vec<String>) {
    if let Some(id) = &el.attrs.id {
        out.push(id.clone());
    }
    for child in &el.children {
        ids(child, out);
    }
}

/// The weather app's `<template name="day">` is the widest fragment in the
/// tree: seven instances, markers in `id`, `src`, `text`, and in `tab-index`,
/// which is an integer by the time it reaches the IR.
#[test]
fn weather_app_expands_its_day_fragment() {
    let dir = workspace_root().join("apps").join("weather");
    let compiled = lumenc::compile_app(&dir).expect("apps/weather compiles");

    let mut found = Vec::new();
    ids(&compiled.ir.root, &mut found);
    for day in 0..7 {
        assert!(
            found.iter().any(|id| id == &format!("day-{day}")),
            "one instance per day: {found:?}"
        );
        assert!(found.iter().any(|id| id == &format!("day-{day}-name")));
    }

    let day = compiled
        .fragments
        .get("day")
        .expect("the artifact carries the declaration");
    assert!(
        day.params.iter().any(|p| p.name == "idx"),
        "the markers the body reads are its parameters"
    );

    // `tab-index="{tab}"` is a marker in a typed attribute: it parses per
    // instance, and each day lands on its own tab stop.
    let mut stops: Vec<i32> = Vec::new();
    fn tab_stops(el: &lumenc::Element, out: &mut Vec<i32>) {
        if el.attrs.classes.iter().any(|c| c == "day")
            && let Some(index) = el.attrs.tab_index
        {
            out.push(index);
        }
        for child in &el.children {
            tab_stops(child, out);
        }
    }
    tab_stops(&compiled.ir.root, &mut stops);
    assert_eq!(stops, vec![2, 3, 4, 5, 6, 7, 8]);
}

/// The pages demo puts its frame in `layout.lmn`, which is not a page: every
/// page instantiates it through the app-wide table.
#[test]
fn pages_demo_wraps_every_page_in_the_shared_layout() {
    let dir = workspace_root().join("apps").join("pages-demo");
    let compiled = lumenc::compile_app(&dir).expect("apps/pages-demo compiles");

    let gates = &compiled.ir.root.children;
    assert!(gates.len() >= 3, "one gate per page: {}", gates.len());
    for gate in gates {
        assert_eq!(gate.tag, "if");
        let frame = &gate.children[0];
        assert_eq!(frame.tag, "column", "each page opens with the shared frame");
        assert_eq!(
            frame.children[0].tag, "row",
            "the frame's nav bar renders ahead of the page's own content"
        );
    }
    assert!(
        compiled.fragments.get("layout").is_some(),
        "the artifact carries the layout declaration"
    );
}
