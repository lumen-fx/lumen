//! Native Model Context Protocol (MCP) wire surface.
//!
//! Implements MCP per <https://modelcontextprotocol.io>:
//!
//! - JSON-RPC 2.0 envelope (same wire format as before).
//! - Lifecycle: `initialize` -> `initialized` notification -> operational.
//!   `shutdown` is accepted but the server is process-resident; we
//!   simply ack and keep the connection alive (the client drops).
//! - Tools: `tools/list` enumerates the lumen.* methods translated into
//!   MCP `Tool { name, description, inputSchema }` objects.
//!   `tools/call` dispatches to the existing [`crate::methods::dispatch_with_ctx`]
//!   table by mapping `name="lumen_inspect_entity"` -> `lumen.inspect_entity`.
//! - Resources / prompts: not implemented yet; advertised as
//!   `{}` capabilities so clients don't probe them.
//!
//! The legacy `lumen.tick` / `lumen.inspect_entity` direct method calls
//! stay alive (server.rs dispatches them too) - the risk register
//! requires one minor-version deprecation window for the IDE inspector
//! that connects via newline-JSON-over-TCP.
//!
//! Transport-agnostic: this module only knows about JSON-RPC messages;
//! `server.rs` provides the byte transport (TCP, stdio, HTTP-POST).

use std::sync::{Arc, RwLock};

use serde_json::{Value, json};

use crate::methods::{DispatchResult, dispatch_with_ctx};
use crate::snapshot::Snapshot;

/// MCP protocol version we implement. Compared against the client's
/// `initialize.params.protocolVersion`; on mismatch we still respond
/// with the version we DO implement (the client decides).
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Build the canonical JSON-RPC error response.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Translate one MCP method to its handler. Returns `Some(response_value)`
/// to send back; `None` for notifications (no response expected).
///
/// Falls back to the legacy `lumen.*` dispatch for non-MCP method names
/// so clients of the pre-W6.11 wire continue to work.
pub async fn handle_mcp(
    method: &str,
    id: Value,
    params: Option<&Value>,
    snapshot: &Arc<RwLock<Snapshot>>,
    is_notification: bool,
) -> Option<Value> {
    match method {
        "initialize" => Some(ok_response(id, initialize_result(params))),
        "initialized" | "notifications/initialized" => {
            // Notification: no response.
            None
        }
        "shutdown" => Some(ok_response(id, Value::Null)),
        "exit" => None,
        "ping" => Some(ok_response(id, json!({}))),
        "tools/list" => Some(ok_response(id, tools_list_result())),
        "tools/call" => Some(handle_tools_call(id, params, snapshot)),
        "resources/list" => Some(ok_response(id, json!({"resources": []}))),
        "prompts/list" => Some(ok_response(id, json!({"prompts": []}))),
        // Legacy direct-method namespace. Kept alive for one minor
        // version per the risk register; the IDE inspector at
        // `assets/client.js` still uses these names.
        m if m.starts_with("lumen.") => {
            if is_notification {
                return None;
            }
            Some(dispatch_legacy(m, id, params, snapshot))
        }
        _ => {
            if is_notification {
                return None;
            }
            Some(error_response(
                id,
                -32601,
                &format!("method not found: {method}"),
            ))
        }
    }
}

