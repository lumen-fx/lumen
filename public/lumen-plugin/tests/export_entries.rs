//! Drives the generated export surface in-process.
//!
//! The fixture cdylib exercises these same entries end to end, but it is
//! built by a nested cargo without instrumentation, so nothing it runs is
//! measured. Here the [`lumen_plugin!`] expansion links into this test
//! binary instead, the no-mangle entry is reached through a local extern
//! block, and every dispatch arm runs where the profiler can see it.
//!
//! One process holds one registration, so everything runs in a single test
//! in a fixed order: the before-init failures first, the successful init,
//! the call arms, the double-init refusal, and the shutdown last.

use std::ffi::CStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lumen_core::property_store::{PropertyKey, PropertyValue};
use lumen_plugin::abi::{Buf, Desc, ERR, HostVtable, OK, PANICKED};
use lumen_plugin::{
    Call, CallOut, Cx, Error, InitCx, Manifest, PluginEvent, PluginFn, Registrar, RuntimePlugin,
    ScriptCommand, ScriptTy, ScriptValue, codec, export,
};

static SHUTDOWN_RAN: AtomicBool = AtomicBool::new(false);

struct TestPlugin;

impl RuntimePlugin for TestPlugin {
    fn register(&self, r: &mut Registrar, cx: &InitCx) -> Result<(), Error> {
        assert_eq!(cx.app_id, "export-harness");
        // The host handle works during registration: both directions reach
        // the vtable this test installed.
        r.host()
            .log(lumen_plugin::abi::LogLevel::Info, "registering");
        assert!(r.host().emit(vec![ScriptCommand::Print("hello".into())]));
        r.script_fn(
            PluginFn::new("add")
                .param("a", ScriptTy::Int)
                .param("b", ScriptTy::Int)
                .ret(ScriptTy::Int)
                .build(|cx| {
                    cx.emit(ScriptCommand::Print("adding".into()));
                    Ok(ScriptValue::I64(cx.int_arg(0) + cx.int_arg(1)))
                }),
        );
        r.script_fn(PluginFn::new("boom").build(|_| panic!("the body panicked")));
        r.script_fn(PluginFn::new("custom").build(|cx: &mut Cx| {
            cx.emit(ScriptCommand::SetProperty {
                key: PropertyKey::Global("k".into()),
                value: PropertyValue::Custom(std::sync::Arc::new(5u8)),
            });
            Ok(ScriptValue::Bool(true))
        }));
        r.prelude("candela", "harness", "fn wrap() {}");
        Ok(())
    }

    fn shutdown(&self) {
        SHUTDOWN_RAN.store(true, Ordering::SeqCst);
        panic!("a panicking shutdown must be swallowed");
    }
}

lumen_plugin::lumen_plugin!(|| TestPlugin);

unsafe extern "C" {
    fn lumen_plugin_v1() -> *const Desc;
}

/// What the host vtable below recorded.
static EMITTED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
static LOGGED: Mutex<Vec<(i32, String)>> = Mutex::new(Vec::new());
static WOKEN: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn emit_event(_ctx: *mut std::ffi::c_void, p: *const u8, len: usize) -> i32 {
    let bytes = unsafe { std::slice::from_raw_parts(p, len) }.to_vec();
    EMITTED.lock().unwrap().push(bytes);
    OK
}

unsafe extern "C" fn log(_ctx: *mut std::ffi::c_void, level: i32, p: *const u8, len: usize) {
    let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, len) }).into_owned();
    LOGGED.lock().unwrap().push((level, text));
}

unsafe extern "C" fn wake(_ctx: *mut std::ffi::c_void) {
    WOKEN.fetch_add(1, Ordering::SeqCst);
}

fn vtable() -> HostVtable {
    HostVtable {
        struct_size: std::mem::size_of::<HostVtable>() as u32,
        _pad: 0,
        ctx: std::ptr::null_mut(),
        emit_event: Some(emit_event),
        log: Some(log),
        wake: Some(wake),
    }
}

