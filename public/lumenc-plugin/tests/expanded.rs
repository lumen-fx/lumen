//! Expands `lumenc_plugin!` inside an instrumented binary and drives the
//! generated descriptor directly, hook by hook. The fixture cdylib exercises
//! the same surface through a real dlopen, but it is built by a nested,
//! uninstrumented cargo, so this is where the export half is measured.

use lumenc_plugin::{CompilerPlugin, Ctx, Error, Finding, LayoutIR, Output, Severity, abi, codec};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cfg {
    mode: String,
}

struct TestPlugin;

impl CompilerPlugin for TestPlugin {
    fn transform_markup(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error> {
        let cfg: Cfg = ctx.config()?;
        match cfg.mode.as_str() {
            "unchanged" => Ok(None),
            "fail" => Err(Error::from("markup says no")),
            "panic" => panic!("markup panicked"),
            "panic-nonstring" => std::panic::panic_any(42usize),
            _ => Ok(Some(format!("{src}!"))),
        }
    }

    fn transform_css(&self, src: &str, _ctx: &Ctx) -> Result<Option<String>, Error> {
        Ok(Some(src.to_uppercase()))
    }

    fn transform_ir(&self, ir: &mut LayoutIR, ctx: &Ctx) -> Result<(), Error> {
        let cfg: Cfg = ctx.config()?;
        if cfg.mode == "fail" {
            return Err(Error::from("ir says no"));
        }
        ir.root.tag = "transformed".to_string();
        Ok(())
    }

    fn lint(&self, _ir: &LayoutIR, _ctx: &Ctx) -> Result<Vec<Finding>, Error> {
        Ok(vec![Finding {
            rule: "expanded".to_string(),
            severity: Severity::Warn,
            message: "from the expanded plugin".to_string(),
            file: None,
            line: 1,
            col: 1,
            suggest: None,
        }])
    }

    fn emit(&self, _ir: &LayoutIR, _ctx: &Ctx) -> Result<Vec<Output>, Error> {
        Ok(vec![Output {
            path: "out.txt".to_string(),
            bytes: b"emitted".to_vec(),
        }])
    }
}

lumenc_plugin::lumenc_plugin!(|| TestPlugin);

// The macro exports the entry as an unmangled symbol inside a private const
// block; this extern declaration links against it from the same binary.
unsafe extern "C" {
    fn lumenc_plugin_v1() -> *const abi::Desc;
}

fn desc() -> &'static abi::Desc {
    unsafe { &*lumenc_plugin_v1() }
}

fn ctx_bytes(mode: &str) -> Vec<u8> {
    let config = if mode.is_empty() {
        String::new()
    } else {
        format!("mode = \"{mode}\"")
    };
    let ctx = Ctx::new(
        std::path::PathBuf::from("/app"),
        std::path::PathBuf::from("/app/main.lmn"),
        std::path::PathBuf::from("/app/main.lmn"),
        false,
        config,
    );
    codec::encode(&ctx).unwrap()
}

/// Call one hook, return the status and the freed-through-`Desc::free`
/// payload copy.
fn call(hook: abi::HookFn, input: &[u8], ctx: &[u8]) -> (i32, Vec<u8>) {
    let mut out = abi::Buf::empty();
    let status = unsafe {
        hook(
            input.as_ptr(),
            input.len(),
            ctx.as_ptr(),
            ctx.len(),
            &mut out,
        )
    };
    let bytes = if out.ptr.is_null() {
        Vec::new()
    } else {
        let copied = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        unsafe { desc().free.unwrap()(out.ptr, out.len, out.cap) };
        copied
    };
    (status, bytes)
}

#[test]
fn the_descriptor_reports_the_crate() {
    let d = desc();
    assert_eq!(d.abi_version, abi::ABI_VERSION);
    assert_eq!(d.struct_size as usize, std::mem::size_of::<abi::Desc>());
    assert_eq!(
        d.ir_format_version,
        lumenc_plugin::lumen_ir::artifact::FORMAT_VERSION
    );
    assert_eq!(d.flags & abi::FLAG_PANIC_ABORT, 0);
    let name = unsafe { std::ffi::CStr::from_ptr(d.name) };
    assert_eq!(name.to_str().unwrap(), "lumenc-plugin");
}

