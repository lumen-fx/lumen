// Drives the full-pipeline headless runtime (`RunOptions` /
// `run_app_headless_rendered`), which lumenc only exposes under the
// `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! `run_app_headless_rendered` - the full-pipeline headless mode behind
//! `lumenc run --headless`. Bounded (`--ticks`-style) runs must boot the
//! offscreen GPU renderer, tick, render, and take the graceful-close
//! path without ever creating a window.
//!
//! Skips itself when the machine has no GPU (same convention as
//! `lumen-render-wgpu/tests/smoke.rs`).

use lumen_render_wgpu::gpu_unavailable_reason;
use lumenc::{HeadlessOptions, RunOptions, run_app_headless_rendered};

const MARKUP: &str = r#"<root style="bg:#101018">
  <label id="hello" style="text-color:#ffffff">hello headless</label>
  <tile style="width:120px; height:40px; bg:#3050c0; radius:6px"/>
</root>"#;

/// Build a temp app dir whose `lumen.toml` disables the MCP server so
/// parallel tests never fight over a TCP port.
fn temp_app_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumenc-headless-test-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .expect("write lumen.toml");
    dir
}

#[test]
fn bounded_headless_run_renders_and_exits() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let dir = temp_app_dir("bounded");
    let opts = RunOptions::new(&dir).with_markup(MARKUP);
    run_app_headless_rendered(
        opts,
        HeadlessOptions {
            dpr: 1.0,
            ticks: Some(5),
        },
    )
    .expect("bounded headless run");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bounded_headless_run_at_fractional_dpr() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let dir = temp_app_dir("dpr");
    let mut opts = RunOptions::new(&dir).with_markup(MARKUP);
    opts.size = (200, 100);
    run_app_headless_rendered(
        opts,
        HeadlessOptions {
            dpr: 1.5,
            ticks: Some(3),
        },
    )
    .expect("headless run at dpr 1.5");
    let _ = std::fs::remove_dir_all(&dir);
}