/// Read a hook's answer out and hand the buffer back through the plugin's
/// own free entry, as the engine does.
fn take(desc: &Desc, buf: Buf) -> Vec<u8> {
    if buf.ptr.is_null() {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
    unsafe { desc.free.expect("a descriptor carries free")(buf.ptr, buf.len, buf.cap) };
    bytes
}

fn ctx_bytes() -> Vec<u8> {
    codec::encode(&InitCx::new(
        std::env::temp_dir(),
        "export-harness".to_string(),
        true,
        false,
        "0.0.0".to_string(),
        String::new(),
    ))
    .expect("the context encodes")
}

struct RefusingPlugin;

impl RuntimePlugin for RefusingPlugin {
    fn register(&self, _r: &mut Registrar, _cx: &InitCx) -> Result<(), Error> {
        Err(Error::from("this plugin refuses to come up"))
    }
}

struct PanickingPlugin;

impl RuntimePlugin for PanickingPlugin {
    fn register(&self, _r: &mut Registrar, _cx: &InitCx) -> Result<(), Error> {
        panic!("register blew up");
    }
}

fn call(desc: &Desc, bytes: &[u8]) -> (i32, Vec<u8>) {
    let mut out = Buf::empty();
    let status = unsafe {
        desc.call.expect("a descriptor carries call")(
            bytes.as_ptr(),
            bytes.len(),
            std::ptr::null(),
            0,
            &mut out,
        )
    };
    (status, take(desc, out))
}

fn call_fn(desc: &Desc, index: u32, args: Vec<ScriptValue>) -> (i32, Vec<u8>) {
    let bytes = codec::encode(&Call { index, args }).expect("the call encodes");
    call(desc, &bytes)
}

#[test]
fn the_exported_descriptor_dispatches_every_arm() {
    let desc = unsafe { &*lumen_plugin_v1() };

    // The handshake fields carry what the macro stamped.
    assert_eq!(desc.abi_version, lumen_plugin::abi::ABI_VERSION);
    assert_eq!(desc.struct_size, std::mem::size_of::<Desc>() as u32);
    assert_eq!(desc.script_wire_version, lumen_plugin::SCRIPT_WIRE_VERSION);
    assert_eq!(desc.flags, 0);
    assert_eq!(
        unsafe { CStr::from_ptr(desc.name) }.to_str().unwrap(),
        "lumen-plugin"
    );
    assert!(
        !unsafe { CStr::from_ptr(desc.version) }
            .to_str()
            .unwrap()
            .is_empty()
    );

    // Before init: shutdown is a no-op, a call is refused.
    unsafe { desc.shutdown.expect("a descriptor carries shutdown")() };
    assert!(!SHUTDOWN_RAN.load(Ordering::SeqCst));
    let (status, msg) = call_fn(desc, 0, Vec::new());
    assert_eq!(status, ERR);
    assert_eq!(String::from_utf8_lossy(&msg), "called before init");

    // Init with bytes that are not a context.
    let table = vtable();
    let mut out = Buf::empty();
    let status = unsafe {
        desc.init.expect("a descriptor carries init")(b"garbage".as_ptr(), 7, &table, &mut out)
    };
    assert_eq!(status, ERR);
    let msg = String::from_utf8_lossy(&take(desc, out)).into_owned();
    assert!(msg.contains("context decode"), "{msg}");

    // A register that answers an error fails the init without registering,
    // and a register that panics is caught on this side of the boundary.
    // Both run through the entry directly: the macro holds one instance per
    // expansion, and these plugins must not become it.
    let ctx = ctx_bytes();
    let mut out = Buf::empty();
    let status = unsafe {
        export::init_entry(
            || {
                static REFUSING: RefusingPlugin = RefusingPlugin;
                &REFUSING
            },
            ctx.as_ptr(),
            ctx.len(),
            &table,
            &mut out,
        )
    };
    assert_eq!(status, ERR);
    let msg = String::from_utf8_lossy(&take(desc, out)).into_owned();
    assert!(msg.contains("refuses to come up"), "{msg}");

    let mut out = Buf::empty();
    let status = unsafe {
        export::init_entry(
            || {
                static PANICKING: PanickingPlugin = PanickingPlugin;
                &PANICKING
            },
            ctx.as_ptr(),
            ctx.len(),
            &table,
            &mut out,
        )
    };
    assert_eq!(status, PANICKED);
    let msg = String::from_utf8_lossy(&take(desc, out)).into_owned();
    assert!(msg.contains("register blew up"), "{msg}");

    // The real init: the manifest lists what register declared, in order.
    let mut out = Buf::empty();
    let status = unsafe { desc.init.expect("init")(ctx.as_ptr(), ctx.len(), &table, &mut out) };
    assert_eq!(status, OK);
    let manifest: Manifest = codec::decode(&take(desc, out)).expect("the manifest decodes");
    let names: Vec<&str> = manifest.fns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["add", "boom", "custom"]);
    assert_eq!(manifest.preludes.len(), 1);
    assert!(manifest.capabilities.is_empty());

    // Registration reached the vtable in both directions.
    let logged = LOGGED.lock().unwrap();
    assert!(logged.iter().any(|(_, m)| m.contains("registering")));
    drop(logged);
    let emitted = EMITTED.lock().unwrap();
    let event: PluginEvent =
        codec::decode(emitted.first().expect("emit reached the host")).expect("the event decodes");
    assert!(matches!(event, PluginEvent::Commands(c) if c.len() == 1));
    drop(emitted);
    assert!(WOKEN.load(Ordering::SeqCst) > 0);

    // A second load of the same library is refused.
    let mut out = Buf::empty();
    let status = unsafe { desc.init.expect("init")(ctx.as_ptr(), ctx.len(), &table, &mut out) };
    assert_eq!(status, ERR);
    let msg = String::from_utf8_lossy(&take(desc, out)).into_owned();
    assert!(msg.contains("already initialized"), "{msg}");

    // The call arms: undecodable input, an index past the table, a working
    // body with its emitted commands, a panicking body, and a body whose
    // answer cannot cross the boundary.
    let (status, msg) = call(desc, b"not a call");
    assert_eq!(status, ERR);
    assert!(String::from_utf8_lossy(&msg).contains("call decode"));

    let (status, msg) = call_fn(desc, 99, Vec::new());
    assert_eq!(status, ERR);
    let msg = String::from_utf8_lossy(&msg).into_owned();
    assert!(msg.contains("no function at index 99"), "{msg}");
    assert!(msg.contains("registered 3"), "{msg}");

    let (status, bytes) = call_fn(desc, 0, vec![ScriptValue::I64(3), ScriptValue::I64(4)]);
    assert_eq!(status, OK);
    let out: CallOut = codec::decode(&bytes).expect("the outcome decodes");
    assert_eq!(out.ret, Ok(ScriptValue::I64(7)));
    assert!(matches!(
        out.commands.as_slice(),
        [ScriptCommand::Print(m)] if m == "adding"
    ));

    let (status, msg) = call_fn(desc, 1, Vec::new());
    assert_eq!(status, PANICKED);
    assert!(String::from_utf8_lossy(&msg).contains("the body panicked"));

    let (status, msg) = call_fn(desc, 2, Vec::new());
    assert_eq!(status, ERR);
    let msg = String::from_utf8_lossy(&msg).into_owned();
    assert!(msg.contains("result encode"), "{msg}");
    assert!(msg.contains("custom"), "{msg}");

    // Shutdown runs the plugin's own hook and swallows its panic.
    unsafe { desc.shutdown.expect("shutdown")() };
    assert!(SHUTDOWN_RAN.load(Ordering::SeqCst));
}
