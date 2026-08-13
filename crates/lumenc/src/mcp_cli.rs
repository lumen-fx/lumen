//! Thin CLI wrappers over the `lumen-mcp` JSON-RPC TCP server.
//!
//! One-shot queries that an AI agent (or human) can pipe to `jq`, `grep`, or
//! `head` without spinning up the full MCP/stdio bridge. Each subcommand
//! opens a TCP connection, sends a single newline-delimited JSON-RPC 2.0
//! request, reads one line back, prints the result, and exits.
//!
//! Port resolution order: `--port N` flag -> `LUMEN_MCP_PORT` env ->
//! `lumen.toml [mcp].port` under `--app <dir>` -> built-in default `7878`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

const DEFAULT_PORT: u16 = 7878;
const CONNECT_TIMEOUT_MS: u64 = 1_000;
const READ_TIMEOUT_MS: u64 = 5_000;

/// Usage block for `lumenc screenshot --help`.
const SCREENSHOT_USAGE: &str = "lumenc screenshot - capture the running app to a PNG

USAGE:
    lumenc screenshot [out.png] [--highlight id1,id2,...] [--lint]
                      [--bounds map.json] [--port P] [--app D]

Writes the PNG to disk (default lumen-screenshot.png) so the bytes never
enter an agent's context window.

    --highlight IDS   Draw neon-magenta outlines around these entity ids.
    --lint            Outline every lint finding instead.
    --bounds FILE     Also write the entity bounds_map as JSON.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";

