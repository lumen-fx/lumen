//! MCP (Model Context Protocol) stdio server.
//!
//! Implements the small subset of MCP that Claude Code expects from a tool
//! server: `initialize` handshake, `notifications/initialized`,
//! `tools/list`, `tools/call`. All other methods return `MethodNotFound`.
//!
//! Protocol framing: each message is a JSON-RPC 2.0 object on its own line
//! (newline-delimited). MCP also defines a chunked framing variant for
//! transports that aren't line-oriented; Claude Code uses the line variant
//! for stdio.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::warn;

use crate::bridge::LumenBridge;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Entry point: run the stdio server forever.
pub async fn run(host: String, port: u16) -> std::io::Result<()> {
    let bridge = Arc::new(LumenBridge::new(host, port));

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match handle(trimmed, &bridge).await {
            Some(r) => r,
            None => continue, // notifications produce no response.
        };
        let mut bytes = match serde_json::to_vec(&response) {
            Ok(b) => b,
            Err(e) => {
                warn!("serialize error: {e}");
                continue;
            }
        };
        bytes.push(b'\n');
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
}

/// Handle one inbound request line. Returns `None` for notifications.
async fn handle(line: &str, bridge: &Arc<LumenBridge>) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(err_resp(Value::Null, -32700, &format!("parse error: {e}")));
        }
    };
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params");
    let is_notification = id.is_none();
    let id = id.unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(initialize_result()),
        "notifications/initialized" => {
            // notification: client confirms it's ready.
            return None;
        }
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => tools_call(params, bridge).await,
        "resources/list" => Ok(resources_list_result()),
        "resources/read" => resources_read(params),
        "prompts/list" => Ok(prompts_list_result()),
        "prompts/get" => prompts_get(params),
        "ping" => Ok(json!({})),
        _ => {
            if is_notification {
                return None;
            }
            return Some(err_resp(id, -32601, &format!("method not found: {method}")));
        }
    };

    if is_notification {
        return None;
    }

    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err((code, msg)) => err_resp(id, code, &msg),
    })
}

