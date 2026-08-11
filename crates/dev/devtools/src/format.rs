//! Pure snapshot -> text formatters for the three devtools tabs. Kept free
//! of ECS so they unit-test directly against a hand-built [`Snapshot`].

use std::collections::HashSet;

use lumen_mcp::{EntityInspect, Snapshot};

use crate::network::NetworkCapture;

/// Hard cap on rendered element-tree lines so a huge app can't make the
/// per-tick body rebuild unbounded.
const MAX_ELEMENT_LINES: usize = 400;

/// Render the live element tree, indented by hierarchy depth. Entities in
/// `excluded` (the devtools overlay's own subtree) are skipped so the panel
/// never inspects itself.
pub fn format_elements(snap: &Snapshot, excluded: &HashSet<u64>) -> String {
    if snap.entities.is_empty() {
        return "Elements\n\n(no snapshot yet - is the MCP/snapshot plugin enabled?)".to_string();
    }
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

    let mut out = String::from("Elements\n\n");
    let mut lines = 0usize;
    for root in roots {
        walk_element(snap, root, 0, excluded, &mut out, &mut lines);
        if lines >= MAX_ELEMENT_LINES {
            out.push_str("\n... (truncated)");
            break;
        }
    }
    out
}

fn walk_element(
    snap: &Snapshot,
    id: u64,
    depth: usize,
    excluded: &HashSet<u64>,
    out: &mut String,
    lines: &mut usize,
) {
    if *lines >= MAX_ELEMENT_LINES || excluded.contains(&id) {
        return;
    }
    let indent = "  ".repeat(depth);
    let inspect = snap.inspect.get(&id);
    out.push_str(&indent);
    out.push_str(&element_line(id, inspect));
    out.push('\n');
    *lines += 1;

    if let Some(i) = inspect {
        let mut kids: Vec<u64> = i
            .children
            .iter()
            .copied()
            .filter(|k| !excluded.contains(k))
            .collect();
        kids.sort_unstable();
        for k in kids {
            walk_element(snap, k, depth + 1, excluded, out, lines);
        }
    }
}

fn element_line(id: u64, inspect: Option<&EntityInspect>) -> String {
    let Some(i) = inspect else {
        return format!("<?> e{id}");
    };
    let tag = i.tag.as_deref().unwrap_or("node");
    let mut s = format!("<{tag}>");
    if let Some(lid) = &i.lumen_id {
        s.push('#');
        s.push_str(lid);
    }
    for c in &i.classes {
        s.push('.');
        s.push_str(c);
    }
    if let Some(t) = &i.transform {
        s.push_str(&format!(" [{:.0}x{:.0}]", t.size.x, t.size.y));
    }
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
    if !flags.is_empty() {
        s.push_str(" :");
        s.push_str(&flags.join(":"));
    }
    s
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
        out.push_str(&e.render());
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
