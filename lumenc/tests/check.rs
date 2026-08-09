// Exercises `lumenc::check_app`, which lumenc only exposes under the
// `dev-run` feature (it loads `lumen.toml` config via the linked runtime).
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbol.
#![cfg(feature = "dev-run")]

//! Smoke tests for `lumenc check_app`. Validates that the apps shipped
//! in `apps/` parse cleanly - guards against directive drift on the
//! markup grammar.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/lumenc` after the workspace restructure.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn counter_app_parses() {
    let dir = workspace_root().join("apps").join("counter");
    let report = lumenc::check_app(&dir).expect("apps/counter parses");
    assert!(report.element_count > 5, "counter has multiple tiles");
    assert!(report.has_script, "counter has a <script> block");
}

#[test]
fn blank_no_css_app_parses() {
    let dir = workspace_root().join("apps").join("blank-no-css");
    let report = lumenc::check_app(&dir).expect("apps/blank-no-css parses");
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
    let dir = workspace_root().join("apps").join("candela-smoke");
    let report = lumenc::check_app(&dir).expect("apps/candela-smoke checks clean under candela");
    assert!(report.has_script, "candela-smoke has a <script> block");
}