fn err_resp(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {},
            "resources": { "listChanged": false, "subscribe": false },
            "prompts": { "listChanged": false }
        },
        "serverInfo": {
            "name": "lumen-mcp-server",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

/// Static tool catalogue. Names mirror the JSON-RPC methods but use
/// underscores (MCP tools conventionally do not contain dots).
fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "lumen_tick",
                "description": "Return the current frame counter and the last tick's wall-clock duration in microseconds (`last_tick_micros`): the whole main schedule, plus extract and scene encode on ticks that rendered.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_list_entities",
                "description": "List all main-world entities with the recognised component types on each.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_find",
                "description": "Selector-style search over the snapshot by text substring, role, or id. Returns up to `limit` entity summaries (id, role, label, bounds, state). Prefer this over `lumen_list_entities` when you already know what you're looking for. Roles: text, bound-text, image, svg, slider, toggle, interactive, scroll, node.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "by_text": { "type": "string", "description": "Case-insensitive substring of TextContent / BindText signal / image source." },
                        "by_role": { "type": "string", "description": "Exact role match (case-insensitive)." },
                        "by_id": { "type": "integer", "description": "Match a specific entity id (Entity::to_bits())." },
                        "limit": { "type": "integer", "description": "Cap on rows. Default 50, max 500." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_element_at",
                "description": "Topmost-hit lookup at a window-coordinate point. Returns the smallest-area entity whose bounds contain (x, y), or hit=false. Coordinates are logical pixels matching Transform.absolute.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number" },
                        "y": { "type": "number" }
                    },
                    "required": ["x", "y"],
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_lint",
                "description": "Snapshot-only lint pass. Surfaces structural UI issues an agent would otherwise discover by trial-and-error: zero-size visible rects, text without TextStyle, focusable elements without labels, gradients with <2 stops, dropped clicks (PointerPressed without ClickEvent), child entities overflowing their parent. Each finding has {entity?, category, severity, fix_hint}.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_diff_since",
                "description": "Diff the current snapshot against a remembered prior tick. Without `tick` (or with tick=0), compares against the immediately-previous remembered snapshot. Returns added/removed/changed entity id lists plus the from/to frame numbers. Use this to see what hot reload, a click, or a script SetSignal actually changed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tick": { "type": "integer", "description": "Compare against the most recent history entry with frame <= tick. Omit for previous-frame diff." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_framework_status",
                "description": "Open issue count (and the first ~10 titles) for the repository this checkout's origin git remote points at, fetched through the gh CLI, plus the most recent main-world tick duration as a liveness check. Reports issues_error instead of a count when gh, the network, or the origin remote isn't available.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_snapshot_text",
                "description": "Compact accessibility-tree-style text dump of the live UI. Prefer this over screenshots when orienting in the tree (10-30x cheaper in agent tokens). One line per entity, sorted by absolute y then x. Columns: id role label x,y wxh state. Returns {summary, lines, truncated, next_cursor, total, next_suggested_tools, stale_for_ms, confidence}. Use lumen_inspect_entity for deep dive on a specific id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_lines": { "type": "integer", "description": "Cap on output rows. Default 200, max 2000." },
                        "cursor": { "type": "integer", "description": "next_cursor from a prior call to paginate." },
                        "omit_invisible": { "type": "boolean", "description": "Skip entities with no transform or zero size. Default true." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_snapshot_tree",
                "description": "Structured JSON element tree of the live UI. Node shape: {id, tag?, lumen_id?, classes, role, label, text?, rect:{x,y,w,h}, flags, children:[...]} - rect is the scroll-corrected on-screen rect in logical pixels, flags is the Hovered/Focused/Pressed/Tab-stop string. Prefer lumen_snapshot_text for cheap orientation; use this when you need hierarchy or markup identity (tag / #id / .classes).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_nodes": { "type": "integer", "description": "Node budget. Default 2000, max 10000; response sets truncated:true when hit." },
                        "omit_invisible": { "type": "boolean", "description": "Skip zero-size / transform-less nodes. Default false." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_signals",
                "description": "List every global reactive signal (PropertyStore cell): {name, value, kind, generation, last_changed_frame}. kind is one of {str, bool, i64, f64, color, vec2, custom}; last_changed_frame is the snapshot frame the cell last changed at (0 = never observed changing). Read-only - write with lumen_set_signal.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter": { "type": "string", "description": "Case-insensitive substring match on the signal name." },
                        "max": { "type": "integer", "description": "Row cap. Default 500." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_set_signal",
                "description": "Write one global signal through the external property bus - the same commit path script Signals::set uses, so the write lands at a tick boundary with ordering semantics intact. Values are stored as canonical strings (true/false for bools, decimal for numbers), matching bind-text / <if eq> expectations. Returns {committed, observed_value, frames_waited}; committed:false on a windowed app usually means unconfirmed (1 Hz snapshot), not failed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Global signal name." },
                        "value": { "type": ["string", "number", "boolean"], "description": "New value." }
                    },
                    "required": ["name", "value"],
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_inspect_entity",
                "description": "Return all recognised component values for a single entity id.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "id": { "type": "integer", "description": "Entity::to_bits() as u64" } },
                    "required": ["id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_list_extracted",
                "description": "Render-world ExtractedRect + ExtractedText for the current frame.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_resources",
                "description": "Current Viewport, PointerState, Modifiers, FocusTracker.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "lumen_recent_messages",
                "description": "Last N messages of a given type. type is one of {PointerMoved, PointerPressed, PointerReleased, ClickEvent, KeyPressed, KeyReleased, MouseWheel, FocusedKey}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string" },
                        "max": { "type": "integer", "default": 32 }
                    },
                    "required": ["type"],
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_simulate",
                "description": "Inject pointer/key/scroll input into the running app and wait until the tick consumes it. Requires the app to opt in via `[mcp] simulate = true` in lumen.toml (or LumenMcpPlugin::with_simulate_enabled). Use `wait_for` with a ring name like \"ClickEvent\" or \"KeyPressed\" to confirm the event reached its handler. Kinds: click, pointer_down, pointer_up, pointer_move, key, type, scroll. pointer_down leaves the button held, so pairing it with pointer_move and pointer_up drives a drag gesture.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["click", "pointer_down", "pointer_up", "pointer_move", "key", "type", "scroll"] },
                        "x": { "type": "number" },
                        "y": { "type": "number" },
                        "dx": { "type": "number" },
                        "dy": { "type": "number" },
                        "button": { "type": "string", "description": "primary | secondary | middle (default primary)" },
                        "key": { "type": "string", "description": "Key name like Enter, Tab, Escape, or a literal char like 'a'." },
                        "modifiers": {
                            "type": "object",
                            "properties": {
                                "shift": { "type": "boolean" },
                                "ctrl": { "type": "boolean" },
                                "alt": { "type": "boolean" },
                                "super": { "type": "boolean" }
                            },
                            "additionalProperties": false
                        },
                        "text": { "type": "string", "description": "Used by kind=type." },
                        "wait_for": { "type": "string", "description": "Optional ring name to poll after dispatch (e.g. ClickEvent, KeyPressed)." }
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                }
            },
            {
                "name": "lumen_screenshot",
                "description": "Capture a base64 PNG of the running app. Optional neon-marker overlay: pass highlight_ids:[u64] to outline specific entities in bright magenta, or highlight_lint:true to outline every lint finding. include_bounds_map returns an [{id,x,y,w,h,role,label}] list alongside the PNG so a downstream tool can place its own overlay without re-querying.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "highlight_ids": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "description": "Entity ids to outline in neon magenta."
                        },
                        "highlight_lint": {
                            "type": "boolean",
                            "description": "Outline every entity flagged by lumen.lint."
                        },
                        "include_bounds_map": {
                            "type": "boolean",
                            "description": "Return bounds_map: [{id,x,y,w,h,role,label}] alongside the PNG."
                        }
                    },
                    "additionalProperties": false
                }
            }
        ]
    })
}

