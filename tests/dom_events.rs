//! C-ABI proof of the dynamic DOM event side (ABI 0.10). Registers a C
//! callback with `lumen_on`, injects a synthetic event through the shared
//! propagation driver, and reads the event back through the accessors.

use lumen::*;
use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_core::prelude::{Entity, World};
use lumen_script::event::{self, EventData};
use std::ffi::{CString, c_void};

fn rec(
    entity: Entity,
    tag: &str,
    id: Option<&str>,
    parent: Option<Entity>,
    kids: &[Entity],
) -> DomRecord {
    DomRecord {
        entity,
        generation: entity.generation().to_bits(),
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: vec![],
        parent,
        children: kids.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    }
}

#[derive(Default)]
struct Capture {
    fired: u32,
    target: u64,
    button: i64,
    key: String,
}

unsafe extern "C" fn on_click(event: *const LumenEvent, user_data: *mut c_void) {
    let cap = unsafe { &mut *(user_data as *mut Capture) };
    let ev = unsafe { &*event };
    cap.fired += 1;
    cap.target = ev.target;
    cap.button = ev.button;
    // Read the string field through the accessor (out-buffer convention).
    let mut buf = [0i8; 64];
    let mut len = 0usize;
    let status = unsafe { lumen_event_key(buf.as_mut_ptr(), buf.len(), &mut len) };
    if status == LumenStatus::Ok {
        let bytes: Vec<u8> = buf[..len].iter().map(|&b| b as u8).collect();
        cap.key = String::from_utf8_lossy(&bytes).into_owned();
    }
    // Exercise a propagation control from C (no-op for this single node).
    let _ = lumen_event_stop_propagation();
}

#[test]
fn c_abi_event_surface() {
    assert_eq!(
        (lumen_abi_version() >> 8) & 0xFF,
        lumen::LUMEN_ABI_MINOR,
        "packed ABI minor matches the exported constant"
    );
    event::clear_all_bindings();

    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), None, &[btn]),
        rec(btn, "button", Some("btn"), Some(root), &[]),
    ]));
    let btn_handle = NodeHandle::new(btn).pack();

    let mut cap = Capture::default();
    let ud = &mut cap as *mut Capture as *mut c_void;
    let etype = CString::new("click").unwrap();
    let token = unsafe { lumen_on(btn_handle, etype.as_ptr(), 0, Some(on_click), ud) };
    assert_ne!(token, 0, "lumen_on returns a token");

    // Inject a synthetic click; native bindings fire directly in the driver.
    let data = EventData {
        event_type: "click".into(),
        target: btn_handle,
        button: 2,
        key: "".into(),
        ..Default::default()
    };
    event::dispatch(data, &[], true, |_| {});

    assert_eq!(cap.fired, 1, "callback fired once");
    assert_eq!(cap.target, btn_handle, "target reads back");
    assert_eq!(cap.button, 2, "button reads back");

    // Unbind: a second injection delivers nothing.
    assert_eq!(lumen_off(token), LumenStatus::Ok);
    let data2 = EventData {
        event_type: "click".into(),
        target: btn_handle,
        ..Default::default()
    };
    event::dispatch(data2, &[], true, |_| {});
    assert_eq!(cap.fired, 1, "unbound callback does not fire again");

    event::clear_all_bindings();
}
