//! Scalar signals on the candela host: the named reactive cells `bind-text`
//! and its siblings read, driven from a script through the name-keyed
//! `signal_get_*` / `signal_set_*` builtins and the prelude's `Signal<T>`
//! method sugar.
//!
//! The sugar is prelude-only: `Signal<T>` holds the signal name, the type
//! argument picks the `impl` block, and each `get` / `set` calls the builtin
//! for that type.

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The value of the last `SetSignal` command for `name`.
fn last_write(cmds: &[ScriptCommand], name: &str) -> Option<String> {
    cmds.iter().rev().find_map(|c| match c {
        ScriptCommand::SetSignal { name: n, value } if n == name => Some(value.clone()),
        _ => None,
    })
}

/// `signal<string>(name).set(v)` writes the same cell
/// `lumen::signal_set(name, v)` does, and `get` reads it back.
#[test]
fn the_signal_handle_drives_the_same_store() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn write_it() {
    let greeting = signal<string>("greeting");
    greeting.set("hi");
}
fn read_it() {
    let greeting = signal<string>("greeting");
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

/// The type argument picks what the one `get` / `set` pair reads and writes,
/// converting across the scalar types the way the underlying builtins do.
#[test]
fn typed_pairs_round_trip() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn seed() {
    let count = signal<int>("count");
    count.set(41);
    let ratio = signal<float>("ratio");
    ratio.set(0.5);
    let done = signal<bool>("done");
    done.set(true);
}
fn bump() {
    let count = signal<int>("count");
    count.set(count.get() + 1);
}
fn count() { let c = signal<int>("count"); return c.get(); }
fn ratio() { let r = signal<float>("ratio"); return r.get(); }
fn done() { let d = signal<bool>("done"); return d.get(); }
fn count_as_float() { let c = signal<float>("count"); return c.get(); }
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

/// A color cell is typed, not a string: `signal<Color>` takes the hex form on
/// `set` and hands back the 0-255 channels on `get`.
#[test]
fn color_pair_reads_channels_back() {
    let mut host = CandelaHost::new();
    let src = r##"
import "lumen.cdl";

fn paint() {
    let accent = signal<Color>("accent");
    accent.set("#ff8800");
}
fn red() {
    let accent = signal<Color>("accent");
    return accent.get().r;
}
fn unset_alpha() {
    let nothing = signal<Color>("nothing");
    return nothing.get().a;
}
fn label() {
    let dynamic = signal<any>("greeting");
    dynamic.set("hi");
    return dynamic.get();
}
fn main() {}
"##;
    host.load(src, "color.cdl").expect("compiles");

    host.call("paint", &[]).expect("paint ok");
    assert_eq!(
        host.call("red", &[]).unwrap().ret,
        Some(ScriptValue::I64(255))
    );
    // A cell holding no color reads as transparent black rather than raising
    // on the missing channel.
    assert_eq!(
        host.call("unset_alpha", &[]).unwrap().ret,
        Some(ScriptValue::I64(0))
    );
    // The dynamic slot reads as `any` off the same string sink.
    assert_eq!(
        host.call("label", &[]).unwrap().ret,
        Some(ScriptValue::Str("hi".to_owned()))
    );
}

/// A handle type keeps its methods after the body that first named it is
/// compiled, so a write in one function and a read in another reach the same
/// pair.
///
/// Both handlers take a parameter with no type on it, so neither is compiled
/// at load and each is specialized by its own call: that is the order that
/// used to lose the methods, with the second call failing on `No method get on
/// type Signal<bool>`.
#[test]
fn a_handle_type_outlives_the_body_that_first_named_it() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn set_cell(name, v) { signal<bool>(name).set(v); }
fn is_on(name) { return signal<bool>(name).get(); }

fn seed(tag) {
    let d = 0;
    while d < 2 {
        set_cell(tag + str(d), true);
        d += 1;
    }
}
fn read(tag) { return is_on(tag + "0"); }
fn main() {}
"#;
    host.load(src, "cross_body.cdl").expect("compiles");

    let tag = || ScriptValue::Str("d".to_owned());
    host.call("seed", &[tag()]).expect("the loop body writes");
    assert_eq!(
        host.call("read", &[tag()]).unwrap().ret,
        Some(ScriptValue::Bool(true)),
        "the read is a separate compile and has to find the same `get`"
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
fn count() { let c = signal<int>("count"); return c.get(); }
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
