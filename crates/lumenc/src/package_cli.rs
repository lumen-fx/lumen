//! `lumenc package <app_dir> [<out_dir>]` - assemble a shippable app folder.
//!
//! The output is a directory an end user can copy anywhere and double-click:
//! the app executable, the shared Lumen runtime library where the app needs
//! one, `lumen.toml`, and the app's own files at the same relative paths the
//! markup names them by. Nothing on the target machine needs a Lumen
//! installation or a toolchain.
//!
//! This is the same job `windeployqt` / `macdeployqt` do for Qt: put the
//! executable, the libraries it opens, and its data in one directory.
//!
//! An app authored against one of the SDKs is a program in its own language,
//! so its own toolchain builds it (the same one `lumenc build` hands off to)
//! and the folder is assembled around the executable that produced. What
//! travels with it follows how the language reaches Lumen: a Rust app links
//! the runtime in, a C++ app calls the C ABI and needs the shared library
//! beside it, and both read their markup and scripts at run time rather than
//! compiling them in.
//!
//! For a markup app the executable is a copy of the prebuilt `lumen-launcher`
//! stub, never a freshly compiled binary, so packaging needs no Rust
//! toolchain. How the artifact gets into it depends on the target:
//!
//! - Windows and Linux: appended to the file, with a footer recording where
//!   it starts. Both program loaders ignore trailing bytes.
//! - macOS, packaged on macOS: linked in as a Mach-O section by a small C
//!   wrapper compiled on the spot, because a code signature has to cover the
//!   whole file and `__LINKEDIT` has to stay last. This needs `cc`.
//! - macOS, packaged anywhere else: shipped as a `.lmna` file beside the
//!   executable, since embedding needs a Mach-O linker.
//!
//! `--target` packages for a platform other than the one you are on. The
//! toolchain files for another platform come from the release channel and are
//! cached per version. Finding a prebuilt file that ships with the toolchain
//! lives here too, so a web build and a package look in the same places for
//! the same reasons.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lumen_ir::layout_ir::relativize_asset_paths;

use crate::app_kind::AppKind;

/// Conventional extension for a compiled-app artifact, matching
/// [`crate::build_cli::ARTIFACT_EXT`].
const ARTIFACT_EXT: &str = "lmna";

/// Trailing marker on an executable that carries its app inside it. Read back
/// by `lumen-launcher`; the two constants must stay in step.
const FOOTER_MAGIC: &[u8; 8] = b"LMNAPACK";

/// Name of the launcher stub as the release channel and a workspace build
/// both produce it.
const STUB_STEM: &str = "lumen-launcher";

/// The prebuilt wasm runtime a web build serves, and the module that
/// instantiates it, under the names the release channel and a workspace build
/// both produce.
const WEB_WASM: &str = "lumen-web.wasm";
const WEB_JS: &str = "lumen-web.js";

/// Cache key for the web runtime, in the slot a target name takes: it is the
/// same pair on every platform, so it is filed under the component rather
/// than under any one of them.
const WEB_COMPONENT: &str = "web";

/// Release asset holding the web runtime, named like the per-target archives.
const WEB_ARCHIVE: &str = "lumen-web.tar.gz";

/// The C wrapper used for a macOS package built on macOS. It is compiled with
/// `-sectcreate __LUMEN __lmna <artifact>`, which puts the artifact in a
/// section of the executable and leaves the layout a code signature needs
/// intact. It declares the four C-ABI entry points itself rather than
/// including a header, so packaging needs no Lumen headers on disk.
const MACOS_WRAPPER_C: &str = r#"
#include <dlfcn.h>
#include <libgen.h>
#include <mach-o/dyld.h>
#include <mach-o/getsect.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const struct mach_header_64 _mh_execute_header;

typedef uint32_t (*abi_version_fn)(void);
typedef void *(*new_from_lmna_fn)(const uint8_t *, size_t, const char *);
typedef uint32_t (*run_fn)(void *);
typedef uint32_t (*run_headless_fn)(void *, uint32_t);
typedef const char *(*last_error_fn)(void);

/* Must match lumen::LUMEN_ABI_{MAJOR,MINOR}. */
#define WANT_ABI_MAJOR 0u
#define WANT_ABI_MINOR 7u

int main(int argc, char **argv) {
    int headless = 0;
    uint32_t ticks = 1;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--headless") == 0) {
            headless = 1;
        } else if (strcmp(argv[i], "--ticks") == 0 && i + 1 < argc) {
            ticks = (uint32_t)strtoul(argv[++i], NULL, 10);
        }
    }

    unsigned long size = 0;
    const uint8_t *data =
        getsectiondata(&_mh_execute_header, "__LUMEN", "__lmna", &size);
    if (data == NULL || size == 0) {
        fprintf(stderr, "this executable carries no app\n");
        return 1;
    }

    char exe[4096];
    uint32_t exe_len = (uint32_t)sizeof(exe);
    if (_NSGetExecutablePath(exe, &exe_len) != 0) {
        fprintf(stderr, "cannot locate this executable\n");
        return 1;
    }
    char dir_buf[4096];
    snprintf(dir_buf, sizeof(dir_buf), "%s", exe);
    char *dir = dirname(dir_buf);

    char lib_path[4200];
    snprintf(lib_path, sizeof(lib_path), "%s/liblumen.dylib", dir);
    void *lib = dlopen(lib_path, RTLD_NOW);
    if (lib == NULL) {
        lib = dlopen("liblumen.dylib", RTLD_NOW);
    }
    if (lib == NULL) {
        fprintf(stderr, "cannot open %s: %s\n", lib_path, dlerror());
        return 1;
    }

    abi_version_fn abi = (abi_version_fn)dlsym(lib, "lumen_abi_version");
    new_from_lmna_fn new_app =
        (new_from_lmna_fn)dlsym(lib, "lumen_app_new_from_lmna");
    run_fn run = (run_fn)dlsym(lib, "lumen_app_run");
    run_headless_fn run_headless =
        (run_headless_fn)dlsym(lib, "lumen_app_run_headless");
    last_error_fn last_error = (last_error_fn)dlsym(lib, "lumen_last_error");
    if (abi == NULL || new_app == NULL || run == NULL || run_headless == NULL) {
        fprintf(stderr, "%s is missing a required entry point\n", lib_path);
        return 1;
    }

    uint32_t packed = abi();
    uint32_t got_major = packed >> 16;
    uint32_t got_minor = (packed >> 8) & 0xFFu;
    if (got_major != WANT_ABI_MAJOR || got_minor < WANT_ABI_MINOR) {
        fprintf(stderr,
                "liblumen ABI mismatch: this app needs %u.%u.x, the library "
                "reports %u.%u.x\n",
                WANT_ABI_MAJOR, WANT_ABI_MINOR, got_major, got_minor);
        return 1;
    }

    void *app = new_app(data, (size_t)size, dir);
    if (app == NULL) {
        const char *msg = last_error ? last_error() : NULL;
        fprintf(stderr, "cannot start the app: %s\n", msg ? msg : "(no detail)");
        return 1;
    }

    uint32_t status = headless ? run_headless(app, ticks) : run(app);
    if (status != 0) {
        const char *msg = last_error ? last_error() : NULL;
        fprintf(stderr, "the app failed (status %u): %s\n", status,
                msg ? msg : "(no detail)");
        return 1;
    }
    return 0;
}
"#;

