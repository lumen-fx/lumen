//! C-ABI proof of the dynamic DOM read side (ABI 0.8). Publishes a DOM
//! snapshot directly, then drives the `lumen_query` / traversal exports the
//! way a C / C++ / Python SDK would.

use lumen_core::node::{DomIndex, DomRecord, publish_dom_index};
use lumen_core::prelude::{Entity, World};
use lumen_ffi::*;
use std::ffi::CString;

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

#[test]
fn c_abi_dom_query_surface() {
    // ABI reports the bumped minor.
    assert_eq!(lumen_abi_version(), LUMEN_ABI_VERSION);
    assert_eq!((LUMEN_ABI_VERSION >> 8) & 0xFF, 11);

    // Publish: root#app > column.list > [save#save.row, cancel#cancel.row].
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let column = w.spawn_empty().id();
    let save = w.spawn_empty().id();
    let cancel = w.spawn_empty().id();
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), &["app"], None, &[column]),
        rec(
            column,
            "column",
            None,
            &["list"],
            Some(root),
            &[save, cancel],
        ),
        rec(save, "button", Some("save"), &["row"], Some(column), &[]),
        rec(
            cancel,
            "button",
            Some("cancel"),
            &["row"],
            Some(column),
            &[],
        ),
    ]));

    let sel_save = CString::new("#save").unwrap();
    let sel_row = CString::new(".row").unwrap();
    let sel_list = CString::new(".list").unwrap();
    let id_save = CString::new("save").unwrap();
    let id_cancel = CString::new("cancel").unwrap();

    unsafe {
        // get_by_id fast path.
        let mut save_h: LumenNode = 0;
        assert_eq!(
            lumen_get_by_id(id_save.as_ptr(), &mut save_h),
            LumenStatus::Ok
        );
        assert!(save_h != 0);
        let mut cancel_h: LumenNode = 0;
        assert_eq!(
            lumen_get_by_id(id_cancel.as_ptr(), &mut cancel_h),
            LumenStatus::Ok
        );

        // query_single / query_len.
        let mut single_h: LumenNode = 0;
        assert_eq!(
            lumen_query_single(sel_save.as_ptr(), &mut single_h),
            LumenStatus::Ok
        );
        assert_eq!(single_h, save_h);

        let mut len: usize = 0;
        assert_eq!(lumen_query_len(sel_row.as_ptr(), &mut len), LumenStatus::Ok);
        assert_eq!(len, 2);

        // `.row` matches two -> single() must error.
        let mut bad: LumenNode = 0;
        assert_eq!(
            lumen_query_single(sel_row.as_ptr(), &mut bad),
            LumenStatus::ErrBadArg
        );

        // query -> list, iterate, free.
        let mut list = LumenNodeList {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        assert_eq!(lumen_query(sel_row.as_ptr(), &mut list), LumenStatus::Ok);
        assert_eq!(list.len, 2);
        let mut first: LumenNode = 0;
        assert_eq!(
            lumen_nodelist_get(
                LumenNodeList {
                    ptr: list.ptr,
                    len: list.len
                },
                0,
                &mut first
            ),
            LumenStatus::Ok
        );
        assert_eq!(first, save_h);
        lumen_nodelist_free(list);

        // Traversal.
        let mut parent: LumenNode = 0;
        assert_eq!(lumen_node_parent(save_h, &mut parent), LumenStatus::Ok);
        assert!(parent != 0);
        let mut next: LumenNode = 0;
        assert_eq!(lumen_node_next(save_h, &mut next), LumenStatus::Ok);
        assert_eq!(next, cancel_h);

        let mut kids = LumenNodeList {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        assert_eq!(lumen_node_children(parent, &mut kids), LumenStatus::Ok);
        assert_eq!(kids.len, 2);
        lumen_nodelist_free(kids);

        let mut closest: LumenNode = 0;
        assert_eq!(
            lumen_node_closest(save_h, sel_list.as_ptr(), &mut closest),
            LumenStatus::Ok
        );
        assert_eq!(closest, parent);

        // document + liveness.
        let mut doc: LumenNode = 0;
        assert_eq!(lumen_document(&mut doc), LumenStatus::Ok);
        assert!(doc != 0);
        let mut valid: std::ffi::c_int = -1;
        assert_eq!(lumen_node_valid(save_h, &mut valid), LumenStatus::Ok);
        assert_eq!(valid, 1);
        assert_eq!(lumen_node_valid(0, &mut valid), LumenStatus::Ok);
        assert_eq!(valid, 0);
    }
}

/// The write side over the C-ABI: a `spawn` -> `append` -> `set_attr` chain
/// queues the right commands on the external DOM bus (in issue order) for
/// the runtime to drain. No app runs here, so we assert the queued commands
/// rather than the applied tree (the applied path is covered by the
/// headless runtime test).
#[test]
fn c_abi_dom_mutation_round_trip() {
    use lumen_script::ScriptCommand;
    // Clear anything a prior test left on the bus.
    let _ = lumen_script::node_query::drain_external_dom_commands();

    let tag = CString::new("button").unwrap();
    let name = CString::new("role").unwrap();
    let value = CString::new("submit").unwrap();

    let parent: LumenNode = 42; // stand-in parent handle
    let mut child: LumenNode = 0;
    unsafe {
        assert_eq!(lumen_node_spawn(tag.as_ptr(), &mut child), LumenStatus::Ok);
        assert!(child != 0, "spawn returns a reserved handle synchronously");
        assert_eq!(lumen_node_append(parent, child), LumenStatus::Ok);
        assert_eq!(
            lumen_node_set_attr(child, name.as_ptr(), value.as_ptr()),
            LumenStatus::Ok
        );
    }

    let cmds = lumen_script::node_query::drain_external_dom_commands();
    assert_eq!(cmds.len(), 3, "spawn + append + set_attr");
    assert!(
        matches!(&cmds[0], ScriptCommand::Spawn { tag, reserved } if tag == "button" && *reserved == child)
    );
    assert!(
        matches!(&cmds[1], ScriptCommand::Insert { parent: p, node, before } if *p == parent && *node == child && *before == 0)
    );
    assert!(
        matches!(&cmds[2], ScriptCommand::SetAttr { node, name, value } if *node == child && name == "role" && value == "submit")
    );

    // The ABI advertises the write-side minor.
    assert_eq!((lumen_abi_version() >> 8) & 0xFF, 11);
}
