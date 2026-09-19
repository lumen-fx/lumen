//! Tokio TCP / stdio server speaking the Model Context Protocol over
//! JSON-RPC 2.0.
//!
//! Two transports:
//!
//! - **TCP** (default, port 7878): accepts connections on `127.0.0.1` and
//!   speaks newline-delimited JSON-RPC 2.0 - one request per line, one
//!   reply per line. This is what the `lumen-mcp-server` stdio bridge and
//!   the `lumenc` CLI (`snapshot`, `screenshot`, `click`, `type`, `key`,
//!   `scroll`, ...) speak.
//!
//! - **stdio** - true MCP transport per the spec: newline-delimited
//!   JSON-RPC on stdin/stdout (Anthropic's stdio variant; the spec
//!   defines content-length framing for "stdio binary" but every
//!   reference client we've measured against accepts the simpler
//!   newline framing too). Used when the host opts into
//!   [`McpTransport::Stdio`] at plugin construction.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use lumen_core::render_world::SurfaceCapture;
use lumen_core::task::{BoxFuture, Timer};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::mcp_protocol::{error_response as mcp_error, handle_mcp};
use crate::simulate::{SimulateQueue, SimulateRequest};
use crate::snapshot::Snapshot;

/// Bundle of cross-thread handles the JSON-RPC handler needs.
///
/// Cheap to clone (a few `Arc`s). The snapshot lock is acquired for one read
/// per request; the surface capture flag is touched per `lumen.screenshot`
/// call and never holds the snapshot lock while waiting for the GPU.
#[derive(Clone)]
pub(crate) struct ServerCtx {
    pub snapshot: Arc<RwLock<Snapshot>>,
    pub surface_capture: Option<SurfaceCapture>,
    pub simulate_queue: SimulateQueue,
    pub simulate_enabled: bool,
    /// Gates `lumen_framework_status`'s issue lookup. Off by default: the
    /// introspection port has no authentication, so a shipped app leaves
    /// subprocess execution (`git`, `gh`) off that surface unless a
    /// developer opts in with `[mcp] issues = true`.
    pub issues_enabled: bool,
    /// What the request handlers wait on while they poll for a frame, a
    /// committed signal, or a finished tick. The app's `TimerService` when
    /// it installed one, [`TokioTimer`] otherwise.
    pub timer: Arc<dyn Timer>,
}

/// Timer of last resort: the one this crate's own runtime provides.
///
/// The server drives its transport on a tokio runtime it owns, so a tokio
/// sleep is always available even in an app that installed no async backend.
pub(crate) struct TokioTimer;

impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> BoxFuture<()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Block the current thread on a tokio current-thread runtime running the
/// TCP server forever. Called from inside the plugin's spawned OS thread.
///
/// The runtime is this crate's own: the transport is written against tokio's
/// networking, and the [`Spawn`](lumen_core::task::Spawn) seam carries no
/// promise that an installed executor drives a tokio reactor. Timing is a
/// different matter, and goes through [`ServerCtx::timer`].
pub fn serve_tcp(port: u16, ctx: ServerCtx) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!("lumen-mcp: failed to build tokio runtime: {e}");
            return;
        }
    };
    rt.block_on(run_tcp(port, ctx));
}

/// MCP-over-stdio. Reads newline-delimited JSON-RPC requests from
/// `stdin`, writes responses to `stdout`. Blocks until stdin closes.
/// Intended for tools that launch lumen as a subprocess and pipe MCP
/// over stdio (the canonical MCP transport per the spec).
pub fn serve_stdio(ctx: ServerCtx) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!("lumen-mcp: failed to build stdio tokio runtime: {e}");
            return;
        }
    };
    rt.block_on(run_stdio(ctx));
}

async fn run_stdio(ctx: ServerCtx) {
    use tokio::io::{AsyncBufReadExt, BufReader, stdin, stdout};
    let mut reader = BufReader::new(stdin());
    let mut writer = stdout();
    let mut line = String::new();
    info!("lumen-mcp: stdio transport ready");
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(e) => {
                warn!("lumen-mcp: stdio read error: {e}");
                return;
            }
        };
        if n == 0 {
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response_value = handle_request(line.trim(), &ctx).await;
        if response_value.is_null() {
            // Notification: no reply.
            continue;
        }
        let mut bytes = match serde_json::to_vec(&response_value) {
            Ok(b) => b,
            Err(e) => {
                warn!("lumen-mcp: stdio serialize error: {e}");
                continue;
            }
        };
        bytes.push(b'\n');
        if let Err(e) = writer.write_all(&bytes).await {
            warn!("lumen-mcp: stdio write error: {e}");
            return;
        }
        if let Err(e) = writer.flush().await {
            warn!("lumen-mcp: stdio flush error: {e}");
            return;
        }
    }
}

async fn run_tcp(port: u16, ctx: ServerCtx) {
    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("lumen-mcp: failed to bind {addr}: {e}");
            return;
        }
    };
    info!("lumen-mcp: listening on {addr}");

    loop {
        let (sock, _peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!("lumen-mcp: accept error: {e}");
                continue;
            }
        };
        let ctx = ctx.clone();
        tokio::spawn(handle_client(sock, ctx));
    }
}