/// A platform to package for, named the way the release assets are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Release asset name, for example `linux-x86_64`.
    name: &'static str,
    /// Operating system family: `linux`, `macos`, or `windows`.
    os: Os,
}

/// The operating system a target runs on. Everything platform-specific about
/// packaging follows from this, not from the architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Linux,
    Macos,
    Windows,
}

impl Target {
    /// Every target the release channel publishes toolchain files for.
    const ALL: [Target; 5] = [
        Target {
            name: "linux-x86_64",
            os: Os::Linux,
        },
        Target {
            name: "linux-aarch64",
            os: Os::Linux,
        },
        Target {
            name: "macos-x86_64",
            os: Os::Macos,
        },
        Target {
            name: "macos-aarch64",
            os: Os::Macos,
        },
        Target {
            name: "windows-x86_64",
            os: Os::Windows,
        },
    ];

    /// The target name for a release-asset name, or `None` for a name no
    /// release covers.
    fn parse(name: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.name == name)
    }

    /// The platform this `lumenc` is running on.
    fn host() -> Target {
        let os = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        Target::parse(&format!("{os}-{arch}")).unwrap_or(Target {
            name: "linux-x86_64",
            os: Os::Linux,
        })
    }

    /// File name of the launcher stub for this target.
    fn stub_name(self) -> String {
        match self.os {
            Os::Windows => format!("{STUB_STEM}.exe"),
            _ => STUB_STEM.to_string(),
        }
    }

    /// File name of the shared Lumen runtime library for this target.
    fn lib_name(self) -> &'static str {
        match self.os {
            Os::Windows => "lumen.dll",
            Os::Macos => "liblumen.dylib",
            Os::Linux => "liblumen.so",
        }
    }

    /// File name the packaged app executable gets.
    fn exe_name(self, app: &str) -> String {
        match self.os {
            Os::Windows => format!("{app}.exe"),
            _ => app.to_string(),
        }
    }

    /// File name of the engine a Rust app links, as its own build produces it.
    /// Distinct from [`Self::lib_name`], which is the C library an app opens:
    /// the two are different crate targets and cannot share a file name.
    fn linked_engine_name(self) -> &'static str {
        match self.os {
            Os::Macos => "liblumen_engine.dylib",
            // Windows never reaches here - see `copy_linked_engine` - but the
            // name is the one cargo would write.
            Os::Windows => "lumen_engine.dll",
            Os::Linux => "liblumen_engine.so",
        }
    }

    /// The Rust target triple for this platform, for an SDK app whose own
    /// toolchain does the cross-compiling.
    fn rust_triple(self) -> &'static str {
        match self.name {
            "linux-x86_64" => "x86_64-unknown-linux-gnu",
            "linux-aarch64" => "aarch64-unknown-linux-gnu",
            "macos-x86_64" => "x86_64-apple-darwin",
            "macos-aarch64" => "aarch64-apple-darwin",
            _ => "x86_64-pc-windows-msvc",
        }
    }

    /// Release asset holding this target's toolchain files.
    fn archive_name(self) -> String {
        match self.os {
            Os::Windows => format!("lumen-{}.zip", self.name),
            _ => format!("lumen-{}.tar.gz", self.name),
        }
    }
}

