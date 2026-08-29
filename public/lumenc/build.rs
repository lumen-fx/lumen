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
//! directory it was installed into. The script standard library that build
//! staged goes there too, under `libs/`, because candela resolves
//! `import "std/..."` against that directory beside the running executable -
//! which is `lumenc` itself while you develop, and the app once it is
//! packaged. Afterwards `lumenc run` and `lumenc package` behave the same as
//! they do on a machine that installed from a release.
//!
//! The tag has to match this crate's version, because the engine and the
//! compiler are one tree. Whether that tag exists is the releases page's
//! answer, not an assumption: a version bumped ahead of its release has
//! nothing to fetch, and this says so instead of asking for it.
//!
//! It does nothing at all in a Lumen checkout, where the engine is already
//! being built beside the compiler, and nothing when `LUMEN_SKIP_ENGINE_BUILD`
//! is set, which is how a distribution packager who builds and places the
//! files themselves turns it off.

use std::path::{Path, PathBuf};

/// Directory the script standard library sits in, beside the binaries that
/// read it. Matches `SCRIPT_LIBRARY_DIR` in `src/package_cli.rs`, the release
/// archives, and the Windows installer.
const SCRIPT_LIBRARY_DIR: &str = "libs";

fn main() {
    println!("cargo:rerun-if-env-changed=LUMEN_SKIP_ENGINE_BUILD");
    println!("cargo:rerun-if-env-changed=CARGO_INSTALL_ROOT");
    println!("cargo:rerun-if-env-changed=LUMEN_GH_REPO");

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
        && manifest.contains("public/lumenc")
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
    // The engine and the compiler are one tree, so the tag has to be this
    // crate's own version rather than whichever is newest. The releases page
    // still decides whether that tag exists: a version that has been bumped
    // ahead of its release has nothing to download, and saying so beats a
    // request for a tag nobody pushed.
    published(&version)?;
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let source = fetch_source(&version, &out_dir)?;

    // The engine a packaged markup, C++, or Python app opens, and the stub
    // `lumenc package` turns into an app executable. A Rust app needs neither:
    // it links the engine through its own cargo build.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    run(
        &cargo,
        &["build", "--release", "-p", "lumen", "-p", "lumen-launcher"],
        &source,
        &[],
    )?;

    let built = source.join("target").join("release");
    std::fs::create_dir_all(bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    for name in [engine_library_name(), launcher_name()] {
        copy(&built.join(name), &bin_dir.join(name))?;
    }

    // The script standard library, staged into the profile directory by the
    // candela host's own build script. It travels whole: the C-backed modules
    // sit in subdirectories under the names their `dylib` blocks record.
    //
    // Reported on its own, because it costs something different from the two
    // files above: an install without it packages and runs, right up to the
    // first script that imports a module.
    if let Err(message) = copy_tree(
        &built.join(SCRIPT_LIBRARY_DIR),
        &bin_dir.join(SCRIPT_LIBRARY_DIR),
    ) {
        warn(&format!(
            "{message}, so the candela standard library is not beside lumenc. \
             `import \"std/...\"` and the array methods will not resolve until a \
             {SCRIPT_LIBRARY_DIR} tree is there or CANDELA_LIB_PATH names one."
        ));
    }
    Ok(())
}

/// Confirm the releases page carries a release for `version`.
///
/// `releases/latest` redirects to the newest published tag, so a version
/// higher than that one has not been released. A page that cannot be reached
/// is a different failure and reads as one; the download that follows is what
/// reports it.
fn published(version: &str) -> Result<(), String> {
    let Some(latest) = latest_release() else {
        return Ok(());
    };
    if semver(version) > semver(&latest) {
        return Err(format!(
            "v{version} has not been released yet, so there is no source archive to build \
             the engine from. The newest release is v{latest}"
        ));
    }
    Ok(())
}

/// The newest published version, or `None` when the releases page did not
/// answer with one.
fn latest_release() -> Option<String> {
    let url = format!("https://github.com/{}/releases/latest", repo());
    let response = ureq::head(&url)
        .config()
        .max_redirects(0)
        .build()
        .call()
        .ok()?;
    let location = response.headers().get("location")?.to_str().ok()?;
    let tag = location.trim_end_matches('/').rsplit('/').next()?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    // A repository with no releases redirects to the releases index, whose
    // last segment is a word rather than a version.
    version.split(['.', '-', '+']).next()?.parse::<u64>().ok()?;
    Some(version.to_string())
}

/// `X.Y.Z` as a comparable triple. Anything unparseable sorts lowest, which
/// keeps an odd tag from declaring a real version unreleased.
fn semver(version: &str) -> (u64, u64, u64) {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let mut parts = core.split('.').map(|p| p.parse().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// The repository releases come from.
fn repo() -> String {
    std::env::var("LUMEN_GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "lumen-fx/lumen".to_string())
}

/// Download and unpack the source for this exact version, returning the
/// directory it unpacked into. A published `lumenc` and the tag it was
/// published from are the same tree, so the engine beside it is the engine it
/// was compiled against.
fn fetch_source(version: &str, out_dir: &Path) -> Result<PathBuf, String> {
    let repo = repo();
    let unpacked = out_dir.join(format!("lumen-{version}"));
    if unpacked.join("Cargo.toml").is_file() {
        return Ok(unpacked);
    }

    let url = format!("https://github.com/{repo}/archive/refs/tags/v{version}.tar.gz");
    println!("cargo:warning=lumenc: fetching the engine source from {url}");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| format!("cannot download the v{version} source from {url}: {e}"))?;
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

/// Copy a directory whole, making what it needs on the way.
///
/// The source is read before the destination is made, so a build that staged
/// no tree reports that and leaves nothing behind rather than an empty
/// directory in the installation.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))?;
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", from.display()))?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &dest)?;
        } else {
            copy(&source, &dest)?;
        }
    }
    Ok(())
}

fn warn(message: &str) {
    println!("cargo:warning=lumenc: {message}");
}
