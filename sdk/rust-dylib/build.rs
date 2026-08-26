//! Stamp the engine build identity into `BUILD_ID`.
//!
//! Runtime modules are version-locked to the exact engine build they compiled
//! against: a module inlines `BUILD_ID` at its own compile time, and the
//! loader compares it against the running engine's value with exact equality
//! before any Rust symbol is touched. Nothing else detects skew - the dynamic
//! linker resolves happily across layout-changed rebuilds - so this string is
//! the whole defense, and it must be deterministic: the same source state,
//! toolchain, and feature set always produce the same value.
//!
//! Format (single spaces, no whitespace inside a field):
//!
//! ```text
//! lumen-engine <version> <source> rustc:<hash> features:<list>
//! ```
//!
//! - `<version>` is `CARGO_PKG_VERSION`.
//! - `<source>` is `git:<describe>` from `git describe --always --dirty
//!   --tags` (commit, tag distance, and a `-dirty` suffix), or `nogit` when
//!   the sources are not a git work tree (a release tarball); there the
//!   version alone carries the identity, which is exactly as precise as a
//!   tagged release needs.
//! - `<hash>` is an fnv1a64 of the full `rustc -vV` output, folding the
//!   compiler version, its commit, and the host triple into one token.
//! - `<list>` is the crate's enabled features, sorted, joined with `+`, or
//!   `none`.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let version = env::var("CARGO_PKG_VERSION").expect("cargo sets this");

    let source = git_describe(&manifest_dir).unwrap_or_else(|| "nogit".to_string());
    let rustc_hash = fnv1a64(rustc_identity().as_bytes());
    let features = enabled_features();

    let build_id =
        format!("lumen-engine {version} {source} rustc:{rustc_hash:016x} features:{features}");

    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets this")).join("build_id.rs");
    let with_nul = format!("{build_id}\0");
    std::fs::write(
        &out,
        format!(
            "/// The identity of this engine build. Runtime modules inline it at their\n\
             /// compile time; the loader compares it against the running engine's value\n\
             /// with exact equality. See `build.rs` for the format.\n\
             pub const BUILD_ID: &str = {build_id:?};\n\
             /// [`BUILD_ID`] with a trailing NUL, for the C-ABI probe surface.\n\
             pub const BUILD_ID_C: &str = {with_nul:?};\n"
        ),
    )
    .expect("write build_id.rs");
}

/// `git describe --always --dirty --tags` for the work tree holding the
/// manifest, or `None` outside one. Also emits rerun lines on the git state
/// files so a new commit or a staged change restamps the id.
fn git_describe(manifest_dir: &PathBuf) -> Option<String> {
    let git_dir = run(Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "--absolute-git-dir"]))?;
    let git_dir = PathBuf::from(git_dir.trim());
    for state in ["HEAD", "index", "packed-refs"] {
        println!("cargo:rerun-if-changed={}", git_dir.join(state).display());
    }
    let described = run(Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["describe", "--always", "--dirty", "--tags"]))?;
    let described: String = described
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if described.is_empty() {
        None
    } else {
        Some(format!("git:{described}"))
    }
}

/// Full `rustc -vV` output: version, commit hash and date, host triple, LLVM.
fn rustc_identity() -> String {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    run(Command::new(rustc).arg("-vV")).expect("rustc -vV runs")
}

/// The crate's enabled features, sorted and `+`-joined, or `none`.
fn enabled_features() -> String {
    let mut features: Vec<String> = env::vars()
        .filter_map(|(k, _)| {
            k.strip_prefix("CARGO_FEATURE_")
                .map(|f| f.to_ascii_lowercase().replace('_', "-"))
        })
        .collect();
    features.sort();
    if features.is_empty() {
        "none".to_string()
    } else {
        features.join("+")
    }
}

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