/// Entry: `lumenc package <app_dir> [<out_dir>] [--name <n>] [--target <t>]
/// [--lib-dir <dir>] [--no-hooks]`.
pub fn cmd_package(args: impl Iterator<Item = String>) -> ExitCode {
    const PACKAGE_USAGE: &str = "lumenc package - assemble a folder to ship

USAGE:
    lumenc package <app_dir> [<out_dir>] [--name N] [--target T]
                   [--lib-dir <dir>] [--zip] [--no-hooks]

Assembles the app executable, the Lumen runtime library, and the app's
files into a folder that runs on a machine with no Lumen installation. A
markup app is compiled into the executable, pages and all; an SDK app is
built by its own toolchain and the folder assembled around what that
produced. <out_dir> defaults to <app_dir>/dist/<name>.

    --name N          Package name (default: the app directory's name).
    --target T        Package for another platform (linux-x86_64 |
                      linux-aarch64 | macos-x86_64 | macos-aarch64 |
                      windows-x86_64), fetching that platform's files
                      from the release channel. An SDK app's own
                      toolchain does the cross-compiling.
    --lib-dir DIR     Take the platform's files from DIR instead of the
                      release channel.
    --zip             Also write <out_dir>.zip, the whole folder in one
                      file to hand to someone.
    --no-hooks        Skip the app's prebuild [[hooks]].";
    let mut no_hooks = false;
    let mut want_zip = false;
    let mut name: Option<String> = None;
    let mut lib_dir: Option<PathBuf> = None;
    let mut target = Target::host();
    let mut positional: Vec<String> = Vec::new();
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{PACKAGE_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--no-hooks" => no_hooks = true,
            "--zip" => want_zip = true,
            "--name" => match args.next() {
                Some(v) => name = Some(v),
                None => return usage_error("--name needs a value"),
            },
            "--lib-dir" => match args.next() {
                Some(v) => lib_dir = Some(PathBuf::from(v)),
                None => return usage_error("--lib-dir needs a directory"),
            },
            "--target" => match args.next() {
                Some(v) => match Target::parse(&v) {
                    Some(t) => target = t,
                    None => return unknown_target(&v),
                },
                None => return usage_error("--target needs a platform name"),
            },
            other => positional.push(other.to_string()),
        }
    }

    let mut positional = positional.into_iter();
    let Some(src) = positional.next() else {
        return usage_error("missing <app_dir>");
    };
    let out_arg = positional.next();
    if let Some(unexpected) = positional.next() {
        return usage_error(&format!("unexpected extra argument '{unexpected}'"));
    }

    // Every asset path baked into the artifact is joined onto this, and the
    // packaged copy needs those paths relative to the app rather than to
    // wherever `lumenc` was run from.
    let src_path = match std::fs::canonicalize(&src) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => return usage_error(&format!("'{src}' is not a directory")),
        Err(e) => return usage_error(&format!("'{src}': {e}")),
    };

    let cfg = crate::LumenToml::load_or_default(&src_path).unwrap_or_default();
    let kind = crate::app_kind::resolve(&src_path, cfg.app.kind);

    // Freezing a Python app runs the interpreter's own machinery against the
    // interpreter it is running under, which is this machine's. There is no
    // flag that makes it emit another platform's executable.
    if kind == AppKind::Python && target != Target::host() {
        eprintln!(
            "lumenc package: a Python app is frozen against the interpreter doing the \
             freezing, so it can only be packaged for the platform you are on. Package \
             it on a {} machine.",
            target.name
        );
        return ExitCode::from(2);
    }

    let app_name = name.unwrap_or_else(|| {
        src_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".to_string())
    });
    let out_dir = out_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| src_path.join("dist").join(&app_name));
    // Writing the package over its own source would copy files onto
    // themselves. A subdirectory of the app is fine, and is the default.
    if std::fs::canonicalize(&out_dir).is_ok_and(|p| p == src_path) {
        return usage_error("the output directory cannot be the app directory itself");
    }

    if !no_hooks
        && let Err(e) = lumen_runtime::hooks::run_hooks(
            &cfg.hooks,
            lumen_runtime::hooks::HookWhen::Prebuild,
            &src_path,
        )
    {
        eprintln!("lumenc package: {e}");
        return ExitCode::FAILURE;
    }

    let assembled = match kind {
        AppKind::Markup => package(&src_path, &out_dir, &app_name, target, lib_dir.as_deref()),
        _ => package_sdk(
            &src_path,
            &out_dir,
            &app_name,
            target,
            lib_dir.as_deref(),
            kind,
        ),
    };
    let summary = match assembled {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("lumenc package: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("lumenc package: {summary}");

    if want_zip {
        match zip_package(&out_dir) {
            Ok(archive) => println!("lumenc package: wrote {}", archive.display()),
            Err(e) => {
                eprintln!("lumenc package: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// The language a kind is written in, for messages.
fn language_of(kind: AppKind) -> &'static str {
    match kind {
        AppKind::Rust => "Rust",
        AppKind::Cpp => "C++",
        AppKind::Python => "Python",
        AppKind::Markup => "markup",
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("lumenc package: {msg}");
    ExitCode::from(2)
}

fn unknown_target(name: &str) -> ExitCode {
    let known: Vec<&str> = Target::ALL.iter().map(|t| t.name).collect();
    eprintln!(
        "lumenc package: no target called '{name}'. Pick one of: {}",
        known.join(", ")
    );
    ExitCode::from(2)
}

/// Build an SDK app with its own toolchain, then assemble the same folder
/// shape around what that build produced. Returns the summary to print.
///
/// How each kind reaches the engine decides what travels beside its
/// executable. A C++ or Python app opens the shared C library, so that library
/// comes from the toolchain and is copied in. A Rust app links the engine
/// instead, and its own build already produced the library it linked, so that
/// one travels along with the standard library both were compiled against.
/// Windows is the exception: no linkable engine exists there, so a Rust app
/// carries the runtime inside itself and needs nothing beside it.
///
/// All three read their markup, stylesheet, and scripts at run time, so unlike
/// a markup app those files travel.
fn package_sdk(
    src: &Path,
    out: &Path,
    app_name: &str,
    target: Target,
    lib_dir: Option<&Path>,
    kind: AppKind,
) -> Result<String, String> {
    let built = match kind {
        AppKind::Rust => build_rust_app(src, target)?,
        AppKind::Cpp => build_cpp_app(src, target)?,
        AppKind::Python => build_python_app(src, app_name, out)?,
        AppKind::Markup => return Err("markup apps are not packaged here".to_string()),
    };

    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let exe_path = out.join(target.exe_name(app_name));
    copy_executable(&built, &exe_path)?;

    let carried = if kind == AppKind::Rust {
        copy_linked_engine(&built, out, target)?
    } else {
        let toolchain = locate_toolchain(target, lib_dir)?;
        copy_c_engine(out, target, &toolchain)?;
        1
    };

    // The freezer's own scratch directories sit under the output so they never
    // touch the app; the package itself has no use for them.
    if kind == AppKind::Python {
        let _ = std::fs::remove_dir_all(out.join(".build"));
    }

    let copied = copy_app_files(src, out, CopyRules::sdk(kind))?;
    Ok(format!(
        "wrote {} from the {} build ({} app file{} beside it, {})",
        exe_path.display(),
        language_of(kind),
        copied,
        if copied == 1 { "" } else { "s" },
        match carried {
            0 => "runtime linked in".to_string(),
            n => format!("with {n} shared librar{}", if n == 1 { "y" } else { "ies" }),
        },
    ))
}

/// Copy the shared engine a Rust app just linked, and the standard library
/// they share, out of the app's own build. Returns how many files travelled.
///
/// Neither comes from an installed toolchain: cargo built the engine as part
/// of building the app, so the copy that belongs in the package is that one,
/// produced by the same compiler as the executable beside it. On Windows the
/// engine is inside the executable and there is nothing to copy.
fn copy_linked_engine(built: &Path, out: &Path, target: Target) -> Result<usize, String> {
    if target.os == Os::Windows {
        return Ok(0);
    }
    let engine = target.linked_engine_name();
    let from = linked_engine_beside(built, engine).ok_or_else(|| {
        format!(
            "the build produced no {engine} beside {}. A Rust app links the engine, so \
             the library has to come out of the same build as the executable.",
            built.display()
        )
    })?;
    std::fs::copy(&from, out.join(engine))
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), out.display()))?;

    let mut carried = 1;
    if let Some(std_lib) = local_shared_std(target)? {
        let dest = out.join(std_lib.file_name().unwrap_or_default());
        std::fs::copy(&std_lib, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", std_lib.display(), dest.display()))?;
        carried += 1;
    }
    Ok(carried)
}

/// Where cargo left the engine library relative to the executable it built:
/// beside it for an ordinary binary, one level up for an example, which cargo
/// writes into a subdirectory of the same profile.
fn linked_engine_beside(built: &Path, engine: &str) -> Option<PathBuf> {
    [built.parent(), built.parent().and_then(Path::parent)]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(engine))
        .find(|p| p.is_file())
}

/// Copy the shared C library an app opens at run time, from the toolchain.
fn copy_c_engine(out: &Path, target: Target, toolchain: &Toolchain) -> Result<(), String> {
    let lib_dest = out.join(target.lib_name());
    std::fs::copy(&toolchain.lib, &lib_dest)
        .map(|_| ())
        .map_err(|e| {
            format!(
                "copy {} -> {}: {e}",
                toolchain.lib.display(),
                lib_dest.display()
            )
        })
}

/// The shared standard library this machine's Rust compiler holds for
/// `target`, asking `rustc` where its own target libraries live rather than
/// guessing at a sysroot layout.
fn local_shared_std(target: Target) -> Result<Option<PathBuf>, String> {
    let (prefix, ext) = match target.os {
        Os::Windows => ("std-", "dll"),
        Os::Macos => ("libstd-", "dylib"),
        Os::Linux => ("libstd-", "so"),
    };
    let mut command = std::process::Command::new("rustc");
    command.arg("--print").arg("target-libdir");
    if target != Target::host() {
        command.arg("--target").arg(target.rust_triple());
    }
    let output = command.output().map_err(|e| {
        format!("cannot run rustc: {e}. Packaging asks it where its shared standard library is.")
    })?;
    if !output.status.success() {
        return Err(format!(
            "rustc does not have the {} standard library installed. Add it with \
             `rustup target add {}`.",
            target.name,
            target.rust_triple()
        ));
    }
    let dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    Ok(find_shared_std(&dir, prefix, ext))
}

/// The shared standard library in `dir`, if it holds one.
fn find_shared_std(dir: &Path, prefix: &str, ext: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        (name.starts_with(prefix) && name.ends_with(ext)).then(|| entry.path())
    })
}

/// Build a Rust SDK app and return the executable cargo produced.
///
/// The build command is the one `lumenc build` uses, in the environment that
/// makes the app link the shared engine library rather than compile the engine
/// into itself. The extra flag makes cargo report where the executable landed,
/// which cannot be worked out from the app directory alone because a workspace
/// puts it under the workspace root rather than the app.
fn build_rust_app(src: &Path, target: Target) -> Result<PathBuf, String> {
    let cross = (target != Target::host()).then(|| target.rust_triple());
    let mut command = std::process::Command::new("cargo");
    command
        .current_dir(src)
        .arg("build")
        .arg("--release")
        .arg("--message-format=json-render-diagnostics")
        .envs(lumen_runtime::app_kind::rust_dynamic_env(cross))
        .stderr(std::process::Stdio::inherit());
    if let Some(triple) = cross {
        command.arg("--target").arg(triple);
    }
    eprintln!("lumenc: cargo build --release (in {})", src.display());
    let output = command
        .output()
        .map_err(|e| format!("cannot run cargo: {e}. A Rust app is built with cargo."))?;
    if !output.status.success() {
        return Err("the cargo build failed".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut executable: Option<PathBuf> = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if let Some(path) = value.get("executable").and_then(|e| e.as_str()) {
            executable = Some(PathBuf::from(path));
        }
    }
    if let Some(path) = executable.filter(|p| p.is_file()) {
        return Ok(path);
    }
    // cargo names the file it wrote, which is the reliable answer because a
    // workspace puts it under the workspace root rather than the app. Fall
    // back to the conventional location for the cases where that path does not
    // resolve here, such as a cargo that builds somewhere else.
    conventional_cargo_executable(src, cross).ok_or_else(|| {
        format!(
            "the cargo build in {} produced no executable to package. A packaged app needs \
             a binary target; a library crate has nothing to ship.",
            src.display()
        )
    })
}

/// The release binary at the layout a single-crate cargo project uses:
/// `target/release/<package name>`, or `target/<triple>/release/<name>` when
/// cross-compiling, with the package name read from the manifest.
fn conventional_cargo_executable(src: &Path, cross: Option<&str>) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(src.join("Cargo.toml")).ok()?;
    let value = toml::from_str::<toml::Value>(&manifest).ok()?;
    let name = value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())?;
    let windows = cross.map_or(cfg!(target_os = "windows"), |t| t.contains("windows"));
    let exe = if windows {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let mut candidate = src.join("target");
    if let Some(triple) = cross {
        candidate.push(triple);
    }
    let candidate = candidate.join("release").join(exe);
    candidate.is_file().then_some(candidate)
}

/// Build a C++ SDK app with CMake and return the executable it produced.
///
/// CMake names the binary in the project's own `CMakeLists.txt`, so there is
/// nothing to read it off; the build tree is searched for the executable it
/// wrote instead, and an ambiguous result is reported rather than guessed at.
fn build_cpp_app(src: &Path, target: Target) -> Result<PathBuf, String> {
    // CMake reads `CMAKE_TOOLCHAIN_FILE` from the environment, which is how a
    // cross build is configured; there is nothing for packaging to invent
    // here, so say plainly what is missing rather than running a build that
    // would quietly produce this machine's binary.
    if target != Target::host() && std::env::var_os("CMAKE_TOOLCHAIN_FILE").is_none() {
        return Err(format!(
            "cross-compiling a C++ app to {} needs a CMake toolchain file for that \
             platform. Set CMAKE_TOOLCHAIN_FILE to it and run this again.",
            target.name
        ));
    }
    let specs = crate::app_kind::dispatch(AppKind::Cpp, src, crate::app_kind::Mode::Build)
        .map_err(|e| e.to_string())?;
    for spec in &specs {
        eprintln!(
            "lumenc: {} {} (in {})",
            spec.program,
            spec.args.join(" "),
            spec.cwd.display()
        );
        let status = std::process::Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .status()
            .map_err(|e| {
                format!(
                    "cannot run {}: {e}. A C++ app is built with CMake.",
                    spec.program
                )
            })?;
        if !status.success() {
            return Err(format!("the {} step failed", spec.program));
        }
    }

    let build_dir = src.join("build");
    let mut found = built_executables(&build_dir);
    match found.len() {
        0 => Err(format!(
            "the CMake build in {} produced no executable to package. Check that the \
             project defines an executable target.",
            build_dir.display()
        )),
        1 => Ok(found.remove(0)),
        _ => {
            // Newest wins, and the alternatives are named so the choice is
            // visible rather than silent.
            found.sort_by_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            let chosen = found.pop().expect("more than one executable");
            eprintln!(
                "lumenc package: the CMake build produced more than one executable ({}); \
                 packaging the most recent, {}.",
                found
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", "),
                chosen.display()
            );
            Ok(chosen)
        }
    }
}

