//! Stamp the engine build identity into `BUILD_ID`.
//!
//! Runtime modules are version-locked to the exact engine build they compiled
//! against: a module inlines `BUILD_ID` at its own compile time, and the
//! loader compares it against the running engine's value with exact equality
//! before any Rust symbol is touched. Nothing else detects skew - the dynamic
//! linker resolves happily across layout-changed rebuilds - so this string is
//! the whole defense, and it must be deterministic: the same source state and
//! toolchain always produce the same value, and different source states never
//! share one.
//!
//! Format (single spaces, no whitespace inside a field):
//!
//! ```text
//! lumen-engine <version> <source> rustc:<hash>
//! ```
//!
//! - `<version>` is `CARGO_PKG_VERSION`.
//! - `<source>` is `git:<describe>` from `git describe --always --dirty
//!   --tags` (commit and tag distance), or `nogit` when the sources are not
//!   a git work tree (a release tarball); there the version alone carries
//!   the identity, which is exactly as precise as a tagged release needs.
//!   A dirty tree extends the `-dirty` suffix with a content hash of the
//!   uncommitted state (`-dirty.<hash>`), so the rule fails closed: two
//!   builds from different dirty states never compare equal, while
//!   rebuilding the same dirty state keeps its id, and a clean tagged
//!   build (the release path) stays deterministic from the commit alone.
//! - `<hash>` is an fnv1a64 of the full `rustc -vV` output, folding the
//!   compiler version, its commit, and the host triple into one token.
//!
//! There is no features field. This crate declares no cargo features by
//! design: the engine graph's feature set is pinned in this manifest's
//! dependency list, so feature skew between an engine and a module can only
//! arrive as a manifest edit, which `<source>` already captures. A field
//! that can only ever read `none` would claim an axis the handshake does
//! not actually check.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let version = env::var("CARGO_PKG_VERSION").expect("cargo sets this");

    let source = git_describe(&manifest_dir).unwrap_or_else(|| "nogit".to_string());
    let rustc_hash = fnv1a64(rustc_identity().as_bytes());

    let build_id = format!("lumen-engine {version} {source} rustc:{rustc_hash:016x}");

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
    } else if described.ends_with("-dirty") {
        let print = dirty_fingerprint(manifest_dir);
        Some(format!("git:{described}.{print:016x}"))
    } else {
        Some(format!("git:{described}"))
    }
}

/// A content hash of everything `--dirty` stands for: the diff of the work
/// tree and index against `HEAD`, plus every untracked, unignored file's
/// path and bytes. Two different dirty states never hash equal, so a stale
/// module built from an earlier edit refuses against a rebuilt engine
/// instead of matching it on the shared `-dirty` marker; rebuilding the
/// same dirty state reproduces the same id, which keeps a module and the
/// engine it was built beside agreeing.
///
/// Each dirty path also gets a rerun line, so editing it again restamps the
/// id. A file that goes from clean to dirty is not watched yet; the stamp
/// catches up on the next run of this script.
fn dirty_fingerprint(manifest_dir: &PathBuf) -> u64 {
    let toplevel = run(Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "--show-toplevel"]))
    .map(|t| PathBuf::from(t.trim()))
    .unwrap_or_else(|| manifest_dir.clone());

    let mut state = Vec::new();
    if let Some(diff) = run(Command::new("git").arg("-C").arg(&toplevel).args([
        "-c",
        "color.ui=false",
        "diff",
        "HEAD",
        "--no-ext-diff",
        "--binary",
    ])) {
        state.extend_from_slice(diff.as_bytes());
    }
    if let Some(untracked) = run(Command::new("git").arg("-C").arg(&toplevel).args([
        "ls-files",
        "--others",
        "--exclude-standard",
    ])) {
        for path in untracked.lines().filter(|l| !l.is_empty()) {
            state.extend_from_slice(path.as_bytes());
            state.push(0);
            if let Ok(bytes) = std::fs::read(toplevel.join(path)) {
                state.extend_from_slice(&bytes);
            }
            state.push(0);
        }
    }
    if let Some(status) = run(Command::new("git")
        .arg("-C")
        .arg(&toplevel)
        .args(["status", "--porcelain"]))
    {
        for line in status.lines() {
            if let Some(path) = line.get(3..).filter(|p| !p.is_empty()) {
                // Rename lines read `old -> new`; watch the side that exists.
                let path = path.rsplit(" -> ").next().unwrap_or(path);
                println!(
                    "cargo:rerun-if-changed={}",
                    toplevel.join(path.trim_matches('"')).display()
                );
            }
        }
    }
    fnv1a64(&state)
}

/// Full `rustc -vV` output: version, commit hash and date, host triple, LLVM.
fn rustc_identity() -> String {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    run(Command::new(rustc).arg("-vV")).expect("rustc -vV runs")
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
