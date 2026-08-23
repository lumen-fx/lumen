//! Puts the candela standard library beside the binaries this build produces.
//!
//! candela reads `import "std/..."` off disk, and it looks for the modules in
//! `libs/` next to the running executable (or wherever `CANDELA_LIB_PATH`
//! points). The same tree backs the array methods, which the compiler pulls in
//! from `std/list` whether or not a program imports anything. The standalone
//! candela toolchain ships that tree next to its own binary; Lumen links the
//! same compiler into `lumenc` and the engine, so the tree has to travel with
//! Lumen's binaries instead, or every `std` import and every `arr.map(f)`
//! fails on a file that was never installed.
//!
//! The modules come out of the candela source cargo already resolved for this
//! build, so the library always matches the compiler linked beside it. The
//! text modules are copied; the three that bind C (`math`, `random`, `time`)
//! are built here into the shared libraries their `dylib` blocks name.
//!
//! Both copies land in the profile directory: one beside the executables cargo
//! links there, one in `deps/`, where cargo puts test binaries. A missing
//! library is not fatal anywhere, so a problem here reports on stderr and
//! leaves the build standing.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The standard library modules that bind a C source file, with the sources
/// each one is built from. The rest of `std` is candela text and is copied.
const NATIVE_MODULES: [(&str, &[&str]); 3] = [
    ("math", &["math.c"]),
    ("random", &["random.c", "pcg_basic.c"]),
    ("time", &["time.c"]),
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    // Without the compiler feature this crate runs precompiled images, which
    // carry their imports already resolved and read no library at all.
    if env::var_os("CARGO_FEATURE_COMPILER").is_none() {
        return;
    }

    let Some(source) = candela_libs() else {
        warn(
            "cannot find the candela source cargo resolved, so the candela standard library was not staged. `import \"std/...\"` and the array methods will not resolve.",
        );
        return;
    };
    println!("cargo::rerun-if-changed={}", source.display());

    for dest in destinations() {
        if let Err(message) = stage(&source, &dest) {
            warn(&format!(
                "{message}. The candela standard library is not at {}, so `import \"std/...\"` and the array methods will not resolve.",
                dest.display()
            ));
        }
    }
}

/// The `libs/` tree in the candela package this crate compiles against.
///
/// It is a dependency's own source directory, which cargo does not name in the
/// environment, so the resolved graph is what answers. Everything the query
/// needs is already on disk (the build is compiling out of it), so it stays
/// offline and locked.
fn candela_libs() -> Option<PathBuf> {
    let cargo = env::var_os("CARGO")?;
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")?;
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline", "--locked"])
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let manifest = metadata
        .get("packages")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("candela-lang"))?
        .get("manifest_path")?
        .as_str()?;
    let libs = Path::new(manifest).parent()?.join("libs");
    libs.is_dir().then_some(libs)
}

/// Where a copy of the library belongs: beside the binaries cargo links, and
/// beside the test binaries it puts one directory further down.
fn destinations() -> Vec<PathBuf> {
    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        return Vec::new();
    };
    // OUT_DIR is `<target>/<profile>/build/<package>-<hash>/out`.
    let Some(profile) = out_dir.ancestors().nth(3) else {
        return Vec::new();
    };
    vec![profile.join("libs"), profile.join("deps").join("libs")]
}

/// Copy the text modules and build the C-backed ones into `dest`.
fn stage(source: &Path, dest: &Path) -> Result<(), String> {
    copy_modules(&source.join("std"), &dest.join("std"))?;
    for (module, sources) in NATIVE_MODULES {
        build_native(source, dest, module, sources)?;
    }
    // The random module is built from a third-party generator that ships its
    // own licence; the notice travels with the binary made from it.
    copy_file(
        &source.join("std_src/random/LICENSE.txt"),
        &dest.join("std_src/random/LICENSE.txt"),
    )
}

/// Copy every `.cdl` module in `from` into `to`.
fn copy_modules(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("cdl")) {
            let Some(name) = path.file_name() else {
                continue;
            };
            copy_file(&path, &to.join(name))?;
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// Build one C-backed module into the shared library its `dylib` block names.
///
/// The path in that block is relative to the module, so the built library goes
/// where the source tree keeps it: `std_src/<module>/<module>.<ext>`.
fn build_native(source: &Path, dest: &Path, module: &str, sources: &[&str]) -> Result<(), String> {
    let src_dir = source.join("std_src").join(module);
    let out_dir = dest.join("std_src").join(module);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let library = out_dir.join(format!("{module}.{}", library_extension()));

    let tool = cc::Build::new()
        .try_get_compiler()
        .map_err(|e| format!("no C compiler to build the candela {module} module: {e}"))?;
    let mut command = tool.to_command();
    if tool.is_like_msvc() {
        // The flag spellings are `cc`'s own, which is what the compiler it
        // hands back is set up for.
        command.arg("-LD");
        command.arg(msvc_flag("-Fe", &library));
        // cl writes its object files into the working directory unless it is
        // told otherwise, and the trailing separator is what makes the
        // argument a directory rather than a file name.
        let mut objects = out_dir.clone().into_os_string();
        objects.push(std::path::MAIN_SEPARATOR_STR);
        command.arg(msvc_flag("-Fo", Path::new(&objects)));
    } else if target_os() == "macos" {
        command.args(["-dynamiclib", "-fPIC", "-o"]).arg(&library);
    } else {
        command.args(["-shared", "-fPIC", "-o"]).arg(&library);
    }
    for name in sources {
        command.arg(src_dir.join(name));
    }

    let status = command
        .status()
        .map_err(|e| format!("cannot run the C compiler for the candela {module} module: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the C compiler rejected the candela {module} module ({status})"
        ));
    }
    Ok(())
}

/// One `cl` flag that carries a path, joined the way the compiler wants it:
/// the value follows the flag with no separator in between.
fn msvc_flag(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path);
    arg
}

/// The operating system this build targets, which is not necessarily the one
/// the build script itself runs on.
fn target_os() -> String {
    env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

/// The shared-library extension candela appends to a `dylib` path that has
/// none, per the platform it is compiling for.
fn library_extension() -> &'static str {
    match target_os().as_str() {
        "windows" => "dll",
        "macos" | "ios" => "dylib",
        _ => "so",
    }
}

fn warn(message: &str) {
    println!("cargo::warning=lumen-script-candela: {message}");
}