/// Freeze a Python SDK app into an executable and return it.
///
/// A Python app has no compiler of its own, so the executable comes from
/// PyInstaller: it bundles the interpreter, the app's modules, and their
/// dependencies into one file. Everything else about the package is the same
/// as for the other kinds - the shared Lumen library goes beside it, and the
/// app's markup and stylesheet travel, because a Python app reads those at run
/// time exactly as a C++ one does.
///
/// The build directories PyInstaller wants land under the output directory
/// rather than in the app, so freezing an app twice leaves nothing behind in
/// the source tree.
fn build_python_app(src: &Path, app_name: &str, out: &Path) -> Result<PathBuf, String> {
    let entry = crate::app_kind::python_entry(src)?;
    let work = out.join(".build");
    std::fs::create_dir_all(&work).map_err(|e| format!("create {}: {e}", work.display()))?;

    eprintln!(
        "lumenc: pyinstaller --onefile {entry} (in {})",
        src.display()
    );
    let status = std::process::Command::new("pyinstaller")
        .current_dir(src)
        .arg("--noconfirm")
        .arg("--onefile")
        .arg("--name")
        .arg(app_name)
        .arg("--distpath")
        .arg(work.join("dist"))
        .arg("--workpath")
        .arg(work.join("work"))
        .arg("--specpath")
        .arg(&work)
        .arg(&entry)
        .status()
        .map_err(|e| {
            format!(
                "cannot run pyinstaller: {e}. A Python app is frozen into an executable \
                 with it; install it with `pip install pyinstaller`."
            )
        })?;
    if !status.success() {
        return Err("the pyinstaller build failed".to_string());
    }

    let dist = work.join("dist");
    for name in [app_name.to_string(), format!("{app_name}.exe")] {
        let candidate = dist.join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "pyinstaller wrote no executable into {}. Check its output above.",
        dist.display()
    ))
}

/// Every executable a CMake build tree holds, ignoring CMake's own machinery
/// and the libraries an executable links.
fn built_executables(build_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Multi-configuration generators (Visual Studio, Xcode) put the binary in
    // a per-configuration subdirectory; single-configuration ones do not.
    let roots = [
        build_dir.to_path_buf(),
        build_dir.join("Release"),
        build_dir.join("bin"),
    ];
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || !is_executable_file(&path) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let skipped_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "so" | "dylib" | "a" | "lib" | "cmake" | "txt"));
            if skipped_ext || name.starts_with("CMake") || name == "Makefile" {
                continue;
            }
            out.push(path);
        }
    }
    out
}

