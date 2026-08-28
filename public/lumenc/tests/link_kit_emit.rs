//! `lumenc link-kit`, the way the release workflow reaches it.
//!
//! The subcommand is absent from `lumenc --help` and nobody runs it by hand,
//! so what this suite is about is that the workflow's own call answers: the
//! exit status a step is gated on, and the kit left behind.

#![cfg(feature = "package")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lumen_modules::link_kit::{Record, RecordEnv};

/// A scratch directory that removes itself when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("lumenc-link-kit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    Scratch(dir)
}

fn lumenc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(args)
        .output()
        .expect("run lumenc")
}

fn stdout(result: &Output) -> String {
    String::from_utf8_lossy(&result.stdout).into_owned()
}

fn stderr(result: &Output) -> String {
    String::from_utf8_lossy(&result.stderr).into_owned()
}

fn text(path: &Path) -> String {
    path.to_str().expect("utf-8 path").to_string()
}

/// A release step is gated on the exit status, so every way of asking for
/// nothing has one.
#[test]
fn the_subcommand_answers_for_itself() {
    let help = lumenc(&["link-kit", "--help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("lumenc link-kit emit"), "{help:?}");

    let none = lumenc(&["link-kit"]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr(&none).contains("USAGE"), "{}", stderr(&none));

    let unknown = lumenc(&["link-kit", "frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        stderr(&unknown).contains("unknown subcommand `frobnicate`"),
        "{}",
        stderr(&unknown)
    );
}

/// The step itself: a record and a stage directory in, a kit out, and the
/// line the workflow log carries.
#[test]
fn an_emit_writes_a_kit_and_says_where_it_put_it() {
    let root = scratch("emit");
    let stage = root.join("stage");
    std::fs::create_dir_all(&stage).expect("create the stage directory");
    std::fs::write(stage.join("aabbccdd-launcher.o"), b"an object").expect("stage a file");

    let record = root.join("record.jsonl");
    let line = serde_json::to_string(&Record {
        out: Some("/b/target/release/deps/lumen_launcher-9f".to_string()),
        argv: vec![
            "/b/target/release/deps/launcher.o".to_string(),
            "-o".to_string(),
            "/b/target/release/deps/lumen_launcher-9f".to_string(),
        ],
        staged_argv: vec![
            "aabbccdd-launcher.o".to_string(),
            "-o".to_string(),
            "/b/target/release/deps/lumen_launcher-9f".to_string(),
        ],
        cwd: "/b".to_string(),
        env: RecordEnv::default(),
    })
    .expect("the record encodes");
    std::fs::write(&record, line + "\n").expect("write the record");

    let out = root.join("kit");
    let result = lumenc(&[
        "link-kit",
        "emit",
        "--record",
        &text(&record),
        "--stage",
        &text(&stage),
        "--out",
        &text(&out),
        "--target",
        "linux-x86_64",
        "--target-dir",
        &text(&root.join("target")),
    ]);
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(stdout(&result).contains("wrote"), "{}", stdout(&result));
    assert!(out.join("manifest.json").is_file());
    assert!(out.join("stage").join("aabbccdd-launcher.o").is_file());

    // A record that is not there fails the step rather than writing a kit
    // with nothing in it.
    let result = lumenc(&[
        "link-kit",
        "emit",
        "--record",
        &text(&root.join("absent.jsonl")),
        "--stage",
        &text(&stage),
        "--out",
        &text(&root.join("empty-kit")),
        "--target",
        "linux-x86_64",
        "--target-dir",
        &text(&root.join("target")),
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(
        stderr(&result).contains("lumenc link-kit emit:"),
        "{}",
        stderr(&result)
    );
    assert!(!root.join("empty-kit").exists());
}
