//! JSON-RPC method dispatch: maps `lumen.*` method names to snapshot reads.
//!
//! Every method here is read-only. Each takes a borrowed [`Snapshot`] (under
//! a read-lock acquired by the caller) and returns a `serde_json::Value` to
//! be embedded in the JSON-RPC response.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::snapshot::{EntityInspect, Snapshot};

/// Dispatch result. `Ok(value)` becomes the JSON-RPC `result`; `Err(s)` is
/// reported as a JSON-RPC error with code `-32601` (method not found) or
/// `-32602` (invalid params).
pub enum DispatchResult {
    /// Successful response payload.
    Ok(Value),
    /// Method not found.
    MethodNotFound,
    /// Invalid params (with explanation).
    InvalidParams(String),
}

/// Top-level dispatch for all snapshot-only methods.
///
/// `lumen.screenshot` is handled out-of-band by the server because it may
/// drive the on-screen `SurfaceCapture` flag and await a fresh frame - those
/// concerns sit outside the read-only snapshot model.
pub fn dispatch_with_ctx(method: &str, params: Option<&Value>, snap: &Snapshot) -> DispatchResult {
    match method {
        "lumen.tick" => DispatchResult::Ok(method_tick(snap)),
        "lumen.list_entities" => DispatchResult::Ok(method_list_entities(snap)),
        "lumen.snapshot_text" => match parse_snapshot_text(params) {
            Ok(p) => DispatchResult::Ok(method_snapshot_text(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.snapshot_tree" => match parse_snapshot_tree(params) {
            Ok(p) => DispatchResult::Ok(method_snapshot_tree(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.signals" => match parse_signals(params) {
            Ok(p) => DispatchResult::Ok(method_signals(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.find" => match parse_find(params) {
            Ok(p) => DispatchResult::Ok(method_find(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.element_at" => match parse_element_at(params) {
            Ok(p) => DispatchResult::Ok(method_element_at(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.framework_status" => DispatchResult::Ok(method_framework_status(snap)),
        "lumen.lint" => DispatchResult::Ok(method_lint(snap)),
        "lumen.diff_since" => match parse_diff(params) {
            Ok(p) => DispatchResult::Ok(method_diff_since(snap, &p)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.inspect_entity" => match parse_id(params) {
            Ok(id) => DispatchResult::Ok(method_inspect_entity(snap, id)),
            Err(e) => DispatchResult::InvalidParams(e),
        },
        "lumen.list_extracted" => DispatchResult::Ok(method_list_extracted(snap)),
        "lumen.resources" => DispatchResult::Ok(method_resources(snap)),
        "lumen.recent_messages" => match parse_recent(params) {
            Ok((kind, max)) => match method_recent_messages(snap, &kind, max) {
                Some(v) => DispatchResult::Ok(v),
                None => DispatchResult::InvalidParams(format!("unknown message type '{kind}'")),
            },
            Err(e) => DispatchResult::InvalidParams(e),
        },
        _ => DispatchResult::MethodNotFound,
    }
}

#[derive(Deserialize)]
struct IdParams {
    id: u64,
}

fn parse_id(params: Option<&Value>) -> Result<u64, String> {
    let Some(p) = params else {
        return Err("missing params object".into());
    };
    let parsed: IdParams =
        serde_json::from_value(p.clone()).map_err(|e| format!("expected {{id: u64}}: {e}"))?;
    Ok(parsed.id)
}

#[derive(Deserialize)]
struct RecentParams {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default = "default_max")]
    max: usize,
}

fn default_max() -> usize {
    32
}

fn parse_recent(params: Option<&Value>) -> Result<(String, usize), String> {
    let Some(p) = params else {
        return Err("missing params {type, max?}".into());
    };
    let parsed: RecentParams = serde_json::from_value(p.clone())
        .map_err(|e| format!("expected {{type: string, max?: usize}}: {e}"))?;
    Ok((parsed.kind, parsed.max))
}

fn method_tick(snap: &Snapshot) -> Value {
    json!({
        "frame": snap.frame,
        "last_tick_micros": snap.last_tick_micros,
    })
}

fn method_list_entities(snap: &Snapshot) -> Value {
    serde_json::to_value(&snap.entities).unwrap_or(Value::Null)
}

fn method_inspect_entity(snap: &Snapshot, id: u64) -> Value {
    match snap.inspect.get(&id) {
        Some(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        None => json!({ "id": id, "found": false }),
    }
}

fn method_list_extracted(snap: &Snapshot) -> Value {
    json!({
        "rects": snap.rects,
        "texts": snap.texts,
    })
}

fn method_resources(snap: &Snapshot) -> Value {
    json!({
        "viewport": snap.viewport,
        "pointer": snap.pointer,
        "modifiers": snap.modifiers,
        "focus": snap.focus,
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SnapshotTextParams {
    max_lines: Option<usize>,
    cursor: Option<u64>,
    omit_invisible: Option<bool>,
}

fn parse_snapshot_text(params: Option<&Value>) -> Result<SnapshotTextParams, String> {
    let Some(p) = params else {
        return Ok(SnapshotTextParams::default());
    };
    serde_json::from_value(p.clone())
        .map_err(|e| format!("expected {{max_lines?, cursor?, omit_invisible?}}: {e}"))
}

const SNAPSHOT_TEXT_DEFAULT_LINES: usize = 200;
const SNAPSHOT_TEXT_MAX_LINES: usize = 2000;

/// Compact a11y-tree-like text dump. Cheaper than a screenshot by 10-30x in
/// agent tokens (Playwright research, 2025-2026) and preserves enough
/// structural detail to orient in the UI. Lines sorted by absolute y then x
/// until snapshot grows a proper hierarchy view.
fn method_snapshot_text(snap: &Snapshot, p: &SnapshotTextParams) -> Value {
    let max_lines = p
        .max_lines
        .unwrap_or(SNAPSHOT_TEXT_DEFAULT_LINES)
        .min(SNAPSHOT_TEXT_MAX_LINES);
    let omit_invisible = p.omit_invisible.unwrap_or(true);
    let cursor = p.cursor.unwrap_or(0);

    let order = order_entities(snap);

    let mut lines: Vec<String> = Vec::with_capacity(max_lines);
    let mut total_visible = 0usize;
    let mut next_cursor: Option<u64> = None;
    let mut skipping_to_cursor = cursor != 0;

    for (inv, depth) in &order {
        if skipping_to_cursor {
            if inv.id == cursor {
                skipping_to_cursor = false;
            }
            continue;
        }
        if omit_invisible && is_invisible(inv) {
            continue;
        }
        total_visible += 1;
        if lines.len() >= max_lines {
            next_cursor = Some(inv.id);
            break;
        }
        lines.push(format_entity_line(inv, *depth));
    }

    json!({
        "summary": format!(
            "{} entities ({} shown; {} total in snapshot)",
            order.len(),
            lines.len(),
            snap.inspect.len(),
        ),
        "lines": lines,
        "truncated": next_cursor.is_some(),
        "next_cursor": next_cursor,
        "total": order.len(),
        "visible_count": total_visible,
        "next_suggested_tools": [
            { "name": "lumen_inspect_entity", "params": {"id": "<id>"}, "why": "deep dive on one row" },
            { "name": "lumen_find", "params": {"by_text": "<substr>"}, "why": "locate a specific label" },
            { "name": "lumen_screenshot", "params": {}, "why": "only when text isn't enough" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

/// Walk the snapshot inspect map and produce a `(entity, depth)` ordering.
/// Rooted at entities with no `parent`, descended via `children` in source
/// order. Orphans without hierarchy are tacked on the end, sorted by absolute
/// (y, x) as before so we don't regress in apps that haven't populated the
/// hierarchy yet.
fn order_entities(snap: &Snapshot) -> Vec<(&EntityInspect, usize)> {
    let mut out: Vec<(&EntityInspect, usize)> = Vec::with_capacity(snap.inspect.len());
    let mut emitted: std::collections::HashSet<u64> = std::collections::HashSet::new();

    let mut roots: Vec<&EntityInspect> = snap
        .inspect
        .values()
        .filter(|inv| inv.parent.is_none())
        .collect();
    roots.sort_by(|a, b| cmp_yx_id(a, b));

    fn descend<'a>(
        snap: &'a Snapshot,
        inv: &'a EntityInspect,
        depth: usize,
        out: &mut Vec<(&'a EntityInspect, usize)>,
        emitted: &mut std::collections::HashSet<u64>,
    ) {
        if !emitted.insert(inv.id) {
            return;
        }
        out.push((inv, depth));
        for cid in &inv.children {
            if let Some(child) = snap.inspect.get(cid) {
                descend(snap, child, depth + 1, out, emitted);
            }
        }
    }

    for root in roots {
        descend(snap, root, 0, &mut out, &mut emitted);
    }

    // Hierarchy-less leftovers: appended in (y, x) order for apps where the
    // hierarchy snapshot system hasn't populated `children` yet.
    let mut orphans: Vec<&EntityInspect> = snap
        .inspect
        .values()
        .filter(|inv| !emitted.contains(&inv.id))
        .collect();
    orphans.sort_by(|a, b| cmp_yx_id(a, b));
    for inv in orphans {
        out.push((inv, 0));
    }
    out
}

fn cmp_yx_id(a: &EntityInspect, b: &EntityInspect) -> std::cmp::Ordering {
    let ay = a.transform.map(|t| t.absolute.y).unwrap_or(f32::INFINITY);
    let by = b.transform.map(|t| t.absolute.y).unwrap_or(f32::INFINITY);
    let ax = a.transform.map(|t| t.absolute.x).unwrap_or(f32::INFINITY);
    let bx = b.transform.map(|t| t.absolute.x).unwrap_or(f32::INFINITY);
    ay.partial_cmp(&by)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(ax.partial_cmp(&bx).unwrap_or(std::cmp::Ordering::Equal))
        .then(a.id.cmp(&b.id))
}

fn is_invisible(inv: &EntityInspect) -> bool {
    match inv.transform {
        None => true,
        Some(t) => t.size.x <= 0.0 || t.size.y <= 0.0,
    }
}

pub(crate) fn role_of(inv: &EntityInspect) -> &'static str {
    if inv.slider_value.is_some() {
        "slider"
    } else if inv.toggleable.is_some() {
        "toggle"
    } else if inv.image_source.is_some() || inv.loaded_image.is_some() {
        "image"
    } else if inv.loaded_svg.is_some() {
        "svg"
    } else if inv.text_content.is_some() {
        "text"
    } else if inv.bind_text.is_some() {
        "bound-text"
    } else if inv.interaction.is_some() || inv.tab_index.is_some() {
        "interactive"
    } else if inv.scroll.is_some() {
        "scroll"
    } else {
        "node"
    }
}

pub(crate) fn label_of(inv: &EntityInspect) -> String {
    if let Some(t) = inv.text_content.as_deref() {
        truncate_label(t)
    } else if let Some(b) = inv.bind_text.as_deref() {
        format!("${b}")
    } else if let Some(s) = inv.image_source.as_deref() {
        truncate_label(s)
    } else {
        String::new()
    }
}

fn truncate_label(s: &str) -> String {
    let trimmed: String = s.chars().take(40).collect();
    let mut out = trimmed.replace('\n', "\\n");
    if s.chars().count() > 40 {
        // horizontal ellipsis
        out.push('\u{2026}');
    }
    out
}

fn state_flags(inv: &EntityInspect) -> String {
    let mut f = String::with_capacity(4);
    if inv.hovered {
        f.push('H');
    }
    if inv.focused {
        f.push('F');
    }
    if inv.pressed {
        f.push('P');
    }
    if inv.tab_index.is_some() {
        f.push('T');
    }
    if f.is_empty() { "-".into() } else { f }
}

fn format_entity_line(inv: &EntityInspect, depth: usize) -> String {
    let (x, y, w, h) = match inv.transform {
        Some(t) => (t.absolute.x, t.absolute.y, t.size.x, t.size.y),
        None => (0.0, 0.0, 0.0, 0.0),
    };
    let label = label_of(inv);
    let quoted = if label.is_empty() {
        String::new()
    } else {
        format!("\"{label}\"")
    };
    let indent: String = "  ".repeat(depth.min(20));
    let role_with_indent = format!("{}{}", indent, role_of(inv));
    format!(
        "{id:>10}  {role:<24} {label:<32} {x:>5.0},{y:<5.0} {w:>4.0}x{h:<4.0} {state}",
        id = inv.id,
        role = role_with_indent,
        label = quoted,
        x = x,
        y = y,
        w = w,
        h = h,
        state = state_flags(inv),
    )
}

/// Conservative staleness estimate in milliseconds. Snapshot is refreshed on
/// the `McpSnapshotSchedule` cadence (default 1 Hz); without a `last_at` field
/// on the lock, we surface a fixed upper bound so agents know the data may be
/// up to ~1 s old.
fn staleness_ms(_snap: &Snapshot) -> u64 {
    1000
}

/// Apply the agent-aware response envelope to a per-method payload object.
/// Merges `summary`, `next_suggested_tools`, `confidence`, and `stale_for_ms`
/// alongside the existing payload fields. Used by new methods (`snapshot_text`
/// already inlines this shape; later additions go through here).
#[allow(dead_code)]
pub(crate) fn envelope(
    payload: Value,
    summary: String,
    next_tools: Vec<Value>,
    snap: &Snapshot,
) -> Value {
    let mut obj: Map<String, Value> = match payload {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("data".into(), other);
            m
        }
    };
    obj.insert("summary".into(), Value::String(summary));
    obj.insert("next_suggested_tools".into(), Value::Array(next_tools));
    obj.insert("confidence".into(), Value::String("high".into()));
    obj.insert("stale_for_ms".into(), json!(staleness_ms(snap)));
    Value::Object(obj)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SnapshotTreeParams {
    max_nodes: Option<usize>,
    omit_invisible: Option<bool>,
}

fn parse_snapshot_tree(params: Option<&Value>) -> Result<SnapshotTreeParams, String> {
    let Some(p) = params else {
        return Ok(SnapshotTreeParams::default());
    };
    serde_json::from_value(p.clone())
        .map_err(|e| format!("expected {{max_nodes?, omit_invisible?}}: {e}"))
}

const SNAPSHOT_TREE_DEFAULT_NODES: usize = 2000;
const SNAPSHOT_TREE_MAX_NODES: usize = 10_000;

/// Structured JSON element tree for MCP clients and agent tooling - what
/// `lumen-mcp-server` proxies as the `lumen_snapshot_tree` tool. Node shape:
/// `{ id, tag?, lumen_id?, classes, role, label, text?, rect: {x, y, w, h},
///    flags, children: [...] }`
/// where `rect` is the scroll-corrected ON-SCREEN rect (same space as
/// `lumen.find` / `lumen.element_at` / the painted frame) and `flags` is
/// the `H`overed/`F`ocused/`P`ressed/`T`ab-stop string from
/// `snapshot_text`. Roots are entities without a `parent`; hierarchy-less
/// orphans append at the root level, sorted by (y, x, id).
fn method_snapshot_tree(snap: &Snapshot, p: &SnapshotTreeParams) -> Value {
    let max_nodes = p
        .max_nodes
        .unwrap_or(SNAPSHOT_TREE_DEFAULT_NODES)
        .min(SNAPSHOT_TREE_MAX_NODES);
    let omit_invisible = p.omit_invisible.unwrap_or(false);

    let mut budget = max_nodes;
    let mut emitted: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut truncated = false;

    fn build_node(
        snap: &Snapshot,
        inv: &EntityInspect,
        omit_invisible: bool,
        budget: &mut usize,
        emitted: &mut std::collections::HashSet<u64>,
        truncated: &mut bool,
    ) -> Option<Value> {
        if !emitted.insert(inv.id) {
            return None;
        }
        if omit_invisible && is_invisible(inv) {
            return None;
        }
        if *budget == 0 {
            *truncated = true;
            return None;
        }
        *budget -= 1;
        let (x, y, w, h) = on_screen_rect(snap, inv);
        let mut children: Vec<Value> = Vec::with_capacity(inv.children.len());
        for cid in &inv.children {
            if let Some(child) = snap.inspect.get(cid)
                && let Some(node) =
                    build_node(snap, child, omit_invisible, budget, emitted, truncated)
            {
                children.push(node);
            }
        }
        let mut node = serde_json::Map::new();
        node.insert("id".into(), json!(inv.id));
        if let Some(tag) = inv.tag.as_deref() {
            node.insert("tag".into(), json!(tag));
        }
        if let Some(lid) = inv.lumen_id.as_deref() {
            node.insert("lumen_id".into(), json!(lid));
        }
        node.insert("classes".into(), json!(inv.classes));
        node.insert("role".into(), json!(role_of(inv)));
        node.insert("label".into(), json!(label_of(inv)));
        if let Some(text) = inv.text_content.as_deref() {
            node.insert("text".into(), json!(text));
        }
        node.insert("rect".into(), json!({ "x": x, "y": y, "w": w, "h": h }));
        node.insert("flags".into(), json!(state_flags(inv)));
        node.insert("children".into(), Value::Array(children));
        Some(Value::Object(node))
    }

    let mut roots: Vec<&EntityInspect> = snap
        .inspect
        .values()
        .filter(|inv| inv.parent.is_none())
        .collect();
    roots.sort_by(|a, b| cmp_yx_id(a, b));

    let mut tree: Vec<Value> = Vec::new();
    for root in roots {
        if let Some(node) = build_node(
            snap,
            root,
            omit_invisible,
            &mut budget,
            &mut emitted,
            &mut truncated,
        ) {
            tree.push(node);
        }
    }
    // Orphans (parent points at an entity the snapshot doesn't know, or
    // hierarchy not yet populated) surface at root level.
    let mut orphans: Vec<&EntityInspect> = snap
        .inspect
        .values()
        .filter(|inv| !emitted.contains(&inv.id))
        .collect();
    orphans.sort_by(|a, b| cmp_yx_id(a, b));
    for inv in orphans {
        if let Some(node) = build_node(
            snap,
            inv,
            omit_invisible,
            &mut budget,
            &mut emitted,
            &mut truncated,
        ) {
            tree.push(node);
        }
    }

    let emitted_count = max_nodes - budget;
    json!({
        "summary": format!(
            "{} node(s) across {} root(s) at frame {}{}",
            emitted_count,
            tree.len(),
            snap.frame,
            if truncated { " (truncated)" } else { "" },
        ),
        "frame": snap.frame,
        "tree": tree,
        "total": emitted_count,
        "truncated": truncated,
        "next_suggested_tools": [
            { "name": "lumen_inspect_entity", "params": {"id": "<id>"}, "why": "deep dive on one node" },
            { "name": "lumen_signals", "params": {}, "why": "see reactive state driving the tree" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SignalsParams {
    filter: Option<String>,
    max: Option<usize>,
}

fn parse_signals(params: Option<&Value>) -> Result<SignalsParams, String> {
    let Some(p) = params else {
        return Ok(SignalsParams::default());
    };
    serde_json::from_value(p.clone()).map_err(|e| format!("expected {{filter?, max?}}: {e}"))
}

const SIGNALS_DEFAULT_MAX: usize = 500;
const SIGNALS_MAX_MAX: usize = 5000;

/// Read-only listing of every global `PropertyStore` cell:
/// `{name, value, kind, generation, last_changed_frame}` per row, sorted
/// by name. `filter` narrows by case-insensitive substring on the name.
/// Writes go through `lumen.set_signal` (transport-intercepted - see
/// `server.rs`), never through here.
fn method_signals(snap: &Snapshot, p: &SignalsParams) -> Value {
    let max = p.max.unwrap_or(SIGNALS_DEFAULT_MAX).min(SIGNALS_MAX_MAX);
    let needle = p
        .filter
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut rows: Vec<&crate::snapshot::SignalView> = snap
        .signals
        .iter()
        .filter(|s| needle.is_empty() || s.name.to_ascii_lowercase().contains(&needle))
        .collect();
    let total = rows.len();
    let truncated = total > max;
    rows.truncate(max);
    json!({
        "summary": format!(
            "{} signal(s){} at frame {}",
            total,
            if needle.is_empty() { String::new() } else { format!(" matching '{needle}'") },
            snap.frame,
        ),
        "signals": rows,
        "total": total,
        "truncated": truncated,
        "frame": snap.frame,
        "next_suggested_tools": [
            { "name": "lumen_set_signal", "params": {"name": "<name>", "value": "<value>"}, "why": "write a signal through the external property bus" },
            { "name": "lumen_snapshot_tree", "params": {}, "why": "see the UI the signals drive" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FindParams {
    by_text: Option<String>,
    by_role: Option<String>,
    by_id: Option<u64>,
    limit: Option<usize>,
}

fn parse_find(params: Option<&Value>) -> Result<FindParams, String> {
    let Some(p) = params else {
        return Ok(FindParams::default());
    };
    serde_json::from_value(p.clone())
        .map_err(|e| format!("expected {{by_text?, by_role?, by_id?, limit?}}: {e}"))
}

const FIND_DEFAULT_LIMIT: usize = 50;
const FIND_MAX_LIMIT: usize = 500;

/// Selector-style search over the snapshot. Mirrors what an agent would do
/// after a `snapshot_text` pass: narrow by substring, role, or id, then call
/// `inspect_entity` for the deep dive.
fn method_find(snap: &Snapshot, p: &FindParams) -> Value {
    let limit = p.limit.unwrap_or(FIND_DEFAULT_LIMIT).min(FIND_MAX_LIMIT);
    let mut matches: Vec<&EntityInspect> = snap
        .inspect
        .values()
        .filter(|inv| match_find(inv, p))
        .collect();
    matches.sort_by_key(|inv| inv.id);
    let truncated = matches.len() > limit;
    let total = matches.len();
    matches.truncate(limit);

    let rows: Vec<Value> = matches
        .iter()
        .map(|inv| entity_summary(snap, inv))
        .collect();
    json!({
        "summary": format!("{total} match(es) for find query; {} returned", rows.len()),
        "results": rows,
        "truncated": truncated,
        "total": total,
        "next_suggested_tools": [
            { "name": "lumen_inspect_entity", "params": {"id": "<id>"}, "why": "deep dive on a match" },
            { "name": "lumen_element_at", "params": {"x": 0, "y": 0}, "why": "narrow by position instead" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

fn match_find(inv: &EntityInspect, p: &FindParams) -> bool {
    if let Some(id) = p.by_id
        && inv.id != id
    {
        return false;
    }
    if let Some(role) = p.by_role.as_deref()
        && !role.is_empty()
        && !role.eq_ignore_ascii_case(role_of(inv))
    {
        return false;
    }
    if let Some(text) = p.by_text.as_deref()
        && !text.is_empty()
    {
        let needle = text.to_lowercase();
        let hay_text = inv.text_content.as_deref().unwrap_or("");
        let hay_bind = inv.bind_text.as_deref().unwrap_or("");
        let hay_img = inv.image_source.as_deref().unwrap_or("");
        if !hay_text.to_lowercase().contains(&needle)
            && !hay_bind.to_lowercase().contains(&needle)
            && !hay_img.to_lowercase().contains(&needle)
        {
            return false;
        }
    }
    true
}

/// Cumulative scroll offset of `inv`'s ANCESTORS (the entity's own
/// [`EntityInspect::scroll_offset`] moves its children, not itself).
///
/// The main-world `Transform.absolute` the snapshot stores is the layout
/// position BEFORE scrolling; what actually paints is
/// `absolute - ancestor_scroll` (see `extract_rects` /
/// `parent_scroll_offsets` in `lumen-core::render_world` and
/// `ancestor_scroll` in `lumen-input` - render and hit-test agree on this).
/// Walks the `parent` chain over the same snapshot, so the reported rect is
/// internally consistent with the frame the snapshot captured.
fn ancestor_scroll_of(snap: &Snapshot, inv: &EntityInspect) -> (f32, f32) {
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    let mut cur = inv.parent;
    // Defensive hop cap: snapshot parent data could transiently contain a
    // cycle mid-reparent; never loop forever on it.
    for _ in 0..256 {
        let Some(pid) = cur else { break };
        let Some(p) = snap.inspect.get(&pid) else {
            break;
        };
        if let Some(off) = p.scroll_offset {
            sx += off.x;
            sy += off.y;
        }
        cur = p.parent;
    }
    (sx, sy)
}

/// On-screen rect of one inspect entry: layout-absolute origin minus the
/// cumulative ancestor scroll (matching the extracted/painted origin),
/// in logical pixels.
fn on_screen_rect(snap: &Snapshot, inv: &EntityInspect) -> (f32, f32, f32, f32) {
    match inv.transform {
        Some(t) => {
            let (sx, sy) = ancestor_scroll_of(snap, inv);
            (t.absolute.x - sx, t.absolute.y - sy, t.size.x, t.size.y)
        }
        None => (0.0, 0.0, 0.0, 0.0),
    }
}

fn entity_summary(snap: &Snapshot, inv: &EntityInspect) -> Value {
    let (x, y, w, h) = on_screen_rect(snap, inv);
    json!({
        "id": inv.id,
        "role": role_of(inv),
        "label": label_of(inv),
        "x": x,
        "y": y,
        "w": w,
        "h": h,
        "state": state_flags(inv),
    })
}

#[derive(Deserialize)]
struct ElementAtParams {
    x: f32,
    y: f32,
}

fn parse_element_at(params: Option<&Value>) -> Result<ElementAtParams, String> {
    let Some(p) = params else {
        return Err("missing params {x, y}".into());
    };
    serde_json::from_value(p.clone()).map_err(|e| format!("expected {{x: f32, y: f32}}: {e}"))
}

/// Topmost-hit lookup at a point. Iterates snapshot inspect entries (which
/// carry computed `Transform.absolute` + size) and returns the smallest-area
/// rect that contains the query point - a coarse proxy for "topmost" since
/// the snapshot does not yet capture z-order or hierarchy.
fn method_element_at(snap: &Snapshot, p: &ElementAtParams) -> Value {
    let mut best: Option<(&EntityInspect, f32)> = None;
    for inv in snap.inspect.values() {
        // Scroll-corrected on-screen rect - the same space the real
        // hit-test (`lumen_input::hit_test`) and the painted frame use.
        let (x, y, w, h) = on_screen_rect(snap, inv);
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let inside = p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h;
        if !inside {
            continue;
        }
        let area = w * h;
        match best {
            None => best = Some((inv, area)),
            Some((_, prev)) if area < prev => best = Some((inv, area)),
            _ => {}
        }
    }
    match best {
        Some((inv, _)) => json!({
            "summary": format!("hit entity {} (role {}) at ({}, {})", inv.id, role_of(inv), p.x, p.y),
            "hit": true,
            "element": entity_summary(snap, inv),
            "next_suggested_tools": [
                { "name": "lumen_inspect_entity", "params": {"id": inv.id}, "why": "deep dive on the hit" },
            ],
            "stale_for_ms": staleness_ms(snap),
            "confidence": "medium",
        }),
        None => json!({
            "summary": format!("no entity at ({}, {})", p.x, p.y),
            "hit": false,
            "element": Value::Null,
            "next_suggested_tools": [
                { "name": "lumen_snapshot_text", "params": {}, "why": "see all visible entities" },
            ],
            "stale_for_ms": staleness_ms(snap),
            "confidence": "high",
        }),
    }
}

/// Surfaces TODO.md punch-list signal so an MCP client can orient itself when
/// working on Lumen itself (rather than on a Lumen app). Includes per-section open
/// counts, the first ~10 open items verbatim, and the most recent main-world
/// tick duration as a liveness check.
fn method_framework_status(snap: &Snapshot) -> Value {
    let todo_path = std::env::var("LUMEN_TODO_PATH")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(find_todo_md);
    let todo = todo_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok());

    let (sections, open_items) = match todo.as_deref() {
        Some(body) => parse_todo_md(body),
        None => (Vec::new(), Vec::new()),
    };
    let total_open: usize = sections.iter().map(|s| s.open).sum();
    let total_done: usize = sections.iter().map(|s| s.done).sum();

    json!({
        "summary": format!(
            "{} open items across {} sections ({} done); last tick {}us",
            total_open,
            sections.len(),
            total_done,
            snap.last_tick_micros,
        ),
        "todo_path": todo_path.as_ref().map(|p| p.display().to_string()),
        "sections": sections,
        "first_open_items": open_items,
        "last_tick_micros": snap.last_tick_micros,
        "frame": snap.frame,
        "next_suggested_tools": [
            { "name": "lumen_snapshot_text", "params": {}, "why": "verify a fix end-to-end" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

#[derive(serde::Serialize)]
struct TodoSection {
    title: String,
    open: usize,
    partial: usize,
    done: usize,
}

fn parse_todo_md(body: &str) -> (Vec<TodoSection>, Vec<String>) {
    let mut sections: Vec<TodoSection> = Vec::new();
    let mut open_items: Vec<String> = Vec::new();
    let mut current_title = String::from("(preamble)");
    let (mut open, mut partial, mut done) = (0usize, 0usize, 0usize);
    let push =
        |sections: &mut Vec<TodoSection>, title: &str, open: usize, partial: usize, done: usize| {
            if open + partial + done > 0 {
                sections.push(TodoSection {
                    title: title.to_string(),
                    open,
                    partial,
                    done,
                });
            }
        };
    for raw in body.lines() {
        let line = raw.trim_start();
        if let Some(rest) = line.strip_prefix("## ") {
            push(&mut sections, &current_title, open, partial, done);
            current_title = rest.trim().to_string();
            open = 0;
            partial = 0;
            done = 0;
            continue;
        }
        let stripped = line.trim_start_matches(['*', '-', ' ']);
        if let Some(rest) = stripped.strip_prefix("[ ]") {
            open += 1;
            if open_items.len() < 10 {
                open_items.push(format!("{}: {}", current_title, rest.trim()));
            }
        } else if stripped.starts_with("[~]") {
            partial += 1;
        } else if stripped.starts_with("[x]") || stripped.starts_with("[X]") {
            done += 1;
        }
    }
    push(&mut sections, &current_title, open, partial, done);
    (sections, open_items)
}

fn find_todo_md() -> Option<std::path::PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        let candidate = cwd.join("TODO.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cwd.pop() {
            break;
        }
    }
    None
}

/// Snapshot-only lint pass. Surfaces structural issues an agent (or human)
/// would otherwise have to discover by running the UI: zero-sized visible
/// rects, text without style, focusable nodes without labels, half-broken
/// gradients, dropped click events.
pub(crate) fn method_lint(snap: &Snapshot) -> Value {
    let mut findings: Vec<Value> = Vec::new();

    for inv in snap.inspect.values() {
        check_zero_size(inv, &mut findings);
        check_text_without_style(inv, &mut findings);
        check_focusable_without_label(inv, &mut findings);
        check_gradient_underdefined(inv, &mut findings);
    }
    check_child_overflow(snap, &mut findings);
    check_dropped_clicks(snap, &mut findings);

    let (errors, warnings) = findings.iter().fold((0usize, 0usize), |(e, w), f| {
        match f.get("severity").and_then(|v| v.as_str()) {
            Some("error") => (e + 1, w),
            _ => (e, w + 1),
        }
    });

    json!({
        "summary": format!("lint: {errors} error(s), {warnings} warning(s) across {} entities", snap.inspect.len()),
        "findings": findings,
        "total": findings.len(),
        "errors": errors,
        "warnings": warnings,
        "next_suggested_tools": [
            { "name": "lumen_inspect_entity", "params": {"id": "<id>"}, "why": "drill into a flagged entity" },
            { "name": "lumen_snapshot_text", "params": {}, "why": "see structural context" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "medium",
    })
}

fn finding(entity: Option<u64>, category: &str, severity: &str, fix_hint: &str) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("category".into(), json!(category));
    o.insert("severity".into(), json!(severity));
    o.insert("fix_hint".into(), json!(fix_hint));
    if let Some(id) = entity {
        o.insert("entity".into(), json!(id));
    }
    Value::Object(o)
}

fn check_zero_size(inv: &EntityInspect, out: &mut Vec<Value>) {
    let Some(t) = inv.transform else {
        return;
    };
    if t.size.x > 0.0 && t.size.y > 0.0 {
        return;
    }
    let visible = inv.visuals.is_some() || inv.text_content.is_some() || inv.image_source.is_some();
    if !visible {
        return;
    }
    out.push(finding(
        Some(inv.id),
        "zero_size_visible",
        "warning",
        "entity has visuals/text/image but zero size - check parent flex/width/height rules",
    ));
}

fn check_text_without_style(inv: &EntityInspect, out: &mut Vec<Value>) {
    let has_text = inv
        .text_content
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if has_text && inv.text_style.is_none() {
        out.push(finding(
            Some(inv.id),
            "text_without_style",
            "warning",
            "TextContent set but no TextStyle - text may use system defaults; add color/size in CSS",
        ));
    }
}

fn check_focusable_without_label(inv: &EntityInspect, out: &mut Vec<Value>) {
    let focusable = inv.tab_index.is_some() || inv.interaction.is_some();
    if !focusable {
        return;
    }
    let has_label = inv
        .text_content
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
        || inv.bind_text.is_some();
    if !has_label {
        out.push(finding(
            Some(inv.id),
            "focusable_without_label",
            "warning",
            "interactive/focusable element has no text label - accessibility tools will skip it",
        ));
    }
}

fn check_gradient_underdefined(inv: &EntityInspect, out: &mut Vec<Value>) {
    let Some(vis) = inv.visuals.as_ref() else {
        return;
    };
    let Some(fill) = vis.fill.as_ref() else {
        return;
    };
    let n_stops = match fill {
        crate::snapshot::FillView::Linear { stops, .. }
        | crate::snapshot::FillView::Radial { stops, .. }
        | crate::snapshot::FillView::Conic { stops, .. } => stops.len(),
        crate::snapshot::FillView::Solid { .. } => return,
    };
    if n_stops < 2 {
        out.push(finding(
            Some(inv.id),
            "gradient_underdefined",
            "warning",
            "gradient has fewer than 2 stops - will render as solid fill",
        ));
    }
}

fn check_child_overflow(snap: &Snapshot, out: &mut Vec<Value>) {
    for inv in snap.inspect.values() {
        let Some(parent_t) = inv.transform else {
            continue;
        };
        if inv.children.is_empty() {
            continue;
        }
        for cid in &inv.children {
            let Some(child) = snap.inspect.get(cid) else {
                continue;
            };
            let Some(child_t) = child.transform else {
                continue;
            };
            let overflow_right =
                child_t.absolute.x + child_t.size.x > parent_t.absolute.x + parent_t.size.x + 0.5;
            let overflow_bottom =
                child_t.absolute.y + child_t.size.y > parent_t.absolute.y + parent_t.size.y + 0.5;
            if overflow_right || overflow_bottom {
                let sides = match (overflow_right, overflow_bottom) {
                    (true, true) => "right+bottom",
                    (true, false) => "right",
                    (false, true) => "bottom",
                    _ => "none",
                };
                out.push(finding(
                    Some(*cid),
                    "child_overflow_parent",
                    "warning",
                    &format!(
                        "child overflows parent {} bounds on {sides}; add overflow=\"clip\" or shrink child",
                        inv.id
                    ),
                ));
            }
        }
    }
}

fn check_dropped_clicks(snap: &Snapshot, out: &mut Vec<Value>) {
    let pressed = snap.pointer_pressed.items.len();
    let clicks = snap.click_event.items.len();
    if pressed >= 4 && pressed > clicks * 2 {
        out.push(finding(
            None,
            "dropped_clicks",
            "warning",
            &format!(
                "{pressed} PointerPressed but only {clicks} ClickEvent - hit-test misses likely; check element bounds or pointer-events"
            ),
        ));
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DiffParams {
    tick: Option<u64>,
}

fn parse_diff(params: Option<&Value>) -> Result<DiffParams, String> {
    let Some(p) = params else {
        return Ok(DiffParams::default());
    };
    serde_json::from_value(p.clone()).map_err(|e| format!("expected {{tick?}}: {e}"))
}

/// Diff current snapshot against a remembered prior tick. Returns the
/// added/removed/changed entity ids since `tick`. Without `tick` (or with
/// `tick=0`), diffs against the immediately-previous remembered snapshot -
/// the common "what did this frame do?" shape.
fn method_diff_since(snap: &Snapshot, p: &DiffParams) -> Value {
    let target_tick = p.tick.unwrap_or(0);
    let baseline: Option<&crate::snapshot::HistorySnapshot> = if target_tick == 0 {
        snap.history.back()
    } else {
        snap.history
            .iter()
            .rev()
            .find(|h| h.frame <= target_tick)
            .or_else(|| snap.history.front())
    };
    let Some(base) = baseline else {
        return json!({
            "summary": "no history yet; call again after a few ticks",
            "added": [], "removed": [], "changed": [],
            "from_frame": Value::Null,
            "to_frame": snap.frame,
            "next_suggested_tools": [],
            "stale_for_ms": staleness_ms(snap),
            "confidence": "low",
        });
    };

    let mut added: Vec<u64> = Vec::new();
    let mut removed: Vec<u64> = Vec::new();
    let mut changed: Vec<u64> = Vec::new();

    for (id, cur_fp) in &snap.fingerprints {
        match base.fingerprints.get(id) {
            None => added.push(*id),
            Some(prev) if prev.0 != cur_fp.0 => changed.push(*id),
            _ => {}
        }
    }
    for id in base.fingerprints.keys() {
        if !snap.fingerprints.contains_key(id) {
            removed.push(*id);
        }
    }
    added.sort();
    removed.sort();
    changed.sort();

    json!({
        "summary": format!(
            "{} added, {} removed, {} changed since frame {}",
            added.len(), removed.len(), changed.len(), base.frame
        ),
        "added": added,
        "removed": removed,
        "changed": changed,
        "from_frame": base.frame,
        "to_frame": snap.frame,
        "next_suggested_tools": [
            { "name": "lumen_inspect_entity", "params": {"id": "<id>"}, "why": "drill into a changed/added entity" },
            { "name": "lumen_snapshot_text", "params": {}, "why": "orient against the new tree" },
        ],
        "stale_for_ms": staleness_ms(snap),
        "confidence": "high",
    })
}

fn method_recent_messages(snap: &Snapshot, kind: &str, max: usize) -> Option<Value> {
    Some(match kind {
        "PointerMoved" => json!(snap.pointer_moved.last_n_owned(max)),
        "PointerPressed" => json!(snap.pointer_pressed.last_n_owned(max)),
        "PointerReleased" => json!(snap.pointer_released.last_n_owned(max)),
        "ClickEvent" => json!(snap.click_event.last_n_owned(max)),
        "KeyPressed" => json!(snap.key_pressed.last_n_owned(max)),
        "KeyReleased" => json!(snap.key_released.last_n_owned(max)),
        "MouseWheel" => json!(snap.mouse_wheel.last_n_owned(max)),
        "FocusedKey" => json!(snap.focused_key.last_n_owned(max)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{Snapshot, TransformView, V2};

    type TestEntry = (u64, f32, f32, f32, f32, Option<&'static str>, bool);

    fn make_snap_with_entities(entries: &[TestEntry]) -> Snapshot {
        let mut snap = Snapshot::default();
        for (id, x, y, w, h, text, hovered) in entries.iter().copied() {
            let mut inv = EntityInspect {
                id,
                ..Default::default()
            };
            inv.transform = Some(TransformView {
                absolute: V2 { x, y },
                size: V2 { x: w, y: h },
            });
            inv.text_content = text.map(|t| t.to_string());
            inv.hovered = hovered;
            snap.inspect.insert(id, inv);
        }
        snap
    }

    #[test]
    fn snapshot_text_orders_by_y_then_x() {
        let snap = make_snap_with_entities(&[
            (1, 0.0, 50.0, 10.0, 10.0, Some("second"), false),
            (2, 0.0, 10.0, 10.0, 10.0, Some("first"), false),
            (3, 99.0, 10.0, 10.0, 10.0, Some("first-right"), false),
        ]);
        let p = SnapshotTextParams::default();
        let v = method_snapshot_text(&snap, &p);
        let lines = v.get("lines").and_then(|l| l.as_array()).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].as_str().unwrap().contains("first"));
        assert!(lines[1].as_str().unwrap().contains("first-right"));
        assert!(lines[2].as_str().unwrap().contains("second"));
    }

    #[test]
    fn snapshot_text_omits_invisible_by_default() {
        let snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 0.0, 0.0, Some("zero-size"), false),
            (2, 0.0, 10.0, 10.0, 10.0, Some("real"), false),
        ]);
        let v = method_snapshot_text(&snap, &SnapshotTextParams::default());
        let lines = v.get("lines").and_then(|l| l.as_array()).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].as_str().unwrap().contains("real"));
    }

    #[test]
    fn snapshot_text_paginates_with_cursor() {
        let entries: Vec<TestEntry> = (0..10u64)
            .map(|i| (i + 1, 0.0, i as f32 * 10.0, 10.0, 10.0, Some("e"), false))
            .collect();
        let snap = make_snap_with_entities(&entries);
        let p = SnapshotTextParams {
            max_lines: Some(3),
            cursor: None,
            omit_invisible: None,
        };
        let v = method_snapshot_text(&snap, &p);
        assert_eq!(v["truncated"], json!(true));
        let next = v["next_cursor"].as_u64().expect("next_cursor present");
        let p2 = SnapshotTextParams {
            max_lines: Some(3),
            cursor: Some(next),
            omit_invisible: None,
        };
        let v2 = method_snapshot_text(&snap, &p2);
        let lines2 = v2["lines"].as_array().unwrap();
        assert!(!lines2.is_empty(), "second page returns rows");
    }

    #[test]
    fn snapshot_text_envelope_shape() {
        let snap = make_snap_with_entities(&[(1, 0.0, 10.0, 10.0, 10.0, Some("x"), false)]);
        let v = method_snapshot_text(&snap, &SnapshotTextParams::default());
        for key in [
            "summary",
            "lines",
            "truncated",
            "total",
            "next_suggested_tools",
            "stale_for_ms",
            "confidence",
        ] {
            assert!(v.get(key).is_some(), "envelope missing {key}: {v}");
        }
    }

    #[test]
    fn dispatch_routes_snapshot_text() {
        let snap = make_snap_with_entities(&[(7, 0.0, 5.0, 10.0, 10.0, Some("hi"), false)]);
        let out = match super::dispatch_with_ctx("lumen.snapshot_text", None, &snap) {
            DispatchResult::Ok(v) => v,
            DispatchResult::MethodNotFound => panic!("method not found"),
            DispatchResult::InvalidParams(e) => panic!("invalid params: {e}"),
        };
        let lines = out["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].as_str().unwrap().contains("hi"));
    }

    #[test]
    fn snapshot_text_indents_by_hierarchy() {
        let mut snap = make_snap_with_entities(&[
            (1, 0.0, 10.0, 100.0, 100.0, None, false),
            (2, 0.0, 20.0, 50.0, 50.0, Some("child"), false),
            (3, 0.0, 30.0, 25.0, 25.0, Some("leaf"), false),
        ]);
        snap.inspect.get_mut(&1).unwrap().children = vec![2];
        snap.inspect.get_mut(&2).unwrap().parent = Some(1);
        snap.inspect.get_mut(&2).unwrap().children = vec![3];
        snap.inspect.get_mut(&3).unwrap().parent = Some(2);

        let v = method_snapshot_text(&snap, &SnapshotTextParams::default());
        let lines = v["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        let first = lines[0].as_str().unwrap();
        let second = lines[1].as_str().unwrap();
        let third = lines[2].as_str().unwrap();
        assert!(
            !second.starts_with("  node") && second.contains("  text")
                || second.contains("    text")
        );
        let depth_of = |s: &str| s.chars().take_while(|c| *c == ' ').count();
        let role_idx = |s: &str| s.find("text").or_else(|| s.find("node")).unwrap_or(0);
        // Each successive row is more indented than the previous.
        assert!(role_idx(second) > role_idx(first));
        assert!(role_idx(third) > role_idx(second));
        let _ = depth_of; // kept for readability if format shifts
    }

    /// `lumen.snapshot_tree` nests children under parents, carries the
    /// markup identity triple (tag / lumen_id / classes), and reports the
    /// scroll-corrected ON-SCREEN rect - the same space `lumen.find` uses.
    #[test]
    fn snapshot_tree_nests_and_reports_identity_and_rects() {
        let mut snap = make_scrolled_snap();
        {
            let container = snap.inspect.get_mut(&1).unwrap();
            container.children = vec![2];
            container.tag = Some("scroll".into());
        }
        {
            let row = snap.inspect.get_mut(&2).unwrap();
            row.children = vec![3];
            row.tag = Some("row".into());
            row.lumen_id = Some("row-1".into());
            row.classes = vec!["list-row".into()];
        }
        let out = method_snapshot_tree(&snap, &SnapshotTreeParams::default());
        let tree = out["tree"].as_array().unwrap();
        assert_eq!(tree.len(), 1, "single root");
        let root = &tree[0];
        assert_eq!(root["id"], json!(1));
        assert_eq!(root["tag"], json!("scroll"));
        let row = &root["children"][0];
        assert_eq!(row["id"], json!(2));
        assert_eq!(row["lumen_id"], json!("row-1"));
        assert_eq!(row["classes"], json!(["list-row"]));
        // Scroll-corrected: layout-absolute 812 - ancestor scroll 200.
        assert_eq!(row["rect"]["y"], json!(612.0));
        let leaf = &row["children"][0];
        assert_eq!(leaf["id"], json!(3));
        assert_eq!(leaf["rect"]["y"], json!(630.0));
        assert_eq!(out["total"], json!(3));
        assert_eq!(out["truncated"], json!(false));
    }

    /// A node budget cap marks the response truncated instead of blowing
    /// up the payload.
    #[test]
    fn snapshot_tree_truncates_at_max_nodes() {
        let entries: Vec<TestEntry> = (0..10u64)
            .map(|i| (i + 1, 0.0, i as f32 * 10.0, 10.0, 10.0, Some("e"), false))
            .collect();
        let snap = make_snap_with_entities(&entries);
        let p = SnapshotTreeParams {
            max_nodes: Some(4),
            omit_invisible: None,
        };
        let out = method_snapshot_tree(&snap, &p);
        assert_eq!(out["truncated"], json!(true));
        assert_eq!(out["total"], json!(4));
    }

    #[test]
    fn signals_lists_and_filters_by_substring() {
        use crate::snapshot::SignalView;
        let snap = Snapshot {
            frame: 9,
            signals: vec![
                SignalView {
                    name: "clicks".into(),
                    value: "3".into(),
                    kind: "str",
                    generation: 3,
                    last_changed_frame: 8,
                },
                SignalView {
                    name: "volume".into(),
                    value: "0.5".into(),
                    kind: "f64",
                    generation: 1,
                    last_changed_frame: 2,
                },
            ],
            ..Default::default()
        };
        let all = method_signals(&snap, &SignalsParams::default());
        assert_eq!(all["total"], json!(2));
        assert_eq!(all["signals"][0]["name"], json!("clicks"));
        assert_eq!(all["signals"][0]["last_changed_frame"], json!(8));

        let filtered = method_signals(
            &snap,
            &SignalsParams {
                filter: Some("VOL".into()),
                max: None,
            },
        );
        assert_eq!(filtered["total"], json!(1));
        assert_eq!(filtered["signals"][0]["name"], json!("volume"));
    }

    #[test]
    fn dispatch_routes_snapshot_tree_and_signals() {
        let snap = make_snap_with_entities(&[(7, 0.0, 5.0, 10.0, 10.0, Some("hi"), false)]);
        for method in ["lumen.snapshot_tree", "lumen.signals"] {
            match super::dispatch_with_ctx(method, None, &snap) {
                DispatchResult::Ok(v) => {
                    assert!(v.get("summary").is_some(), "{method} carries envelope");
                }
                DispatchResult::MethodNotFound => panic!("{method} not routed"),
                DispatchResult::InvalidParams(e) => panic!("{method} invalid params: {e}"),
            }
        }
    }

    #[test]
    fn find_by_text_matches_case_insensitively() {
        let snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 10.0, 10.0, Some("Submit"), false),
            (2, 0.0, 10.0, 10.0, 10.0, Some("Cancel"), false),
        ]);
        let p = FindParams {
            by_text: Some("submit".into()),
            ..Default::default()
        };
        let out = method_find(&snap, &p);
        let results = out["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["id"], json!(1));
    }

    #[test]
    fn find_by_role_text() {
        let snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 10.0, 10.0, Some("hi"), false),
            (2, 0.0, 10.0, 10.0, 10.0, None, false),
        ]);
        let p = FindParams {
            by_role: Some("text".into()),
            ..Default::default()
        };
        let out = method_find(&snap, &p);
        let results = out["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["id"], json!(1));
    }

    #[test]
    fn find_limit_truncates() {
        let entries: Vec<TestEntry> = (0..10u64)
            .map(|i| (i + 1, 0.0, i as f32 * 10.0, 10.0, 10.0, Some("e"), false))
            .collect();
        let snap = make_snap_with_entities(&entries);
        let p = FindParams {
            by_text: Some("e".into()),
            limit: Some(3),
            ..Default::default()
        };
        let out = method_find(&snap, &p);
        assert_eq!(out["truncated"], json!(true));
        assert_eq!(out["total"], json!(10));
        assert_eq!(out["results"].as_array().unwrap().len(), 3);
    }

    /// Build a snapshot shaped like a scrolled list: entity 1 is a scroll
    /// container (own `scroll_offset` (0, 200)); entity 2 is a row whose
    /// layout-absolute y is below the 600 px viewport; entity 3 nests one
    /// level deeper (scroll offsets must sum over ALL ancestors).
    fn make_scrolled_snap() -> Snapshot {
        let mut snap = Snapshot::default();
        let mut container = EntityInspect {
            id: 1,
            ..Default::default()
        };
        container.transform = Some(TransformView {
            absolute: V2 { x: 0.0, y: 0.0 },
            size: V2 { x: 400.0, y: 600.0 },
        });
        container.scroll_offset = Some(V2 { x: 0.0, y: 200.0 });
        snap.inspect.insert(1, container);
        let mut row = EntityInspect {
            id: 2,
            ..Default::default()
        };
        row.transform = Some(TransformView {
            absolute: V2 { x: 0.0, y: 812.0 },
            size: V2 { x: 400.0, y: 60.0 },
        });
        row.text_content = Some("row".into());
        row.parent = Some(1);
        snap.inspect.insert(2, row);
        let mut label = EntityInspect {
            id: 3,
            ..Default::default()
        };
        label.transform = Some(TransformView {
            absolute: V2 { x: 8.0, y: 830.0 },
            size: V2 { x: 100.0, y: 20.0 },
        });
        label.text_content = Some("label".into());
        label.parent = Some(2);
        snap.inspect.insert(3, label);
        snap
    }

    /// Pins `lumen.find`'s reported rect to the ON-SCREEN origin: the
    /// snapshot's layout-absolute `Transform` minus the cumulative
    /// ancestor scroll - the same value `extract_rects` bakes into
    /// `ExtractedRect.origin` and `lumen_input::hit_test` tests against.
    /// Regression test for find reporting pre-scroll coordinates
    /// (y=812 on a 600 px window).
    #[test]
    fn find_reports_scroll_corrected_rect() {
        let snap = make_scrolled_snap();
        let p = FindParams {
            by_id: Some(2),
            ..Default::default()
        };
        let out = method_find(&snap, &p);
        let row = &out["results"].as_array().unwrap()[0];
        assert_eq!(row["y"], json!(612.0), "absolute 812 - ancestor scroll 200");
        assert_eq!(row["x"], json!(0.0));
        // Nested one level deeper: the sum still only counts ANCESTOR
        // offsets (entity 2 has none of its own).
        let p3 = FindParams {
            by_id: Some(3),
            ..Default::default()
        };
        let out3 = method_find(&snap, &p3);
        let label = &out3["results"].as_array().unwrap()[0];
        assert_eq!(label["y"], json!(630.0));
        // The container itself is not moved by its OWN scroll offset.
        let p1 = FindParams {
            by_id: Some(1),
            ..Default::default()
        };
        let out1 = method_find(&snap, &p1);
        assert_eq!(out1["results"].as_array().unwrap()[0]["y"], json!(0.0));
    }

    /// `lumen.element_at` must hit-test in the same scroll-corrected
    /// space: the post-scroll point finds the row, the pre-scroll point
    /// misses it.
    #[test]
    fn element_at_hits_scrolled_content() {
        let snap = make_scrolled_snap();
        let hit = method_element_at(&snap, &ElementAtParams { x: 200.0, y: 620.0 });
        assert_eq!(hit["hit"], json!(true));
        assert_eq!(hit["element"]["id"], json!(2));
        assert_eq!(hit["element"]["y"], json!(612.0));
    }

    #[test]
    fn element_at_picks_smallest_containing_rect() {
        let snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 100.0, 100.0, None, false),
            (2, 20.0, 20.0, 30.0, 30.0, Some("button"), false),
        ]);
        let p = ElementAtParams { x: 25.0, y: 25.0 };
        let out = method_element_at(&snap, &p);
        assert_eq!(out["hit"], json!(true));
        assert_eq!(out["element"]["id"], json!(2));
    }

    #[test]
    fn element_at_returns_no_hit_outside() {
        let snap = make_snap_with_entities(&[(1, 0.0, 0.0, 10.0, 10.0, None, false)]);
        let p = ElementAtParams { x: 100.0, y: 100.0 };
        let out = method_element_at(&snap, &p);
        assert_eq!(out["hit"], json!(false));
    }

    #[test]
    fn framework_status_parses_todo_sections() {
        let body =
            "# H\n\n## A\n\n* [x] done\n* [ ] open one\n* [~] partial\n\n## B\n\n* [ ] open two\n";
        let (sections, open_items) = parse_todo_md(body);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "A");
        assert_eq!(sections[0].done, 1);
        assert_eq!(sections[0].open, 1);
        assert_eq!(sections[0].partial, 1);
        assert_eq!(sections[1].open, 1);
        assert!(open_items.iter().any(|s| s.contains("open one")));
        assert!(open_items.iter().any(|s| s.contains("open two")));
    }

    #[test]
    fn lint_flags_text_without_style() {
        let snap = make_snap_with_entities(&[(1, 0.0, 0.0, 10.0, 10.0, Some("hi"), false)]);
        let out = method_lint(&snap);
        let findings = out["findings"].as_array().unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f["category"] == json!("text_without_style") && f["entity"] == json!(1))
        );
    }

    #[test]
    fn lint_flags_zero_size_visible() {
        let mut snap = make_snap_with_entities(&[(1, 0.0, 0.0, 0.0, 0.0, None, false)]);
        snap.inspect.get_mut(&1).unwrap().image_source = Some("x.png".into());
        let out = method_lint(&snap);
        let findings = out["findings"].as_array().unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f["category"] == json!("zero_size_visible"))
        );
    }

    #[test]
    fn lint_flags_child_overflow() {
        let mut snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 50.0, 50.0, None, false),
            (2, 10.0, 10.0, 200.0, 200.0, None, false),
        ]);
        snap.inspect.get_mut(&1).unwrap().children = vec![2];
        snap.inspect.get_mut(&2).unwrap().parent = Some(1);
        let out = method_lint(&snap);
        let findings = out["findings"].as_array().unwrap();
        assert!(
            findings.iter().any(|f| f["category"] == json!("child_overflow_parent")
                && f["entity"] == json!(2))
        );
    }

    #[test]
    fn diff_since_reports_added_removed_changed() {
        use crate::snapshot::{EntityFingerprint, HistorySnapshot};
        let mut snap = make_snap_with_entities(&[
            (1, 0.0, 0.0, 10.0, 10.0, Some("kept"), false),
            (2, 0.0, 10.0, 10.0, 10.0, Some("changed"), false),
            (3, 0.0, 20.0, 10.0, 10.0, Some("new"), false),
        ]);
        snap.fingerprints.insert(1, EntityFingerprint(0xAA));
        snap.fingerprints.insert(2, EntityFingerprint(0xBB22));
        snap.fingerprints.insert(3, EntityFingerprint(0xCC));
        let mut prev = std::collections::HashMap::new();
        prev.insert(1u64, EntityFingerprint(0xAA));
        prev.insert(2u64, EntityFingerprint(0xBB11));
        prev.insert(99u64, EntityFingerprint(0xDD));
        snap.history.push_back(HistorySnapshot {
            frame: 5,
            fingerprints: prev,
        });
        snap.frame = 6;

        let out = method_diff_since(&snap, &DiffParams::default());
        let added = out["added"].as_array().unwrap();
        let removed = out["removed"].as_array().unwrap();
        let changed = out["changed"].as_array().unwrap();
        assert_eq!(added, &vec![json!(3)]);
        assert_eq!(removed, &vec![json!(99)]);
        assert_eq!(changed, &vec![json!(2)]);
        assert_eq!(out["from_frame"], json!(5));
        assert_eq!(out["to_frame"], json!(6));
    }

    #[test]
    fn envelope_helper_merges_keys() {
        let snap = Snapshot::default();
        let out = envelope(
            json!({"foo": 1}),
            "hi".into(),
            vec![json!({"name": "t", "why": "w"})],
            &snap,
        );
        assert_eq!(out["foo"], json!(1));
        assert_eq!(out["summary"], json!("hi"));
        assert!(out["next_suggested_tools"].is_array());
        assert_eq!(out["confidence"], json!("high"));
        assert!(out["stale_for_ms"].is_u64());
    }
}
