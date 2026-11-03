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
            ..HeadlessOptions::default()
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
            ..HeadlessOptions::default()
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
    // loop is parked when the deadline passes. The `armed` line marks the end
    // of startup, where the wait on the timer begins.
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

    // The wake is timed from the moment the timer is armed, so a slow start on
    // a loaded runner does not count against it. Startup has its own, looser
    // bound so a child that never gets to `on_ready` still fails the test.
    let startup_limit = Duration::from_secs(60);
    let limit = Duration::from_secs(6);
    let mut armed: Option<Duration> = None;
    let mut fired = None;
    let mut log = Vec::new();
    while fired.is_none() {
        let deadline = armed.map_or(startup_limit, |at| at + limit);
        let Some(left) = deadline.checked_sub(started.elapsed()) else {
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
            "the timer never fired on an idle headless app within {limit:?} of being armed; \
             armed at {armed:?} after spawn, exited with {exited:?}\n{}",
            log.join("\n")
        );
    };
    assert!(
        fired >= Duration::from_millis(1500),
        "the timer fired early, after {fired:?}"
    );
}

/// Where the window-state file of `app_id` lands under a state root that the
/// spawned lumenc is pointed at with [`state_env`].
fn saved_state_path(root: &std::path::Path, app_id: &str) -> std::path::PathBuf {
    let base = if cfg!(target_os = "macos") {
        root.join("Library").join("Application Support")
    } else {
        root.to_path_buf()
    };
    base.join("lumen").join(app_id).join("window-state.toml")
}

/// Point every per-user state directory lumenc consults at `root`, so the
/// test never reads or writes the real user's window state.
fn state_env(cmd: &mut std::process::Command, root: &std::path::Path) {
    cmd.env("XDG_STATE_HOME", root).env("LOCALAPPDATA", root);
    if cfg!(target_os = "macos") {
        cmd.env("HOME", root);
    }
}

/// A headless run sizes its viewport from `--size`, else `[window] size`,
/// even for an app that remembers its window: the size saved by the last
/// windowed run belongs to that window, and the headless run leaves the saved
/// file as it was.
#[test]
fn a_headless_run_takes_its_size_over_the_saved_window_state() {
    use std::process::Command;

    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let app_id = "lumenc-headless-size-over-state";
    let dir = temp_app_dir("size-over-state");
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[app]\nid = \"{app_id}\"\n\n[window]\nsize = [800, 600]\nremember_state = true\n\n\
             [mcp]\nport = 0\n"
        ),
    )
    .expect("write lumen.toml");
    std::fs::create_dir_all(dir.join("src")).expect("create src");
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root>\n  <label text=\"size\" />\n  <script src=\"main.cdl\" />\n</root>\n",
    )
    .expect("write main.lmn");
    std::fs::write(
        dir.join("src/main.cdl"),
        "import \"lumen.cdl\";\n\
         fn on_ready() {\n\
             let s = window::size();\n\
             print(\"viewport \" + string(s[0]) + \"x\" + string(s[1]));\n\
         }\n\
         fn main() {}\n",
    )
    .expect("write main.cdl");

    let state_root = dir.join("state");
    let saved = saved_state_path(&state_root, app_id);
    std::fs::create_dir_all(saved.parent().expect("state dir")).expect("create state dir");
    let saved_body = "size = [920, 654]\nmaximized = false\n";
    std::fs::write(&saved, saved_body).expect("write saved window state");

    let run = |size: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumenc"));
        cmd.arg("run")
            .arg(&dir)
            .arg("--headless")
            .args(["--ticks", "5"]);
        if let Some(size) = size {
            cmd.args(["--size", size]);
        }
        cmd.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
        state_env(&mut cmd, &state_root);
        let out = cmd.output().expect("run lumenc run --headless");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "lumenc run --headless failed: {}\nstdout:\n{stdout}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    };

    let flagged = run(Some("500x300"));
    let configured = run(None);
    let left = std::fs::read_to_string(&saved).expect("read saved window state");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        flagged.contains("viewport 500.0x300.0"),
        "`--size 500x300` lost to the saved window state:\n{flagged}"
    );
    assert!(
        configured.contains("viewport 800.0x600.0"),
        "`[window] size` lost to the saved window state:\n{configured}"
    );
    assert_eq!(
        left, saved_body,
        "a headless run rewrote the saved window state"
    );
}
