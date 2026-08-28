//! The recorder as a build runs it: the real binary, in the linker's place,
//! with a linker behind it.
//!
//! Everything here spawns `link-recorder` rather than calling into it. What
//! the shim is for is the moment between rustc handing a linker its arguments
//! and the linker returning, and the two halves of that - the record it leaves
//! and the status it passes back - are only observable from outside the
//! process.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that removes itself when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("link-recorder-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    Scratch(dir)
}

/// A program that ignores its arguments and exits with `code`, which is what
/// a linker is to this shim. Writing one is the platform-specific part: a Unix
/// shell script needs its interpreter line and its execute bit, and Windows
/// runs a batch file.
#[cfg(unix)]
fn linker_exiting(dir: &Path, code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(format!("linker-{code}.sh"));
    fs::write(&path, format!("#!/bin/sh\nexit {code}\n")).expect("write the linker");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod the linker");
    path
}

#[cfg(windows)]
fn linker_exiting(dir: &Path, code: i32) -> PathBuf {
    let path = dir.join(format!("linker-{code}.cmd"));
    fs::write(&path, format!("@exit /b {code}\r\n")).expect("write the linker");
    path
}

/// The recorder, with every variable it reads either set or cleared: the
/// process running the tests may have any of them set already.
struct Run {
    command: Command,
}

impl Run {
    fn new() -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_link-recorder"));
        for name in [
            "LUMEN_LINK_RECORD",
            "LUMEN_LINK_STAGE",
            "LUMEN_REAL_LINKER",
            "LUMEN_REAL_LINKER_HINT",
            "LUMEN_LINK_RSP_STYLE",
        ] {
            command.env_remove(name);
        }
        Run { command }
    }

    fn env(mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> Run {
        self.command.env(name, value);
        self
    }

    fn recording(self, record: &Path, stage: &Path) -> Run {
        self.env("LUMEN_LINK_RECORD", record)
            .env("LUMEN_LINK_STAGE", stage)
    }

    fn args<S: AsRef<std::ffi::OsStr>>(mut self, args: &[S]) -> Run {
        self.command.args(args);
        self
    }

    fn run(mut self) -> Output {
        self.command.output().expect("run link-recorder")
    }
}

/// The single line the recorder appended, as JSON.
fn one_record(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path).expect("read the record");
    let mut lines = text.lines();
    let line = lines.next().expect("the recorder wrote a line");
    assert_eq!(lines.next(), None, "one link, one line: {text}");
    serde_json::from_str(line).expect("the line is JSON")
}

fn strings(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list: {value}"))
        .iter()
        .map(|v| v.as_str().expect("a string").to_string())
        .collect()
}

fn path_arg(path: &Path) -> String {
    path.to_str().expect("utf-8 path").to_string()
}

/// A path as a line of a GNU-shaped response file, where a backslash escapes
/// the character after it and a path separator therefore doubles.
fn rsp_line(path: &Path) -> String {
    path_arg(path).replace('\\', "\\\\")
}

/// A staged name is the first four bytes of the content hash, a hyphen, and
/// the file's own name.
fn is_staged_name(name: &str, base: &str) -> bool {
    match name.split_once('-') {
        Some((hash, rest)) => {
            hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) && rest == base
        }
        None => false,
    }
}

