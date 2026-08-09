//! `lumenc` Lumen AOT compiler and runner CLI.
//!
//! Subcommands dispatch into either `lumenc` lib functions or `lumenc::mcp_cli` handlers.
//! `--help` prints [`USAGE`]; `--version` prints the `CARGO_PKG_VERSION`.

mod update_check;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // Earliest reachable instant, used only when `LUMEN_BOOT_TRACE` is set:
    // the windowed backend times exec->first-frame from here for the startup
    // marker (parity with the headless boot-trace). A single `OnceLock::set`;
    // a normal run pays nothing else.
    lumen_core::app::mark_process_start();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = argv.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    // Look for a newer release on a background thread while the command runs,
    // and say so afterwards. Most invocations skip it outright; see
    // `update_check::start`.
    let update = update_check::start(cmd, rest);
    let code = dispatch(cmd, rest.to_vec());
    if let Some(update) = update {
        update.finish();
    }
    code
}

fn dispatch(cmd: &str, args: Vec<String>) -> ExitCode {
    let args = args.into_iter();
    match cmd {
        "run" => cmd_run(args),
        // `check` / `build` compile source via the runtime's `check_app` /
        // `compile_app`, so they need both the parser (`runtime-parse`) and the
        // static runtime (`dev-run`). Absent in a thin `dlopen-run` launcher.
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "check" => cmd_check(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "build" => lumenc::build_cli::cmd_build(args),
        "new" => cmd_new(args),
        #[cfg(feature = "runtime-parse")]
        "fmt" => cmd_fmt(args),
        // The MCP-driven inspection / automation subcommands read `lumen.toml`
        // and defer to the runtime, so they are gated with `dev-run`.
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "snapshot" => lumenc::mcp_cli::cmd_snapshot(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "find" => lumenc::mcp_cli::cmd_find(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "element-at" => lumenc::mcp_cli::cmd_element_at(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "click" => lumenc::mcp_cli::cmd_click(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "type" => lumenc::mcp_cli::cmd_type(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "key" => lumenc::mcp_cli::cmd_key(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "scroll" => lumenc::mcp_cli::cmd_scroll(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "lint" => lumenc::mcp_cli::cmd_lint(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "diff" => lumenc::mcp_cli::cmd_diff(args),
        #[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
        "screenshot" => lumenc::mcp_cli::cmd_screenshot(args),
        #[cfg(feature = "bundle")]
        "bundle" => lumenc::bundle_cli::cmd_bundle(args),
        "i18n" => lumenc::i18n_cli::cmd_i18n(args),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("lumenc {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("lumenc: unknown command `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `lumenc run` on the static path (`dev-run`): parse in-process and drive the
/// statically-linked runtime, with full state-preserving hot-reload.
#[cfg(feature = "dev-run")]
fn cmd_run(args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir: Option<String> = None;
    let mut profile: Option<lumenc::profile::ProfileMode> = None;
    let mut artifact: Option<PathBuf> = None;
    let mut headless = false;
    let mut size: Option<(u32, u32)> = None;
    let mut dpr: Option<f32> = None;
    let mut ticks: Option<u64> = None;
    let mut no_hooks = false;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-hooks" => no_hooks = true,
            "--profile" => {
                let Some(v) = args.next() else {
                    eprintln!("lumenc run: --profile needs a value (chrome|stderr)");
                    return ExitCode::from(2);
                };
                match lumenc::profile::ProfileMode::try_from(v.as_str()) {
                    Ok(m) => profile = Some(m),
                    Err(e) => {
                        eprintln!("lumenc run: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            s if s.starts_with("--profile=") => {
                let v = &s["--profile=".len()..];
                match lumenc::profile::ProfileMode::try_from(v) {
                    Ok(m) => profile = Some(m),
                    Err(e) => {
                        eprintln!("lumenc run: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            "--headless" => headless = true,
            s if s == "--size" || s.starts_with("--size=") => {
                let v = match s.strip_prefix("--size=") {
                    Some(v) => Some(v.to_string()),
                    None => args.next(),
                };
                match v.as_deref().and_then(parse_size) {
                    Some(wh) => size = Some(wh),
                    None => {
                        eprintln!("lumenc run: --size needs WxH (e.g. --size 1280x800)");
                        return ExitCode::from(2);
                    }
                }
            }
            s if s == "--dpr" || s.starts_with("--dpr=") => {
                let v = match s.strip_prefix("--dpr=") {
                    Some(v) => Some(v.to_string()),
                    None => args.next(),
                };
                match v.and_then(|v| v.parse::<f32>().ok()).filter(|d| *d > 0.0) {
                    Some(d) => dpr = Some(d),
                    None => {
                        eprintln!("lumenc run: --dpr needs a positive number (e.g. --dpr 1.5)");
                        return ExitCode::from(2);
                    }
                }
            }
            // Load a precompiled AOT artifact instead of parsing source.
            "--artifact" => {
                let Some(v) = args.next() else {
                    eprintln!("lumenc run: --artifact needs a path");
                    return ExitCode::from(2);
                };
                artifact = Some(PathBuf::from(v));
            }
            s if s.starts_with("--artifact=") => {
                artifact = Some(PathBuf::from(&s["--artifact=".len()..]));
            }
            s if s == "--ticks" || s.starts_with("--ticks=") => {
                let v = match s.strip_prefix("--ticks=") {
                    Some(v) => Some(v.to_string()),
                    None => args.next(),
                };
                match v.and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) => ticks = Some(n),
                    None => {
                        eprintln!("lumenc run: --ticks needs a tick count (e.g. --ticks 120)");
                        return ExitCode::from(2);
                    }
                }
            }
            _ if dir.is_none() => dir = Some(a),
            _ => {
                eprintln!("lumenc run: unexpected argument '{a}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("lumenc run: missing <dir>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    // Reroute SDK-authored apps (Rust / C++ / Python) to their native
    // toolchain before assuming the built-in markup runtime path. A
    // `lumen.toml [app] kind` value overrides auto-detection; otherwise the
    // directory contents decide (see `lumenc::app_kind`).
    let dir_path = PathBuf::from(&dir);
    let cfg = match lumenc::LumenToml::load_or_default(&dir_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("lumenc run: lumen.toml: {e}");
            return ExitCode::from(2);
        }
    };
    let kind = lumenc::app_kind::resolve(&dir_path, cfg.app.kind);
    if kind != lumenc::app_kind::AppKind::Markup {
        if headless || artifact.is_some() || size.is_some() || dpr.is_some() || ticks.is_some() {
            eprintln!(
                "lumenc run: --headless / --artifact / --size / --dpr / --ticks apply only to \
                 markup apps; {kind:?} apps run via their native toolchain"
            );
            return ExitCode::from(2);
        }
        return lumenc::app_kind::run_app_external(kind, &dir_path);
    }
    if !headless && (size.is_some() || dpr.is_some() || ticks.is_some()) {
        eprintln!("lumenc run: --size / --dpr / --ticks require --headless");
        return ExitCode::from(2);
    }
    // `[[hooks]]`: build native artifacts before the run (`prebuild`), then
    // run any `prerun` setup commands. `--no-hooks` skips both. `check`
    // never reaches this arm, so it stays side-effect free.
    if !no_hooks {
        if let Err(e) = lumen_runtime::hooks::run_hooks(
            &cfg.hooks,
            lumen_runtime::hooks::HookWhen::Prebuild,
            &dir_path,
        ) {
            eprintln!("lumenc run: {e}");
            return ExitCode::FAILURE;
        }
        if let Err(e) = lumen_runtime::hooks::run_hooks(
            &cfg.hooks,
            lumen_runtime::hooks::HookWhen::Prerun,
            &dir_path,
        ) {
            eprintln!("lumenc run: {e}");
            return ExitCode::FAILURE;
        }
    }
    // Resolve `profile`: CLI flag wins; otherwise read `[profile] mode` from `lumen.toml`. A `lumen.toml` parse failure aborts the command.
    if profile.is_none() {
        match lumenc::LumenToml::load_or_default(&PathBuf::from(&dir)) {
            Ok(cfg) => {
                if let Some(mode) = cfg.profile.mode.as_deref()
                    && mode != "off"
                {
                    match lumenc::profile::ProfileMode::try_from(mode) {
                        Ok(m) => profile = Some(m),
                        Err(e) => {
                            eprintln!("lumenc run: lumen.toml [profile]: {e}");
                            return ExitCode::from(2);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("lumenc run: lumen.toml: {e}");
                return ExitCode::from(2);
            }
        }
    }
    // Install the profiler subscriber before `App::new` so the first tick's spans are captured. The guard is held until `run_app` returns; its `Drop` flushes the chrome writer.
    let _guard = match profile {
        Some(mode) => match lumenc::profile::install(mode) {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("lumenc run: profiler init failed: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let mut opts = lumenc::RunOptions::new(PathBuf::from(dir));
    if let Some((w, h)) = size {
        opts.size = (w, h);
    }
    if let Some(path) = artifact {
        opts.artifact = Some(path);
    }
    let result = if headless {
        lumenc::run_app_headless_rendered(
            opts,
            lumenc::HeadlessOptions {
                dpr: dpr.unwrap_or(1.0),
                ticks,
            },
        )
    } else {
        lumenc::run_app(opts)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lumenc run: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lumenc run` on the thin link-not-embed launcher path (`dlopen-run`, no
/// `dev-run`): compile the app to LMNA bytes in-process, then dlopen the shared
/// liblumen and drive it over the C-ABI. No backend is static-linked here.
///
/// Alpha scope: markup apps only (no SDK app-kind reroute), no `--profile` /
/// `--size` / `--dpr`, and no state-preserving hot-reload; re-run to pick up
/// edits. See `docs/design/link-not-embed.md`.
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run")))]
fn cmd_run(args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir: Option<String> = None;
    let mut headless = false;
    let mut ticks: Option<u32> = None;
    let mut no_hooks = false;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headless" => headless = true,
            "--no-hooks" => no_hooks = true,
            s if s == "--ticks" || s.starts_with("--ticks=") => {
                let v = match s.strip_prefix("--ticks=") {
                    Some(v) => Some(v.to_string()),
                    None => args.next(),
                };
                match v.and_then(|v| v.parse::<u32>().ok()) {
                    Some(n) => ticks = Some(n),
                    None => {
                        eprintln!("lumenc run: --ticks needs a tick count (e.g. --ticks 120)");
                        return ExitCode::from(2);
                    }
                }
            }
            // Flags the static (`dev-run`) launcher supports but the thin
            // launcher does not (yet). Reject clearly rather than ignore.
            s if s == "--profile"
                || s.starts_with("--profile=")
                || s == "--size"
                || s.starts_with("--size=")
                || s == "--dpr"
                || s.starts_with("--dpr=") =>
            {
                eprintln!(
                    "lumenc run: '{s}' is not supported by the thin (dlopen) launcher; \
                     rebuild lumenc with --features dev-run for that flag"
                );
                return ExitCode::from(2);
            }
            _ if dir.is_none() => dir = Some(a),
            _ => {
                eprintln!("lumenc run: unexpected argument '{a}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("lumenc run: missing <dir>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let dir_path = PathBuf::from(&dir);
    // `[[hooks]]`: this launcher links no `lumen-runtime` (that's the whole
    // point of `dlopen-run`), so it cannot use `lumen_runtime::hooks` /
    // `LumenToml`. `thin_run_hooks` reads the same schema straight off the
    // raw `toml::Value`, mirroring `lumenc::compile::entry_name`'s existing
    // pattern for reading `lumen.toml` without the full config parser.
    if !no_hooks {
        if let Err(e) = thin_run_hooks(&dir_path, "prebuild") {
            eprintln!("lumenc run: {e}");
            return ExitCode::FAILURE;
        }
        if let Err(e) = thin_run_hooks(&dir_path, "prerun") {
            eprintln!("lumenc run: {e}");
            return ExitCode::FAILURE;
        }
    }
    // 1. Compile source -> LMNA bytes in-process (parser only; no runtime).
    let bytes = match lumenc::compile::compile_dir_to_lmna(&dir_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("lumenc run: compile: {e}");
            return ExitCode::FAILURE;
        }
    };
    // 2. dlopen liblumen and drive the app over the C-ABI. Headless runs a tick
    //    budget (default 1 when unspecified: build-and-tick smoke).
    let headless_ticks = if headless {
        Some(ticks.unwrap_or(1))
    } else {
        None
    };
    match lumenc::loader::run_via_dlopen(&bytes, &dir_path, headless_ticks) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lumenc run: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Minimal `[[hooks]]` reader + runner for the thin (`dlopen-run`) launcher.
/// It links no `lumen-runtime`, so it cannot use the full `LumenToml` /
/// `lumen_runtime::hooks` machinery; instead it reads the raw `toml::Value`
/// directly, the same way `lumenc::compile::entry_name` reads `[app] entry`
/// without the full config parser. Runs every `[[hooks]]` entry whose `when`
/// matches `trigger` and whose `os` (if set) matches
/// `std::env::consts::OS`, in declaration order, skipping a hook whose
/// declared outputs are all already newer than its declared inputs (see
/// `lumen_runtime::hooks::run_hooks` for the same staleness rule).
///
/// Unlike the full config parser, a malformed `lumen.toml` or an unknown
/// `when` / `os` value here is not a hard error - it is silently skipped,
/// matching this launcher's existing lenient `lumen.toml`-is-optional
/// stance (see `entry_name`). A lumenc built with `dev-run` (the default)
/// never takes this path.
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run")))]
fn thin_run_hooks(dir: &std::path::Path, trigger: &str) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(dir.join("lumen.toml")) else {
        return Ok(());
    };
    // `toml::from_str` parses a document; the `FromStr` impl parses a single
    // TOML value and would reject every real `lumen.toml`, skipping all hooks
    // without a word.
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Ok(());
    };
    let Some(hooks) = value.get("hooks").and_then(|h| h.as_array()) else {
        return Ok(());
    };
    let os = std::env::consts::OS;
    for hook in hooks {
        if hook.get("when").and_then(|v| v.as_str()) != Some(trigger) {
            continue;
        }
        if let Some(hook_os) = hook.get("os").and_then(|v| v.as_str())
            && hook_os != os
        {
            continue;
        }
        let Some(run) = hook
            .get("run")
            .and_then(|v| v.as_str())
            .filter(|r| !r.trim().is_empty())
        else {
            continue;
        };
        let inputs = thin_hook_paths(hook, "inputs");
        let outputs = thin_hook_paths(hook, "outputs");
        if thin_hook_is_stale_free(dir, &inputs, &outputs) {
            continue;
        }
        let status = thin_shell_command(run)
            .current_dir(dir)
            .status()
            .map_err(|e| format!("hook `{run}`: failed to run: {e}"))?;
        if !status.success() {
            return Err(format!("hook `{run}` exited with {status}"));
        }
    }
    Ok(())
}

/// `sh -c "<run>"` on unix, `cmd /C "<run>"` on windows - mirrors
/// `lumen_runtime::hooks`'s own shell dispatch.
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run"), unix))]
fn thin_shell_command(run: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(run);
    cmd
}

/// `sh -c "<run>"` on unix, `cmd /C "<run>"` on windows - mirrors
/// `lumen_runtime::hooks`'s own shell dispatch.
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run"), windows))]
fn thin_shell_command(run: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("cmd");
    cmd.arg("/C").arg(run);
    cmd
}

/// Read a `[[hooks]]` entry's `inputs` / `outputs` string array (absent ->
/// empty).
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run")))]
fn thin_hook_paths(hook: &toml::Value, key: &str) -> Vec<String> {
    hook.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Same staleness rule as `lumen_runtime::hooks::is_stale_free`: skip only
/// when both lists are non-empty, every output exists, every input exists,
/// and every output's mtime is at least as new as the newest input's mtime.
#[cfg(all(feature = "dlopen-run", not(feature = "dev-run")))]
fn thin_hook_is_stale_free(dir: &std::path::Path, inputs: &[String], outputs: &[String]) -> bool {
    if inputs.is_empty() || outputs.is_empty() {
        return false;
    }
    let mtime = |rel: &str| -> Option<std::time::SystemTime> {
        let p = std::path::Path::new(rel);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        std::fs::metadata(full).ok()?.modified().ok()
    };
    let Some(newest_input) = inputs
        .iter()
        .map(|p| mtime(p))
        .collect::<Option<Vec<_>>>()
        .and_then(|v| v.into_iter().max())
    else {
        return false;
    };
    let Some(output_mtimes) = outputs.iter().map(|p| mtime(p)).collect::<Option<Vec<_>>>() else {
        return false;
    };
    output_mtimes.into_iter().all(|out| out >= newest_input)
}

/// Fallback `lumenc run` when neither run backend is compiled in. Building the
/// binary without `dev-run` and without `dlopen-run` yields a compiler with no
/// way to run an app; say so instead of failing to build.
#[cfg(all(not(feature = "dev-run"), not(feature = "dlopen-run")))]
fn cmd_run(_args: impl Iterator<Item = String>) -> ExitCode {
    eprintln!(
        "lumenc run: this lumenc was built without a run backend. Rebuild with \
         --features dev-run (static runtime) or --features dlopen-run (thin launcher)."
    );
    ExitCode::from(2)
}

/// Parse `--size WxH` (e.g. `1280x800`). Zero dimensions are rejected.
#[cfg(feature = "dev-run")]
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let w = w.trim().parse::<u32>().ok().filter(|v| *v > 0)?;
    let h = h.trim().parse::<u32>().ok().filter(|v| *v > 0)?;
    Some((w, h))
}

/// Handles `lumenc fmt <file> [--check]`. Rewrites `<file>` in place.
/// With `--check`, exits non-zero when the file would change and leaves the bytes untouched.
#[cfg(feature = "runtime-parse")]
fn cmd_fmt(args: impl Iterator<Item = String>) -> ExitCode {
    let mut path: Option<String> = None;
    let mut check_only = false;
    for a in args {
        match a.as_str() {
            "--check" => check_only = true,
            other if !other.starts_with("--") => path = Some(other.to_string()),
            other => {
                eprintln!("lumenc fmt: unknown flag `{other}`\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(path) = path else {
        eprintln!("lumenc fmt: missing <file>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let p = PathBuf::from(&path);
    if check_only {
        match lumenc::formatter::check_file(&p) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => {
                eprintln!("lumenc fmt: {path} is not formatted");
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("lumenc fmt: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        match lumenc::formatter::format_file(&p) {
            Ok(true) => {
                println!("lumenc fmt: rewrote {path}");
                ExitCode::SUCCESS
            }
            Ok(false) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("lumenc fmt: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
fn cmd_check(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(dir) = args.next() else {
        eprintln!("lumenc check: missing <dir>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    match lumenc::check_app(&PathBuf::from(&dir)) {
        Ok(report) => {
            println!(
                "{dir}: ok ({} elements, script: {})",
                report.element_count,
                if report.has_script { "yes" } else { "none" },
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc check: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Handles `lumenc new <template> <name>` and `lumenc new --list`.
/// Scaffolds a directory `<name>` from one of the built-in gallery
/// templates (see [`lumenc::scaffold::TEMPLATES`]).
fn cmd_new(args: impl Iterator<Item = String>) -> ExitCode {
    let mut args = args;
    let Some(template) = args.next() else {
        eprintln!("lumenc new: missing <template>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    if template == "--list" || template == "-l" {
        println!("Available templates:\n");
        let width = lumenc::scaffold::TEMPLATES
            .iter()
            .map(|t| t.name.len())
            .max()
            .unwrap_or(0);
        for t in lumenc::scaffold::TEMPLATES {
            println!("    {:width$}  {}", t.name, t.description, width = width);
        }
        println!("\nScaffold one with: lumenc new <template> <name>");
        return ExitCode::SUCCESS;
    }
    let Some(name) = args.next() else {
        eprintln!("lumenc new: missing <name>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(&name);
    if dir.exists() {
        eprintln!("lumenc new: {name} already exists; refusing to overwrite");
        return ExitCode::FAILURE;
    }
    let files = match lumenc::scaffold::find(&template) {
        Some(t) => t.files,
        None => {
            eprintln!(
                "lumenc new: unknown template '{template}' (available: {})",
                lumenc::scaffold::template_names(),
            );
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("lumenc new: create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    for (rel, body) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("lumenc new: create {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("lumenc new: write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    println!("created {name}/ from template '{template}'.\nrun it with: lumenc run {name}");
    ExitCode::SUCCESS
}

const USAGE: &str = "lumenc - Lumen markup runner

USAGE:
    lumenc run <dir> [--profile chrome|tracy|stderr]
                     [--headless [--size WxH] [--dpr N] [--ticks N]]
                     [--no-hooks]
                          Run <dir>/main.lmn (+ optional main.css).
                          --profile chrome writes lumen-trace.json (open
                          in chrome://tracing or https://ui.perfetto.dev).
                          --profile tracy connects to tracy-profiler.
                          --profile stderr dumps per-system spans live.
                          All --profile modes need a lumenc built with
                          `--features profiling` (tracy additionally
                          needs `--features profiling-tracy`); default
                          builds compile no span instrumentation and
                          error here with a rebuild hint.
                          --headless is the automation/CI mode: the full
                          pipeline (layout, GPU rendering, MCP server,
                          simulate, screenshots, hot reload) runs with
                          ZERO windows - the desktop is never touched.
                          Ticks run on demand (MCP wake / animations /
                          dirty state) and the process idles otherwise.
                          --size sets the logical viewport (default:
                          lumen.toml [window] size, else 960x720);
                          --dpr scales the offscreen target (screenshot
                          pixels = logical x dpr; default 1.0);
                          --ticks N runs exactly N ticks then exits
                          (bounded CI runs). SIGINT/SIGTERM exit 0 via
                          the graceful-close path.
                          Runs `lumen.toml`'s `[[hooks]]` `prebuild` then
                          `prerun` entries first (skipped when an entry's
                          declared outputs are already newer than its
                          inputs); --no-hooks skips both. `check` never
                          runs hooks.
    lumenc check <dir>    Parse without spawning a window (CI gate)
    lumenc new <template> <name>
                          Scaffold a fresh app directory from the
                          template gallery: hello | counter | form |
                          todo | dashboard | settings | hotkeys.
                          Every template ships main.lmn (+ CSS +
                          Rhai + lumen.toml) and a README explaining
                          the concepts it demonstrates.
    lumenc new --list     Print the template gallery with one-line
                          descriptions.
    lumenc fmt <file>     Reformat a `.lmn` markup file in place. Pass
                          `--check` to exit non-zero on diff without
                          rewriting (CI gate).
    lumenc snapshot [--text|--json] [--max-lines N] [--cursor C]
                          [--include-invisible] [--port P] [--app <dir>]
                          One-shot a11y-tree-style text dump of the
                          running app's UI via the MCP TCP server.
                          Port resolution: --port > LUMEN_MCP_PORT >
                          lumen.toml [mcp].port (with --app) > 7878.
    lumenc find [--text S] [--role R] [--id N] [--limit N] [--json]
                          Selector search over the live snapshot. Exits
                          non-zero with no matches; otherwise prints one
                          row per hit (id role label bounds state).
    lumenc element-at <x> <y> [--json]
                          Topmost entity at logical-pixel point (x, y).
                          Exits non-zero on miss.
    lumenc click <x> <y> [--button primary|secondary|middle]
                          [--wait-for ClickEvent] [--port P] [--app D]
                          Inject a click via lumen.simulate. Requires
                          [mcp] simulate = true in lumen.toml.
    lumenc type <text> [--wait-for KeyPressed]
                          Type a string into the focused entity.
    lumenc key <name> [--shift] [--ctrl] [--alt] [--super]
                          Inject one keypress (Enter|Tab|Escape|a|...).
    lumenc scroll <x> <y> <dx> <dy>
                          Inject a wheel event at (x, y) of (dx, dy) px.
    lumenc lint [--json]  Snapshot-only lint pass; one finding per line.
                          Exits non-zero if any error-severity finding.
    lumenc lint --css-cascade [<dir>] [--json]
                          Offline static check that flags every rule
                          whose resolved value flips between the old
                          first-wins ordering and the CSS Cascade-5
                          last-wins ordering. Exits non-zero with at
                          least one divergence.
    lumenc lint --signals [<app-dir>] [--json] [--strict]
                          Offline signal lint. Reads <app-dir>/main.lmn
                          + main.rhai and the optional [signals]
                          schema in lumen.toml; flags untyped writes,
                          bare {name} interpolation ambiguities,
                          schema mismatches, untracked binds, and
                          orphan writes. --strict upgrades warnings
                          to errors.
    lumenc diff [tick] [--json]
                          Show added/removed/changed entity ids since
                          `tick` (or previous tick if omitted).
    lumenc screenshot [out.png] [--highlight id1,id2,...] [--lint]
                          [--bounds map.json]
                          Capture the app to disk. With --highlight or
                          --lint, draws neon-magenta outlines around the
                          chosen entities (or every lint finding). With
                          --bounds, also writes the entity bounds_map
                          JSON.
    lumenc build <app_dir> <out.lmna> [--no-hooks]
                          Ahead-of-time compile `<app_dir>` (parse
                          main.lmn + main.css once, run the cascade, bake
                          scripts) into a precompiled artifact. Run it with
                          `lumenc run <dir> --artifact <out.lmna>`; a runtime
                          built with `--no-default-features` (no parser)
                          loads only this. Mirrors Qt's `qmlcachegen`.
                          Runs `lumen.toml`'s `[[hooks]]` `prebuild` entries
                          first; --no-hooks skips them.
    lumenc run <dir> --artifact <file> [--headless] [--ticks N]
                          Run from a precompiled artifact instead of source.
    lumenc bundle <app_dir> <out.lpak> [--no-hooks]
                          Pack `<app_dir>` (main.lmn / main.css /
                          main.rhai / images / fonts) into a single
                          `.lpak` archive. Mirrors GTK's
                          `glib-compile-resources` + Qt's `rcc`.
                          Runs `lumen.toml`'s `[[hooks]]` `prebuild` entries
                          first; --no-hooks skips them.
    lumenc bundle --static <app_dir> <out_dir> [--no-hooks]
                          Build a per-app trimmed static runtime seam:
                          resolve the app's `[capabilities]` (lumen.toml +
                          source scan), map to a cargo `--features` set, and
                          build `lumen-ffi` with only those subsystems. The
                          shared cdylib / dev path stay full-featured.
    lumenc i18n extract <app_dir> [--lang en-US]
                          Scan `.lmn` + `.rhai` files for
                          `t!(\"key\", ...)` / `lumen.tr(\"key\", ...)` /
                          `translatable=\"key\"` and write / merge
                          `<app_dir>/locale/<lang>.ftl`. Idempotent:
                          existing entries are preserved; new keys
                          are appended with placeholder values.
    lumenc --help         Show this help
    lumenc --version      Print version

A markup app directory must contain `main.lmn`; `main.css` is optional and
the `<script>` tag inside `main.lmn` loads into the Rhai host. `run` and
`build` auto-detect SDK-authored apps (Rust: `Cargo.toml` depending on
`lumen`; C++: `CMakeLists.txt`; Python: a `.py` importing `lumen`) and
reroute to their native toolchain (`cargo` / `cmake` / the interpreter).
Set `[app] kind = \"markup\"|\"rust\"|\"cpp\"|\"python\"` in `lumen.toml`
to override detection.

An installed lumenc looks for a newer release once a day and prints one line
when it finds one. Set LUMEN_NO_UPDATE_CHECK to turn that off; a pinned
install (`install.sh --version`) is never checked.";
