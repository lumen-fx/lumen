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
    // CARGO_MANIFEST_DIR is `<root>/crates/lumenc`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/lumenc sits two levels below the workspace root")
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

/// An unknown attribute reaches the terminal, at its own severity. The
/// severity word was hardcoded `info` for every finding, which read as a
/// style nudge for something that drops what the author wrote.
#[test]
fn unknown_attribute_prints_a_warning_on_check() {
    let dir = std::env::temp_dir().join(format!("lumenc-unknown-attr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create app dir");
    std::fs::write(
        dir.join("main.lmn"),
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
