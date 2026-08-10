//! `lumenc bundle [--static] <app_dir> <out>` subcommand.
//!
//! Two shapes:
//!
//! - Default (asset pack): walks `app_dir` and ships every regular file
//!   (skipping dotfiles and `target/` directories) into a single `.lpak`
//!   archive. Mirrors `glib-compile-resources` (GTK) and `rcc` (Qt).
//! - `--static` (Part B tree-shaking): resolves the app's capability set
//!   (`lumen.toml [capabilities]` + a conservative source scan), maps it to a
//!   cargo `--features` list, and builds the per-app static runtime seam
//!   (`lumen-ffi`) with `--no-default-features --features "<set>"` so the
//!   binary carries only the subsystems that app uses. The shared dlopen'd
//!   cdylib and the dev `lumenc run` path stay full-featured and untrimmed.

use std::path::PathBuf;
use std::process::ExitCode;

/// Top-level entry: parse args and pack (or statically build) the directory.
///
/// Expected argv: `[--static] <app_dir> <out>`. Surfaces `--help` to the
/// shared usage block.
pub fn cmd_bundle(args: impl Iterator<Item = String>) -> ExitCode {
    let mut static_build = false;
    let mut no_hooks = false;
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--static" => static_build = true,
            "--no-hooks" => no_hooks = true,
            other => positional.push(other.to_string()),
        }
    }
    let mut it = positional.into_iter();
    let Some(src) = it.next() else {
        eprintln!("lumenc bundle: missing <app_dir>");
        return ExitCode::from(2);
    };
    let Some(out) = it.next() else {
        eprintln!(
            "lumenc bundle: missing <{}>",
            if static_build { "out_dir" } else { "out.lpak" }
        );
        return ExitCode::from(2);
    };
    if let Some(unexpected) = it.next() {
        eprintln!("lumenc bundle: unexpected extra argument '{unexpected}'");
        return ExitCode::from(2);
    }
    let src_path = PathBuf::from(&src);
    let out_path = PathBuf::from(&out);
    if !src_path.is_dir() {
        eprintln!("lumenc bundle: '{src}' is not a directory");
        return ExitCode::from(2);
    }

    // `[[hooks]]`: build native artifacts before packing / building the seam.
    // Requires a `dev-run` lumenc (`lumen.toml` config + hook execution live
    // in `lumen-runtime`); a thin build without it has no config loader to
    // read `[[hooks]]` from and silently skips them.
    if !no_hooks && let Err(code) = run_prebuild_hooks(&src_path) {
        return code;
    }

    if static_build {
        return cmd_bundle_static(&src_path, &out_path);
    }

    match lumen_assets::LumenBundle::pack_dir(&src_path, &out_path) {
        Ok(count) => {
            println!(
                "lumenc bundle: packed {count} files -> {}",
                out_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc bundle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run every `prebuild` `[[hooks]]` entry for `src_path`. Requires `dev-run`
/// (config parsing + hook execution live in `lumen-runtime`); a lumenc built
/// without it links no config loader, so it treats an app as having no
/// declared hooks rather than failing the bundle outright.
#[cfg(feature = "dev-run")]
fn run_prebuild_hooks(src_path: &std::path::Path) -> Result<(), ExitCode> {
    let cfg = match crate::config::LumenToml::load_or_default(src_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lumenc bundle: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    lumen_runtime::hooks::run_hooks(
        &cfg.hooks,
        lumen_runtime::hooks::HookWhen::Prebuild,
        src_path,
    )
    .map_err(|e| {
        eprintln!("lumenc bundle: {e}");
        ExitCode::FAILURE
    })
}

/// Thin-build fallback: no `lumen-runtime` is linked, so there is no
/// `LumenToml` to read `[[hooks]]` from. Silently a no-op.
#[cfg(not(feature = "dev-run"))]
fn run_prebuild_hooks(_src_path: &std::path::Path) -> Result<(), ExitCode> {
    Ok(())
}

/// `lumenc bundle --static`: resolve the per-app capability set and build the
/// trimmed static runtime seam. Requires the `dev-run` build of lumenc (config
/// + capability inference live in `lumen-runtime`).
#[cfg(feature = "dev-run")]
fn cmd_bundle_static(src_path: &std::path::Path, out_path: &std::path::Path) -> ExitCode {
    use crate::config::{BundleCapabilities, LumenToml};

    let cfg = match LumenToml::load_or_default(src_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lumenc bundle --static: {e}");
            return ExitCode::FAILURE;
        }
    };
    let caps = BundleCapabilities::resolve(src_path, &cfg);
    let features = caps.to_features();
    let feature_arg = features.join(",");

    println!(
        "lumenc bundle --static: resolved capabilities for {}",
        src_path.display()
    );
    println!("    audio      = {}", caps.audio);
    println!("    http-fetch = {}", caps.http_fetch);
    println!("    mcp        = {}", caps.mcp);
    println!("    async      = {}", caps.async_rt);
    println!("    script host= {:?}", caps.host);
    println!(
        "    runtime features: --no-default-features --features \"{}\"",
        if feature_arg.is_empty() {
            "<none>".to_string()
        } else {
            feature_arg.clone()
        }
    );

    // Locate the Lumen workspace to build the seam from. An in-tree dev build
    // finds it via the compile-time manifest dir (lumenc/ -> workspace root);
    // an override lets a relocated toolchain point at the source tree.
    let workspace_dir = std::env::var_os("LUMEN_WORKSPACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        });
    if !workspace_dir.join("Cargo.toml").is_file() {
        eprintln!(
            "lumenc bundle --static: cannot locate the Lumen workspace to build the \
             trimmed runtime seam (looked in {}). Set LUMEN_WORKSPACE_DIR to the Lumen \
             source tree. Resolved feature set above is still valid.",
            workspace_dir.display()
        );
        return ExitCode::FAILURE;
    }

    // Build the per-app static seam (lumen-ffi cdylib) with exactly this app's
    // features. `panic = unwind` is preserved by the workspace release profile
    // (the C-ABI's catch_unwind depends on it).
    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(&workspace_dir)
        .arg("build")
        .arg("--release")
        .arg("-p")
        .arg("lumen-ffi")
        .arg("--no-default-features");
    if !feature_arg.is_empty() {
        cmd.arg("--features").arg(&feature_arg);
    }
    println!("lumenc bundle --static: building trimmed runtime seam (this may take a while)...");
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lumenc bundle --static: failed to invoke cargo: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !status.success() {
        eprintln!("lumenc bundle --static: seam build failed");
        return ExitCode::FAILURE;
    }

    // Report the produced artifact + size, and copy it beside the app.
    let lib_name = if cfg!(target_os = "windows") {
        "lumen_ffi.dll"
    } else if cfg!(target_os = "macos") {
        "liblumen_ffi.dylib"
    } else {
        "liblumen_ffi.so"
    };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_dir.join("target"));
    let built = target_dir.join("release").join(lib_name);
    if let Ok(meta) = std::fs::metadata(&built) {
        let mb = meta.len() as f64 / (1024.0 * 1024.0);
        println!(
            "lumenc bundle --static: built {} ({mb:.1} MiB)",
            built.display()
        );
        if let Err(e) = std::fs::create_dir_all(out_path) {
            eprintln!("lumenc bundle --static: create {}: {e}", out_path.display());
            return ExitCode::FAILURE;
        }
        let dest = out_path.join(lib_name);
        if let Err(e) = std::fs::copy(&built, &dest) {
            eprintln!(
                "lumenc bundle --static: copy seam -> {}: {e}",
                dest.display()
            );
            return ExitCode::FAILURE;
        }
        println!("lumenc bundle --static: staged seam -> {}", dest.display());
    } else {
        eprintln!(
            "lumenc bundle --static: seam built but artifact not found at {}",
            built.display()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Thin-build fallback: capability inference + the config parser live behind
/// `dev-run`, so a parser-only `lumenc` library cannot resolve a bundle's
/// feature set.
#[cfg(not(feature = "dev-run"))]
fn cmd_bundle_static(_src_path: &std::path::Path, _out_path: &std::path::Path) -> ExitCode {
    eprintln!(
        "lumenc bundle --static requires a dev-run build of lumenc (capability \
         inference lives in lumen-runtime)"
    );
    ExitCode::FAILURE
}
