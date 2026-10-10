//! Points the compiler at the prebuilt engine this crate re-exports.
//!
//! A Lumen release ships, per desktop target, `lumenui-engine-<target>.tar.zst`
//! (the engine's compiled crates, the proc-macro libraries they need, and any
//! native library they link but do not carry) and `lumenui-engine-<target>.json`
//! (the manifest of that set: which rustc built it, for which target, from
//! which Lumen version). This script finds the set for this crate's own
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
//!    otherwise from the release tagged `v<this crate's version>`: the crate
//!    and the set are one build, so no other release fits. Downloads are
//!    verified against the release's `sha256sums.txt` and kept in the user
//!    cache directory, keyed by version and target.
//! 4. The manifest's rustc is compared with the one cargo runs before the
//!    compiler is pointed at anything, because the compiler's own answer to a
//!    mismatch (E0514) does not say what to do about it.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const OVERRIDE: &str = "LUMEN_ENGINE_ARTIFACTS";
const MANIFEST: &str = "manifest.json";
/// The marker a finished unpack leaves, so a cache interrupted halfway is
/// fetched again rather than trusted.
const COMPLETE: &str = ".complete";

fn main() {
    for var in [OVERRIDE, "LUMEN_GH_REPO", "DOCS_RS"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(lumenui_docs_only)");

    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo:rustc-cfg=lumenui_docs_only");
        return;
    }
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let version = env("CARGO_PKG_VERSION")?;
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
            check_manifest(&manifest, &version, &target)?;
            dir
        }
        None => fetch(&version, &target)?,
    };
    println!("cargo:rerun-if-changed={}", dir.join(MANIFEST).display());

    let deps = dir.join("deps");
    let native = dir.join("native");
    println!("cargo:rustc-link-search=crate={}", deps.display());
    println!("cargo:rustc-link-search=dependency={}", deps.display());
    if native.is_dir() {
        println!("cargo:rustc-link-search=native={}", native.display());
    }
    Ok(())
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("cargo did not set {name} for the build script"))
}

// ---------------------------------------------------------------- manifest

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

// ------------------------------------------------------------------- fetch

/// The release's name for a target, the one every asset name carries.
fn target_name(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "x86_64-apple-darwin" => "macos-x86_64",
        "aarch64-apple-darwin" => "macos-aarch64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        "aarch64-pc-windows-msvc" => "windows-aarch64",
        _ => return None,
    })
}

fn repo() -> String {
    std::env::var("LUMEN_GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "lumen-fx/lumen".to_string())
}

/// Where downloaded sets are kept: the platform cache directory, keyed by
/// version and target so an upgrade never reuses the old set.
fn cache_dir(version: &str, name: &str) -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(
        base.join("lumen")
            .join("lumenui-engine")
            .join(version)
            .join(name),
    )
}

fn fetch(version: &str, target: &str) -> Result<PathBuf, String> {
    let name = target_name(target).ok_or_else(|| {
        format!(
            "Lumen releases ship no prebuilt engine for {target}. Build one from a Lumen \
             checkout with tools/release/lumenui-engine.py and point {OVERRIDE} at it"
        )
    })?;
    let dir = cache_dir(version, name).ok_or_else(|| {
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

    let base = format!("https://github.com/{}/releases/download/v{version}", repo());
    let sums_url = format!("{base}/sha256sums.txt");
    let sums = String::from_utf8(get(&sums_url).map_err(|e| {
        format!(
            "cannot read {sums_url}: {e}. lumenui {version} links the prebuilt engine of \
             the Lumen v{version} release; if that release is not published, or this \
             machine is offline, set {OVERRIDE} to an unpacked engine set"
        )
    })?)
    .map_err(|_| format!("{sums_url} is not text"))?;

    // The manifest first and on its own: it names the compiler the set needs,
    // so a wrong toolchain is reported before the large download.
    let manifest_name = format!("lumenui-engine-{name}.json");
    let manifest_bytes = get_verified(&base, &manifest_name, &sums)?;
    let manifest = parse_manifest(&String::from_utf8_lossy(&manifest_bytes))
        .map_err(|e| format!("{manifest_name}: {e}"))?;
    check_manifest(&manifest, version, target)?;

    let archive_name = format!("lumenui-engine-{name}.tar.zst");
    println!(
        "cargo:warning=lumenui: downloading the prebuilt engine ({archive_name}) into {}",
        dir.display()
    );
    let parent = dir
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let download = parent.join(format!(".{archive_name}.{}", std::process::id()));
    let result = download_verified(&base, &archive_name, &sums, &download)
        .and_then(|()| unpack(&download, &dir));
    let _ = std::fs::remove_file(&download);
    result?;
    Ok(dir)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Download one release asset and check it against its line in
/// `sha256sums.txt`; an asset the file does not list is refused.
fn get_verified(base: &str, name: &str, sums: &str) -> Result<Vec<u8>, String> {
    let want = checksum_for(sums, name)
        .ok_or_else(|| format!("the release's sha256sums.txt lists no {name}"))?;
    let url = format!("{base}/{name}");
    let bytes = get(&url).map_err(|e| format!("cannot download {url}: {e}"))?;
    let got = hex(&Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "{name} does not match the release's checksum (expected {want}, got {got})"
        ));
    }
    Ok(bytes)
}

/// The digest `sha256sum` wrote for `name`: `<hex>  <name>`, or `<hex> *<name>`
/// in binary mode.
fn checksum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name).then_some(digest)
    })
}

/// [`get_verified`] for an asset too large to hold in memory: streamed into
/// `path`, hashed on the way.
fn download_verified(base: &str, name: &str, sums: &str, path: &Path) -> Result<(), String> {
    let want = checksum_for(sums, name)
        .ok_or_else(|| format!("the release's sha256sums.txt lists no {name}"))?;
    let url = format!("{base}/{name}");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("cannot download {url}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n])
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    let got = hex(&hasher.finalize());
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "{name} does not match the release's checksum (expected {want}, got {got})"
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Unpack beside `dir` and move it into place, so a concurrent build never
/// sees half a set and an interrupted one leaves nothing that looks finished.
fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dir.display()))?;
    let staging = parent.join(format!(".unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let file =
        std::fs::File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(std::io::BufReader::new(file))
        .map_err(|e| format!("the engine archive is not zstd: {e}"))?;
    tar::Archive::new(decoder)
        .unpack(&staging)
        .map_err(|e| format!("cannot unpack the engine archive: {e}"))?;
    std::fs::write(staging.join(COMPLETE), b"")
        .map_err(|e| format!("write {}: {e}", staging.display()))?;
    if dir.join(COMPLETE).is_file() {
        // Another build finished the same set while this one downloaded.
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(());
    }
    // What is there is an earlier attempt that never finished.
    let _ = std::fs::remove_dir_all(dir);
    if let Err(e) = std::fs::rename(&staging, dir) {
        // Another build finished the same set first; theirs is as good.
        let _ = std::fs::remove_dir_all(&staging);
        if !dir.join(COMPLETE).is_file() {
            return Err(format!("move the engine set into {}: {e}", dir.display()));
        }
    }
    Ok(())
}
