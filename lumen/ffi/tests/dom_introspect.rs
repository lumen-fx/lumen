//! C-ABI proof of the low-level introspection read side (ABI 0.11).
//! Publishes a DOM + introspection snapshot directly, then drives the
//! `lumen_node_*` / global introspection exports the way an SDK would,
//! releasing every owned buffer through its matching free fn.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_core::prelude::{Entity, World};
use lumen_ffi::*;
use lumen_script::introspect::{
    FrameInfo, IntrospectSnapshot, NodeGeometry, NodeRect, PointerSnapshot, publish_introspection,
};
use std::collections::HashMap;
use std::ffi::{CStr, CString};

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
        classes: vec!["row".to_string()],
        parent,
        children: children.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    }
}

#[test]
fn c_abi_introspection_surface() {
    // ABI reports the bumped minor.
    assert_eq!(
        (lumen_abi_version() >> 8) & 0xFF,
        lumen_ffi::LUMEN_ABI_MINOR,
        "packed ABI minor matches the exported constant"
    );

    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), None, &[btn]),
        rec(btn, "button", Some("btn"), Some(root), &[]),
    ]));
    let handle: LumenNode = NodeHandle::new(btn).pack();

    let mut geometry = HashMap::new();
    geometry.insert(
        handle,
        NodeGeometry {
            rect: NodeRect {
                width: 100.0,
                height: 40.0,
                client_x: 5.0,
                client_y: 6.0,
                ..Default::default()
            },
            visible: true,
            z_index: 3,
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
        PointerSnapshot {
            x: 12.0,
            y: 34.0,
            inside: true,
            ..Default::default()
        },
        FrameInfo {
            frame: 9,
            dt_ms: 16.0,
            dirty_count: 2,
        },
        vec![("count".to_string(), "3".to_string())],
    ));

    // rect
    unsafe {
        let mut r = std::mem::zeroed::<LumenRect>();
        assert_eq!(lumen_node_rect(handle, &mut r), LumenStatus::Ok);
        assert_eq!(r.width, 100.0);
        assert_eq!(r.height, 40.0);
        assert_eq!(r.client_x, 5.0);
    }
    // is_visible / z_index
    unsafe {
        let mut vis = 0;
        assert_eq!(lumen_node_is_visible(handle, &mut vis), LumenStatus::Ok);
        assert_eq!(vis, 1);
        let mut z = 0;
        assert_eq!(lumen_node_z_index(handle, &mut z), LumenStatus::Ok);
        assert_eq!(z, 3);
    }
    // entity_id
    unsafe {
        let (mut idx, mut gen_) = (0u32, 0u32);
        assert_eq!(
            lumen_node_entity_id(handle, &mut idx, &mut gen_),
            LumenStatus::Ok
        );
        assert_eq!(idx, btn.to_bits() as u32);
    }
    // component -> kvlist
    unsafe {
        let name = CString::new("LayoutBox").unwrap();
        let mut list = std::mem::zeroed::<LumenKVList>();
        assert_eq!(
            lumen_node_component(handle, name.as_ptr(), &mut list),
            LumenStatus::Ok
        );
        assert_eq!(list.len, 1);
        let kv = &*list.ptr;
        let key = CStr::from_ptr(kv.key).to_str().unwrap();
        let val = CStr::from_ptr(kv.value).to_str().unwrap();
        assert_eq!(key, "width");
        assert_eq!(val, "100");
        lumen_kvlist_free(list);

        // Unknown component is an error.
        let bad = CString::new("Nope").unwrap();
        let mut list2 = std::mem::zeroed::<LumenKVList>();
        assert_eq!(
            lumen_node_component(handle, bad.as_ptr(), &mut list2),
            LumenStatus::ErrBadArg
        );
    }
    // classes -> strlist
    unsafe {
        let mut list = std::mem::zeroed::<LumenStrList>();
        assert_eq!(lumen_node_classes(handle, &mut list), LumenStatus::Ok);
        assert_eq!(list.len, 1);
        let s = CStr::from_ptr(*list.ptr).to_str().unwrap();
        assert_eq!(s, "row");
        lumen_strlist_free(list);
    }
    // signals_all -> kvlist
    unsafe {
        let mut list = std::mem::zeroed::<LumenKVList>();
        assert_eq!(lumen_signals_all(&mut list), LumenStatus::Ok);
        assert_eq!(list.len, 1);
        lumen_kvlist_free(list);
    }
    // dump_tree -> owned string
    unsafe {
        let mut out: *mut std::os::raw::c_char = std::ptr::null_mut();
        assert_eq!(lumen_dump_tree(&mut out), LumenStatus::Ok);
        let dump = CStr::from_ptr(out).to_str().unwrap().to_string();
        assert!(dump.contains("button#btn"), "dump_tree: {dump}");
        lumen_string_free(out);
    }
    // pointer_state / frame_info
    unsafe {
        let mut p = std::mem::zeroed::<LumenPointerState>();
        assert_eq!(lumen_pointer_state(&mut p), LumenStatus::Ok);
        assert_eq!(p.x, 12.0);
        assert_eq!(p.inside, 1);
        let mut f = std::mem::zeroed::<LumenFrameInfo>();
        assert_eq!(lumen_frame_info(&mut f), LumenStatus::Ok);
        assert_eq!(f.frame, 9);
        assert_eq!(f.dirty_count, 2);
    }
    // Null / stale handle: no panic, clear status.
    unsafe {
        let mut r = std::mem::zeroed::<LumenRect>();
        assert_eq!(lumen_node_rect(0, &mut r), LumenStatus::ErrInvalidHandle);
    }
}