fn initialize_result(params: Option<&Value>) -> Value {
    let _client_version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false },
            // No resources/prompts surface yet; advertise empty so a
            // client doesn't probe them and doesn't reject the
            // handshake on missing capabilities.
            "resources": {},
            "prompts": {},
        },
        "serverInfo": {
            "name": "lumen-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// MCP tool descriptions for the lumen.* method surface.
fn tools_list_result() -> Value {
    let tools = vec![
        tool_descriptor(
            "lumen_tick",
            "Get the current frame number + last-tick duration.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_list_entities",
            "List every snapshot entity with its recognised component type names.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_snapshot_text",
            "Compact a11y-tree-style text dump of the UI. Cheaper than a screenshot for LLM agents.",
            json!({
                "type": "object",
                "properties": {
                    "max_lines": { "type": "integer", "minimum": 1 },
                    "cursor": { "type": "integer", "minimum": 0 },
                    "omit_invisible": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_snapshot_tree",
            "Structured JSON element tree (id, tag, classes, rect, text, flags, children). Backs the browser inspector.",
            json!({
                "type": "object",
                "properties": {
                    "max_nodes": { "type": "integer", "minimum": 1 },
                    "omit_invisible": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_signals",
            "List global reactive signals (PropertyStore): name, value, kind, generation, last-changed frame.",
            json!({
                "type": "object",
                "properties": {
                    "filter": { "type": "string" },
                    "max": { "type": "integer", "minimum": 1 }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_set_signal",
            "Write one global signal through the external property bus (same path Signals::set uses).",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "value": { "type": ["string", "number", "boolean"] }
                },
                "required": ["name", "value"],
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_find",
            "Find entities by text substring, role name, or id.",
            json!({
                "type": "object",
                "properties": {
                    "by_text": { "type": "string" },
                    "by_role": { "type": "string" },
                    "by_id": { "type": "integer" },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_element_at",
            "Topmost-hit entity at logical-pixel (x, y).",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["x", "y"],
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_framework_status",
            "TODO.md progress + liveness check.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_lint",
            "Snapshot-only structural lint (zero-size visible, gradients, dropped clicks, ...).",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_diff_since",
            "Added/removed/changed entity ids since the given tick (or the previous remembered tick).",
            json!({
                "type": "object",
                "properties": {
                    "tick": { "type": "integer", "minimum": 0 }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_inspect_entity",
            "Deep inspection of all recognised component values on one entity.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer" }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_list_extracted",
            "Extracted draw-list (rects + texts) from the render world.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_resources",
            "Pointer / modifier / focus resource snapshot.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool_descriptor(
            "lumen_recent_messages",
            "Tail of a named message ring buffer.",
            json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string" },
                    "max": { "type": "integer", "minimum": 1 }
                },
                "required": ["type"],
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_screenshot",
            "PNG screenshot of the surface (optional highlight overlays).",
            json!({
                "type": "object",
                "properties": {
                    "highlight_ids": { "type": "array", "items": { "type": "integer" } },
                    "highlight_lint": { "type": "boolean" },
                    "include_bounds_map": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
        ),
        tool_descriptor(
            "lumen_simulate",
            "Inject a synthetic pointer/key/scroll event. Opt-in at plugin construction.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string" }
                },
                "required": ["kind"],
                "additionalProperties": true
            }),
        ),
    ];
    json!({ "tools": tools })
}

fn tool_descriptor(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

/// Handle a `tools/call` request synchronously against the snapshot.
/// `lumen_screenshot` and `lumen_simulate` cannot be routed here because
/// they require the live surface-capture / simulate queue handles -
/// `server.rs` intercepts those names before delegating here.
pub fn handle_tools_call(
    id: Value,
    params: Option<&Value>,
    snapshot: &Arc<RwLock<Snapshot>>,
) -> Value {
    let Some(p) = params else {
        return error_response(id, -32602, "tools/call requires {name, arguments?}");
    };
    let name = match p.get("name").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return error_response(id, -32602, "tools/call: missing 'name'");
        }
    };
    let arguments = p.get("arguments").cloned();
    let legacy_method = tool_name_to_legacy(name);
    // Screenshot / simulate / set_signal are intercepted by the server
    // transport (they need the surface-capture / simulate-queue / property
    // bus handles this layer doesn't carry).
    if legacy_method == "lumen.screenshot"
        || legacy_method == "lumen.simulate"
        || legacy_method == "lumen.set_signal"
    {
        return error_response(
            id,
            -32603,
            "internal: screenshot/simulate/set_signal must be handled by transport layer",
        );
    }
    let snap = match snapshot.read() {
        Ok(s) => s,
        Err(_) => return error_response(id, -32603, "snapshot lock poisoned"),
    };
    match dispatch_with_ctx(&legacy_method, arguments.as_ref(), &snap) {
        DispatchResult::Ok(result) => {
            let content = json!([
                { "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default() }
            ]);
            ok_response(
                id,
                json!({
                    "content": content,
                    "isError": false,
                    // MCP allows extension fields; keep the structured JSON so non-text
                    // tooling can consume it directly without re-parsing the text leaf.
                    "structuredContent": result,
                }),
            )
        }
        DispatchResult::MethodNotFound => {
            error_response(id, -32601, &format!("unknown tool: {name}"))
        }
        DispatchResult::InvalidParams(msg) => error_response(id, -32602, &msg),
    }
}

/// Map `lumen_foo_bar` -> `lumen.foo_bar`. MCP tool names use `_`;
/// the legacy method namespace uses `.` as separator.
pub fn tool_name_to_legacy(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("lumen_") {
        format!("lumen.{rest}")
    } else {
        name.to_string()
    }
}

fn dispatch_legacy(
    method: &str,
    id: Value,
    params: Option<&Value>,
    snapshot: &Arc<RwLock<Snapshot>>,
) -> Value {
    let snap = match snapshot.read() {
        Ok(s) => s,
        Err(_) => return error_response(id, -32603, "snapshot lock poisoned"),
    };
    match dispatch_with_ctx(method, params, &snap) {
        DispatchResult::Ok(result) => ok_response(id, result),
        DispatchResult::MethodNotFound => {
            error_response(id, -32601, &format!("method not found: {method}"))
        }
        DispatchResult::InvalidParams(msg) => error_response(id, -32602, &msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_translation() {
        assert_eq!(tool_name_to_legacy("lumen_tick"), "lumen.tick");
        assert_eq!(
            tool_name_to_legacy("lumen_inspect_entity"),
            "lumen.inspect_entity"
        );
        assert_eq!(tool_name_to_legacy("foo"), "foo");
    }

    #[tokio::test]
    async fn initialize_returns_capabilities() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let out = handle_mcp("initialize", json!(1), None, &snap, false).await;
        let out = out.expect("response");
        let result = &out["result"];
        assert_eq!(result["protocolVersion"], json!(MCP_PROTOCOL_VERSION));
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], json!("lumen-mcp"));
    }

    #[tokio::test]
    async fn tools_list_includes_every_lumen_method() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let out = handle_mcp("tools/list", json!(1), None, &snap, false).await;
        let out = out.expect("response");
        let tools = out["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for required in &[
            "lumen_tick",
            "lumen_list_entities",
            "lumen_snapshot_text",
            "lumen_find",
            "lumen_inspect_entity",
            "lumen_lint",
            "lumen_screenshot",
        ] {
            assert!(
                names.contains(required),
                "missing tool {required} in {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn tools_call_dispatches_to_method() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let params = json!({"name": "lumen_tick", "arguments": {}});
        let out = handle_mcp("tools/call", json!(1), Some(&params), &snap, false).await;
        let out = out.expect("response");
        let result = &out["result"];
        assert_eq!(result["isError"], json!(false));
        let structured = &result["structuredContent"];
        assert!(structured.get("frame").is_some());
    }

    #[tokio::test]
    async fn legacy_lumen_dot_method_still_dispatches() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let out = handle_mcp("lumen.tick", json!(1), None, &snap, false).await;
        let out = out.expect("response");
        assert!(out["result"].get("frame").is_some());
    }

    #[tokio::test]
    async fn initialized_notification_returns_none() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let out = handle_mcp("notifications/initialized", Value::Null, None, &snap, true).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn unknown_method_yields_error() {
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        let out = handle_mcp("bogus/method", json!(2), None, &snap, false).await;
        let out = out.expect("response");
        assert!(out.get("error").is_some());
    }
}
