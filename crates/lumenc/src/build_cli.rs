//! `lumenc build <app_dir> <out>` - ahead-of-time compile a Lumen app.
//!
//! Parses `<app_dir>/main.lmn` + optional `main.css` **once**, runs the full
//! cascade, resolves asset / include / import paths, bakes the combined
//! script source, and writes the result as a precompiled
//! [`crate::artifact`] blob. A runtime built without the `runtime-parse`
//! feature loads that blob directly (`lumenc run <dir> --artifact <out>`),
//! shipping without the markup parser.
//!
//! Sits beside `lumenc bundle`: `bundle` archives raw source files into a
//! `.lpak`; `build` emits the compiled representation. A future `.lpak v2`
//! will carry the `build` artifact so one archive ships the compiled app.

use std::path::PathBuf;
use std::process::ExitCode;

/// Conventional extension for a compiled-app artifact.
pub const ARTIFACT_EXT: &str = "lmna";

/// Entry: `lumenc build <app_dir> <out.lmna> [--no-hooks]`.
pub fn cmd_build(args: impl Iterator<Item = String>) -> ExitCode {
    const BUILD_USAGE: &str = "lumenc build - ahead-of-time compile an app

USAGE:
    lumenc build <app_dir> <out.lmna> [--no-hooks]

Parses main.lmn + main.css once, runs the cascade, and bakes the scripts
into a precompiled artifact. Run it with
`lumenc run <dir> --artifact <out.lmna>`; a runtime built with no parser
loads only this. An SDK app is rerouted to its own toolchain, and the
<out.lmna> argument does not apply.

    --no-hooks        Skip the app's prebuild [[hooks]].";
    let mut no_hooks = false;
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{BUILD_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--no-hooks" => no_hooks = true,
            other => positional.push(other.to_string()),
        }
    }
    let mut args = positional.into_iter();
    let Some(src) = args.next() else {
        eprintln!("lumenc build: missing <app_dir>");
        return ExitCode::from(2);
    };
    let src_path = PathBuf::from(&src);
    if !src_path.is_dir() {
        eprintln!("lumenc build: '{src}' is not a directory");
        return ExitCode::from(2);
    }
    // Reroute SDK-authored apps (Rust / C++ / Python) to their native build
    // toolchain before assuming the AOT markup-compile path. `[app] kind`
    // overrides auto-detection; otherwise the directory contents decide.
    let cfg = crate::LumenToml::load_or_default(&src_path).unwrap_or_default();
    let kind = crate::app_kind::resolve(&src_path, cfg.app.kind);
    if kind != crate::app_kind::AppKind::Markup {
        // The `.lmna` out path is markup-only; ignore any trailing args for a
        // native SDK build (`cargo build --release` / CMake configure+build).
        for _ in args {}
        return crate::app_kind::build_app_external(kind, &src_path);
    }
    let Some(out) = args.next() else {
        eprintln!("lumenc build: missing <out.{ARTIFACT_EXT}>");
        return ExitCode::from(2);
    };
    if let Some(unexpected) = args.next() {
        eprintln!("lumenc build: unexpected extra argument '{unexpected}'");
        return ExitCode::from(2);
    }
    // `[[hooks]]`: build native artifacts before the AOT compile. `check`
    // never calls this - only `build` (here), `bundle`, and `run` do.
    if !no_hooks
        && let Err(e) = lumen_runtime::hooks::run_hooks(
            &cfg.hooks,
            lumen_runtime::hooks::HookWhen::Prebuild,
            &src_path,
        )
    {
        eprintln!("lumenc build: {e}");
        return ExitCode::FAILURE;
    }
    let out_path = PathBuf::from(&out);
    let compiled = match crate::compile_app(&src_path) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("lumenc build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let element_count = count_elements(&compiled.ir.root);
    match crate::artifact::write(&out_path, &compiled) {
        Ok(()) => {
            let size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            println!(
                "lumenc build: compiled {element_count} elements -> {} ({size} bytes)",
                out_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc build: write {}: {e}", out_path.display());
            ExitCode::FAILURE
        }
    }
}

fn count_elements(el: &crate::Element) -> usize {
    1 + el.children.iter().map(count_elements).sum::<usize>()
}