/// Map an MCP `tools/call` invocation onto a `lumen.*` JSON-RPC call.
async fn tools_call(
    params: Option<&Value>,
    bridge: &Arc<LumenBridge>,
) -> Result<Value, (i64, String)> {
    let Some(p) = params else {
        return Err((-32602, "missing params".into()));
    };
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "missing tool name".to_string()))?;
    let args = p.get("arguments").cloned();
    let rpc_method = match name {
        "lumen_tick" => "lumen.tick",
        "lumen_list_entities" => "lumen.list_entities",
        "lumen_snapshot_text" => "lumen.snapshot_text",
        "lumen_snapshot_tree" => "lumen.snapshot_tree",
        "lumen_signals" => "lumen.signals",
        "lumen_set_signal" => "lumen.set_signal",
        "lumen_find" => "lumen.find",
        "lumen_element_at" => "lumen.element_at",
        "lumen_framework_status" => "lumen.framework_status",
        "lumen_lint" => "lumen.lint",
        "lumen_diff_since" => "lumen.diff_since",
        "lumen_inspect_entity" => "lumen.inspect_entity",
        "lumen_list_extracted" => "lumen.list_extracted",
        "lumen_resources" => "lumen.resources",
        "lumen_recent_messages" => "lumen.recent_messages",
        "lumen_screenshot" => "lumen.screenshot",
        "lumen_simulate" => "lumen.simulate",
        _ => return Err((-32601, format!("unknown tool: {name}"))),
    };

    match bridge.call(rpc_method, args).await {
        Ok(result) => {
            // Spec: tools/call returns { content: [...], isError?: bool }.
            // We wrap the JSON result as a single text-content block.
            let text =
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| "<unserializable>".into());
            Ok(json!({
                "content": [ { "type": "text", "text": text } ],
                "isError": false
            }))
        }
        Err(msg) => Ok(json!({
            "content": [ { "type": "text", "text": msg } ],
            "isError": true
        })),
    }
}

// --- Resources ----------------------------------------------------------
//
// Surfaces the small set of files an agent typically needs context on
// (lumen.toml, src/main.lmn, src/main.css, docs/*.md) as read-only MCP
// `resources/`. The catalogue is discovered relative to the working
// directory at startup - we walk up to six parents looking for the same
// anchor the rest of the toolchain uses for a project root: a `lumen.toml`
// (a single app checkout) or a `Cargo.toml` declaring `[workspace]` (the
// Lumen framework checkout itself, the way `lumenc bundle --static` locates
// its own workspace).

const RESOURCE_GLOB_LIMIT: usize = 64;

fn is_project_root(dir: &std::path::Path) -> bool {
    if dir.join("lumen.toml").is_file() {
        return true;
    }
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .is_ok_and(|manifest| manifest.contains("[workspace]"))
}

fn project_root() -> Option<std::path::PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        if is_project_root(&cwd) {
            return Some(cwd);
        }
        if !cwd.pop() {
            break;
        }
    }
    None
}

fn enumerate_resources() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let Some(root) = project_root() else {
        return out;
    };
    for tail in ["lumen.toml", "src/main.lmn", "src/main.css"] {
        let p = root.join(tail);
        if p.is_file() {
            out.push(p);
        }
    }
    if let Ok(apps) = std::fs::read_dir(root.join("apps")) {
        for entry in apps.flatten() {
            for tail in ["src/main.lmn", "src/main.css", "lumen.toml"] {
                let p = entry.path().join(tail);
                if p.is_file() && out.len() < RESOURCE_GLOB_LIMIT {
                    out.push(p);
                }
            }
        }
    }
    if let Ok(docs) = std::fs::read_dir(root.join("docs")) {
        for entry in docs.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("md")
                && out.len() < RESOURCE_GLOB_LIMIT
            {
                out.push(p);
            }
        }
    }
    out
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => "application/toml",
        Some("css") => "text/css",
        Some("md") => "text/markdown",
        Some("lmn") => "text/x-lumen",
        _ => "text/plain",
    }
}

fn path_to_uri(p: &std::path::Path) -> String {
    format!("file://{}", p.display())
}

fn resources_list_result() -> Value {
    let items: Vec<Value> = enumerate_resources()
        .into_iter()
        .map(|p| {
            let uri = path_to_uri(&p);
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            json!({
                "uri": uri,
                "name": name,
                "description": format!("source file: {}", p.display()),
                "mimeType": mime_for(&p),
            })
        })
        .collect();
    json!({ "resources": items })
}