/// Whether a path is a file the OS would run: the executable bit on Unix, a
/// `.exe` suffix on Windows.
fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    }
}

/// Compile the app, gather the toolchain files, and write the folder.
/// Returns the one-line summary to print.
fn package(
    src: &Path,
    out: &Path,
    app_name: &str,
    target: Target,
    lib_dir: Option<&Path>,
) -> Result<String, String> {
    let compiled = crate::compile_app(src).map_err(|e| e.to_string())?;
    let artifact = build_artifact(compiled, src)?;

    let toolchain = locate_toolchain(target, lib_dir)?;

    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;

    let exe_path = out.join(target.exe_name(app_name));
    let mut sidecar = false;
    match (target.os, cfg!(target_os = "macos")) {
        // A Mach-O signature covers the whole file, so the artifact goes in a
        // section instead of past the end. That needs a linker.
        (Os::Macos, true) => embed_via_cc(&exe_path, &artifact)?,
        (Os::Macos, false) => {
            copy_executable(&toolchain.stub, &exe_path)?;
            let path = out.join(format!("{app_name}.{ARTIFACT_EXT}"));
            std::fs::write(&path, &artifact)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            sidecar = true;
        }
        _ => {
            copy_executable(&toolchain.stub, &exe_path)?;
            append_artifact(&exe_path, &artifact)?;
        }
    }

    copy_c_engine(out, target, &toolchain)?;

    let copied = copy_app_files(src, out, CopyRules::markup())?;

    if sidecar {
        println!(
            "lumenc package: cross-packaging to macOS, so the app ships as \
             {app_name}.{ARTIFACT_EXT} beside the executable"
        );
    }
    Ok(format!(
        "wrote {} for {} ({} app file{} beside it)",
        exe_path.display(),
        target.name,
        copied,
        if copied == 1 { "" } else { "s" }
    ))
}

/// Serialize the compiled app, with asset paths rewritten relative to the app
/// so they still resolve once the package is copied to another machine. The
/// compiler resolves them against the app directory on this machine, which is
/// the right answer for `lumenc run` and the wrong one for a shipped folder.
fn build_artifact(
    mut compiled: lumen_ir::artifact::CompiledApp,
    src: &Path,
) -> Result<Vec<u8>, String> {
    let mut outside: Vec<String> = Vec::new();
    relativize_asset_paths(&mut compiled.ir.root, src, &mut outside);
    for path in &outside {
        eprintln!(
            "lumenc package: warning: {path} is outside the app directory, so it is not \
             copied into the package"
        );
    }
    lumen_ir::artifact::serialize(&compiled).map_err(|e| e.to_string())
}

/// The two toolchain files a package is assembled from.
struct Toolchain {
    stub: PathBuf,
    lib: PathBuf,
}

/// Find the launcher stub and the shared runtime library for `target`.
///
/// `--lib-dir` wins outright. Otherwise a package for this machine's own
/// platform uses the files shipped with the running `lumenc`, which is what an
/// installed toolchain has beside it and what `LUMEN_LIB_DIR` points at in a
/// source tree. A package for another platform comes from the per-version
/// cache, and is fetched from the release channel when the cache is empty.
fn locate_toolchain(target: Target, lib_dir: Option<&Path>) -> Result<Toolchain, String> {
    let wanted = [target.stub_name(), target.lib_name().to_string()];
    let cache = cache_dir(target);
    let dirs = search_dirs(lib_dir, target == Target::host(), cache.clone());

    if let Some(dir) = first_dir_with(&dirs, &wanted) {
        return Ok(Toolchain {
            stub: dir.join(&wanted[0]),
            lib: dir.join(&wanted[1]),
        });
    }
    // Another platform's files are not on this machine until they are
    // fetched; this machine's own come with the installation, so an empty
    // search there is a real failure rather than a cache miss.
    if target != Target::host()
        && let Some(dir) = cache
    {
        fetch_release_files(
            &target.archive_name(),
            &wanted,
            &dir,
            &format!(
                "A release older than app packaging ships no launcher; build the {} files \
                 yourself and pass --lib-dir instead.",
                target.name
            ),
        )?;
        return Ok(Toolchain {
            stub: dir.join(&wanted[0]),
            lib: dir.join(&wanted[1]),
        });
    }

    Err(format!(
        "cannot find {} and {} for {}. Looked in: {}. Set LUMEN_LIB_DIR (or pass \
         --lib-dir) to the directory holding them; in a Lumen source tree that is \
         target/release after `cargo build --release -p lumen-launcher -p lumen`.",
        wanted[0],
        wanted[1],
        target.name,
        searched(&dirs)
    ))
}

/// The prebuilt runtime a web build copies beside the pages it emits.
pub struct WebRuntimeFiles {
    /// The wasm runtime that loads the app's compiled artifact in a browser.
    pub wasm: PathBuf,
    /// The module glue that instantiates it.
    pub js: PathBuf,
}

/// Find the prebuilt web runtime through the same search [`locate_toolchain`]
/// runs for the launcher stub and the shared library: `--lib-dir` first, then
/// the files shipped with the running `lumenc`, then `LUMEN_LIB_DIR`, then the
/// per-version cache, which is filled from the release channel when it is
/// empty.
///
/// The runtime is one prebuilt pair for every app and every platform, so
/// unlike a package there is no target to pick: a web build on any machine
/// wants the same two files.
///
/// A `--lib-dir` says where the files are, so a build that names one and
/// comes up empty is answered rather than fetched: the caller already
/// decided which copy it wants.
pub fn locate_web_runtime(lib_dir_flag: Option<&Path>) -> Result<WebRuntimeFiles, String> {
    let wanted = [WEB_WASM.to_string(), WEB_JS.to_string()];
    let cache = cache_dir_for(WEB_COMPONENT);
    let dirs = search_dirs(lib_dir_flag, true, cache.clone());

    if let Some(dir) = first_dir_with(&dirs, &wanted) {
        return Ok(WebRuntimeFiles {
            wasm: dir.join(&wanted[0]),
            js: dir.join(&wanted[1]),
        });
    }
    if let Some(dir) = lib_dir_flag {
        return Err(format!(
            "cannot find {WEB_WASM} and {WEB_JS} in {}.",
            dir.display()
        ));
    }
    if let Some(dir) = cache {
        fetch_release_files(
            WEB_ARCHIVE,
            &wanted,
            &dir,
            "A release older than the web target ships no web runtime; build it yourself \
             and pass --lib-dir instead.",
        )?;
        return Ok(WebRuntimeFiles {
            wasm: dir.join(&wanted[0]),
            js: dir.join(&wanted[1]),
        });
    }

    Err(format!(
        "cannot find {WEB_WASM} and {WEB_JS}. Looked in: {}. Set LUMEN_LIB_DIR (or pass \
         --lib-dir) to the directory holding them.",
        searched(&dirs)
    ))
}

