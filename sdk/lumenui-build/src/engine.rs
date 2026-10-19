//! Points the compiler at the prebuilt engine `lumenui` re-exports.
//!
//! A Lumen release ships, per desktop target, `lumenui-engine-<target>.tar.zst`
//! (the engine's compiled crates, the proc-macro libraries they need, and any
//! native library they link but do not carry) and `lumenui-engine-<target>.json`
//! (the manifest of that set: which rustc built it, for which target, from
//! which Lumen version). [`link_engine`] finds the set for the calling crate's
//! version and tells rustc to search it, so `extern crate lumenui_engine`
//! resolves and the app links the engine without compiling any of it.
//!
//! In order:
//!
//! 1. docs.rs builds with no network: the crate documents itself without the
//!    engine (`cfg(lumenui_docs_only)`), and nothing is fetched.
//! 2. Cross-compiling is refused. The set holds proc-macro libraries, which
//!    run inside the compiler and so are built for the machine the build runs
//!    on; a set for another target has nothing this machine can load.
//! 3. The set comes from `LUMEN_ENGINE_ARTIFACTS` when that names a directory,
//!    otherwise from the release tagged `v<version>`: the crate and the set are
//!    one build, so no other release fits.
//! 4. The manifest's rustc is compared with the one cargo runs before the
//!    compiler is pointed at anything, because the compiler's own answer to a
//!    mismatch (E0514) does not say what to do about it.
//! 5. The set's `lumen-crash` helper is copied into the profile directory
//!    (`target/debug`, `target/release`), beside the app's executable, where
//!    a crashing app looks for it. A plain `cargo run` then writes full crash
//!    reports with no packaging step. The copy is skipped when an identical
//!    file is already there.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fetch::{self, COMPLETE, Packing};
use crate::{env, fail};

const OVERRIDE: &str = "LUMEN_ENGINE_ARTIFACTS";
const MANIFEST: &str = "manifest.json";
/// The crash helper the set carries under `bin/`.
const HELPER: &str = "lumen-crash";

/// The body of `lumenui`'s build script: point the compiler at the prebuilt
/// engine of the release `version` names. Exits the build with a message
/// saying what to do when that cannot happen.
pub fn link_engine(version: &str) {
    for var in [OVERRIDE, "LUMEN_GH_REPO", "DOCS_RS"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo::rustc-check-cfg=cfg(lumenui_docs_only)");

    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo:rustc-cfg=lumenui_docs_only");
        return;
    }
    if let Err(message) = run(version) {
        fail(&message);
    }
}

fn run(version: &str) -> Result<(), String> {
    let target = env("TARGET")?;
    let host = env("HOST")?;
    if target != host {
        return Err(format!(
            "lumenui {version} cannot cross-compile yet: this build targets {target} on a \
             {host} machine. The prebuilt engine carries proc-macro libraries the compiler \
             loads, and those are built for the machine they run on. Build on a {target} \
             machine instead."
        ));
    }

    let dir = match std::env::var_os(OVERRIDE).filter(|v| !v.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            let manifest = read_manifest(&dir.join(MANIFEST))?;
            check_manifest(&manifest, version, &target)?;
            dir
        }
        None => fetch_set(version, &target)?,
    };
    println!("cargo:rerun-if-changed={}", dir.join(MANIFEST).display());

    let deps = dir.join("deps");
    let native = dir.join("native");
    println!("cargo:rustc-link-search=crate={}", deps.display());
    println!("cargo:rustc-link-search=dependency={}", deps.display());
    if native.is_dir() {
        println!("cargo:rustc-link-search=native={}", native.display());
    }
    // The app builds and runs without the helper, so failing to place it (a
    // running copy Windows will not replace, say) is a warning, not an error.
    if let Err(message) = place_helper(&dir, &target) {
        println!("cargo:warning=lumenui: {message}");
    }
    Ok(())
}

