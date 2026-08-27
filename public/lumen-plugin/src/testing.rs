//! Test support: build the fixture plugin cdylib and hand back its path.
//! Compiled only under the `testing` feature; used by this crate's own
//! integration tests and by the runtime's.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a nested cargo builds its fixtures: `<target>/<name>` normally,
/// and `<coverage target>/debug/<name>` under `cargo llvm-cov`. The
/// coverage tree's location is read off the inherited `LLVM_PROFILE_FILE`
/// (its directory is the coverage target dir), because the report step
/// walks that tree's `debug` profile directory for objects and an artifact
/// outside it maps no counters.
fn nested_target_dir(workspace_root: &Path, name: &str) -> PathBuf {
    if std::env::var_os("CARGO_LLVM_COV").is_some()
        && let Some(profile_pattern) = std::env::var_os("LLVM_PROFILE_FILE")
        && let Some(coverage_root) = Path::new(&profile_pattern).parent()
    {
        return coverage_root.join("debug").join(name);
    }
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"))
        .join(name)
}

/// Build the fixture crate (`public/lumen-plugin/fixture`) and return the
/// produced cdylib path. Builds into a dedicated subdirectory of the target
/// dir so a nested cargo never contends with the outer one; a fresh tree
/// pays one build, a warm one a no-op check. Under `cargo llvm-cov` the
/// nested build is instrumented by the inherited rustc wrapper and the
/// dlopened fixture's counters land in the inherited `LLVM_PROFILE_FILE`
/// pool, so its lines grade with everything else.
pub fn fixture_cdylib() -> PathBuf {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate sits two levels under the workspace root")
        .to_path_buf();
    let target_dir = nested_target_dir(&workspace_root, "rt-plugin-fixture");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(&workspace_root)
        .args([
            "build",
            "-p",
            "lumen-plugin-fixture",
            "--message-format=json-render-diagnostics",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "fixture build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // Cargo has reported lib target names both hyphenated and
        // underscored across versions; accept either.
        let name = msg["target"]["name"].as_str().unwrap_or_default();
        if msg["reason"] != "compiler-artifact"
            || !matches!(name, "lumen_plugin_fixture" | "lumen-plugin-fixture")
        {
            continue;
        }
        if let Some(files) = msg["filenames"].as_array() {
            for f in files {
                let path = PathBuf::from(f.as_str().unwrap_or_default());
                if matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("so" | "dylib" | "dll")
                ) {
                    return path;
                }
            }
        }
    }
    panic!("fixture build produced no cdylib artifact");
}

/// Build the fixture and copy it to a file of its own, named after `tag`.
///
/// A runtime plugin holds one instance per process and its registration is
/// decided by the configuration its init was given, so a test that wants a
/// different configuration needs a different library. Copying gives the copy
/// its own inode, which is what makes the dynamic loader map a second
/// independent image of the same code rather than hand back the first.
pub fn fixture_copy(tag: &str) -> PathBuf {
    let built = fixture_cdylib();
    let extension = built
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("lumen-plugin-fixtures-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the fixture copy directory is writable");
    let copy = dir.join(format!(
        "{}lumen_plugin_fixture_{tag}.{extension}",
        std::env::consts::DLL_PREFIX
    ));
    std::fs::copy(&built, &copy).expect("the fixture copies");
    copy
}