/// The whole of what a build gets out of the shim: the line as rustc wrote it
/// with the response file expanded, the same line with every input replaced by
/// the copy the kit will carry, and the copies themselves.
#[test]
fn a_recorded_link_stages_its_inputs_and_passes_the_linker_through() {
    let root = scratch("staged");
    let stage = root.join("stage");
    let record = root.join("record.jsonl");

    let object = root.join("app.o");
    fs::write(&object, b"an object file").expect("write the object");
    let rlib = root.join("libdep.rlib");
    fs::write(&rlib, b"an archive").expect("write the rlib");
    let missing = root.join("deleted.o");
    // The output of a rebuild is a file that is already there, and staging it
    // would copy the last binary into the kit for nothing.
    let out = root.join("app");
    fs::write(&out, b"the previous build").expect("write the old output");

    // rustc hands the linker part of its line in a response file, so a
    // recorder that did not expand would record one argument and stage
    // nothing.
    let response = root.join("args.rsp");
    fs::write(&response, format!("-lm\n{}\n", rsp_line(&rlib))).expect("write the response file");

    let result = Run::new()
        .recording(&record, &stage)
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .args(&[
            "-m64".to_string(),
            path_arg(&object),
            path_arg(&missing),
            format!("@{}", path_arg(&response)),
            "-o".to_string(),
            path_arg(&out),
        ])
        .run();
    assert!(
        result.status.success(),
        "the linker exited 0, so the shim does: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let line = one_record(&record);
    assert_eq!(line["out"].as_str(), Some(path_arg(&out).as_str()));
    let argv = strings(&line, "argv");
    assert_eq!(
        argv,
        vec![
            "-m64".to_string(),
            path_arg(&object),
            path_arg(&missing),
            "-lm".to_string(),
            path_arg(&rlib),
            "-o".to_string(),
            path_arg(&out),
        ],
        "the response file is expanded in place"
    );

    let staged = strings(&line, "staged_argv");
    let differ: Vec<usize> = (0..argv.len()).filter(|i| argv[*i] != staged[*i]).collect();
    assert_eq!(
        differ,
        vec![1, 4],
        "exactly the two arguments naming a file that exists: {staged:?}"
    );
    assert!(is_staged_name(&staged[1], "app.o"), "{staged:?}");
    assert!(is_staged_name(&staged[4], "libdep.rlib"), "{staged:?}");
    assert_eq!(
        fs::read(stage.join(&staged[1])).expect("the object was copied"),
        b"an object file"
    );
    assert_eq!(
        fs::read(stage.join(&staged[4])).expect("the rlib was copied"),
        b"an archive"
    );
    assert!(
        !stage.join("app").exists(),
        "the output is not staged even though it is there"
    );
}

/// The MSVC shape: a UTF-16 response file, whitespace-separated, naming the
/// output in the spelling that linker takes.
#[test]
fn a_utf16_response_file_expands_and_names_the_output() {
    let root = scratch("utf16");
    let stage = root.join("stage");
    let record = root.join("record.jsonl");

    let object = root.join("main.o");
    fs::write(&object, b"an object file").expect("write the object");
    let out = root.join("app.exe");

    // Quoted, which is how that linker's response files carry a path with a
    // space in it; a backslash there is a path separator rather than an escape.
    let mut bytes = vec![0xFF, 0xFE];
    let text = format!("\"/OUT:{}\" \"{}\"", path_arg(&out), path_arg(&object));
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let response = root.join("args.rsp");
    fs::write(&response, &bytes).expect("write the response file");

    let result = Run::new()
        .recording(&record, &stage)
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .args(&[format!("@{}", path_arg(&response))])
        .run();
    assert!(result.status.success(), "{result:?}");

    let line = one_record(&record);
    assert_eq!(line["out"].as_str(), Some(path_arg(&out).as_str()));
    let argv = strings(&line, "argv");
    assert_eq!(
        argv,
        vec![format!("/OUT:{}", path_arg(&out)), path_arg(&object)]
    );
    let staged = strings(&line, "staged_argv");
    assert_eq!(staged[0], argv[0], "the output argument is passed through");
    assert!(is_staged_name(&staged[1], "main.o"), "{staged:?}");
}

/// A build reads the linker's status, so the shim's own is the linker's and
/// nothing else. With no record asked for, nothing is written either.
#[test]
fn the_linkers_status_is_the_shims_status() {
    let root = scratch("status");
    let record = root.join("record.jsonl");

    let result = Run::new()
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 3))
        .args(&["-o", "app"])
        .run();
    assert_eq!(result.status.code(), Some(3));
    assert!(!record.exists(), "nothing was asked to be recorded");
}

