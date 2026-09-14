//! Test support shared by the suites that drive the registry seam.
//!
//! Every one of them needs the same two things: a program that stands in for
//! `lpm`, and a resolution for it to print. The stand-in's source lives in
//! `tests/fixtures/lpm-stub.rs` and is compiled here once per test binary.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Build the `lpm` stand-in and hand back where it landed.
pub fn lpm_stub() -> &'static Path {
    static STUB: OnceLock<PathBuf> = OnceLock::new();
    STUB.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("lumenc-lpm-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
        let out = dir.join(if cfg!(windows) { "lpm.exe" } else { "lpm" });
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("lpm-stub.rs");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&out)
            .arg(&source)
            .output()
            .expect("rustc runs");
        assert!(
            built.status.success(),
            "the lpm stand-in did not build:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
        out
    })
}

/// Write the schema-1 resolution the stand-in prints, and hand back the file.
/// `packages` is the body of the `packages` array.
pub fn stub_answer(path: &Path, packages: &str) -> PathBuf {
    std::fs::write(
        path,
        format!("{{\"schema\":1,\"lock\":\"/dev/null\",\"packages\":[{packages}]}}"),
    )
    .expect("the stand-in answer is writable");
    path.to_path_buf()
}

/// One `lumen`-platform package of the resolution, at `dir`, carrying `file`.
pub fn lumen_package(name: &str, version: &str, target: &str, dir: &Path, file: &str) -> String {
    format!(
        "{{\"name\":\"{name}\",\"version\":\"{version}\",\"platform\":\"lumen\",\
         \"target\":\"{target}\",\"dir\":{},\"files\":[\"{file}\"]}}",
        serde_json::to_string(&dir.display().to_string()).expect("a path encodes"),
    )
}