async fn handle_client(stream: tokio::net::TcpStream, ctx: ServerCtx) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let first = match reader.read_line(&mut line).await {
        Ok(n) => n,
        Err(e) => {
            warn!("lumen-mcp: read error: {e}");
            return;
        }
    };
    if first == 0 {
        return;
    }
    // Process the first line, then loop on subsequent newline-delimited
    // JSON-RPC messages from the same connection.
    if let Err(e) = process_jsonrpc_line(&line, &ctx, &mut write_half).await {
        warn!("lumen-mcp: write error: {e}");
        return;
    }
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(e) => {
                warn!("lumen-mcp: read error: {e}");
                return;
            }
        };
        if n == 0 {
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = process_jsonrpc_line(&line, &ctx, &mut write_half).await {
            warn!("lumen-mcp: write error: {e}");
            return;
        }
    }
}

async fn process_jsonrpc_line(
    line: &str,
    ctx: &ServerCtx,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
) -> std::io::Result<()> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let response_value = handle_request(trimmed, ctx).await;
    // MCP notifications (e.g. `notifications/initialized`) carry no
    // `id` and expect no reply - `handle_request` returns `Value::Null`
    // for those. Skip writing so the line transport doesn't emit a
    // bogus `null\n` that some clients trip on.
    if response_value.is_null() {
        return Ok(());
    }
    let mut bytes = match serde_json::to_vec(&response_value) {
        Ok(b) => b,
        Err(e) => {
            warn!("lumen-mcp: serialize error: {e}");
            return Ok(());
        }
    };
    bytes.push(b'\n');
    write_half.write_all(&bytes).await
}

/// Top-level request handler. Accepts a single JSON-RPC envelope (as
/// a string slice) and returns the response value.
///
/// Routing:
/// 1. `lumen.screenshot` / `lumen.simulate` / `tools/call name=lumen_screenshot|lumen_simulate`
///    are intercepted here because they need transport-side resources
///    (surface capture flag, simulate queue) the MCP protocol module
///    doesn't see.
/// 2. Everything else delegates to [`handle_mcp`], which speaks native
///    MCP (`initialize`, `tools/list`, `tools/call`, ...) and falls back
///    to the legacy `lumen.*` dispatch for clients still on the
///    pre-W6.11 wire.
async fn handle_request(line: &str, ctx: &ServerCtx) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return mcp_error(Value::Null, -32700, &format!("parse error: {e}"));
        }
    };

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(|v| v.as_str())
        .map(String::from);
    let params = request.get("params").cloned();
    // JSON-RPC notifications have no `id`. We still always return a
    // response value on the wire for non-notification calls. For true
    // notifications (`id` absent), the caller drops the response
    // (`process_jsonrpc_line` / `run_stdio` skip writing on `Value::Null`).
    let is_notification = request.get("id").is_none();

    let Some(method) = method else {
        return mcp_error(id, -32600, "invalid request: missing method");
    };

    // Transport-side intercepts. These methods touch resources
    // (`SurfaceCapture`, `SimulateQueue`, an awaited `gh` subprocess) that
    // the protocol layer doesn't carry. Handled both via the legacy dotted
    // name and via `tools/call(name="lumen_...")`.
    let intercepted_tool = (method == "tools/call")
        .then(|| {
            params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .flatten();
    let intercepted_args = if intercepted_tool.is_some() {
        params.as_ref().and_then(|p| p.get("arguments")).cloned()
    } else {
        None
    };

    let (screenshot_args, simulate_args, set_signal_args, framework_status, wrap_in_tool_envelope) =
        match (method.as_str(), intercepted_tool.as_deref()) {
            ("lumen.screenshot", _) => (Some(params.clone()), None, None, false, false),
            ("lumen.simulate", _) => (None, Some(params.clone()), None, false, false),
            ("lumen.set_signal", _) => (None, None, Some(params.clone()), false, false),
            ("lumen.framework_status", _) => (None, None, None, true, false),
            ("tools/call", Some("lumen_screenshot")) => {
                (Some(intercepted_args.clone()), None, None, false, true)
            }
            ("tools/call", Some("lumen_simulate")) => {
                (None, Some(intercepted_args.clone()), None, false, true)
            }
            ("tools/call", Some("lumen_set_signal")) => {
                (None, None, Some(intercepted_args.clone()), false, true)
            }
            ("tools/call", Some("lumen_framework_status")) => (None, None, None, true, true),
            _ => (None, None, None, false, false),
        };

    if let Some(args) = screenshot_args {
        let result = run_screenshot(ctx, args.as_ref()).await;
        return wrap_or_raw(id, result, wrap_in_tool_envelope);
    }
    if let Some(args) = simulate_args {
        let result = run_simulate(ctx, args.as_ref()).await;
        return wrap_or_raw(id, result, wrap_in_tool_envelope);
    }
    if let Some(args) = set_signal_args {
        let result = run_set_signal(ctx, args.as_ref()).await;
        return wrap_or_raw(id, result, wrap_in_tool_envelope);
    }
    if framework_status {
        let result = run_framework_status(ctx).await;
        return wrap_or_raw(id, result, wrap_in_tool_envelope);
    }

    match handle_mcp(
        &method,
        id.clone(),
        params.as_ref(),
        &ctx.snapshot,
        is_notification,
    )
    .await
    {
        Some(resp) => resp,
        None => Value::Null,
    }
}

fn wrap_or_raw(id: Value, result: Value, wrap_in_tool_envelope: bool) -> Value {
    if wrap_in_tool_envelope {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [
                    { "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default() }
                ],
                "isError": false,
                "structuredContent": result,
            },
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })
    }
}