#[test]
fn markup_thunk_covers_change_unchanged_and_error() {
    let hook = desc().transform_markup.unwrap();
    let (status, bytes) = call(hook, b"hello", &ctx_bytes(""));
    assert_eq!(status, abi::OK);
    assert_eq!(bytes, b"hello!");

    let (status, bytes) = call(hook, b"hello", &ctx_bytes("unchanged"));
    assert_eq!(status, abi::UNCHANGED);
    assert!(bytes.is_empty());

    let (status, bytes) = call(hook, b"hello", &ctx_bytes("fail"));
    assert_eq!(status, abi::ERR);
    assert_eq!(bytes, b"markup says no");

    // Invalid UTF-8 input is an error, not a panic.
    let (status, bytes) = call(hook, b"\xff\xfe", &ctx_bytes(""));
    assert_eq!(status, abi::ERR);
    assert!(String::from_utf8_lossy(&bytes).contains("source decode"));

    // A garbage context is refused before the hook body runs.
    let (status, bytes) = call(hook, b"hello", b"\x01\x02\x03");
    assert_eq!(status, abi::ERR);
    assert!(String::from_utf8_lossy(&bytes).contains("context decode"));
}

#[test]
fn panics_cross_as_status_codes_with_their_message() {
    let hook = desc().transform_markup.unwrap();
    let (status, bytes) = call(hook, b"x", &ctx_bytes("panic"));
    assert_eq!(status, abi::PANICKED);
    assert_eq!(bytes, b"markup panicked");

    let (status, bytes) = call(hook, b"x", &ctx_bytes("panic-nonstring"));
    assert_eq!(status, abi::PANICKED);
    assert_eq!(bytes, b"non-string panic payload");
}

#[test]
fn css_thunk_round_trips_text() {
    let hook = desc().transform_css.unwrap();
    let (status, bytes) = call(hook, b"a { b: c; }", &ctx_bytes(""));
    assert_eq!(status, abi::OK);
    assert_eq!(bytes, b"A { B: C; }");
}

#[test]
fn ir_thunk_round_trips_the_tree_and_reports_bad_bytes() {
    let hook = desc().transform_ir.unwrap();
    let ir = LayoutIR::default();
    let encoded = codec::encode(&ir).unwrap();
    let (status, bytes) = call(hook, &encoded, &ctx_bytes(""));
    assert_eq!(status, abi::OK);
    let back: LayoutIR = codec::decode(&bytes).unwrap();
    assert_eq!(back.root.tag, "transformed");

    let (status, bytes) = call(hook, &encoded, &ctx_bytes("fail"));
    assert_eq!(status, abi::ERR);
    assert_eq!(bytes, b"ir says no");

    let (status, bytes) = call(hook, b"\x00garbage", &ctx_bytes(""));
    assert_eq!(status, abi::ERR);
    assert!(String::from_utf8_lossy(&bytes).contains("IR decode"));
}

#[test]
fn lint_and_emit_thunks_encode_their_payloads() {
    let ir = codec::encode(&LayoutIR::default()).unwrap();

    let (status, bytes) = call(desc().lint.unwrap(), &ir, &ctx_bytes(""));
    assert_eq!(status, abi::OK);
    let findings: Vec<Finding> = codec::decode(&bytes).unwrap();
    assert_eq!(findings[0].rule, "expanded");
    assert_eq!(findings[0].severity, Severity::Warn);

    let (status, bytes) = call(desc().emit.unwrap(), &ir, &ctx_bytes(""));
    assert_eq!(status, abi::OK);
    let outputs: Vec<Output> = codec::decode(&bytes).unwrap();
    assert_eq!(outputs[0].path, "out.txt");
    assert_eq!(outputs[0].bytes, b"emitted");

    // Bad IR bytes surface the same way on the read-only hooks.
    let (status, _) = call(desc().lint.unwrap(), b"nope", &ctx_bytes(""));
    assert_eq!(status, abi::ERR);
    let (status, _) = call(desc().emit.unwrap(), b"nope", &ctx_bytes(""));
    assert_eq!(status, abi::ERR);
}
