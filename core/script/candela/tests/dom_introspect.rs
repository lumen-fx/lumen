//! Proof that the candela procedural introspection API (phase 5) dispatches
//! through the host block against a published snapshot. Value maps marshal
//! back as candela `{string: T}` maps.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_core::prelude::{Entity, World};
use lumen_script::introspect::{
    FrameInfo, IntrospectSnapshot, NodeGeometry, NodeRect, PointerSnapshot, publish_introspection,
};
use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;
use std::collections::HashMap;

fn rec(
    entity: Entity,
    tag: &str,
    id: Option<&str>,
    parent: Option<Entity>,
    children: &[Entity],
) -> DomRecord {
    DomRecord {
        entity,
        generation: entity.generation().to_bits(),
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: vec![],
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
    {string: float} node_rect(int);
    bool node_is_visible(int);
    int node_z_index(int);
    {string: string} node_component(int, string);
    {string: float} frame_info();
}
fn probe_btn() { return lumen::node_get_by_id("btn"); }
fn probe_rect(h) { return lumen::node_rect(h); }
fn probe_vis(h) { return lumen::node_is_visible(h); }
fn probe_z(h) { return lumen::node_z_index(h); }
fn probe_lb(h) { return lumen::node_component(h, "LayoutBox"); }
fn probe_frame() { return lumen::frame_info(); }
fn main() {}
"#;

#[test]
fn candela_procedural_introspection_dispatches() {
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), None, &[btn]),
        rec(btn, "button", Some("btn"), Some(root), &[]),
    ]));
    let handle = NodeHandle::new(btn).pack();

    let mut geometry = HashMap::new();
    geometry.insert(
        handle,
        NodeGeometry {
            rect: NodeRect {
                width: 100.0,
                height: 40.0,
                ..Default::default()
            },
            visible: true,
            z_index: 5,
            ..Default::default()
        },
    );
    let mut components = HashMap::new();
    components.insert(
        handle,
        vec![(
            "LayoutBox".to_string(),
            vec![("width".to_string(), "100".to_string())],
        )],
    );
    publish_introspection(IntrospectSnapshot::new(
        geometry,
        components,
        vec!["LayoutBox".to_string()],
        PointerSnapshot::default(),
        FrameInfo {
            frame: 7,
            dt_ms: 16.0,
            dirty_count: 0,
        },
        vec![],
    ));

    let mut host = CandelaHost::new();
    host.load(SRC, "introspect.cdl").expect("script compiles");

    let btn_id = match host.call("probe_btn", &[]).unwrap().ret {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("probe_btn returned {other:?}"),
    };
    assert!(btn_id != 0);

    // node_rect returns a {string: float} map.
    match host
        .call("probe_rect", &[ScriptValue::I64(btn_id)])
        .unwrap()
        .ret
    {
        Some(ScriptValue::Map(m)) => {
            let w = m
                .iter()
                .find(|(k, _)| k.as_str() == "width")
                .map(|(_, v)| v.clone());
            assert_eq!(
                w,
                Some(ScriptValue::F64(100.0)),
                "rect.width via candela map"
            );
        }
        other => panic!("probe_rect returned {other:?}"),
    }

    assert_eq!(
        host.call("probe_vis", &[ScriptValue::I64(btn_id)])
            .unwrap()
            .ret,
        Some(ScriptValue::Bool(true))
    );
    assert_eq!(
        host.call("probe_z", &[ScriptValue::I64(btn_id)])
            .unwrap()
            .ret,
        Some(ScriptValue::I64(5))
    );

    match host
        .call("probe_lb", &[ScriptValue::I64(btn_id)])
        .unwrap()
        .ret
    {
        Some(ScriptValue::Map(m)) => {
            let w = m
                .iter()
                .find(|(k, _)| k.as_str() == "width")
                .map(|(_, v)| v.clone());
            assert_eq!(w, Some(ScriptValue::Str("100".into())));
        }
        other => panic!("probe_lb returned {other:?}"),
    }

    match host.call("probe_frame", &[]).unwrap().ret {
        Some(ScriptValue::Map(m)) => {
            let f = m
                .iter()
                .find(|(k, _)| k.as_str() == "frame")
                .map(|(_, v)| v.clone());
            assert_eq!(f, Some(ScriptValue::F64(7.0)));
        }
        other => panic!("probe_frame returned {other:?}"),
    }
}