/// Try the live on-screen surface readback path; if no `SurfaceCapture`
/// resource is wired up, fall back to the headless framebuffer captured in
/// the snapshot.
///
/// Timing trade-off: we sleep up to ~500 ms in 16 ms slices waiting for the
/// render thread to fulfill the request. 16 ms ~ one 60 Hz frame; 500 ms is
/// generous enough that even a stalled-but-recovering app produces output,
/// short enough that a truly dead app fails the call instead of hanging an
/// MCP client.
///
/// Optional params (all default off):
/// * `highlight_ids: [u64]` - draw a bright neon outline around each id.
/// * `highlight_lint: bool` - draw an outline around every lint finding.
/// * `include_bounds_map: bool` - return an `[{id,x,y,w,h,role,label}]` map
///   so a downstream tool can lay its own overlay without re-querying.
async fn run_screenshot(ctx: &ServerCtx, params: Option<&Value>) -> Value {
    let opts = parse_screenshot_opts(params);

    // 1. Capture raw RGBA8 + dimensions, from the live surface or the
    //    headless fallback.
    let (mut rgba8, width, height, source) = match capture_raw(ctx).await {
        Some(t) => t,
        None => {
            return json!({
                "available": false,
                "reason": "no SurfaceCapture wired and no HeadlessRenderer present",
            });
        }
    };

    // 2. Compute bounds_map + highlight set if requested.
    let need_bounds =
        opts.include_bounds_map || !opts.highlight_ids.is_empty() || opts.highlight_lint;
    let (bounds_map, highlight_set) = if need_bounds {
        compute_bounds_and_highlights(ctx, &opts)
    } else {
        (Vec::new(), Vec::new())
    };

    // 3. Draw neon rectangles for each highlighted entity. Bright magenta
    //    with a thin white inner stroke - picked to pop over arbitrary UI
    //    palettes.
    if !highlight_set.is_empty() {
        for entry in &bounds_map {
            if !highlight_set.contains(&entry.id) {
                continue;
            }
            draw_neon_rect(
                &mut rgba8,
                width,
                height,
                entry.x as i32,
                entry.y as i32,
                entry.w as i32,
                entry.h as i32,
            );
        }
    }

    // 4. Re-encode + assemble response.
    let b64 = match encode_png_base64(width, height, &rgba8) {
        Some(b) => b,
        None => {
            return json!({
                "available": false,
                "reason": "PNG encoder failed",
            });
        }
    };
    let mut out = json!({
        "available": true,
        "png_base64": b64,
        "width": width,
        "height": height,
        "encoding": "base64-png",
        "source": source,
    });
    if opts.include_bounds_map {
        let m: Vec<Value> = bounds_map
            .iter()
            .map(|b| {
                json!({
                    "id": b.id,
                    "x": b.x,
                    "y": b.y,
                    "w": b.w,
                    "h": b.h,
                    "role": b.role,
                    "label": b.label,
                })
            })
            .collect();
        out["bounds_map"] = Value::Array(m);
    }
    if !highlight_set.is_empty() {
        out["highlighted"] = json!(highlight_set);
    }
    out
}

#[derive(Default)]
struct ScreenshotOpts {
    highlight_ids: Vec<u64>,
    highlight_lint: bool,
    include_bounds_map: bool,
}

fn parse_screenshot_opts(params: Option<&Value>) -> ScreenshotOpts {
    let Some(p) = params else {
        return ScreenshotOpts::default();
    };
    let highlight_ids = p
        .get("highlight_ids")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default();
    let highlight_lint = p
        .get("highlight_lint")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let include_bounds_map = p
        .get("include_bounds_map")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    ScreenshotOpts {
        highlight_ids,
        highlight_lint,
        include_bounds_map,
    }
}

struct BoundsEntry {
    id: u64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    role: &'static str,
    label: String,
}

/// Walk the snapshot, build `BoundsEntry` for every visible entity, and
/// resolve the highlight set (caller-supplied ids plus lint-finding ids when
/// `highlight_lint=true`). One snapshot read-lock; lint is computed inline.
fn compute_bounds_and_highlights(
    ctx: &ServerCtx,
    opts: &ScreenshotOpts,
) -> (Vec<BoundsEntry>, Vec<u64>) {
    let Ok(snap) = ctx.snapshot.read() else {
        return (Vec::new(), Vec::new());
    };
    let mut bounds: Vec<BoundsEntry> = Vec::with_capacity(snap.inspect.len());
    for inv in snap.inspect.values() {
        let Some(t) = inv.transform else {
            continue;
        };
        if t.size.x <= 0.0 || t.size.y <= 0.0 {
            continue;
        }
        bounds.push(BoundsEntry {
            id: inv.id,
            x: t.absolute.x,
            y: t.absolute.y,
            w: t.size.x,
            h: t.size.y,
            role: crate::methods::role_of(inv),
            label: crate::methods::label_of(inv),
        });
    }
    let mut highlights: Vec<u64> = opts.highlight_ids.clone();
    if opts.highlight_lint {
        let lint_value = crate::methods::method_lint(&snap);
        if let Some(findings) = lint_value.get("findings").and_then(|v| v.as_array()) {
            for f in findings {
                if let Some(id) = f.get("entity").and_then(|v| v.as_u64()) {
                    highlights.push(id);
                }
            }
        }
    }
    highlights.sort();
    highlights.dedup();
    (bounds, highlights)
}

