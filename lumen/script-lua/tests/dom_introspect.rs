//! Phase-5 lua introspection bindings parity: `n:rect()` / `n:is_visible()`
//! / `n:z_index()` / `n:component(name)` and the `frame_info()` global read
//! the published snapshot and marshal to lua tables / scalars.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_script::ScriptCommand;
use lumen_script::ScriptHost;
use lumen_script::introspect::{
    FrameInfo, IntrospectSnapshot, NodeGeometry, NodeRect, PointerSnapshot, publish_introspection,
};
use lumen_script_lua::LuaHost;

use bevy_ecs::world::World;
use std::collections::HashMap;

fn publish_fixture() -> u64 {
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    let rec = |entity, tag: &str, id: Option<&str>, parent, children: &[_]| DomRecord {
        entity,
        generation: 0,
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: vec![],
        parent,
        children: children.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    };
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
            vec![
                ("width".to_string(), "100".to_string()),
                ("height".to_string(), "40".to_string()),
            ],
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
        vec![("count".to_string(), "3".to_string())],
    ));
    handle
}

#[test]
fn lua_introspection_reads_snapshot() {
    let _btn = publish_fixture();
    let src = r#"
        function read()
            local b = get_by_id("btn")
            print("w=" .. b:rect().width)
            print("vis=" .. tostring(b:is_visible()))
            print("z=" .. b:z_index())
            print("lb=" .. b:component("LayoutBox").width)
            print("frame=" .. frame_info().frame)
            print("sig=" .. signals_all().count)
        end
    "#;
    let mut host = LuaHost::new();
    host.load(src).expect("compiles");
    let out = host.call("read", &[]).expect("read runs");
    let prints: Vec<String> = out
        .commands
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        prints,
        vec![
            "w=100.0".to_string(),
            "vis=true".to_string(),
            "z=5".to_string(),
            "lb=100".to_string(),
            "frame=7".to_string(),
            "sig=3".to_string(),
        ]
    );
}