/// Guessing a linker would silently link with one the record does not name,
/// so an unanswerable request says what to set instead.
#[test]
fn a_linker_that_cannot_be_found_is_named_rather_than_guessed() {
    let root = scratch("no-linker");

    let result = Run::new().args(&["-o", "app"]).run();
    assert_eq!(result.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(stderr.contains("LUMEN_REAL_LINKER"), "{stderr}");
    assert!(stderr.contains("LUMEN_REAL_LINKER_HINT"), "{stderr}");

    let hints = std::env::join_paths([root.join("one"), root.join("two")]).expect("join paths");
    let result = Run::new()
        .env("LUMEN_REAL_LINKER_HINT", &hints)
        .args(&["-o", "app"])
        .run();
    assert_eq!(result.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        stderr.contains("no entry of LUMEN_REAL_LINKER_HINT"),
        "{stderr}"
    );

    // A name that is not a program at all reports what it tried to run.
    let result = Run::new()
        .env("LUMEN_REAL_LINKER", root.join("not-a-program"))
        .args(&["-o", "app"])
        .run();
    assert_eq!(result.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(stderr.contains("cannot run the linker"), "{stderr}");
}

/// The hint list is for a caller that knows where a linker might be and not
/// which one is installed: the first entry that is a file wins.
#[test]
fn the_first_hint_that_exists_is_the_linker() {
    let root = scratch("hint");
    let hints =
        std::env::join_paths([root.join("absent"), linker_exiting(&root, 0)]).expect("join paths");

    let result = Run::new()
        .env("LUMEN_REAL_LINKER_HINT", &hints)
        .args(&["-o", "app"])
        .run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// A half-staged kit links binaries that cannot be reproduced, so a staging
/// failure fails the build rather than the kit.
#[test]
fn a_stage_directory_that_cannot_be_made_fails_the_link() {
    let root = scratch("bad-stage");
    let stage = root.join("stage");
    // A file where the directory has to go.
    fs::write(&stage, b"not a directory").expect("write the blocker");

    let result = Run::new()
        .recording(&root.join("record.jsonl"), &stage)
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .args(&["-o", "app"])
        .run();
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(stderr.contains("cannot create"), "{stderr}");
}

/// Response files nest, and a cycle of them would not terminate.
#[test]
fn response_files_that_nest_too_deep_are_refused() {
    let root = scratch("nested");
    let file = |n: usize| root.join(format!("args{n}.rsp"));
    for n in 0..10 {
        fs::write(file(n), format!("@{}\n", rsp_line(&file(n + 1)))).expect("write");
    }
    fs::write(file(10), "-lm\n").expect("write the last");

    let result = Run::new()
        .recording(&root.join("record.jsonl"), &root.join("stage"))
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .args(&[format!("@{}", path_arg(&file(0)))])
        .run();
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(stderr.contains("eight deep"), "{stderr}");
}

/// `LUMEN_LINK_RSP_STYLE` settles the shape for a toolchain that writes
/// neither of the two rustc does: the bytes here are UTF-8, which reads as one
/// argument per line until the variable says otherwise.
#[test]
fn the_response_style_variable_overrides_the_shape_the_bytes_suggest() {
    let root = scratch("style");
    let record = root.join("record.jsonl");
    let response = root.join("args.rsp");
    fs::write(&response, "-a -b\n-c\n").expect("write the response file");

    let result = Run::new()
        .recording(&record, &root.join("stage"))
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .env("LUMEN_LINK_RSP_STYLE", "msvc")
        .args(&[format!("@{}", path_arg(&response))])
        .run();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        strings(&one_record(&record), "argv"),
        vec!["-a", "-b", "-c"]
    );

    let record = root.join("gnu.jsonl");
    let result = Run::new()
        .recording(&record, &root.join("stage"))
        .env("LUMEN_REAL_LINKER", linker_exiting(&root, 0))
        .env("LUMEN_LINK_RSP_STYLE", "gnu")
        .args(&[format!("@{}", path_arg(&response))])
        .run();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(strings(&one_record(&record), "argv"), vec!["-a -b", "-c"]);
}
