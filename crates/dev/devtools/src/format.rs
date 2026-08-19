//! Pure snapshot -> text formatters for the three devtools tabs. Kept free
//! of ECS so they unit-test directly against a hand-built [`Snapshot`].

use std::collections::HashSet;

use lumen_mcp::{EntityInspect, Snapshot};

use crate::network::NetworkCapture;

/// Hard cap on rendered element-tree lines so a huge app can't make the
/// per-tick body rebuild unbounded.
const MAX_ELEMENT_LINES: usize = 400;

/// One line of the Elements tree, ready to become a clickable row. The
/// label is split into parts so the panel can color them Chrome-style
/// (tag / id+class / dimensions / interaction state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementRow {
    /// Entity bits of the element the row describes.
    pub id: u64,
    /// Hierarchy depth (0 = root), for indentation.
    pub depth: usize,
    /// `<tag>` part.
    pub tag: String,
    /// `#id.class` part (empty when the element has neither).
    pub meta: String,
    /// ` [WxH]` part (empty without a transform).
    pub dims: String,
    /// ` :hover:focus:press` part (empty when idle).
    pub flags: String,
}

impl ElementRow {
    /// The full one-line label (all parts joined).
    pub fn label(&self) -> String {
        format!("{}{}{}{}", self.tag, self.meta, self.dims, self.flags)
    }
}

/// Flatten the live element tree into depth-annotated rows in document
/// order. Entities in `excluded` (the devtools overlay's own subtree) are
/// skipped so the panel never inspects itself. Capped at
/// [`MAX_ELEMENT_LINES`] rows.
pub fn element_rows(snap: &Snapshot, excluded: &HashSet<u64>) -> Vec<ElementRow> {
    // Roots: entities whose parent is absent or itself excluded / missing.
    let present: HashSet<u64> = snap.entities.iter().map(|e| e.id).collect();
    let mut roots: Vec<u64> = snap
        .entities
        .iter()
        .map(|e| e.id)
        .filter(|id| !excluded.contains(id))
        .filter(|id| {
            snap.inspect
                .get(id)
                .and_then(|i| i.parent)
                .map(|p| !present.contains(&p) || excluded.contains(&p))
                .unwrap_or(true)
        })
        .collect();
    roots.sort_unstable();

    let mut rows = Vec::new();
    for root in roots {
        walk_element(snap, root, 0, excluded, &mut rows);
        if rows.len() >= MAX_ELEMENT_LINES {
            break;
        }
    }
    rows
}

/// Render the live element tree as one indented text block. Kept for the
/// unit tests and as the plain-text fallback; the panel itself spawns one
/// row entity per [`ElementRow`].
pub fn format_elements(snap: &Snapshot, excluded: &HashSet<u64>) -> String {
    if snap.entities.is_empty() {
        return "Elements\n\n(no snapshot yet - is the MCP/snapshot plugin enabled?)".to_string();
    }
    let rows = element_rows(snap, excluded);
    let mut out = String::from("Elements\n\n");
    for r in &rows {
        out.push_str(&"  ".repeat(r.depth));
        out.push_str(&r.label());
        out.push('\n');
    }
    if rows.len() >= MAX_ELEMENT_LINES {
        out.push_str("\n... (truncated)");
    }
    out
}

fn walk_element(
    snap: &Snapshot,
    id: u64,
    depth: usize,
    excluded: &HashSet<u64>,
    rows: &mut Vec<ElementRow>,
) {
    if rows.len() >= MAX_ELEMENT_LINES || excluded.contains(&id) {
        return;
    }
    let inspect = snap.inspect.get(&id);
    rows.push(element_row(id, depth, inspect));

    if let Some(i) = inspect {
        let mut kids: Vec<u64> = i
            .children
            .iter()
            .copied()
            .filter(|k| !excluded.contains(k))
            .collect();
        kids.sort_unstable();
        for k in kids {
            walk_element(snap, k, depth + 1, excluded, rows);
        }
    }
}

fn element_row(id: u64, depth: usize, inspect: Option<&EntityInspect>) -> ElementRow {
    let Some(i) = inspect else {
        return ElementRow {
            id,
            depth,
            tag: format!("<?> e{id}"),
            meta: String::new(),
            dims: String::new(),
            flags: String::new(),
        };
    };
    let mut meta = String::new();
    if let Some(lid) = &i.lumen_id {
        meta.push('#');
        meta.push_str(lid);
    }
    for c in &i.classes {
        meta.push('.');
        meta.push_str(c);
    }
    let dims = match &i.transform {
        Some(t) => format!(" [{:.0}x{:.0}]", t.size.x, t.size.y),
        None => String::new(),
    };
    let mut flags = Vec::new();
    if i.hovered {
        flags.push("hover");
    }
    if i.focused {
        flags.push("focus");
    }
    if i.pressed {
        flags.push("press");
    }
    ElementRow {
        id,
        depth,
        tag: format!("<{}>", i.tag.as_deref().unwrap_or("node")),
        meta,
        dims,
        flags: if flags.is_empty() {
            String::new()
        } else {
            format!(" :{}", flags.join(":"))
        },
    }
}

fn element_line(id: u64, inspect: Option<&EntityInspect>) -> String {
    element_row(id, 0, inspect).label()
}