/// Acquire raw RGBA8 + dimensions from the renderer through `SurfaceCapture`.
///
/// Asks for a fresh frame and waits for it. If the wait runs out (nothing in
/// the render world services capture requests, or the app is stalled) the
/// last frame the store holds is returned instead, tagged `surface-stale` so
/// a client can tell it is looking at an older frame.
async fn capture_raw(ctx: &ServerCtx) -> Option<(Vec<u8>, u32, u32, &'static str)> {
    let capture = ctx.surface_capture.clone()?;
    capture.request();
    for _ in 0..32 {
        if !capture.is_requested()
            && let Some(frame) = capture.read()
        {
            return Some((frame.rgba8, frame.width, frame.height, "surface"));
        }
        ctx.timer.sleep(Duration::from_millis(16)).await;
    }
    let frame = capture.read()?;
    Some((frame.rgba8, frame.width, frame.height, "surface-stale"))
}

fn encode_png_base64(width: u32, height: u32, rgba8: &[u8]) -> Option<String> {
    use base64::Engine as _;
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;
    let mut out: Vec<u8> = Vec::with_capacity(rgba8.len() / 2);
    let encoder = PngEncoder::new(&mut out);
    if encoder
        .write_image(rgba8, width, height, image::ExtendedColorType::Rgba8)
        .is_err()
    {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&out))
}

