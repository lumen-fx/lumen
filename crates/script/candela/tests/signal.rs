//! Scalar signals on the candela host: the named reactive cells `bind-text`
//! and its siblings read, driven from a script through the name-keyed
//! `signal_get_*` / `signal_set_*` builtins and the prelude's `Signal` method
//! sugar.
//!
//! The sugar is prelude-only: `Signal` holds the signal name and each method
//! calls the matching builtin, the same shape `ArraySignal` uses.

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The value of the last `SetSignal` command for `name`.
fn last_write(cmds: &[ScriptCommand], name: &str) -> Option<String> {
    cmds.iter().rev().find_map(|c| match c {
        ScriptCommand::SetSignal { name: n, value } if n == name => Some(value.clone()),
        _ => None,
    })
}

/// `signal(name).set(v)` writes the same cell `lumen::signal_set(name, v)`
/// does, and `get` reads it back.
#[test]
fn the_signal_handle_drives_the_same_store() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn write_it() {
    let greeting = signal("greeting");
    greeting.set("hi");
}
fn read_it() {
    let greeting = signal("greeting");
    return greeting.get();
}
fn read_free() { return lumen::signal_get("greeting"); }
fn main() {}
"#;
    host.load(src, "scalar.cdl")
        .expect("compiles via the prelude");

    let out = host.call("write_it", &[]).expect("write ok");
    assert_eq!(last_write(&out.commands, "greeting").as_deref(), Some("hi"));
    assert_eq!(
        host.call("read_it", &[]).unwrap().ret,
        Some(ScriptValue::Str("hi".to_owned()))
    );
    assert_eq!(
        host.call("read_free", &[]).unwrap().ret,
        Some(ScriptValue::Str("hi".to_owned()))
    );
}

/// The typed pairs read and write the same cell, converting across the scalar
/// types the way the underlying builtins do.
#[test]
fn typed_pairs_round_trip() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    let count = signal("count");
    count.set_int(41);
    let ratio = signal("ratio");
    ratio.set_float(0.5);
    let done = signal("done");
    done.set_bool(true);
}
fn bump() {
    let count = signal("count");
    count.set_int(count.get_int() + 1);
}
fn count() { let c = signal("count"); return c.get_int(); }
fn ratio() { let r = signal("ratio"); return r.get_float(); }
fn done() { let d = signal("done"); return d.get_bool(); }
fn count_as_float() { let c = signal("count"); return c.get_float(); }
fn main() {}
"#;
    host.load(src, "typed.cdl").expect("compiles");

    host.call("seed", &[]).expect("seed ok");
    host.call("bump", &[]).expect("bump ok");

    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(42))
    );
    assert_eq!(
        host.call("ratio", &[]).unwrap().ret,
        Some(ScriptValue::F64(0.5))
    );
    assert_eq!(
        host.call("done", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
    // A getter converts across the scalar types.
    assert_eq!(
        host.call("count_as_float", &[]).unwrap().ret,
        Some(ScriptValue::F64(42.0))
    );
}

/// A color cell is typed, not a string: `set_color` takes the hex form and
/// `get_color` hands back the 0-255 channel map.
#[test]
fn color_pair_reads_channels_back() {
    let mut host = CandelaHost::new();
    let src = r##"
import "lumen.cdl";

fn paint() {
    let accent = signal("accent");
    accent.set_color("#ff8800");
}
fn red() {
    let accent = signal("accent");
    let c = accent.get_color();
    return c.get("r");
}
fn main() {}
"##;
    host.load(src, "color.cdl").expect("compiles");

    host.call("paint", &[]).expect("paint ok");
    assert_eq!(
        host.call("red", &[]).unwrap().ret,
        Some(ScriptValue::I64(255))
    );
}

/// The cell lives in the same mirror the host side writes through, so a value
/// set from `ScriptContext` is visible to the handle and the other way round.
#[test]
fn the_mirror_is_shared_with_the_host_side_context() {
    use lumen_script::ScriptContext;

    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn count() { let c = signal("count"); return c.get_int(); }
fn main() {}
"#;
    host.load(src, "shared.cdl").expect("compiles");

    let mut ctx = lumen_script_candela::CandelaScriptContext::new(&mut host);
    ctx.set("count", ScriptValue::I64(7));

    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(7))
    );
}