/// The directories a toolchain artifact is looked for in, in order: the
/// `--lib-dir` flag, the directory holding the running `lumenc`,
/// `LUMEN_LIB_DIR`, then the per-version cache. The middle two are skipped
/// when `installed` is false, which is another platform's files: an
/// installation carries its own platform's, never a second one's.
fn search_dirs(lib_dir: Option<&Path>, installed: bool, cache: Option<PathBuf>) -> Vec<PathBuf> {
    let exe_dir = installed
        .then(|| std::env::current_exe().ok())
        .flatten()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let env_dir = installed
        .then(|| std::env::var_os("LUMEN_LIB_DIR"))
        .flatten()
        .map(PathBuf::from);
    ordered_dirs(lib_dir, exe_dir, env_dir, cache)
}

/// The search order itself, with the two directories [`search_dirs`] reads
/// from the process environment passed in.
fn ordered_dirs(
    lib_dir: Option<&Path>,
    exe_dir: Option<PathBuf>,
    env_dir: Option<PathBuf>,
    cache: Option<PathBuf>,
) -> Vec<PathBuf> {
    [lib_dir.map(Path::to_path_buf), exe_dir, env_dir, cache]
        .into_iter()
        .flatten()
        .collect()
}

/// The first of `dirs` holding every one of `names`. A directory carrying
/// some of them is passed over: a half-populated one cannot assemble
/// anything, and a later directory may be complete.
fn first_dir_with(dirs: &[PathBuf], names: &[String]) -> Option<PathBuf> {
    dirs.iter()
        .find(|dir| names.iter().all(|name| dir.join(name).is_file()))
        .cloned()
}

/// The searched directories as an error message names them.
fn searched(dirs: &[PathBuf]) -> String {
    if dirs.is_empty() {
        return "nowhere".to_string();
    }
    dirs.iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where files that did not come with this installation are kept between
/// runs: under the platform cache directory, keyed by Lumen version and by
/// `component` (a target name, or the web runtime) so an upgrade never
/// reuses the old ones.
fn cache_dir_for(component: &str) -> Option<PathBuf> {
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
            .join("toolchain")
            .join(env!("CARGO_PKG_VERSION"))
            .join(component),
    )
}

/// Where another platform's toolchain files are kept between runs.
fn cache_dir(target: Target) -> Option<PathBuf> {
    cache_dir_for(target.name)
}

/// Copy a file and make it executable on Unix, since a copy keeps the source
/// permissions but an archive member may arrive without them.
fn copy_executable(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
    set_executable(to)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Append the artifact and the footer that says where it starts.
fn append_artifact(exe: &Path, artifact: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(exe)
        .map_err(|e| format!("open {}: {e}", exe.display()))?;
    file.write_all(artifact)
        .and_then(|()| file.write_all(FOOTER_MAGIC))
        .and_then(|()| file.write_all(&(artifact.len() as u64).to_le_bytes()))
        .map_err(|e| format!("write {}: {e}", exe.display()))
}

/// Build the macOS executable by compiling the C wrapper with the artifact
/// linked in as a Mach-O section. `cc` ad-hoc-signs the result, which is what
/// makes it runnable on Apple silicon.
fn embed_via_cc(exe: &Path, artifact: &[u8]) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("lumen-package-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
    let wrapper = tmp.join("wrapper.c");
    let payload = tmp.join(format!("app.{ARTIFACT_EXT}"));
    std::fs::write(&wrapper, MACOS_WRAPPER_C)
        .map_err(|e| format!("write {}: {e}", wrapper.display()))?;
    std::fs::write(&payload, artifact).map_err(|e| format!("write {}: {e}", payload.display()))?;

    let status = std::process::Command::new("cc")
        .arg(&wrapper)
        .arg("-o")
        .arg(exe)
        .arg("-sectcreate")
        .arg("__LUMEN")
        .arg("__lmna")
        .arg(&payload)
        .status()
        .map_err(|e| {
            format!(
                "cannot run cc: {e}. A macOS package is linked, not appended, so it needs \
                 the Xcode Command Line Tools (xcode-select --install)."
            )
        })?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err("cc failed to build the app executable".to_string());
    }
    Ok(())
}

/// What to leave behind when copying an app directory into a package: build
/// inputs and build trees, which differ per app kind.
struct CopyRules {
    /// Directory names never descended into.
    skip_dirs: &'static [&'static str],
    /// File extensions never copied.
    skip_exts: &'static [&'static str],
    /// Exact file names never copied.
    skip_files: &'static [&'static str],
}

impl CopyRules {
    /// A markup app: the markup, the stylesheet, and the scripts are all
    /// compiled into the executable, so they stay behind.
    fn markup() -> Self {
        Self {
            skip_dirs: &["target"],
            skip_exts: &["lmn", "css", "rhai", "lua", "cdl"],
            skip_files: &[],
        }
    }

    /// An SDK app: its markup, stylesheet, and scripts are read at run time by
    /// the app itself, so they travel. What stays behind is the source it was
    /// compiled from and the build tree that compile left.
    fn sdk(kind: AppKind) -> Self {
        match kind {
            AppKind::Cpp => Self {
                skip_dirs: &["target", "build", "src", "include"],
                skip_exts: &["cpp", "cc", "cxx", "h", "hpp"],
                skip_files: &["CMakeLists.txt"],
            },
            // The frozen executable carries the modules, so the sources stay
            // behind along with the interpreter's caches and the freezer's
            // recipe file.
            AppKind::Python => Self {
                skip_dirs: &["build", "dist", "__pycache__", ".venv", "venv"],
                skip_exts: &["py", "pyc", "spec"],
                skip_files: &["pyproject.toml", "requirements.txt"],
            },
            _ => Self {
                skip_dirs: &["target", "src"],
                skip_exts: &["rs"],
                skip_files: &["Cargo.toml", "Cargo.lock"],
            },
        }
    }
}

/// Copy the app's own files into the package, at the same relative paths.
///
/// Everything the app directory holds travels except what the executable
/// already carries and what is not part of the shipped app: dotfiles, the
/// build inputs and outputs named by `rules`, and whichever directory the
/// package is being written into. Copying whole rather than only the files the
/// markup names is deliberate: an app reaches many of its files at run time,
/// through a script that plays a sound or a translation the locale picks, and
/// a static reading of the markup cannot see those.
fn copy_app_files(src: &Path, out: &Path, rules: CopyRules) -> Result<usize, String> {
    let mut count = 0usize;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || rules.skip_dirs.contains(&name.as_str())
                || rules.skip_files.contains(&name.as_str())
            {
                continue;
            }
            if path.is_dir() {
                // Never descend towards the package being written, so the
                // default `dist/<name>` output and anything else already
                // built there stays out of it.
                if !holds(&path, out) {
                    stack.push(path);
                }
                continue;
            }
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| rules.skip_exts.contains(&e))
            {
                continue;
            }
            let Ok(rel) = path.strip_prefix(src) else {
                continue;
            };
            let dest = out.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("copy {} -> {}: {e}", path.display(), dest.display()))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Write the finished package into a single `.zip` beside it, and return