/// Copy the set's crash helper beside the executables this build produces.
///
/// The profile directory is three levels above `OUT_DIR`
/// (`<profile>/build/<crate>-<hash>/out`), with or without `--target`. A set
/// with no helper (one built before it joined the set) is not an error: the
/// app then writes the short report itself.
fn place_helper(dir: &Path, target: &str) -> Result<(), String> {
    let name = if target.contains("windows") {
        format!("{HELPER}.exe")
    } else {
        HELPER.to_string()
    };
    let from = dir.join("bin").join(&name);
    if !from.is_file() {
        println!(
            "cargo:warning=lumenui: the prebuilt engine set in {} has no bin/{name}, so a \
             crash leaves a short report and no minidump",
            dir.display()
        );
        return Ok(());
    }
    let out_dir = PathBuf::from(env("OUT_DIR")?);
    let Some(profile) = out_dir.ancestors().nth(3) else {
        return Err(format!(
            "cannot find the profile directory above {}",
            out_dir.display()
        ));
    };
    let to = profile.join(&name);
    let bytes = std::fs::read(&from).map_err(|e| format!("read {}: {e}", from.display()))?;
    if std::fs::read(&to).is_ok_and(|have| have == bytes) {
        return Ok(());
    }
    // Written beside and renamed into place, so an app still running the old
    // helper never sees a half-written file.
    let staging = profile.join(format!(".{name}.{}", std::process::id()));
    std::fs::write(&staging, &bytes).map_err(|e| format!("write {}: {e}", staging.display()))?;
    set_executable(&staging)?;
    std::fs::rename(&staging, &to).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        format!("move {name} into {}: {e}", profile.display())
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// What the set says about itself.
struct Manifest {
    lumen_version: String,
    triple: String,
    rustc_release: String,
    rustc_commit: String,
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "{} is not a prebuilt engine set: cannot read {}: {e}",
            path.parent().unwrap_or(path).display(),
            path.display()
        )
    })?;
    parse_manifest(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not a manifest: {e}"))?;
    let field = |path: &[&str]| -> Result<String, String> {
        let mut at = &value;
        for key in path {
            at = at
                .get(key)
                .ok_or_else(|| format!("the manifest has no `{}`", path.join(".")))?;
        }
        at.as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("the manifest's `{}` is not a string", path.join(".")))
    };
    Ok(Manifest {
        lumen_version: field(&["lumen_version"])?,
        triple: field(&["triple"])?,
        rustc_release: field(&["rustc", "release"])?,
        rustc_commit: field(&["rustc", "commit_hash"])?,
    })
}

/// The rustc first, so a wrong toolchain is the error an app author sees,
/// then the set's own identity.
fn check_manifest(manifest: &Manifest, version: &str, target: &str) -> Result<(), String> {
    let (release, commit) = rustc_version()?;
    if release != manifest.rustc_release || commit != manifest.rustc_commit {
        return Err(format!(
            "lumenui {version} needs rustc {want}, and this build runs rustc {release}. \
             The prebuilt engine is readable only by the compiler that built it. Add a \
             rust-toolchain.toml beside your Cargo.toml with:\n\n    [toolchain]\n    \
             channel = \"{want}\"\n",
            want = manifest.rustc_release,
        ));
    }
    if manifest.triple != target {
        return Err(format!(
            "the prebuilt engine set is for {}, and this build targets {target}",
            manifest.triple
        ));
    }
    if manifest.lumen_version != version {
        return Err(format!(
            "the prebuilt engine set is from Lumen {}, and this is lumenui {version}; \
             the two are one build and must match",
            manifest.lumen_version
        ));
    }
    Ok(())
}

/// `release` and `commit-hash` from `$RUSTC -vV`, the pair the manifest
/// records.
fn rustc_version() -> Result<(String, String), String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(&rustc)
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run {rustc} -vV: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
            .ok_or_else(|| format!("{rustc} -vV printed no `{key}` line"))
    };
    Ok((line("release:")?, line("commit-hash:")?))
}

fn fetch_set(version: &str, target: &str) -> Result<PathBuf, String> {
    let name = fetch::target_name(target).ok_or_else(|| {
        format!(
            "Lumen releases ship no prebuilt engine for {target}. Build one from a Lumen \
             checkout with tools/release/lumenui-engine.py and point {OVERRIDE} at it"
        )
    })?;
    let dir = fetch::cache_dir("lumenui-engine", version, name).ok_or_else(|| {
        format!(
            "cannot tell where the user cache directory is (HOME / LOCALAPPDATA unset); \
             set {OVERRIDE} to an unpacked engine set instead"
        )
    })?;
    if dir.join(COMPLETE).is_file() {
        let manifest = read_manifest(&dir.join(MANIFEST))?;
        check_manifest(&manifest, version, target)?;
        return Ok(dir);
    }

    let base = fetch::release_base(version);
    let sums = fetch::checksums(
        &base,
        &format!(
            "lumenui {version} links the prebuilt engine of the Lumen v{version} release; if \
             that release is not published, or this machine is offline, set {OVERRIDE} to an \
             unpacked engine set"
        ),
    )?;

    // The manifest first and on its own: it names the compiler the set needs,
    // so a wrong toolchain is reported before the large download.
    let manifest_name = format!("lumenui-engine-{name}.json");
    let manifest_bytes = fetch::get_verified(&base, &manifest_name, &sums)?;
    let manifest = parse_manifest(&String::from_utf8_lossy(&manifest_bytes))
        .map_err(|e| format!("{manifest_name}: {e}"))?;
    check_manifest(&manifest, version, target)?;

    let archive_name = format!("lumenui-engine-{name}.tar.zst");
    println!(
        "cargo:warning=lumenui: downloading the prebuilt engine ({archive_name}) into {}",
        dir.display()
    );
    fetch::fetch_archive(&base, &archive_name, &sums, Packing::TarZstd, &dir)?;
    Ok(dir)
}
