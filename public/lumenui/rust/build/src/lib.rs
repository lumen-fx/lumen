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
    // The whole of `src/` is watched, since a new page there is a new part
    // of the UI. That reruns this script on every edit to the app's Rust
    // too, so the compile itself only runs when what it reads changed.
    println!("cargo:rerun-if-changed={}", config.display());
    for watched in ["src", "locale"] {
        let path = dir.join(watched);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-env-changed={LUMENC}");

    let out = PathBuf::from(env("OUT_DIR")?)
        .join("lumen")
        .join("app.lmna");
    let stamp = out.with_extension("stamp");
    let lumenc = find_lumenc(version)?;
    let inputs = compile_inputs(version, &dir, &lumenc);
    if out.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s == inputs) {
        println!("cargo:rustc-env={UI_ENV}={}", out.display());
        return Ok(());
    }

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
    // Written last, so a compile that failed halfway is redone next time.
    let _ = std::fs::write(&stamp, &inputs);
    println!("cargo:rustc-env={UI_ENV}={}", out.display());
    Ok(())
}

/// What a compiled UI was made from, as a digest the stamp keeps: the
/// version, the UI sources ([`ui_fingerprint`]) and the `lumenc` that
/// compiles them ([`lumenc_identity`]). A `lumenc` of the same version built
/// from another commit may write another artifact format, which the engine
/// beside it reads and the old artifact's does not.
fn compile_inputs(version: &str, dir: &Path, lumenc: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!(
        "{version}\n{}\n{}",
        lumenc_identity(lumenc),
        ui_fingerprint(dir)
    ));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `lumenc` binary as found on disk: its path, size and modification
/// time, so a rebuilt or replaced compiler reads as another one. A bare name
/// is looked up on `PATH`, as running it does.
fn lumenc_identity(lumenc: &Path) -> String {
    let found = if lumenc.components().count() == 1 {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .flat_map(|d| [d.join(lumenc), d.join(lumenc).with_extension("exe")])
            .find(|p| p.is_file())
            .unwrap_or_else(|| lumenc.to_path_buf())
    } else {
        lumenc.to_path_buf()
    };
    let meta = std::fs::metadata(&found).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    format!(
        "{} {:?} {:?}",
        found.display(),
        meta.map(|m| m.len()),
        modified
    )
}

/// What the UI compile reads, as one string to compare: the path and text of
/// every UI source under `src/` and `locale/`, `lumen.toml` and `Cargo.toml`
/// (whose dependencies name the modules the app links), and the
/// `#[widget(...)]` lines of the Rust sources (the tags the app's widgets
/// add). Any other edit to the app's Rust leaves it unchanged.
fn ui_fingerprint(dir: &Path) -> String {
    const UI: &[&str] = &["lmn", "css", "cdl", "rhai", "lua", "ftl"];
    let mut files: Vec<PathBuf> = vec![dir.join("lumen.toml"), dir.join("Cargo.toml")];
    let mut pending = vec![dir.join("src"), dir.join("locale")];
    while let Some(at) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut print = String::new();
    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let body = if ext == "rs" {
            text.lines()
                .filter(|l| l.contains("#[widget("))
                .collect::<Vec<_>>()
                .join("\n")
        } else if UI.contains(&ext) || name == "lumen.toml" || name == "Cargo.toml" {
            text
        } else {
            continue;
        };
        print.push_str(&format!("{}\n{}\n{body}\n", path.display(), body.len()));
    }
    print
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A UI edit, a new page and a widget's tag change what the compile
    /// reads; an edit to the rest of the app's Rust does not.
    #[test]
    fn the_fingerprint_follows_what_the_compile_reads() {
        let dir = std::env::temp_dir().join(format!("lumenui-build-print-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src").join("parts")).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[app]\nentry = \"main.lmn\"\n").unwrap();
        std::fs::write(dir.join("src").join("main.lmn"), "<root/>").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        let first = ui_fingerprint(&dir);

        std::fs::write(dir.join("src").join("main.rs"), "fn main() { run(); }\n").unwrap();
        assert_eq!(ui_fingerprint(&dir), first, "a Rust edit");

        std::fs::write(
            dir.join("src").join("main.rs"),
            "#[widget(tag = \"gauge\")]\nfn main() { run(); }\n",
        )
        .unwrap();
        let tagged = ui_fingerprint(&dir);
        assert_ne!(tagged, first, "a widget's tag");

        std::fs::write(dir.join("src").join("parts").join("side.lmn"), "<label/>").unwrap();
        let paged = ui_fingerprint(&dir);
        assert_ne!(paged, tagged, "a new markup file");

        std::fs::write(dir.join("src").join("main.lmn"), "<root><label/></root>").unwrap();
        assert_ne!(ui_fingerprint(&dir), paged, "a markup edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A compiled UI is reused only with the compiler that made it: a
    /// `lumenc` of the same version rebuilt in place, which may write another
    /// artifact format, compiles the UI again.
    #[test]
    fn a_rebuilt_lumenc_compiles_the_ui_again() {
        let dir = std::env::temp_dir().join(format!("lumenui-build-lumenc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[app]\nentry = \"main.lmn\"\n").unwrap();
        std::fs::write(dir.join("src").join("main.lmn"), "<root/>").unwrap();
        let lumenc = dir.join("lumenc");
        std::fs::write(&lumenc, "format 24").unwrap();
        let first = compile_inputs("0.0.12", &dir, &lumenc);
        assert_eq!(
            compile_inputs("0.0.12", &dir, &lumenc),
            first,
            "nothing moved"
        );

        std::fs::write(&lumenc, "format 25, rebuilt").unwrap();
        assert_ne!(
            compile_inputs("0.0.12", &dir, &lumenc),
            first,
            "the same version, another build"
        );
        let other = dir.join("other-lumenc");
        std::fs::copy(&lumenc, &other).unwrap();
        assert_ne!(
            compile_inputs("0.0.12", &dir, &other),
            compile_inputs("0.0.12", &dir, &lumenc),
            "another compiler"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