/// where it landed.
///
/// A folder is what runs; a zip is what gets sent. The archive holds the
/// folder itself, so unpacking it gives back the same directory rather than
/// scattering an executable and its libraries into whatever the reader had
/// open. The executable bit is recorded, which matters on Linux and macOS
/// where a lost permission is the difference between an app and a file that
/// will not start.
fn zip_package(out: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "package".to_string());
    let archive_path = out.with_extension("zip");
    let file = std::fs::File::create(&archive_path)
        .map_err(|e| format!("create {}: {e}", archive_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);

    let mut stack = vec![out.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(rel) = path.strip_prefix(out) else {
                continue;
            };
            // A zip names its members with forward slashes whatever wrote it,
            // so the separator is spelled out rather than taken from the
            // platform: a Windows-built archive with backslashes in it unpacks
            // as files with slashes in their names.
            let mut inside = name.clone();
            for part in rel.components() {
                inside.push('/');
                inside.push_str(&part.as_os_str().to_string_lossy());
            }
            let options =
                SimpleFileOptions::default().unix_permissions(if is_executable_file(&path) {
                    0o755
                } else {
                    0o644
                });
            zip.start_file(inside, options)
                .map_err(|e| format!("write {}: {e}", archive_path.display()))?;
            let bytes =
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            zip.write_all(&bytes)
                .map_err(|e| format!("write {}: {e}", archive_path.display()))?;
        }
    }
    zip.finish()
        .map_err(|e| format!("finish {}: {e}", archive_path.display()))?;
    Ok(archive_path)
}

/// Whether `dir` is `inner` or holds it, comparing resolved paths so a
/// relative `dist/App` and an absolute output directory still match.
fn holds(dir: &Path, inner: &Path) -> bool {
    match (std::fs::canonicalize(dir), std::fs::canonicalize(inner)) {
        (Ok(dir), Ok(inner)) => inner.starts_with(&dir),
        _ => inner.starts_with(dir),
    }
}

// ============================================================
// Fetching another platform's toolchain files
// ============================================================

/// Download this Lumen version's release `archive`, check it against the
/// checksums published with the release, and put the `wanted` members of it
/// in `dest`. `hint` closes the message when the release does not carry them,
/// and says how to supply them by hand instead.
fn fetch_release_files(
    archive: &str,
    wanted: &[String],
    dest: &Path,
    hint: &str,
) -> Result<(), String> {
    let repo = std::env::var("LUMEN_GH_REPO").unwrap_or_else(|_| "lumen-fx/lumen".to_string());
    let version = env!("CARGO_PKG_VERSION");
    let base = format!("https://github.com/{repo}/releases/download/v{version}");

    println!("lumenc: fetching {archive} from {base}");

    let sums = http_get(&format!("{base}/sha256sums.txt"))?;
    let sums = String::from_utf8(sums).map_err(|_| {
        "the release checksums are not text; refusing to install from it".to_string()
    })?;
    let want = sums
        .lines()
        .find_map(|line| {
            let (hash, name) = line.split_once("  ")?;
            (name.trim() == archive).then(|| hash.trim().to_lowercase())
        })
        .ok_or_else(|| {
            format!(
                "the v{version} release publishes no checksum for {archive}, so it cannot be \
                 verified. {hint}"
            )
        })?;

    let bytes = http_get(&format!("{base}/{archive}"))?;
    let got = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };
    if got != want {
        return Err(format!(
            "{archive} does not match the checksum published with the release; \
             nothing was installed"
        ));
    }

    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let found = if archive.ends_with(".zip") {
        extract_zip(&bytes, wanted, dest)?
    } else {
        extract_tar_gz(&bytes, wanted, dest)?
    };
    for name in wanted {
        if !found.contains(name) {
            return Err(format!("{archive} carries no {name}. {hint}"));
        }
    }
    Ok(())
}

/// Fetch a URL whole, following redirects.
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut response = ureq::get(url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {url}: {e}"))?;
    Ok(bytes)
}

/// Write out the named members of a `.tar.gz`, ignoring their directories.
fn extract_tar_gz(bytes: &[u8], wanted: &[String], dest: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| format!("cannot read the release archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read the release archive: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("cannot read the release archive: {e}"))?
            .into_owned();
        let Some(name) = file_name_of(&path) else {
            continue;
        };
        if !wanted.contains(&name) {
            continue;
        }
        let out = dest.join(&name);
        entry
            .unpack(&out)
            .map_err(|e| format!("write {}: {e}", out.display()))?;
        set_executable(&out)?;
        found.push(name);
    }
    Ok(found)
}

/// Write out the named members of a `.zip`, ignoring their directories.
fn extract_zip(bytes: &[u8], wanted: &[String], dest: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("cannot read the release archive: {e}"))?;
    for i in 0..archive.len() {
        let mut member = archive
            .by_index(i)
            .map_err(|e| format!("cannot read the release archive: {e}"))?;
        let Some(name) = member.enclosed_name().as_deref().and_then(file_name_of) else {
            continue;
        };
        if !wanted.contains(&name) {
            continue;
        }
        let out = dest.join(&name);
        let mut file =
            std::fs::File::create(&out).map_err(|e| format!("write {}: {e}", out.display()))?;
        std::io::copy(&mut member, &mut file)
            .map_err(|e| format!("write {}: {e}", out.display()))?;
        found.push(name);
    }
    Ok(found)
}