/// Stamp a bright magenta rectangle outline (3 px thick) with a 1 px white
/// inner stroke around `(x, y, w, h)` in `rgba8`. Used by the "neon marker"
/// overlay for screenshot debugging.
fn draw_neon_rect(rgba8: &mut [u8], width: u32, height: u32, x: i32, y: i32, w: i32, h: i32) {
    const NEON: [u8; 4] = [255, 0, 200, 255]; // magenta
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(width as i32);
    let y1 = (y + h).min(height as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // 3 px-thick magenta border, then a 1 px inner white highlight for
    // contrast against magenta UIs.
    for stroke in 0..3i32 {
        stroke_rect(
            rgba8,
            width,
            height,
            Rect {
                x0: x0 - stroke,
                y0: y0 - stroke,
                x1: x1 + stroke,
                y1: y1 + stroke,
            },
            NEON,
        );
    }
    stroke_rect(
        rgba8,
        width,
        height,
        Rect {
            x0: x0 + 1,
            y0: y0 + 1,
            x1: x1 - 1,
            y1: y1 - 1,
        },
        WHITE,
    );
}

#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

fn stroke_rect(rgba8: &mut [u8], width: u32, height: u32, r: Rect, color: [u8; 4]) {
    if r.x1 <= r.x0 || r.y1 <= r.y0 {
        return;
    }
    for x in r.x0..r.x1 {
        plot(rgba8, width, height, x, r.y0, color);
        plot(rgba8, width, height, x, r.y1 - 1, color);
    }
    for y in r.y0..r.y1 {
        plot(rgba8, width, height, r.x0, y, color);
        plot(rgba8, width, height, r.x1 - 1, y, color);
    }
}

fn plot(rgba8: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let idx = ((y as u32 * width + x as u32) * 4) as usize;
    if idx + 4 <= rgba8.len() {
        rgba8[idx..idx + 4].copy_from_slice(&color);
    }
}

/// `lumen_framework_status`: open-issue count for the repository this
/// checkout's `origin` git remote points at, plus the same tick-liveness
/// numbers every other tool reports.
///
/// Fetching issues means awaiting a `gh` subprocess with a network round
/// trip, which is why this is a transport-side intercept rather than a
/// `dispatch_with_ctx` case: that dispatch table is synchronous snapshot
/// reads only. A checkout with no network, no `gh`, or no GitHub `origin`
/// remote gets `issues_error` set instead of a hang or a silent zero.
///
/// Gated by `ctx.issues_enabled` (`[mcp] issues = true`, off by default):
/// the introspection port has no authentication, so spawning `git`/`gh`
/// from an unauthenticated caller is opt-in the same way `lumen.simulate`
/// gates input injection. Off, this never touches a subprocess.
async fn run_framework_status(ctx: &ServerCtx) -> Value {
    let (frame, last_tick_micros) = match ctx.snapshot.read() {
        Ok(snap) => (snap.frame, snap.last_tick_micros),
        Err(_) => (0, 0),
    };

    if !ctx.issues_enabled {
        return disabled_framework_status_json(frame, last_tick_micros);
    }

    let report = crate::issues::framework_issues_report(&crate::issues::GhConfig::default()).await;
    framework_status_json(&report, frame, last_tick_micros)
}

/// The `[mcp] issues` opt-in is off: never touches `git`/`gh`, and says how
/// to turn the lookup on.
fn disabled_framework_status_json(frame: u64, last_tick_micros: u64) -> Value {
    json!({
        "summary": format!("issue lookup disabled; last tick {last_tick_micros}us"),
        "enabled": false,
        "hint": "enable with [mcp] issues = true in lumen.toml (or LumenMcpPlugin::with_issues_enabled(true))",
        "repo": Value::Null,
        "open_issues": Value::Null,
        "truncated": false,
        "first_open_issues": Value::Array(Vec::new()),
        "issues_error": Value::Null,
        "last_tick_micros": last_tick_micros,
        "frame": frame,
        "next_suggested_tools": [
            { "name": "lumen_snapshot_text", "params": {}, "why": "verify a fix end-to-end" },
        ],
        "confidence": "high",
    })
}

/// Shape a resolved [`crate::issues::IssuesReport`] into the
/// `lumen_framework_status` response, alongside the tick-liveness numbers
/// every other tool reports. Pure and synchronous - every summary and
/// confidence branch is reachable with a hand-built `IssuesReport`, with no
/// process or network involved.
///
/// `report.error` alone decides which branch this is in: a resolution
/// failure carries no repo, a fetch failure carries a repo but no counts,
/// and success (by construction, see `crate::issues::report_from_fetch_result`)
/// always carries both a repo and a count when there is no error.
fn framework_status_json(
    report: &crate::issues::IssuesReport,
    frame: u64,
    last_tick_micros: u64,
) -> Value {
    let summary = match (&report.error, &report.repo) {
        (Some(err), Some(repo)) => {
            format!("could not list open issues on {repo}: {err}; last tick {last_tick_micros}us")
        }
        (Some(err), None) => {
            format!(
                "could not determine which repository to query: {err}; last tick {last_tick_micros}us"
            )
        }
        (None, _) => {
            let n = report.open_issues.unwrap_or(0);
            let repo = report.repo.as_deref().unwrap_or("?");
            if report.truncated {
                format!(
                    "{n}+ open issue(s) on {repo} (list truncated at {n}); last tick {last_tick_micros}us"
                )
            } else {
                format!("{n} open issue(s) on {repo}; last tick {last_tick_micros}us")
            }
        }
    };
    let confidence = match (&report.error, report.truncated) {
        (Some(_), _) => "low",
        (None, true) => "medium",
        (None, false) => "high",
    };

    json!({
        "summary": summary,
        "enabled": true,
        "repo": report.repo,
        "open_issues": report.open_issues,
        "truncated": report.truncated,
        "first_open_issues": report.first_open,
        "issues_error": report.error,
        "last_tick_micros": last_tick_micros,
        "frame": frame,
        "next_suggested_tools": [
            { "name": "lumen_snapshot_text", "params": {}, "why": "verify a fix end-to-end" },
        ],
        "confidence": confidence,
    })
}

/// Write one global signal through the external typed-property bus - the
/// SAME ingress `lumen_core::signals::Signals::set` mirrors its writes
/// through (`push_external_property` -> `drain_external_properties` /
/// `commit_external_properties` on the next tick), so ordering semantics
/// against script writes hold: the write commits at a tick boundary, never
/// mid-schedule.
///
/// Params: `{ name: string, value: string | number | bool }`. All values
/// are written as the canonical `Str` variant `Signals::set` produces
/// (`true`/`false` for bools, decimal repr for numbers), which keeps
/// `bind-text` / `<if>` comparators working unchanged.
///
/// After the push, the simulate queue's event-loop waker (wired
/// unconditionally by the plugin) nudges a parked loop so the write is
/// consumed within a frame instead of at the next incidental OS event.
/// The handler then polls the snapshot for up to ~500 ms to report
/// whether the write's tick ran (headless apps snapshot per-tick; a
/// windowed app's 1 Hz snapshot throttle can time this confirmation out
/// even though the write itself landed - `committed: false` therefore
/// means "unconfirmed", not "failed").
async fn run_set_signal(ctx: &ServerCtx, params: Option<&Value>) -> Value {
    let Some(p) = params else {
        return json!({"error": "missing params {name, value}"});
    };
    let Some(name) = p.get("name").and_then(|v| v.as_str()) else {
        return json!({"error": "missing 'name' (string)"});
    };
    if name.trim().is_empty() {
        return json!({"error": "'name' must be non-empty"});
    }
    let value = match p.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "true" } else { "false" }.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => {
            return json!({
                "error": format!("'value' must be string | number | bool, got {other}"),
            });
        }
        None => {
            return json!({"error": "missing 'value' (string | number | bool)"});
        }
    };

    let start_frame = ctx.snapshot.read().map(|s| s.frame).unwrap_or_default();

    let pushed = lumen_core::property_store::push_external_property(
        lumen_core::property_store::PropertyKey::global(name),
        lumen_core::property_store::PropertyValue::Str(std::sync::Arc::<str>::from(value.as_str())),
    );
    if !pushed {
        return json!({
            "error": "external property bus disconnected",
            "name": name,
        });
    }
    // Nudge a parked event loop (headless idle-park / windowed
    // RedrawScheduler) so the bus drains this frame.
    ctx.simulate_queue.wake();

    const WAIT_DEADLINE: Duration = Duration::from_millis(500);
    let wait_start = std::time::Instant::now();
    let (committed, observed_value, frames_waited) = loop {
        let (frame_now, observed) = match ctx.snapshot.read() {
            Ok(snap) => (
                snap.frame,
                snap.signals
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| s.value.clone()),
            ),
            Err(_) => (start_frame, None),
        };
        let frames = frame_now.wrapping_sub(start_frame);
        if observed.as_deref() == Some(value.as_str()) {
            break (true, observed, frames);
        }
        if wait_start.elapsed() >= WAIT_DEADLINE {
            break (false, observed, frames);
        }
        ctx.timer.sleep(Duration::from_millis(1)).await;
    };

    json!({
        "summary": if committed {
            format!("signal '{name}' = '{value}' committed after {frames_waited} snapshot frame(s)")
        } else {
            format!(
                "signal '{name}' = '{value}' queued on the property bus; commit unconfirmed within 500 ms (windowed apps snapshot at 1 Hz)"
            )
        },
        "name": name,
        "value": value,
        "committed": committed,
        "observed_value": observed_value,
        "frames_waited": frames_waited,
        "next_suggested_tools": [
            { "name": "lumen_signals", "params": {"filter": name}, "why": "confirm the stored value" },
            { "name": "lumen_snapshot_tree", "params": {}, "why": "see bound elements update" },
        ],
        "confidence": if committed { "high" } else { "medium" },
    })
}

