//! The build-script half of the Lumen Rust SDK.
//!
//! A Rust app on Lumen is described by its `lumen.toml`, like any Lumen app,
//! and its UI (the markup, stylesheets and scripts under `src/`) is compiled
//! when the crate builds. Call [`compile`] from the app's `build.rs`:
//!
//! ```rust,ignore
//! // build.rs
//! fn main() {
//!     lumenui_build::compile();
//! }
//! ```
//!
//! and start the app from what it compiled with `lumenui::app!()`. A markup or
//! style error then fails `cargo build`, with the compiler's message.
//!
//! The compiler is the `lumenc` of this crate's own Lumen release, because an
//! artifact is read only by the engine of the release that wrote it. The one
//! used is, in order: the program `LUMEN_LUMENC` names (`lumenc run`,
//! `build` and `package` set it to themselves), a `lumenc` on `PATH` of that
//! version, or the release's toolchain archive for this machine, downloaded
//! once, checked against the release's `sha256sums.txt`, and kept in the
//! user cache directory.

mod engine;
mod fetch;

use std::path::{Path, PathBuf};
use std::process::Command;

use fetch::{COMPLETE, Packing};

pub use engine::link_engine;

/// The variable naming the `lumenc` to compile with.
const LUMENC: &str = "LUMEN_LUMENC";

/// The variable the app's `lumenui::ui!()` reads the artifact's path from.
const UI_ENV: &str = "LUMENUI_UI";

/// Compile the calling crate's UI into the artifact its executable carries.
///
/// Reads `lumen.toml` beside the crate's `Cargo.toml`, compiles what it names
/// into `$OUT_DIR/lumen/app.lmna`, and tells cargo to run again when
/// `lumen.toml`, anything under `src/`, or `locale/` changes. Exits the build
/// with the compiler's message when the UI does not compile, and with a
/// message saying what to do when no compiler can be found.
pub fn compile() {
    if let Err(message) = try_compile() {
        fail(&message);
    }
}

fn try_compile() -> Result<(), String> {
    let version = env!("CARGO_PKG_VERSION");
    let dir = PathBuf::from(env("CARGO_MANIFEST_DIR")?);
    let config = dir.join("lumen.toml");
    if !config.is_file() {
        return Err(format!(
            "lumenui_build::compile() compiles the UI `lumen.toml` describes, and there is \
             none beside {}. Add one naming the entry markup under src/:\n\n    [app]\n    \
             entry = \"main.lmn\"\n",
            dir.join("Cargo.toml").display()
        ));
    }
    println!("cargo:rerun-if-changed={}", config.display());
    for watched in ["src", "locale"] {
        let path = dir.join(watched);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-env-changed={LUMENC}");

    let lumenc = find_lumenc(version)?;
    let out = PathBuf::from(env("OUT_DIR")?)
        .join("lumen")
        .join("app.lmna");
    let output = Command::new(&lumenc)
        .arg("build")
        .arg("--ui")
        .arg(&dir)
        .arg(&out)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", lumenc.display()))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "the app's UI does not compile:\n{}",
            stderr.trim_end()
        ));
    }
    for line in stderr.lines().filter(|l| l.contains("warning")) {
        println!("cargo:warning={line}");
    }
    println!("cargo:rustc-env={UI_ENV}={}", out.display());
    Ok(())
}

/// The `lumenc` to compile with: see the crate docs for the order.
fn find_lumenc(version: &str) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(LUMENC).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(path);
        return match lumenc_version(&path) {
            Some(found) if found == version => Ok(path),
            Some(found) => Err(format!(
                "{LUMENC} names {}, which is lumenc {found}; this app builds with Lumen \
                 {version}, whose engine reads only what lumenc {version} compiles. Point \
                 {LUMENC} at lumenc {version}, or unset it to use the release's",
                path.display()
            )),
            None => Err(format!(
                "{LUMENC} names {}, which does not run as lumenc",
                path.display()
            )),
        };
    }
    if lumenc_version(Path::new("lumenc")).as_deref() == Some(version) {
        return Ok(PathBuf::from("lumenc"));
    }
    download_lumenc(version)
}

/// What `lumenc --version` says its version is.
fn lumenc_version(lumenc: &Path) -> Option<String> {
    let out = Command::new(lumenc).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .nth(1)
        .map(str::to_string)
        .filter(|_| text.starts_with("lumenc "))
}

/// The release's toolchain for this machine, from the cache or downloaded.
fn download_lumenc(version: &str) -> Result<PathBuf, String> {
    let host = env("HOST")?;
    let name = fetch::target_name(&host).ok_or_else(|| {
        format!(
            "Lumen releases ship no lumenc for {host}. Install one built from source \
             (`cargo install lumenc --version {version}`) and set {LUMENC} to it"
        )
    })?;
    let dir = fetch::cache_dir("toolchain", version, name).ok_or_else(|| {
        format!(
            "cannot tell where the user cache directory is (HOME / LOCALAPPDATA unset); set \
             {LUMENC} to a lumenc {version} instead"
        )
    })?;
    let exe = dir.join("bin").join(if host.contains("windows") {
        "lumenc.exe"
    } else {
        "lumenc"
    });
    if !dir.join(COMPLETE).is_file() {
        let base = fetch::release_base(version);
        let sums = fetch::checksums(
            &base,
            &format!(
                "The app's UI compiles with lumenc {version}; if the Lumen v{version} release \
                 is not published, or this machine is offline, set {LUMENC} to a lumenc \
                 {version}"
            ),
        )?;
        let (asset, packing) = if host.contains("windows") {
            (format!("lumen-{name}.zip"), Packing::Zip)
        } else {
            (format!("lumen-{name}.tar.gz"), Packing::TarGz)
        };
        println!(
            "cargo:warning=lumenui-build: downloading lumenc {version} ({asset}) into {}",
            dir.display()
        );
        fetch::fetch_archive(&base, &asset, &sums, packing, &dir)?;
    }
    if !exe.is_file() {
        return Err(format!(
            "the Lumen v{version} toolchain unpacked into {} has no {}",
            dir.display(),
            exe.display()
        ));
    }
    Ok(exe)
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("cargo did not set {name} for the build script"))
}

/// End the build script with `message`.
fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(1);
}
