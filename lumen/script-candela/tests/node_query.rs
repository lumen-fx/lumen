//! Proof that the candela procedural DOM read API dispatches through the
//! host block against a real snapshot. candela's value type is an integer,
//! so nodes are `int` ids interned host-side; a script calls
//! `lumen::node_*` and gets ids back.

use lumen_core::node::{DomIndex, DomRecord, publish_dom_index};
use lumen_core::prelude::{Entity, World};
use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

fn rec(
    entity: Entity,
    tag: &str,
    id: Option<&str>,
    classes: &[&str],
    parent: Option<Entity>,
    children: &[Entity],
) -> DomRecord {
    DomRecord {
        entity,
        generation: entity.generation().to_bits(),
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: classes.iter().map(|s| s.to_string()).collect(),
        parent,
        children: children.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    }
}

const SRC: &str = r#"
host "lumen" {
    int node_get_by_id(string);
    int node_parent(int);
    int node_closest(int, string);
    bool node_valid(int);
}
fn probe_save() { return lumen::node_get_by_id("save"); }
fn probe_parent(h) { return lumen::node_parent(h); }
fn probe_closest(h) { return lumen::node_closest(h, ".list"); }
fn probe_valid(h) { return lumen::node_valid(h); }
fn main() {}
"#;

#[test]
fn candela_procedural_node_api_dispatches() {
    // Publish a fixture snapshot: root#app > column.list > button#save.
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let column = w.spawn_empty().id();
    let save = w.spawn_empty().id();
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), &["app"], None, &[column]),
        rec(column, "column", None, &["list"], Some(root), &[save]),
        rec(save, "button", Some("save"), &["row"], Some(column), &[]),
    ]));

    let mut host = CandelaHost::new();
    host.load(SRC, "node.cdl").expect("script compiles");

    let save_id = match host.call("probe_save", &[]).unwrap().ret {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("probe_save returned {other:?}"),
    };
    assert!(save_id != 0, "#save resolves to a nonzero node id");

    let parent_id = match host
        .call("probe_parent", &[ScriptValue::I64(save_id)])
        .unwrap()
        .ret
    {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("probe_parent returned {other:?}"),
    };
    assert!(parent_id != 0, "save has a parent");

    // closest(save, ".list") is the column, the same entity as parent;
    // interning is idempotent, so the ids match.
    let closest_id = match host
        .call("probe_closest", &[ScriptValue::I64(save_id)])
        .unwrap()
        .ret
    {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("probe_closest returned {other:?}"),
    };
    assert_eq!(
        closest_id, parent_id,
        "closest('.list') is the parent column"
    );

    // Liveness: a real id is valid, 0 is not.
    assert_eq!(
        host.call("probe_valid", &[ScriptValue::I64(save_id)])
            .unwrap()
            .ret,
        Some(ScriptValue::Bool(true))
    );
    assert_eq!(
        host.call("probe_valid", &[ScriptValue::I64(0)])
            .unwrap()
            .ret,
        Some(ScriptValue::Bool(false))
    );
}
