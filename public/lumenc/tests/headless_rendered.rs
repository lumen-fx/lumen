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
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
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

/// A timer armed once the app has gone idle fires on its own. Nothing else
/// ticks the app here: no input, no MCP wake, no animation. Before the loop
/// slept until the timer's deadline it parked on events alone, and the
/// callback waited for an unrelated tick that never came.
#[test]
fn an_idle_app_wakes_for_its_timer() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let dir = temp_app_dir("timer");
    std::fs::create_dir_all(dir.join("src")).expect("create src");
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root>\n  <label text=\"timer\" />\n  <script src=\"main.cdl\" />\n</root>\n",
    )
    .expect("write main.lmn");
    // Long enough that startup's own follow-up frames have settled and the
    // loop is parked when the deadline passes. The `armed` line separates the
    // app's startup from the wait on the timer when the bound is missed.
    std::fs::write(
        dir.join("src/main.cdl"),
        "import \"lumen.cdl\";\n\
         fn on_ready() { print(\"timer armed\"); lumen::set_timeout(\"t\", 1500); }\n\
         fn on_timer(name: string) { print(\"timer fired \" + name); }\n\
         fn main() {}\n",
    )
    .expect("write main.cdl");

    // The boot trace times each startup phase on stderr, so a missed bound
    // shows where the time went.
    let mut child = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("run")
        .arg(&dir)
        .arg("--headless")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("LUMEN_BOOT_TRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lumenc run --headless");
    let started = Instant::now();
    let (tx, rx) = mpsc::channel::<(bool, Duration, String)>();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_tx = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if out_tx.send((false, started.elapsed(), line)).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send((true, started.elapsed(), line)).is_err() {
                break;
            }
        }
    });

    let limit = Duration::from_secs(6);
    let mut armed = None;
    let mut fired = None;
    let mut log = Vec::new();
    while fired.is_none() {
        let Some(left) = limit.checked_sub(started.elapsed()) else {
            break;
        };
        let Ok((is_err, at, line)) = rx.recv_timeout(left) else {
            break;
        };
        if !is_err && line.contains("timer armed") {
            armed = Some(at);
        }
        if !is_err && line.contains("timer fired t") {
            fired = Some(at);
        }
        log.push(format!(
            "{at:>12.3?} {} {line}",
            if is_err { "err" } else { "out" }
        ));
    }
    let exited = child.try_wait().ok().flatten();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let Some(fired) = fired else {
        panic!(
            "the timer never fired on an idle headless app within {limit:?} of spawn; \
             armed at {armed:?}, exited with {exited:?}\n{}",
            log.join("\n")
        );
    };
    assert!(
        fired >= Duration::from_millis(1500),
        "the timer fired early, after {fired:?}"
    );
}