/// The last component of an archive member path, as an owned string.
fn file_name_of(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names_match_the_release_assets() {
        assert_eq!(
            Target::parse("windows-x86_64").map(|t| t.archive_name()),
            Some("lumen-windows-x86_64.zip".to_string())
        );
        assert_eq!(
            Target::parse("linux-aarch64").map(|t| t.archive_name()),
            Some("lumen-linux-aarch64.tar.gz".to_string())
        );
        assert!(Target::parse("plan9-x86_64").is_none());
    }

    #[test]
    fn a_windows_package_keeps_the_exe_suffix_on_any_host() {
        let windows = Target::parse("windows-x86_64").expect("known target");
        assert_eq!(windows.exe_name("Notes"), "Notes.exe");
        assert_eq!(windows.stub_name(), "lumen-launcher.exe");
        let linux = Target::parse("linux-x86_64").expect("known target");
        assert_eq!(linux.exe_name("Notes"), "Notes");
    }

    /// The order every toolchain artifact is looked for in. A flag beats an
    /// installation, an installation beats an environment override, and the
    /// cache is the last resort because reaching it can mean a download.
    #[test]
    fn the_search_order_runs_flag_installation_environment_cache() {
        let dirs = ordered_dirs(
            Some(Path::new("/flag")),
            Some(PathBuf::from("/beside-lumenc")),
            Some(PathBuf::from("/env")),
            Some(PathBuf::from("/cache")),
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/flag"),
                PathBuf::from("/beside-lumenc"),
                PathBuf::from("/env"),
                PathBuf::from("/cache"),
            ]
        );
        assert!(ordered_dirs(None, None, None, None).is_empty());
    }

    /// Another platform's files never come from this installation, so the
    /// flag and the cache are the only places they can be.
    #[test]
    fn a_cross_target_search_skips_this_machines_own_files() {
        let cache = PathBuf::from("/cache");
        assert_eq!(
            search_dirs(None, false, Some(cache.clone())),
            vec![cache.clone()]
        );
        assert_eq!(
            search_dirs(Some(Path::new("/flag")), false, Some(cache.clone())),
            vec![PathBuf::from("/flag"), cache]
        );
    }

    /// A directory carrying one of the two files cannot assemble anything,
    /// so the search goes on to the next one instead of stopping there.
    #[test]
    fn a_half_populated_directory_is_passed_over() {
        let tmp = std::env::temp_dir().join(format!("lumen-web-probe-{}", std::process::id()));
        let partial = tmp.join("partial");
        let full = tmp.join("full");
        std::fs::create_dir_all(&partial).expect("mkdir");
        std::fs::create_dir_all(&full).expect("mkdir");
        std::fs::write(partial.join(WEB_WASM), b"wasm").expect("write");
        std::fs::write(full.join(WEB_WASM), b"wasm").expect("write");
        std::fs::write(full.join(WEB_JS), b"js").expect("write");

        let wanted = [WEB_WASM.to_string(), WEB_JS.to_string()];
        assert_eq!(
            first_dir_with(&[partial.clone(), full.clone()], &wanted),
            Some(full.clone())
        );

        // `--lib-dir` names a complete directory, so the search ends there
        // and never reaches the cache or the release channel.
        let found = locate_web_runtime(Some(&full)).expect("the flagged directory has both");
        assert_eq!(found.wasm, full.join(WEB_WASM));
        assert_eq!(found.js, full.join(WEB_JS));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The web runtime is the same pair everywhere, so it caches under its
    /// own component name rather than under a platform's.
    #[test]
    fn the_web_runtime_caches_beside_the_per_target_toolchains() {
        let Some(dir) = cache_dir_for(WEB_COMPONENT) else {
            return;
        };
        let text = dir.to_string_lossy();
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.ends_with("web"));
    }

    #[test]
    fn the_cache_is_keyed_by_version_and_target() {
        let Some(dir) = cache_dir(Target::parse("macos-aarch64").expect("known target")) else {
            return;
        };
        let text = dir.to_string_lossy();
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.ends_with("macos-aarch64"));
    }

    /// An appended package is the stub, then the artifact, then the footer -
    /// the exact shape `lumen-launcher` reads back.
    #[test]
    fn appending_leaves_the_footer_last() {
        let tmp = std::env::temp_dir().join(format!("lumen-append-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let exe = tmp.join("App");
        std::fs::write(&exe, b"stub").expect("write stub");
        append_artifact(&exe, b"LMNA-bytes").expect("append");

        let image = std::fs::read(&exe).expect("read back");
        assert_eq!(&image[image.len() - 8..], &10u64.to_le_bytes());
        assert_eq!(&image[image.len() - 16..image.len() - 8], FOOTER_MAGIC);
        assert!(image.starts_with(b"stub"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The library a Rust app links and the one a C caller opens are separate
    /// crate targets, so they must never answer with the same file name: two
    /// targets cannot write one file.
    #[test]
    fn the_linked_engine_and_the_c_library_are_different_files() {
        for target in Target::ALL {
            assert_ne!(
                target.linked_engine_name(),
                target.lib_name(),
                "{} names both libraries the same",
                target.name
            );
        }
        let linux = Target::parse("linux-x86_64").expect("known target");
        assert_eq!(linux.linked_engine_name(), "liblumen_engine.so");
        assert_eq!(
            Target::parse("macos-aarch64")
                .expect("known target")
                .linked_engine_name(),
            "liblumen_engine.dylib"
        );
    }

    /// Every target names a Rust triple its own toolchain understands, which
    /// is what an SDK app is cross-compiled with.
    #[test]
    fn every_target_names_a_rust_triple() {
        for target in Target::ALL {
            let triple = target.rust_triple();
            let arch = target.name.split('-').next_back().expect("arch");
            assert!(
                triple.starts_with(arch),
                "{} maps to {triple}, which is not that architecture",
                target.name
            );
        }
    }

    /// cargo writes a binary beside the engine it linked, and an example one
    /// directory further down. Both have to resolve, or a packaged app would
    /// ship without the library it needs.
    #[test]
    fn the_linked_engine_is_found_beside_a_binary_or_an_example() {
        let tmp = std::env::temp_dir().join(format!("lumen-linked-{}", std::process::id()));
        let profile = tmp.join("release");
        let examples = profile.join("examples");
        std::fs::create_dir_all(&examples).expect("mkdir");
        let engine = profile.join("liblumen_engine.so");
        std::fs::write(&engine, b"stand-in engine").expect("write engine");

        assert_eq!(
            linked_engine_beside(&profile.join("app"), "liblumen_engine.so"),
            Some(engine.clone()),
            "beside an ordinary binary"
        );
        assert_eq!(
            linked_engine_beside(&examples.join("counter"), "liblumen_engine.so"),
            Some(engine),
            "one level up from an example"
        );
        assert_eq!(
            linked_engine_beside(&profile.join("app"), "liblumen_missing.so"),
            None,
            "a library that was never built"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A Windows Rust app carries the runtime inside its executable, so there
    /// is no library to copy and nothing to look for.
    #[test]
    fn a_windows_rust_app_carries_no_library() {
        let tmp = std::env::temp_dir().join(format!("lumen-winpkg-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let windows = Target::parse("windows-x86_64").expect("known target");

        let carried = copy_linked_engine(&tmp.join("App.exe"), &tmp, windows).expect("no-op");
        assert_eq!(carried, 0);
        assert!(
            std::fs::read_dir(&tmp)
                .expect("read back")
                .flatten()
                .next()
                .is_none(),
            "nothing was written"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The C library travels under the name the app looks for on the platform
    /// it is packaged for, not under the name it had in the toolchain.
    #[test]
    fn the_c_library_travels_under_the_targets_name() {
        let tmp = std::env::temp_dir().join(format!("lumen-clib-{}", std::process::id()));
        let out = tmp.join("out");
        std::fs::create_dir_all(&out).expect("mkdir");
        let source = tmp.join("whatever-it-was-called");
        std::fs::write(&source, b"stand-in library").expect("write library");

        let windows = Target::parse("windows-x86_64").expect("known target");
        copy_c_engine(
            &out,
            windows,
            &Toolchain {
                stub: tmp.join("unused"),
                lib: source,
            },
        )
        .expect("copy");
        assert!(out.join("lumen.dll").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A Python app's sources are inside the frozen executable, so they stay
    /// behind along with the interpreter's caches.
    #[test]
    fn a_python_package_leaves_the_sources_behind() {
        let rules = CopyRules::sdk(AppKind::Python);
        assert!(rules.skip_exts.contains(&"py"));
        assert!(rules.skip_dirs.contains(&"__pycache__"));
        // What the app reads at run time still travels.
        assert!(!rules.skip_exts.contains(&"lmn"));
        assert!(!rules.skip_exts.contains(&"css"));
    }
}