/// `lumenc screenshot [out.png] [--highlight ids] [--lint] [--bounds map.json]`:
/// capture a PNG, optionally with a neon-marker overlay around the listed
/// entities (or every lint finding). Writes the PNG to disk so the bytes
/// never enter the agent's context window.
pub fn cmd_screenshot(args: impl Iterator<Item = String>) -> ExitCode {
    let mut out_path = PathBuf::from("lumen-screenshot.png");
    let mut bounds_path: Option<PathBuf> = None;
    let mut highlight_ids: Vec<u64> = Vec::new();
    let mut highlight_lint = false;
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut positional_seen = false;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{SCREENSHOT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--highlight" => match args.next() {
                Some(list) => {
                    for tok in list.split(',') {
                        match tok.trim().parse::<u64>() {
                            Ok(id) => highlight_ids.push(id),
                            Err(_) => {
                                return usage_err(&format!(
                                    "lumenc screenshot: --highlight: '{tok}' is not a u64"
                                ));
                            }
                        }
                    }
                }
                None => return usage_err("lumenc screenshot: --highlight needs id1,id2,..."),
            },
            "--lint" => highlight_lint = true,
            "--bounds" => match args.next() {
                Some(p) => bounds_path = Some(PathBuf::from(p)),
                None => return usage_err("lumenc screenshot: --bounds needs a path"),
            },
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc screenshot: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc screenshot: --app needs a directory"),
            },
            s if !s.starts_with("--") && !positional_seen => {
                positional_seen = true;
                out_path = PathBuf::from(s);
            }
            other => return usage_err(&format!("lumenc screenshot: unknown flag '{other}'")),
        }
    }

    let mut params = serde_json::Map::new();
    if !highlight_ids.is_empty() {
        params.insert("highlight_ids".into(), json!(highlight_ids));
    }
    if highlight_lint {
        params.insert("highlight_lint".into(), json!(true));
    }
    if bounds_path.is_some() {
        params.insert("include_bounds_map".into(), json!(true));
    }

    let port = resolve_port(port, app_dir.as_deref());
    let result = match call(port, "lumen.screenshot", Value::Object(params)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lumenc screenshot: {e}");
            return ExitCode::FAILURE;
        }
    };
    if result.get("available").and_then(|v| v.as_bool()) != Some(true) {
        let reason = result
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unavailable");
        eprintln!("lumenc screenshot: {reason}");
        return ExitCode::FAILURE;
    }
    let Some(b64) = result.get("png_base64").and_then(|v| v.as_str()) else {
        eprintln!("lumenc screenshot: missing png_base64 in response");
        return ExitCode::FAILURE;
    };
    use base64::Engine as _;
    let png = match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("lumenc screenshot: decode: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out_path, &png) {
        eprintln!("lumenc screenshot: write {}: {e}", out_path.display());
        return ExitCode::FAILURE;
    }
    let w = result.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
    let h = result.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
    let highlighted = result
        .get("highlighted")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    println!(
        "wrote {} ({}x{}{})",
        out_path.display(),
        w,
        h,
        if highlighted > 0 {
            format!(", {highlighted} highlighted")
        } else {
            String::new()
        }
    );
    if let Some(p) = bounds_path
        && let Some(map) = result.get("bounds_map")
    {
        let body = serde_json::to_string_pretty(map).unwrap_or_else(|_| "[]".into());
        if let Err(e) = std::fs::write(&p, body) {
            eprintln!("lumenc screenshot: write {}: {e}", p.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", p.display());
    }
    ExitCode::SUCCESS
}

/// `lumenc lint [--port P] [--app D] [--json] [--css-cascade [<dir>]]` -
/// surface snapshot-only findings (default) or, with `--css-cascade`, run
/// an offline static analysis that flags rules whose resolved value flips
/// between the old first-wins cascade and the new CSS Cascade-5 last-wins
/// cascade. The static mode reads `<dir>/main.css` directly - no running
/// MCP server required.
pub fn cmd_lint(args: impl Iterator<Item = String>) -> ExitCode {
    const LINT_USAGE: &str = "lumenc lint - lint the running app, or lint sources offline

USAGE:
    lumenc lint [--json] [--port P] [--app D]
    lumenc lint --css-cascade [<dir>] [--json]
    lumenc lint --signals [<app-dir>] [--json] [--strict]

With no mode flag, runs a snapshot-only lint pass against the running app
and prints one finding per line. Exits non-zero if any finding is an
error.

    --css-cascade     Offline static check that flags every rule whose
                      resolved value flips between the old first-wins
                      ordering and CSS Cascade-5 last-wins ordering.
    --signals         Offline signal lint over the app's markup, script,
                      and [signals] schema.
    --strict          Upgrade warnings to errors (--signals).
    --json            One JSON object per finding.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut as_json = false;
    let mut strict = false;
    let mut css_cascade_dir: Option<Option<PathBuf>> = None;
    let mut signals_dir: Option<Option<PathBuf>> = None;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{LINT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc lint: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc lint: --app needs a directory"),
            },
            "--json" => as_json = true,
            "--strict" => strict = true,
            "--css-cascade" => {
                // Accept an optional positional `<dir>` immediately
                // after the flag so callers can write either
                //   `lumenc lint --css-cascade apps/widget-garden`
                // or
                //   `lumenc lint --css-cascade --app apps/widget-garden`.
                let next_dir = match args.peek() {
                    Some(s) if !s.starts_with("--") => {
                        let s = args.next().unwrap();
                        Some(PathBuf::from(s))
                    }
                    _ => None,
                };
                css_cascade_dir = Some(next_dir);
            }
            "--signals" => {
                // Mirror the `--css-cascade [<dir>]` shape: accept an
                // optional positional `<app-dir>` right after the flag.
                let next_dir = match args.peek() {
                    Some(s) if !s.starts_with("--") => {
                        let s = args.next().unwrap();
                        Some(PathBuf::from(s))
                    }
                    _ => None,
                };
                signals_dir = Some(next_dir);
            }
            other => return usage_err(&format!("lumenc lint: unknown flag '{other}'")),
        }
    }
    if let Some(dir_opt) = signals_dir {
        let dir = dir_opt
            .or(app_dir.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        return run_signals_lint(&dir, as_json, strict);
    }
    if let Some(dir_opt) = css_cascade_dir {
        let dir = dir_opt
            .or(app_dir.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        return run_css_cascade_lint(&dir, as_json);
    }
    let port = resolve_port(port, app_dir.as_deref());
    match call(port, "lumen.lint", json!({})) {
        Ok(result) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into())
                );
                return if result.get("errors").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                };
            }
            if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
                println!("# {summary}");
            }
            let mut had_error = false;
            if let Some(findings) = result.get("findings").and_then(|v| v.as_array()) {
                for f in findings {
                    let category = f.get("category").and_then(|v| v.as_str()).unwrap_or("?");
                    let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("warn");
                    let hint = f.get("fix_hint").and_then(|v| v.as_str()).unwrap_or("");
                    let entity = f
                        .get("entity")
                        .and_then(|v| v.as_u64())
                        .map(|id| format!("e{id} "))
                        .unwrap_or_default();
                    println!("{severity:<7} {entity}{category}: {hint}");
                    if severity == "error" {
                        had_error = true;
                    }
                }
            }
            if had_error {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("lumenc lint: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Static `--css-cascade` lint: parse `<dir>/main.css`, walk every
/// (selector, property) pair, and emit a finding wherever the legacy
/// first-wins ordering would resolve to a different value than the
/// new CSS Cascade-5 last-wins ordering. Exits non-zero when any
/// divergence is found (CI gate).
fn run_css_cascade_lint(dir: &std::path::Path, as_json: bool) -> ExitCode {
    let css_path = dir.join("main.css");
    let src = match std::fs::read_to_string(&css_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No main.css -> no divergence, no findings.
            if as_json {
                println!("{{\"findings\":[]}}");
            } else {
                println!("# {}: no main.css - nothing to lint", dir.display());
            }
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!(
                "lumenc lint --css-cascade: read {}: {e}",
                css_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let sheet = match crate::parser_css::parse_css(&src) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "lumenc lint --css-cascade: parse {}: {e}",
                css_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let findings = crate::parser_css::cascade_lint(&sheet);
    if as_json {
        let arr: Vec<_> = findings
            .iter()
            .map(|d| {
                json!({
                    "selector": d.selector,
                    "property": d.property,
                    "first_wins": d.first_wins,
                    "last_wins": d.last_wins,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "css_path": css_path.display().to_string(),
                "findings": arr,
            }))
            .unwrap_or_else(|_| "null".into())
        );
    } else {
        println!("# {}: {} divergence(s)", css_path.display(), findings.len());
        for d in &findings {
            println!(
                "warn   {} :: {} - first-wins='{}' vs last-wins='{}'",
                d.selector, d.property, d.first_wins, d.last_wins
            );
        }
    }
    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Static `--signals` lint - defer to [`crate::lint_signals_cli`].
/// Builds the argv shape that module expects so the existing
/// `cmd_lint_signals` entry point handles parsing, schema load,
/// scan, and emit.
fn run_signals_lint(dir: &std::path::Path, as_json: bool, strict: bool) -> ExitCode {
    let mut argv: Vec<String> = vec![dir.display().to_string()];
    if as_json {
        argv.push("--json".to_string());
    }
    if strict {
        argv.push("--strict".to_string());
    }
    crate::lint_signals_cli::cmd_lint_signals(argv.into_iter())
}

/// `lumenc diff [tick] [--port P] [--app D] [--json]` - show what changed
/// since the given tick (or previous tick if omitted).
pub fn cmd_diff(args: impl Iterator<Item = String>) -> ExitCode {
    const DIFF_USAGE: &str = "lumenc diff - show what changed in the running app

USAGE:
    lumenc diff [tick] [--json] [--port P] [--app D]

Prints the entity ids added, removed, and changed since `tick`, or since
the previous tick when it is omitted.

    --json            Print the raw JSON-RPC result.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";
    let mut tick: Option<u64> = None;
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut as_json = false;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{DIFF_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc diff: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc diff: --app needs a directory"),
            },
            "--json" => as_json = true,
            s if !s.starts_with("--") => match s.parse() {
                Ok(n) => tick = Some(n),
                Err(_) => return usage_err("lumenc diff: tick must be an integer"),
            },
            other => return usage_err(&format!("lumenc diff: unknown flag '{other}'")),
        }
    }
    let mut params = serde_json::Map::new();
    if let Some(t) = tick {
        params.insert("tick".into(), json!(t));
    }
    let port = resolve_port(port, app_dir.as_deref());
    match call(port, "lumen.diff_since", Value::Object(params)) {
        Ok(result) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into())
                );
                return ExitCode::SUCCESS;
            }
            if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
                println!("# {summary}");
            }
            let print_list = |label: &str, sign: char, key: &str| {
                if let Some(arr) = result.get(key).and_then(|v| v.as_array()) {
                    if arr.is_empty() {
                        return;
                    }
                    println!("{label}:");
                    for v in arr {
                        if let Some(id) = v.as_u64() {
                            println!("  {sign} {id}");
                        }
                    }
                }
            };
            print_list("added", '+', "added");
            print_list("removed", '-', "removed");
            print_list("changed", '~', "changed");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc diff: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lumenc click <x> <y> [--button B] [--wait-for R] [--port P] [--app D]` -
/// inject a click via lumen.simulate.
pub fn cmd_click(args: impl Iterator<Item = String>) -> ExitCode {
    let (pos, opts) = match parse_simulate_args(args, &["click"]) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let [xs, ys]: [&String; 2] = match pos.as_slice() {
        [x, y] => [x, y],
        _ => return usage_err("lumenc click: expected <x> <y>"),
    };
    let x: f32 = match xs.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc click: x must be a number"),
    };
    let y: f32 = match ys.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc click: y must be a number"),
    };
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), json!("click"));
    params.insert("x".into(), json!(x));
    params.insert("y".into(), json!(y));
    if let Some(b) = &opts.button {
        params.insert("button".into(), json!(b));
    }
    if let Some(w) = &opts.wait_for {
        params.insert("wait_for".into(), json!(w));
    }
    run_simulate(
        opts.port,
        opts.app_dir.as_deref(),
        Value::Object(params),
        opts.as_json,
    )
}

/// `lumenc type <text> [--wait-for R] [--port P] [--app D]` - inject text.
pub fn cmd_type(args: impl Iterator<Item = String>) -> ExitCode {
    let (pos, opts) = match parse_simulate_args(args, &["type"]) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let text = match pos.as_slice() {
        [t] => t.clone(),
        _ => return usage_err("lumenc type: expected <text>"),
    };
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), json!("type"));
    params.insert("text".into(), json!(text));
    if let Some(w) = &opts.wait_for {
        params.insert("wait_for".into(), json!(w));
    }
    run_simulate(
        opts.port,
        opts.app_dir.as_deref(),
        Value::Object(params),
        opts.as_json,
    )
}

/// `lumenc key <name> [--shift] [--ctrl] [--alt] [--super] [--wait-for R]`:
/// inject a single key press.
pub fn cmd_key(args: impl Iterator<Item = String>) -> ExitCode {
    let (pos, opts) = match parse_simulate_args(args, &["key"]) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let key = match pos.as_slice() {
        [k] => k.clone(),
        _ => return usage_err("lumenc key: expected <key>"),
    };
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), json!("key"));
    params.insert("key".into(), json!(key));
    let modifiers = json!({
        "shift": opts.shift,
        "ctrl": opts.ctrl,
        "alt": opts.alt,
        "super": opts.super_,
    });
    params.insert("modifiers".into(), modifiers);
    if let Some(w) = &opts.wait_for {
        params.insert("wait_for".into(), json!(w));
    }
    run_simulate(
        opts.port,
        opts.app_dir.as_deref(),
        Value::Object(params),
        opts.as_json,
    )
}

/// `lumenc scroll <x> <y> <dx> <dy>` - inject a wheel event.
pub fn cmd_scroll(args: impl Iterator<Item = String>) -> ExitCode {
    let (pos, opts) = match parse_simulate_args(args, &["scroll"]) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let [xs, ys, dxs, dys]: [&String; 4] = match pos.as_slice() {
        [a, b, c, d] => [a, b, c, d],
        _ => return usage_err("lumenc scroll: expected <x> <y> <dx> <dy>"),
    };
    let x: f32 = match xs.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc scroll: x must be a number"),
    };
    let y: f32 = match ys.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc scroll: y must be a number"),
    };
    let dx: f32 = match dxs.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc scroll: dx must be a number"),
    };
    let dy: f32 = match dys.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc scroll: dy must be a number"),
    };
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), json!("scroll"));
    params.insert("x".into(), json!(x));
    params.insert("y".into(), json!(y));
    params.insert("dx".into(), json!(dx));
    params.insert("dy".into(), json!(dy));
    if let Some(w) = &opts.wait_for {
        params.insert("wait_for".into(), json!(w));
    }
    run_simulate(
        opts.port,
        opts.app_dir.as_deref(),
        Value::Object(params),
        opts.as_json,
    )
}

#[derive(Default)]
struct SimulateOpts {
    port: Option<u16>,
    app_dir: Option<PathBuf>,
    button: Option<String>,
    wait_for: Option<String>,
    shift: bool,
    ctrl: bool,
    alt: bool,
    super_: bool,
    as_json: bool,
}

/// Usage block for one of the four `lumen.simulate` subcommands. They share an
/// argument parser, so they share the shape of their help: the per-command
/// synopsis first, then the flags every one of them accepts.
fn simulate_usage(verb: &str) -> String {
    let head = match verb {
        "click" => {
            "lumenc click - inject a click\n\nUSAGE:\n    lumenc click <x> <y> \
             [--button primary|secondary|middle] [--wait-for R]\n\nCoordinates are \
             logical pixels."
        }
        "type" => {
            "lumenc type - type a string into the focused element\n\nUSAGE:\n    \
             lumenc type <text> [--wait-for R]"
        }
        "key" => {
            "lumenc key - inject one key press\n\nUSAGE:\n    lumenc key <name> \
             [--shift] [--ctrl] [--alt] [--super] [--wait-for R]\n\n<name> is a key \
             name (Enter | Tab | Escape | a | ...). --cmd is an alias for --super."
        }
        _ => {
            "lumenc scroll - inject a wheel event\n\nUSAGE:\n    lumenc scroll <x> <y> \
             <dx> <dy> [--wait-for R]\n\nScrolls by (dx, dy) pixels at logical point \
             (x, y)."
        }
    };
    format!(
        "{head}\n\nRequires [mcp] simulate = true in the running app's lumen.toml.\n\n    \
         --wait-for RING   Block until the named event ring records a new entry.\n    \
         --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,\n                      \
         lumen.toml [mcp] port (with --app), then 7878.\n    \
         --app DIR         App directory to read [mcp] port from.\n    \
         --json            Print the raw JSON-RPC result."
    )
}

fn parse_simulate_args(
    args: impl Iterator<Item = String>,
    verb: &[&str],
) -> Result<(Vec<String>, SimulateOpts), ExitCode> {
    let mut pos: Vec<String> = Vec::new();
    let mut opts = SimulateOpts::default();
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{}", simulate_usage(verb[0]));
                return Err(ExitCode::SUCCESS);
            }
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => opts.port = Some(n),
                None => {
                    return Err(usage_err(&format!(
                        "lumenc {}: --port needs a u16",
                        verb[0]
                    )));
                }
            },
            "--app" => match args.next() {
                Some(d) => opts.app_dir = Some(PathBuf::from(d)),
                None => {
                    return Err(usage_err(&format!(
                        "lumenc {}: --app needs a directory",
                        verb[0]
                    )));
                }
            },
            "--button" => match args.next() {
                Some(b) => opts.button = Some(b),
                None => {
                    return Err(usage_err(&format!(
                        "lumenc {}: --button needs a value",
                        verb[0]
                    )));
                }
            },
            "--wait-for" => match args.next() {
                Some(w) => opts.wait_for = Some(w),
                None => {
                    return Err(usage_err(&format!(
                        "lumenc {}: --wait-for needs a ring name",
                        verb[0]
                    )));
                }
            },
            "--shift" => opts.shift = true,
            "--ctrl" => opts.ctrl = true,
            "--alt" => opts.alt = true,
            "--super" | "--cmd" => opts.super_ = true,
            "--json" => opts.as_json = true,
            s if !s.starts_with("--") => pos.push(s.to_string()),
            other => {
                return Err(usage_err(&format!(
                    "lumenc {}: unknown flag '{other}'",
                    verb[0]
                )));
            }
        }
    }
    Ok((pos, opts))
}

fn run_simulate(
    port: Option<u16>,
    app_dir: Option<&std::path::Path>,
    params: Value,
    as_json: bool,
) -> ExitCode {
    let port = resolve_port(port, app_dir);
    match call(port, "lumen.simulate", params) {
        Ok(result) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into())
                );
                return ExitCode::SUCCESS;
            }
            if result.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                eprintln!(
                    "{}",
                    result
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("input simulation disabled")
                );
                if let Some(hint) = result.get("hint").and_then(|v| v.as_str()) {
                    eprintln!("hint: {hint}");
                }
                return ExitCode::FAILURE;
            }
            if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
                println!("{summary}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc simulate: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lumenc find` - selector-style search by text / role / id.
pub fn cmd_find(args: impl Iterator<Item = String>) -> ExitCode {
    let mut by_text: Option<String> = None;
    let mut by_role: Option<String> = None;
    let mut by_id: Option<u64> = None;
    let mut limit: Option<usize> = None;
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut as_json = false;
    const FIND_USAGE: &str = "lumenc find - selector search over the live snapshot

USAGE:
    lumenc find [--text S] [--role R] [--id N] [--limit N] [--json]
                [--port P] [--app D]

Prints one row per hit (id role label bounds state). Exits non-zero when
nothing matches.

    --text S          Match elements whose label contains S.
    --role R          Match elements with this a11y role.
    --id N            Match one entity id.
    --limit N         Stop after N hits.
    --json            Print the raw JSON-RPC result.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{FIND_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--text" => match args.next() {
                Some(v) => by_text = Some(v),
                None => return usage_err("lumenc find: --text needs a string"),
            },
            "--role" => match args.next() {
                Some(v) => by_role = Some(v),
                None => return usage_err("lumenc find: --role needs a string"),
            },
            "--id" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => by_id = Some(n),
                None => return usage_err("lumenc find: --id needs an integer"),
            },
            "--limit" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => limit = Some(n),
                None => return usage_err("lumenc find: --limit needs an integer"),
            },
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc find: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc find: --app needs a directory"),
            },
            "--json" => as_json = true,
            other => return usage_err(&format!("lumenc find: unknown flag '{other}'")),
        }
    }
    let mut params = serde_json::Map::new();
    if let Some(t) = by_text {
        params.insert("by_text".into(), json!(t));
    }
    if let Some(r) = by_role {
        params.insert("by_role".into(), json!(r));
    }
    if let Some(i) = by_id {
        params.insert("by_id".into(), json!(i));
    }
    if let Some(l) = limit {
        params.insert("limit".into(), json!(l));
    }

    let port = resolve_port(port, app_dir.as_deref());
    match call(port, "lumen.find", Value::Object(params)) {
        Ok(result) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into())
                );
                return ExitCode::SUCCESS;
            }
            let results = result.get("results").and_then(|v| v.as_array());
            let count = results.map(|r| r.len()).unwrap_or(0);
            if let Some(rows) = results {
                for row in rows {
                    print_summary_row(row);
                }
            }
            if count == 0 {
                eprintln!("no matches");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc find: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lumenc element-at <x> <y>` - topmost-hit lookup.
pub fn cmd_element_at(args: impl Iterator<Item = String>) -> ExitCode {
    let mut positional: Vec<String> = Vec::new();
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut as_json = false;
    const ELEMENT_AT_USAGE: &str = "lumenc element-at - topmost element at a point

USAGE:
    lumenc element-at <x> <y> [--json] [--port P] [--app D]

Coordinates are logical pixels. Exits non-zero when nothing is there.

    --json            Print the raw JSON-RPC result.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{ELEMENT_AT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc element-at: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc element-at: --app needs a directory"),
            },
            "--json" => as_json = true,
            s if !s.starts_with("--") => positional.push(s.to_string()),
            other => return usage_err(&format!("lumenc element-at: unknown flag '{other}'")),
        }
    }
    let [xs, ys] = match positional.as_slice() {
        [x, y] => [x.clone(), y.clone()],
        _ => return usage_err("lumenc element-at: expected <x> <y>"),
    };
    let x: f32 = match xs.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc element-at: x must be a number"),
    };
    let y: f32 = match ys.parse() {
        Ok(v) => v,
        Err(_) => return usage_err("lumenc element-at: y must be a number"),
    };

    let port = resolve_port(port, app_dir.as_deref());
    match call(port, "lumen.element_at", json!({"x": x, "y": y})) {
        Ok(result) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into())
                );
                return ExitCode::SUCCESS;
            }
            if result.get("hit").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(elem) = result.get("element") {
                    print_summary_row(elem);
                }
                ExitCode::SUCCESS
            } else {
                eprintln!("no entity at ({x}, {y})");
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("lumenc element-at: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_summary_row(row: &Value) {
    let id = row.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("?");
    let label = row.get("label").and_then(|v| v.as_str()).unwrap_or("");
    let x = row.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = row.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let w = row.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let h = row.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let state = row.get("state").and_then(|v| v.as_str()).unwrap_or("-");
    let quoted = if label.is_empty() {
        String::new()
    } else {
        format!("\"{label}\"")
    };
    println!("{id:>10}  {role:<11} {quoted:<32} {x:>5.0},{y:<5.0} {w:>4.0}x{h:<4.0} {state}");
}

/// `lumenc snapshot` - compact a11y-tree text dump.
pub fn cmd_snapshot(args: impl Iterator<Item = String>) -> ExitCode {
    let mut max_lines: Option<usize> = None;
    let mut cursor: Option<u64> = None;
    let mut omit_invisible: Option<bool> = None;
    let mut port: Option<u16> = None;
    let mut app_dir: Option<PathBuf> = None;
    let mut output = OutputMode::Text;
    const SNAPSHOT_USAGE: &str = "lumenc snapshot - a11y-tree text dump of the running app

USAGE:
    lumenc snapshot [--text|--json] [--max-lines N] [--cursor C]
                    [--include-invisible] [--port P] [--app D]

    --text            Indented text tree (default).
    --json            Print the raw JSON-RPC result.
    --max-lines N     Stop after N lines and report a resume cursor.
    --cursor C        Resume a truncated dump at cursor C.
    --include-invisible
                      Include elements the app is not painting.
                      --no-omit-invisible is an alias.
    --port P          MCP port. Resolution order: --port, LUMEN_MCP_PORT,
                      lumen.toml [mcp] port (with --app), then 7878.
    --app DIR         App directory to read [mcp] port from.";
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{SNAPSHOT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--text" => output = OutputMode::Text,
            "--json" => output = OutputMode::Json,
            "--max-lines" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => max_lines = Some(n),
                None => return usage_err("lumenc snapshot: --max-lines needs an integer"),
            },
            "--cursor" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => cursor = Some(n),
                None => return usage_err("lumenc snapshot: --cursor needs an integer"),
            },
            "--no-omit-invisible" => omit_invisible = Some(false),
            "--include-invisible" => omit_invisible = Some(false),
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => port = Some(n),
                None => return usage_err("lumenc snapshot: --port needs a u16"),
            },
            "--app" => match args.next() {
                Some(d) => app_dir = Some(PathBuf::from(d)),
                None => return usage_err("lumenc snapshot: --app needs a directory"),
            },
            other => return usage_err(&format!("lumenc snapshot: unknown flag '{other}'")),
        }
    }
    let port = resolve_port(port, app_dir.as_deref());
    let params = build_params(max_lines, cursor, omit_invisible);

    match call(port, "lumen.snapshot_text", params) {
        Ok(result) => match output {
            OutputMode::Json => {
                let body = serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into());
                println!("{body}");
                ExitCode::SUCCESS
            }
            OutputMode::Text => {
                print_text_snapshot(&result);
                ExitCode::SUCCESS
            }
        },
        Err(e) => {
            eprintln!("lumenc snapshot: {e}");
            ExitCode::FAILURE
        }
    }
}

enum OutputMode {
    Text,
    Json,
}

fn build_params(
    max_lines: Option<usize>,
    cursor: Option<u64>,
    omit_invisible: Option<bool>,
) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(n) = max_lines {
        m.insert("max_lines".into(), json!(n));
    }
    if let Some(c) = cursor {
        m.insert("cursor".into(), json!(c));
    }
    if let Some(b) = omit_invisible {
        m.insert("omit_invisible".into(), json!(b));
    }
    Value::Object(m)
}

fn print_text_snapshot(result: &Value) {
    if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
        println!("# {summary}");
    }
    if let Some(lines) = result.get("lines").and_then(|v| v.as_array()) {
        for line in lines {
            if let Some(s) = line.as_str() {
                println!("{s}");
            }
        }
    }
    if result.get("truncated").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(c) = result.get("next_cursor").and_then(|v| v.as_u64()) {
            println!("# truncated - resume with --cursor {c}");
        } else {
            println!("# truncated");
        }
    }
}

fn resolve_port(arg_port: Option<u16>, app_dir: Option<&std::path::Path>) -> u16 {
    if let Some(p) = arg_port {
        return p;
    }
    if let Ok(s) = std::env::var("LUMEN_MCP_PORT")
        && let Ok(n) = s.parse::<u16>()
    {
        return n;
    }
    if let Some(dir) = app_dir
        && let Ok(cfg) = crate::config::LumenToml::load_or_default(dir)
        && let Some(p) = cfg.mcp.port
    {
        return p;
    }
    DEFAULT_PORT
}

/// One-shot JSON-RPC call. Connects, writes one line, reads one line, drops.
fn call(port: u16, method: &str, params: Value) -> Result<Value, String> {
    let addr = format!("127.0.0.1:{port}");
    let stream = TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|e| format!("parse address {addr}: {e}"))?,
        Duration::from_millis(CONNECT_TIMEOUT_MS),
    )
    .map_err(|e| format!("connect {addr}: {e} (is `lumenc run` running with MCP enabled?)"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))
        .map_err(|e| format!("set timeout: {e}"))?;

    let req = json!({
        "jsonrpc": "2.0",
        "method": method,
        "id": 1,
        "params": params,
    });
    let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;

    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("clone stream: {e}"))?;
    writer
        .write_all(line.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    writer.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    writer.flush().map_err(|e| format!("flush: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader
        .read_line(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    let response: Value =
        serde_json::from_str(buf.trim()).map_err(|e| format!("parse response: {e}"))?;
    if let Some(err) = response.get("error") {
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        return Err(format!("RPC error {code}: {msg}"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(2)
}