/// Render the inspect pane for one selected element: identity line, box
/// geometry, then the compact component facts Chrome would put in the
/// Styles / Computed panes.
pub fn format_inspect(inspect: &EntityInspect) -> String {
    let mut out = element_line(inspect.id, Some(inspect));
    out.push('\n');
    if let Some(t) = &inspect.transform {
        out.push_str(&format!(
            "box  x {:.0}  y {:.0}  w {:.0}  h {:.0}\n",
            t.absolute.x, t.absolute.y, t.size.x, t.size.y
        ));
    }
    if let Some(s) = &inspect.style {
        out.push_str(&format!(
            "style  {}  w {}  h {}  pad {:?}  margin {:?}\n",
            s.flex_direction, s.width, s.height, s.padding, s.margin
        ));
    }
    if let Some(v) = &inspect.visuals {
        let fill = match &v.fill {
            Some(lumen_mcp::FillView::Solid { color }) => String::from(color),
            Some(_) => "gradient".to_string(),
            None => "none".to_string(),
        };
        out.push_str(&format!("fill {fill}  radius {:.0}\n", v.radius));
    }
    if let Some(ts) = &inspect.text_style {
        out.push_str(&format!(
            "text  {}  {:.0}px  {}\n",
            String::from(&ts.color),
            ts.size_px,
            ts.align
        ));
    }
    if let Some(text) = &inspect.text_content {
        let mut t: String = text.chars().take(80).collect();
        if text.chars().count() > 80 {
            t.push_str("...");
        }
        out.push_str(&format!("content {t:?}\n"));
    }
    if let Some(o) = inspect.opacity {
        out.push_str(&format!("opacity {o:.2}\n"));
    }
    if let Some(src) = &inspect.image_source {
        out.push_str(&format!("image {src}\n"));
    }
    out
}

/// Render the Signals list plus a compact Performance summary.
pub fn format_signals(snap: &Snapshot) -> String {
    let mut out = String::from("Signals + Performance\n\n");
    out.push_str(&format!(
        "frame {}   tick {:.2} ms   entities {}\n\n",
        snap.frame,
        snap.last_tick_micros as f64 / 1000.0,
        snap.entities.len()
    ));
    if snap.signals.is_empty() {
        out.push_str("(no global signals)");
        return out;
    }
    for s in &snap.signals {
        out.push_str(&format!("{} = {}  ({})\n", s.name, s.value, s.kind));
    }
    out
}

/// Render the captured HTTP request/response entries (newest last).
pub fn format_network(cap: &NetworkCapture) -> String {
    let mut out = String::from("Network\n\n");
    if cap.is_empty() {
        out.push_str("(no requests captured - call fetch()/http() from a script)");
        return out;
    }
    for e in cap.iter() {
        out.push_str(&String::from(e));
        out.push('\n');
    }
    out
}

#[cfg(test)]
// `Snapshot` has ~30 fields; building test fixtures by assigning the two or
// three that matter after `default()` is far clearer than a giant literal.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use lumen_mcp::{EntityInspect, EntityView, SignalView, Snapshot};

    fn inspect(
        tag: &str,
        id: Option<&str>,
        parent: Option<u64>,
        children: Vec<u64>,
    ) -> EntityInspect {
        EntityInspect {
            tag: Some(tag.to_string()),
            lumen_id: id.map(str::to_string),
            parent,
            children,
            ..Default::default()
        }
    }

    fn snap_with_tree() -> Snapshot {
        let mut snap = Snapshot::default();
        // 1 = app root (column) -> 2 = app child (text); 3 = devtools node.
        snap.entities = vec![
            EntityView {
                id: 1,
                components: vec![],
            },
            EntityView {
                id: 2,
                components: vec![],
            },
            EntityView {
                id: 3,
                components: vec![],
            },
        ];
        snap.inspect
            .insert(1, inspect("column", None, None, vec![2]));
        snap.inspect
            .insert(2, inspect("text", None, Some(1), vec![]));
        snap.inspect
            .insert(3, inspect("column", Some("dt-secret"), None, vec![]));
        snap
    }

    #[test]
    fn elements_render_tree_and_exclude_self() {
        let snap = snap_with_tree();
        let excluded: HashSet<u64> = [3].into_iter().collect();
        let out = format_elements(&snap, &excluded);
        assert!(out.contains("<column>"));
        assert!(out.contains("<text>"));
        // The devtools node (id 3, #dt-secret) must not appear.
        assert!(
            !out.contains("dt-secret"),
            "overlay must not inspect itself: {out}"
        );
        // Child is indented under the root.
        let text_line = out.lines().find(|l| l.contains("<text>")).unwrap();
        assert!(
            text_line.starts_with("  "),
            "child should be indented: {text_line:?}"
        );
    }

    #[test]
    fn elements_empty_shows_hint() {
        let snap = Snapshot::default();
        let out = format_elements(&snap, &HashSet::new());
        assert!(out.contains("no snapshot"));
    }

    #[test]
    fn signals_render_values_and_perf() {
        let mut snap = Snapshot::default();
        snap.frame = 42;
        snap.last_tick_micros = 1500;
        snap.signals = vec![SignalView {
            name: "clicks".into(),
            value: "7".into(),
            kind: "i64",
            generation: 1,
            last_changed_frame: 40,
        }];
        let out = format_signals(&snap);
        assert!(out.contains("frame 42"));
        assert!(out.contains("1.50 ms"));
        assert!(out.contains("clicks = 7"));
    }

    #[test]
    fn network_empty_and_populated() {
        let mut cap = NetworkCapture::default();
        assert!(format_network(&cap).contains("no requests"));
        cap.apply(lumen_core::net_capture::NetEvent::Started {
            tag: "t".into(),
            method: "GET".into(),
            url: "https://example/api".into(),
        });
        let out = format_network(&cap);
        assert!(out.contains("https://example/api"));
    }
}
