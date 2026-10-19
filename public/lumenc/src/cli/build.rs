//! `lumenc build <app_dir> <out>` - ahead-of-time compile a Lumen app.
//!
//! Parses `<app_dir>/src/main.lmn` + optional `src/main.css` **once**, runs the full
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

/// Entry: `lumenc build <app_dir> <out.lmna> [--ui] [--no-hooks]`.
pub fn cmd_build(args: impl Iterator<Item = String>) -> ExitCode {
    const BUILD_USAGE: &str = "lumenc build - ahead-of-time compile an app

USAGE:
    lumenc build <app_dir> <out.lmna> [--ui] [--no-hooks] [--offline]

Parses src/main.lmn + src/main.css once, runs the cascade, and bakes the
scripts
into a precompiled artifact. Run it with
`lumenc run <dir> --artifact <out.lmna>`; a runtime built with no parser
loads only this. An SDK app is rerouted to its own toolchain, and the
<out.lmna> argument does not apply, unless --ui is given.

    --ui              Compile the UI of an app of any kind to <out.lmna>,
                      whatever language its code is in. A Rust app's build
                      script runs this.
    --no-hooks        Skip the app's prebuild [[hooks]].
    --offline         Resolve the app's registry packages from what is
                      already downloaded, and never reach the network.";
    let mut no_hooks = false;
    let mut ui_only = false;
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{BUILD_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--no-hooks" => no_hooks = true,
            "--ui" => ui_only = true,
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
    if kind != crate::app_kind::AppKind::Markup && !ui_only {
        // Without `--ui` the `.lmna` out path is markup-only; ignore any
        // trailing args for a native SDK build (`cargo build --release` /
        // CMake configure+build).
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
    let compile = if ui_only {
        crate::compile_app_ui
    } else {
        crate::compile_app
    };
    let mut compiled = match compile(&src_path) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("lumenc build: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The UI of an SDK app ships inside its executable, which runs from
    // wherever it was copied, so its images are named relative to the app
    // directory, the way a package names them.
    if ui_only {
        let root = std::path::absolute(&src_path).unwrap_or_else(|_| src_path.clone());
        let mut outside = Vec::new();
        lumen_ir::layout_ir::relativize_asset_paths(&mut compiled.ir.root, &root, &mut outside);
        for path in outside {
            eprintln!(
                "lumenc build: warning: {path} is outside the app directory, so a copy of the \
                 app elsewhere will not find it"
            );
        }
    }
    let element_count = count_elements(&compiled.ir.root);
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty())
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("lumenc build: create {}: {e}", parent.display());
        return ExitCode::FAILURE;
    }
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
