//! Builds the engine and the launcher stub when `lumenc` is installed from
//! source.
//!
//! `cargo install lumenc` gives you a compiler and nothing else, and a
//! compiler on its own cannot package an app: `lumenc package` assembles a
//! folder out of the launcher stub and the shared Lumen library, and those are
//! release files, not cargo artifacts. On a platform the release channel
//! covers that is fine, because the installer puts all three in place. On one
//! it does not cover - Windows on ARM has no hosted runner to build it - a
//! source install is the only way to get Lumen at all, and it has to produce
//! the same three files.
//!
//! So this script fetches the matching tagged source, builds `liblumen` and
//! `lumen-launcher` from it, and puts them where `lumenc` looks first: the
//! directory it was installed into. Afterwards `lumenc package` behaves the
//! same as it does on a machine that installed from a release.
//!
//! It does nothing at all in a Lumen checkout, where the engine is already
//! being built beside the compiler, and nothing when `LUMEN_SKIP_ENGINE_BUILD`
//! is set, which is how a distribution packager who builds and places the
//! files themselves turns it off.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LUMEN_SKIP_ENGINE_BUILD");
    println!("cargo:rerun-if-env-changed=CARGO_INSTALL_ROOT");

    if std::env::var_os("LUMEN_SKIP_ENGINE_BUILD").is_some() {
        return;
    }
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    if in_lumen_workspace(&manifest_dir) {
        return;
    }
    let Some(bin_dir) = install_bin_dir() else {
        warn(
            "cannot tell where lumenc is being installed, so the engine was not built. Set CARGO_INSTALL_ROOT, or build liblumen and lumen-launcher yourself and point LUMEN_LIB_DIR at them.",
        );
        return;
    };

    // A failure here leaves a working compiler, so it is reported and not
    // fatal: `lumenc run`, `build`, and `check` need none of this, and a
    // packaging attempt says exactly what is missing and where to put it.
    if let Err(message) = build_engine(&bin_dir) {
        warn(&format!(
            "{message}. lumenc itself is installed and works; `lumenc package` needs \
             liblumen and lumen-launcher, so build them from a checkout and point \
             LUMEN_LIB_DIR at them."
        ));
    }
}

/// Whether this manifest is the one in a Lumen checkout rather than an
/// unpacked copy of the published crate. The engine sits two directories up,
/// as the workspace root that lists this crate as a member.
fn in_lumen_workspace(manifest_dir: &Path) -> bool {
    let Some(root) = manifest_dir.parent().and_then(Path::parent) else {
        return false;
    };
    let Ok(manifest) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return false;
    };
    manifest.contains("[workspace]")
        && manifest.contains("crates/lumenc")
        && root.join("src/lib.rs").is_file()
}

/// Where the installed `lumenc` will land. `cargo install --root` is not
/// visible to a build script, but the environment variable that does the same
/// thing is, and `CARGO_HOME` covers the default.
fn install_bin_dir() -> Option<PathBuf> {
    let root = std::env::var_os("CARGO_INSTALL_ROOT")
        .or_else(|| std::env::var_os("CARGO_HOME"))
        .map(PathBuf::from)?;
    Some(root.join("bin"))
}

/// Fetch the tagged source, build the two files, and copy them into `bin_dir`.
fn build_engine(bin_dir: &Path) -> Result<(), String> {
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo always sets CARGO_PKG_VERSION");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let source = fetch_source(&version, &out_dir)?;

    // The engine is built to be linked, not just opened: a Rust app takes the
    // shared library as a Rust dependency, which means both sides have to
    // agree about the standard library. `-C prefer-dynamic` is what makes them
    // share one copy of it, and the rpath is what lets the copy sit beside the
    // library rather than in a system directory.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    run(
        &cargo,
        &["build", "--release", "-p", "lumen"],
        &source,
        &[("RUSTFLAGS", &dynamic_rustflags())],
    )?;
    run(
        &cargo,
        &["build", "--release", "-p", "lumen-launcher"],
        &source,
        &[],
    )?;

    let built = source.join("target").join("release");
    std::fs::create_dir_all(bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    for name in [engine_library_name(), launcher_name()] {
        copy(&built.join(name), &bin_dir.join(name))?;
    }
    if let Some(std_lib) = shared_std_library() {
        let name = std_lib.file_name().unwrap_or_default();
        copy(&std_lib, &bin_dir.join(name))?;
    }
    Ok(())
}

/// Download and unpack the source for this exact version, returning the
/// directory it unpacked into. A published `lumenc` and the tag it was
/// published from are the same tree, so the engine beside it is the engine it
/// was compiled against.
fn fetch_source(version: &str, out_dir: &Path) -> Result<PathBuf, String> {
    let repo = std::env::var("LUMEN_GH_REPO").unwrap_or_else(|_| "lumen-fx/lumen".to_string());
    let unpacked = out_dir.join(format!("lumen-{version}"));
    if unpacked.join("Cargo.toml").is_file() {
        return Ok(unpacked);
    }

    let url = format!("https://github.com/{repo}/archive/refs/tags/v{version}.tar.gz");
    println!("cargo:warning=lumenc: fetching the engine source from {url}");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.body_mut().as_reader(), &mut bytes)
        .map_err(|e| format!("cannot read {url}: {e}"))?;

    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(out_dir)
        .map_err(|e| format!("cannot unpack the engine source: {e}"))?;
    if !unpacked.join("Cargo.toml").is_file() {
        return Err(format!(
            "the source archive for v{version} does not hold the tree that was expected"
        ));
    }
    Ok(unpacked)
}

/// The flags a shared engine is built with. Kept in step with
/// `lumen_runtime::app_kind::rust_dynamic_env`, which applies the same two to
/// an app; a library and an app that disagree here do not link.
fn dynamic_rustflags() -> String {
    let mut flags = String::from("-C prefer-dynamic");
    if cfg!(target_os = "macos") {
        flags.push_str(" -C link-arg=-Wl,-rpath,@loader_path");
    } else if !cfg!(target_os = "windows") {
        flags.push_str(" -C link-arg=-Wl,-rpath,$ORIGIN");
    }
    flags
}

/// The shared Rust standard library the engine was just linked against, which
/// has to travel with it.
fn shared_std_library() -> Option<PathBuf> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(rustc)
        .arg("--print")
        .arg("target-libdir")
        .output()
        .ok()?;
    let dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    let (prefix, ext) = if cfg!(target_os = "windows") {
        ("std-", "dll")
    } else if cfg!(target_os = "macos") {
        ("libstd-", "dylib")
    } else {
        ("libstd-", "so")
    };
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        (name.starts_with(prefix) && name.ends_with(ext)).then(|| entry.path())
    })
}

fn engine_library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "lumen.dll"
    } else if cfg!(target_os = "macos") {
        "liblumen.dylib"
    } else {
        "liblumen.so"
    }
}

fn launcher_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "lumen-launcher.exe"
    } else {
        "lumen-launcher"
    }
}

fn run(program: &str, args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(cwd);
    for (key, value) in env {
        command.env(key, value);
    }
    let status = command
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` failed", args.join(" ")))
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))
}

fn warn(message: &str) {
    println!("cargo:warning=lumenc: {message}");
}