fn resources_read(params: Option<&Value>) -> Result<Value, (i64, String)> {
    let Some(p) = params else {
        return Err((-32602, "missing params".into()));
    };
    let uri = p
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "missing uri".to_string()))?;
    let path_str = uri
        .strip_prefix("file://")
        .ok_or((-32602, format!("unsupported URI scheme: {uri}")))?;
    let path = std::path::PathBuf::from(path_str);
    let allowed = enumerate_resources();
    if !allowed.iter().any(|a| a == &path) {
        return Err((-32602, format!("uri not in resource catalogue: {uri}")));
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| (-32603, format!("read {path_str}: {e}")))?;
    Ok(json!({
        "contents": [
            {
                "uri": uri,
                "mimeType": mime_for(&path),
                "text": text,
            }
        ]
    }))
}

// --- Prompts ------------------------------------------------------------
//
// Reusable templates that walk an agent through common Lumen workflows.
// Each prompt is a static `messages` payload - no template variables yet,
// just clear instructions for a fresh session.

fn prompts_list_result() -> Value {
    json!({
        "prompts": [
            {
                "name": "debug-layout-issue",
                "description": "Step-by-step workflow for diagnosing a broken Lumen UI: lint -> snapshot -> find -> inspect -> screenshot.",
                "arguments": []
            },
            {
                "name": "add-new-component",
                "description": "Pattern for adding a new component to the Lumen framework (lumen-primitives). Walks the file list and existing conventions.",
                "arguments": []
            }
        ]
    })
}

fn prompts_get(params: Option<&Value>) -> Result<Value, (i64, String)> {
    let Some(p) = params else {
        return Err((-32602, "missing params".into()));
    };
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "missing prompt name".to_string()))?;
    let body = match name {
        "debug-layout-issue" => include_str!("../prompts/debug-layout-issue.md"),
        "add-new-component" => include_str!("../prompts/add-new-component.md"),
        other => return Err((-32602, format!("unknown prompt: {other}"))),
    };
    Ok(json!({
        "description": format!("Lumen workflow: {name}"),
        "messages": [
            {
                "role": "user",
                "content": { "type": "text", "text": body }
            }
        ]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Value {
        tools_list_result()["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == json!(name))
            .unwrap_or_else(|| panic!("no tool named {name}"))
            .clone()
    }

    #[test]
    fn simulate_schema_lists_every_kind() {
        // The schema sets additionalProperties:false, so a kind missing
        // from the enum is rejected before it reaches the app - which
        // accepts all seven.
        let kinds = tool("lumen_simulate")["inputSchema"]["properties"]["kind"]["enum"].clone();
        let kinds: Vec<&str> = kinds
            .as_array()
            .expect("enum array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for expected in [
            "click",
            "pointer_down",
            "pointer_up",
            "pointer_move",
            "key",
            "type",
            "scroll",
        ] {
            assert!(
                kinds.contains(&expected),
                "{expected} missing from {kinds:?}"
            );
        }
    }

    #[test]
    fn every_tool_declares_an_object_schema() {
        for t in tools_list_result()["tools"].as_array().expect("tools") {
            let schema = &t["inputSchema"];
            assert_eq!(schema["type"], json!("object"), "{}", t["name"]);
            assert!(t["description"].is_string(), "{}", t["name"]);
        }
    }

    /// A scratch directory under the OS temp dir, cleaned up on drop, so
    /// `is_project_root` can be exercised against synthetic markers without
    /// touching the process's actual working directory (which the other
    /// `project_root` tests would race on if run in parallel).
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "lumen-mcp-server-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_bare_directory_is_not_a_project_root() {
        let dir = ScratchDir::new("bare");
        assert!(!is_project_root(&dir.0));
    }

    #[test]
    fn a_lumen_toml_marks_an_app_root() {
        let dir = ScratchDir::new("app");
        std::fs::write(dir.0.join("lumen.toml"), "").expect("write lumen.toml");
        assert!(is_project_root(&dir.0));
    }

    #[test]
    fn a_workspace_cargo_toml_marks_the_framework_checkout_root() {
        let dir = ScratchDir::new("workspace");
        std::fs::write(dir.0.join("Cargo.toml"), "[workspace]\nmembers = []\n")
            .expect("write Cargo.toml");
        assert!(is_project_root(&dir.0));
    }

    #[test]
    fn a_plain_crate_cargo_toml_does_not_mark_a_root() {
        let dir = ScratchDir::new("crate");
        std::fs::write(dir.0.join("Cargo.toml"), "[package]\nname = \"x\"\n")
            .expect("write Cargo.toml");
        assert!(!is_project_root(&dir.0));
    }
}