/// Enqueue a [`SimulateRequest`] and poll the snapshot until the tick
/// consumes it. If `wait_for` is set (one of the `lumen.recent_messages`
/// ring names) the call waits for that ring to grow as well; otherwise a
/// single advanced frame counter is enough.
async fn run_simulate(ctx: &ServerCtx, params: Option<&Value>) -> Value {
    if !ctx.simulate_enabled {
        return json!({
            "summary": "input simulation disabled",
            "enabled": false,
            "hint": "enable with LumenMcpPlugin::with_simulate_enabled(true) at app startup",
            "confidence": "high",
        });
    }
    let Some(p) = params else {
        return json!({"error": "missing params {kind, ...}"});
    };
    let req: SimulateRequest = match serde_json::from_value(p.clone()) {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "error": format!("invalid simulate params: {e}"),
                "hint": "kind is one of click | pointer_move | pointer_down | pointer_up | key | type | scroll",
            });
        }
    };

    let wait_for = req.wait_for.clone();
    let (start_frame, ring_start) = snapshot_metrics(ctx, wait_for.as_deref());
    let seq = ctx.simulate_queue.push(req.clone());

    // W6 T4: wait for THIS request's tick, not merely "a" tick. The queue
    // drains one request per tick in FIFO order and publishes the popped
    // sequence number only at end-of-tick (`TickStage::A11ySync`), so
    // `completed_seq() >= seq` means the request's full tick ran - a
    // follow-up `lumen.simulate` (e.g. an Escape right after a click) can
    // no longer land before this one's systems finished. The frame /
    // message-ring metrics are kept for the human-readable report.
    // Poll cadence: check IMMEDIATELY, then yield-spin for the first
    // ~2 ms, then fall back to 1 ms sleep slices, overall deadline
    // ~500 ms. The old loop slept a fixed 16 ms before its FIRST check,
    // so every simulate RPC cost >= one frame period even though a
    // headless tick completes in ~1 ms - external drivers that gate
    // their next event on this response (the cross-framework scroll
    // bench, agent scripts) were throttled to ~50 Hz by the RPC alone.
    // The yield-spin phase matters because tokio's timer quantizes
    // sleeps to ~1 ms: a woken headless tick finishes in well under
    // 2 ms, and responding within microseconds of `completed_seq`
    // (instead of at the next timer edge) keeps the RPC's latency
    // jitter out of externally reconstructed frame intervals. Bounded
    // at 2 ms of cooperative yields per call, so a stalled app costs
    // the runtime thread nothing measurable before the sleep fallback.
    const SPIN_WINDOW: Duration = Duration::from_millis(2);
    const WAIT_DEADLINE: Duration = Duration::from_millis(500);
    let wait_start = std::time::Instant::now();
    let (waited_frames, ring_growth, tick_done) = loop {
        let (frame_now, ring_now) = snapshot_metrics(ctx, wait_for.as_deref());
        let waited_frames = frame_now.wrapping_sub(start_frame);
        let ring_growth = ring_now.saturating_sub(ring_start);
        let tick_done = ctx.simulate_queue.completed_seq() >= seq;
        let ring_ok = wait_for.is_none() || ring_growth > 0;
        let elapsed = wait_start.elapsed();
        if (tick_done && ring_ok) || elapsed >= WAIT_DEADLINE {
            break (waited_frames, ring_growth, tick_done);
        }
        if elapsed < SPIN_WINDOW {
            tokio::task::yield_now().await;
        } else {
            ctx.timer.sleep(Duration::from_millis(1)).await;
        }
    };

    let last_events = wait_for
        .as_deref()
        .and_then(|name| tail_ring(ctx, name, ring_growth.max(1)));

    json!({
        "summary": describe_simulate(&req, tick_done, waited_frames, ring_growth),
        "enabled": true,
        "tick_completed": tick_done,
        "frames_waited": waited_frames,
        "ring_growth": ring_growth,
        "wait_for": wait_for,
        "events": last_events,
        "next_suggested_tools": [
            { "name": "lumen_snapshot_text", "params": {}, "why": "see how the UI changed" },
            { "name": "lumen_recent_messages", "params": {"type": "ClickEvent"}, "why": "inspect resulting events" },
        ],
        "confidence": "medium",
    })
}

fn describe_simulate(
    req: &SimulateRequest,
    tick_done: bool,
    waited_frames: u64,
    ring_growth: usize,
) -> String {
    use crate::simulate::SimulateKind;
    let action = match &req.kind {
        SimulateKind::PointerMove { x, y } => format!("pointer_move ({x}, {y})"),
        SimulateKind::PointerDown { x, y, .. } => format!("pointer_down ({x}, {y})"),
        SimulateKind::PointerUp { x, y, .. } => format!("pointer_up ({x}, {y})"),
        SimulateKind::Click { x, y, .. } => format!("click ({x}, {y})"),
        SimulateKind::Key { key, .. } => format!("key {key}"),
        SimulateKind::Type { text } => format!("type '{text}'"),
        SimulateKind::Scroll { x, y, dx, dy } => format!("scroll ({x},{y}) by ({dx},{dy})"),
    };
    match (tick_done, ring_growth) {
        (false, _) => format!("{action} enqueued but its tick did not complete within 500 ms"),
        (true, g) if g > 0 => {
            format!("{action} -> +{g} event(s) on watched ring after {waited_frames} frame(s)")
        }
        (true, _) => format!("{action} tick completed after {waited_frames} frame(s)"),
    }
}

fn snapshot_metrics(ctx: &ServerCtx, ring: Option<&str>) -> (u64, usize) {
    let Ok(snap) = ctx.snapshot.read() else {
        return (0, 0);
    };
    let len = ring.map(|name| ring_len(&snap, name)).unwrap_or(0);
    (snap.frame, len)
}

fn ring_len(snap: &Snapshot, name: &str) -> usize {
    match name {
        "PointerMoved" => snap.pointer_moved.items.len(),
        "PointerPressed" => snap.pointer_pressed.items.len(),
        "PointerReleased" => snap.pointer_released.items.len(),
        "ClickEvent" => snap.click_event.items.len(),
        "KeyPressed" => snap.key_pressed.items.len(),
        "KeyReleased" => snap.key_released.items.len(),
        "MouseWheel" => snap.mouse_wheel.items.len(),
        "FocusedKey" => snap.focused_key.items.len(),
        _ => 0,
    }
}

fn tail_ring(ctx: &ServerCtx, name: &str, n: usize) -> Option<Value> {
    let snap = ctx.snapshot.read().ok()?;
    Some(match name {
        "PointerMoved" => json!(snap.pointer_moved.last_n_owned(n)),
        "PointerPressed" => json!(snap.pointer_pressed.last_n_owned(n)),
        "PointerReleased" => json!(snap.pointer_released.last_n_owned(n)),
        "ClickEvent" => json!(snap.click_event.last_n_owned(n)),
        "KeyPressed" => json!(snap.key_pressed.last_n_owned(n)),
        "KeyReleased" => json!(snap.key_released.last_n_owned(n)),
        "MouseWheel" => json!(snap.mouse_wheel.last_n_owned(n)),
        "FocusedKey" => json!(snap.focused_key.last_n_owned(n)),
        _ => return None,
    })
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn neon_rect_paints_outline_only() {
        let w = 10u32;
        let h = 10u32;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        draw_neon_rect(&mut buf, w, h, 2, 2, 6, 6);
        let pixel = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        // Outline pixel on the magenta border.
        assert_eq!(pixel(2, 2), [255, 0, 200, 255]);
        // Centre pixel stays untouched.
        assert_eq!(pixel(5, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn neon_rect_clamps_to_bounds() {
        let w = 4u32;
        let h = 4u32;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // Out-of-bounds rect must not panic / write past end.
        draw_neon_rect(&mut buf, w, h, -5, -5, 20, 20);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use lumen_core::render_world::SurfaceFrame;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Timer that returns instantly and counts the waits, so the capture
    /// poll loop runs its full 32 rounds in microseconds.
    struct CountingTimer(Arc<AtomicUsize>);

    impl Timer for CountingTimer {
        fn sleep(&self, _duration: Duration) -> BoxFuture<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }

    fn ctx_with(capture: Option<SurfaceCapture>, waits: &Arc<AtomicUsize>) -> ServerCtx {
        ServerCtx {
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
            surface_capture: capture,
            simulate_queue: SimulateQueue::default(),
            simulate_enabled: false,
            issues_enabled: false,
            timer: Arc::new(CountingTimer(Arc::clone(waits))),
        }
    }

    fn frame(width: u32, height: u32, value: u8) -> SurfaceFrame {
        SurfaceFrame {
            width,
            height,
            rgba8: vec![value; (width * height * 4) as usize],
        }
    }

    /// Timer that stands in for a render frame passing: each wait services
    /// a pending capture the way a renderer would.
    struct ServicingTimer {
        capture: SurfaceCapture,
        frame: SurfaceFrame,
    }

    impl Timer for ServicingTimer {
        fn sleep(&self, _duration: Duration) -> BoxFuture<()> {
            if self.capture.is_requested() {
                self.capture.write(self.frame.clone());
                self.capture.clear_request();
            }
            Box::pin(std::future::ready(()))
        }
    }

    /// A renderer that services the request hands back the fresh frame.
    #[tokio::test]
    async fn a_serviced_request_returns_the_captured_frame() {
        let waits = Arc::new(AtomicUsize::new(0));
        let capture = SurfaceCapture::default();
        let mut ctx = ctx_with(Some(capture.clone()), &waits);
        ctx.timer = Arc::new(ServicingTimer {
            capture,
            frame: frame(2, 2, 7),
        });

        let (rgba8, width, height, source) = capture_raw(&ctx).await.expect("a frame");
        assert_eq!((width, height), (2, 2));
        assert_eq!(rgba8, vec![7u8; 16]);
        assert_eq!(source, "surface");
    }

    /// Nothing services the request: the handler waits, gives up, and
    /// answers with the frame already in the store, marked stale.
    #[tokio::test]
    async fn an_unserviced_request_falls_back_to_the_last_frame() {
        let waits = Arc::new(AtomicUsize::new(0));
        let capture = SurfaceCapture::default();
        capture.write(frame(1, 1, 3));
        let ctx = ctx_with(Some(capture), &waits);

        let (rgba8, width, height, source) = capture_raw(&ctx).await.expect("a stale frame");
        assert_eq!((width, height), (1, 1));
        assert_eq!(rgba8, vec![3u8; 4]);
        assert_eq!(source, "surface-stale");
        assert_eq!(
            waits.load(Ordering::SeqCst),
            32,
            "the poll loop waits on the installed timer, not on tokio directly"
        );
    }

    /// No frame was ever captured: the screenshot call reports that rather
    /// than inventing pixels.
    #[tokio::test]
    async fn no_frame_at_all_reports_unavailable() {
        let waits = Arc::new(AtomicUsize::new(0));
        let ctx = ctx_with(Some(SurfaceCapture::default()), &waits);
        assert!(capture_raw(&ctx).await.is_none());

        let response = run_screenshot(&ctx, None).await;
        assert_eq!(response["available"], serde_json::json!(false));
    }
}

#[cfg(test)]
mod framework_status_tests {
    use super::*;
    use crate::issues::IssuesReport;

    /// Timer that never sleeps - nothing in this module's tests waits on a
    /// frame or a GPU readback, so this keeps them instant.
    struct InstantTimer;

    impl Timer for InstantTimer {
        fn sleep(&self, _duration: Duration) -> BoxFuture<()> {
            Box::pin(std::future::ready(()))
        }
    }

    fn test_ctx(issues_enabled: bool) -> ServerCtx {
        ServerCtx {
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
            surface_capture: None,
            simulate_queue: SimulateQueue::default(),
            simulate_enabled: false,
            issues_enabled,
            timer: Arc::new(InstantTimer),
        }
    }

    fn report(
        repo: Option<&str>,
        open_issues: Option<usize>,
        truncated: bool,
        first_open: &[&str],
        error: Option<&str>,
    ) -> IssuesReport {
        IssuesReport {
            repo: repo.map(str::to_string),
            open_issues,
            truncated,
            first_open: first_open.iter().map(|s| s.to_string()).collect(),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_resolution_failure_carries_no_repo_and_low_confidence() {
        let r = report(None, None, false, &[], Some("no 'origin' git remote"));
        let v = framework_status_json(&r, 42, 1000);
        assert_eq!(v["enabled"], json!(true));
        assert_eq!(v["repo"], Value::Null);
        assert_eq!(v["open_issues"], Value::Null);
        assert_eq!(v["confidence"], json!("low"));
        assert!(
            v["summary"]
                .as_str()
                .unwrap()
                .contains("could not determine which repository to query")
        );
    }

    #[test]
    fn a_fetch_failure_keeps_the_repo_and_reports_the_error() {
        let r = report(
            Some("lumen-fx/lumen"),
            None,
            false,
            &[],
            Some("gh not found"),
        );
        let v = framework_status_json(&r, 1, 1);
        assert_eq!(v["repo"], json!("lumen-fx/lumen"));
        assert_eq!(v["open_issues"], Value::Null);
        assert_eq!(v["confidence"], json!("low"));
        assert!(
            v["summary"]
                .as_str()
                .unwrap()
                .contains("could not list open issues on lumen-fx/lumen")
        );
    }

    #[test]
    fn an_exact_count_is_high_confidence() {
        let r = report(
            Some("lumen-fx/lumen"),
            Some(3),
            false,
            &["#1 a", "#2 b"],
            None,
        );
        let v = framework_status_json(&r, 7, 500);
        assert_eq!(v["open_issues"], json!(3));
        assert_eq!(v["truncated"], json!(false));
        assert_eq!(v["confidence"], json!("high"));
        assert_eq!(v["first_open_issues"], json!(["#1 a", "#2 b"]));
        assert!(
            v["summary"]
                .as_str()
                .unwrap()
                .contains("3 open issue(s) on lumen-fx/lumen")
        );
    }

    #[test]
    fn a_truncated_count_is_medium_confidence_and_says_so() {
        let r = report(Some("lumen-fx/lumen"), Some(200), true, &[], None);
        let v = framework_status_json(&r, 7, 500);
        assert_eq!(v["truncated"], json!(true));
        assert_eq!(v["confidence"], json!("medium"));
        let summary = v["summary"].as_str().unwrap();
        assert!(summary.contains("200+ open issue(s)"));
        assert!(summary.contains("truncated"));
    }

    #[tokio::test]
    async fn disabled_by_default_never_builds_a_report_and_says_how_to_enable_it() {
        let ctx = test_ctx(false);
        let v = run_framework_status(&ctx).await;
        assert_eq!(v["enabled"], json!(false));
        assert_eq!(v["repo"], Value::Null);
        assert_eq!(v["open_issues"], Value::Null);
        assert_eq!(v["issues_error"], Value::Null);
        assert_eq!(v["confidence"], json!("high"));
        assert!(v["hint"].as_str().unwrap().contains("[mcp] issues = true"));
    }

    #[tokio::test]
    async fn the_legacy_dotted_method_reaches_the_intercept_unwrapped() {
        let ctx = test_ctx(false);
        let resp = handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"lumen.framework_status"}"#,
            &ctx,
        )
        .await;
        assert_eq!(resp["result"]["enabled"], json!(false));
        assert!(
            resp["result"].get("content").is_none(),
            "the dotted method must return the raw result, not the tool envelope"
        );
    }

    #[tokio::test]
    async fn tools_call_wraps_the_same_result_in_the_tool_envelope() {
        let ctx = test_ctx(false);
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lumen_framework_status","arguments":{}}}"#;
        let resp = handle_request(line, &ctx).await;
        assert_eq!(resp["result"]["isError"], json!(false));
        assert_eq!(resp["result"]["structuredContent"]["enabled"], json!(false));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"enabled\": false"));
    }
}
