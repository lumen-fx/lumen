//! Compiling a script ahead of time and running the result without the
//! compiler.
//!
//! `CandelaHost::compile_bytecode` is the build-time half of the candela
//! host: it produces the `.cdlb` image a runtime that links only
//! `candela-vm` loads. These tests drive that image through the VM the way
//! such a runtime would, so the two halves are held to the same contract
//! from one place.

use lumen_script::{ScriptError, ScriptFn, ScriptHost, ScriptNs, ScriptTy, ScriptValue};
use lumen_script_candela::candela::{HostRegistry, LoadError, Value, load_program};
use lumen_script_candela::{CandelaHost, CandelaVmHost, HOST_NAMESPACE};

/// A program with no `host` block, so an empty registry is enough to load it.
const STANDALONE: &str = r#"
fn double(n: int) -> int {
    return n * 2;
}

fn undeclared(n) {
    return n;
}

fn main() {}
"#;

#[test]
fn a_compiled_program_runs_under_the_vm_alone() {
    let bytes = CandelaHost::new()
        .compile_bytecode(STANDALONE, "standalone.cdl")
        .expect("the program compiles");
    let hosts = HostRegistry::new();
    let mut program = load_program(&bytes, &hosts).expect("the image loads");
    program.run();

    let value = program
        .call("double", &[Value::Int(21)])
        .expect("an exported function is callable by name");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn only_an_annotated_function_is_callable_by_name() {
    let bytes = CandelaHost::new()
        .compile_bytecode(STANDALONE, "standalone.cdl")
        .expect("the program compiles");
    let hosts = HostRegistry::new();
    let program = load_program(&bytes, &hosts).expect("the image loads");
    let exports: Vec<&str> = program.exports().collect();

    assert!(
        exports.contains(&"double"),
        "a function that annotates every parameter is exported: {exports:?}"
    );
    assert!(
        !exports.contains(&"undeclared"),
        "a bare parameter has no declared type to check a host's argument \
         against, so the function is not exported: {exports:?}"
    );
    assert!(
        !exports.contains(&"main"),
        "main is the entry point, not an entry: {exports:?}"
    );
}

/// The parameter types Lumen's own handlers are written with, one function
/// each. An event handler takes the event token as an `int`, an id-routed
/// handler takes the element id as a `string`, and a control handler takes
/// the id beside the value the control carries.
#[test]
fn every_handler_shape_lumen_dispatches_is_exportable() {
    const HANDLERS: &str = r#"
fn on_event(ev: int) {}
fn on_click(id: string) {}
fn on_toggle(id: string, checked: bool) {}
fn on_slider(id: string, value: float) {}
fn on_text_input(id: string, text: string) {}
fn on_derive(theme: string, scale: float, quiet: bool) -> string { return theme; }
fn on_ready() {}

fn main() {}
"#;
    let bytes = CandelaHost::new()
        .compile_bytecode(HANDLERS, "handlers.cdl")
        .expect("the handlers compile");
    let program = load_program(&bytes, &HostRegistry::new()).expect("the image loads");
    let exports: Vec<&str> = program.exports().collect();

    for handler in [
        "on_event",
        "on_click",
        "on_toggle",
        "on_slider",
        "on_text_input",
        "on_derive",
        "on_ready",
    ] {
        assert!(
            exports.contains(&handler),
            "`{handler}` must be callable by name: {exports:?}"
        );
    }
}

#[test]
fn the_prelude_binds_its_host_functions_by_name() {
    // What the fixture script itself reaches for. The prelude declares the
    // whole Lumen surface, so a load that registers only these two must still
    // fail - but it must fail naming the rest, never these.
    let source = r#"
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "ready");
}

fn main() {}
"#;
    let bytes = CandelaHost::new()
        .compile_bytecode(source, "smoke.cdl")
        .expect("the program compiles");

    let mut hosts = HostRegistry::new();
    hosts.register_host_fn(
        HOST_NAMESPACE,
        "signal_set",
        |_name: String, _value: String| {},
    );

    let Err(error) = load_program(&bytes, &hosts) else {
        panic!("the rest of the surface is unregistered, so the load fails");
    };
    let LoadError::HostBinding(binding) = error else {
        panic!("the image decodes and only its host bindings are missing: {error}");
    };
    let text = binding.to_string();
    assert!(
        !text.contains("`signal_set`"),
        "a registered closure binds to its declaration by name: {text}"
    );
    assert!(
        text.contains("signal_get"),
        "the ones with no closure behind them are what it names: {text}"
    );
}

#[test]
fn a_program_that_does_not_compile_reports_where() {
    let error = CandelaHost::new()
        .compile_bytecode("fn main() { let x = ", "broken.cdl")
        .expect_err("an unfinished statement is a compile error");
    let ScriptError::Compile { uri, line, .. } = error else {
        panic!("a build tool gets the position, not just a message: {error:?}");
    };
    assert_eq!(uri, "broken.cdl");
    assert!(line > 0, "the line is the user's own, not the prelude's");
}

/// A build has no plugin in it, so a namespace only a plugin would declare has
/// to be spelled by the source.
///
/// The build does not object: a call into an undeclared namespace compiles, and
/// the function it sits in is left out of the image. What the author gets is
/// an app that starts and a call that is not there, so the block is the thing
/// to check when a plugin function goes missing from an artifact.
#[test]
fn a_call_into_an_undeclared_namespace_does_not_reach_the_image() {
    const UNDECLARED: &str = r#"
fn go() { return gpio::level(21); }

fn main() {}
"#;
    let bytes = CandelaHost::new()
        .compile_bytecode(UNDECLARED, "undeclared.cdl")
        .expect("the build does not object");
    let mut hosts = HostRegistry::new();
    hosts.register_host_fn("gpio", "level", |pin: i64| -> i64 { pin * 2 });

    let program = load_program(&bytes, &hosts).expect("the image loads");
    assert!(
        !program.exports().any(|name| name == "go"),
        "the function holding the undeclared call is not in the image"
    );
}

/// A declaration the runtime has no closure behind fails the load by name.
///
/// This is what an artifact does when the app was built against a plugin that
/// is not installed. The image is well formed, so the failure is a binding
/// error naming the function, not a decode error and not a panic.
#[test]
fn a_declared_plugin_function_nobody_registered_fails_the_load_by_name() {
    const DECLARED: &str = r#"
host "gpio" {
    int level(int);
}

fn go() { return gpio::level(21); }

fn main() {}
"#;
    let bytes = CandelaHost::new()
        .compile_bytecode(DECLARED, "gpio.cdl")
        .expect("the program compiles");

    let Err(error) = load_program(&bytes, &HostRegistry::new()) else {
        panic!("nothing is registered under `gpio`, so the load fails");
    };
    let LoadError::HostBinding(binding) = error else {
        panic!("the image decodes and only its host bindings are missing: {error}");
    };
    assert!(
        binding.to_string().contains("level"),
        "the error names the function that has nothing behind it: {binding}"
    );

    // The same image with the plugin present is the app that works.
    let mut hosts = HostRegistry::new();
    hosts.register_host_fn("gpio", "level", |pin: i64| -> i64 { pin * 2 });
    let mut program = load_program(&bytes, &hosts).expect("the declaration binds");
    program.run();
    assert_eq!(
        program.call("go", &[]).expect("go is callable"),
        Value::Int(42)
    );
}

/// A plugin's own namespace ([`ScriptNs::Named`]), the shape a portable
/// plugin registers under.
fn gpio_level() -> ScriptFn {
    ScriptFn::new("level")
        .ns(ScriptNs::Named("gpio".to_owned()))
        .param("pin", ScriptTy::Int)
        .ret(ScriptTy::Int)
        .build(|cx| Ok(ScriptValue::I64(cx.int_arg(0) * 2)))
}

/// A std module's namespace ([`ScriptNs::Builtin`]), the shape `std/audio`
/// registers under: it rides the prelude's own `host "lumen" { .. }` block,
/// so there is no namespace of its own to hand-write a declaration for.
fn module_echo_len() -> ScriptFn {
    ScriptFn::new("echo_len")
        .ns(ScriptNs::Builtin)
        .param("text", ScriptTy::Str)
        .ret(ScriptTy::Int)
        .build(|cx| Ok(ScriptValue::I64(cx.str_arg(0).len() as i64)))
}

/// #206: a plugin function registered before the compile is declared in the
/// image without the source hand-writing `host "gpio" { .. }`, the same fold
/// [`CandelaHost::load`] and [`CandelaHost::compile_check`] already do.
///
/// Before the fix, `compile_bytecode` never saw the registration:
/// `gpio::level` read as a call into an undeclared namespace exactly like
/// [`a_call_into_an_undeclared_namespace_does_not_reach_the_image`], so `go`
/// compiled clean but was left out of the image and calling it returned
/// nothing. That is a silent no-op an author would only find by testing the
/// shipped artifact by hand.
#[test]
fn a_registered_named_namespace_fn_needs_no_hand_written_block() {
    const SOURCE: &str = r#"
fn go() -> int { return gpio::level(21); }

fn main() {}
"#;
    let mut host = CandelaHost::new();
    host.register_script_fn(&gpio_level())
        .expect("gpio::level declares");
    let bytes = host
        .compile_bytecode(SOURCE, "gpio_plugin.cdl")
        .expect("the registration folds a declaration in, so the call resolves");

    let mut hosts = HostRegistry::new();
    hosts.register_host_fn("gpio", "level", |pin: i64| -> i64 { pin * 2 });
    let mut program = load_program(&bytes, &hosts).expect("the folded declaration binds");
    program.run();
    assert_eq!(
        program.call("go", &[]).expect("go is callable"),
        Value::Int(42)
    );
}

/// #206: a std module's function, registered under the reserved `lumen`
/// namespace, needs no `host` block at all: it rides the prelude's own,
/// exactly as it does for a live-compiled app. Before the fix this namespace
/// was already declared (by the spliced prelude) but the specific function
/// was not, which candela treats the same as a fully undeclared namespace:
/// `go` compiled but dropped out of the image silently.
#[test]
fn a_registered_module_fn_under_the_lumen_namespace_needs_no_hand_written_block() {
    const SOURCE: &str = r#"
import "lumen.cdl";

fn go() -> int { return lumen::echo_len("hello"); }

fn main() {}
"#;
    let mut host = CandelaHost::new();
    host.register_script_fn(&module_echo_len())
        .expect("lumen::echo_len declares");
    let bytes = host
        .compile_bytecode(SOURCE, "module_fn.cdl")
        .expect("the registration folds into the prelude's own lumen block");

    // The artifact host binds the closure the same way the source declared
    // it: the same registration, made again before the image loads.
    let mut vm_host = CandelaVmHost::new(bytes);
    vm_host
        .register_script_fn(&module_echo_len())
        .expect("the same registration binds before the image loads");
    vm_host.load("", "module_fn.cdl").expect("the image loads");
    let outcome = vm_host.call("go", &[]).expect("go is callable");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(5)));
}

/// #206, the failure side: a `.cdlb` declares a module function (because
/// something registered it at compile time) but nothing registers the
/// closure before the artifact loads, because the app was built against a
/// module that is not present at load time. This is a load failure naming the
/// function, not a panic and not a silent no-op: the same contract
/// [`a_declared_plugin_function_nobody_registered_fails_the_load_by_name`]
/// already holds a hand-written `host` block to.
#[test]
fn a_folded_module_fn_nobody_registers_at_load_fails_the_load_by_name() {
    const SOURCE: &str = r#"
import "lumen.cdl";

fn go() -> int { return lumen::echo_len("hello"); }

fn main() {}
"#;
    let mut host = CandelaHost::new();
    host.register_script_fn(&module_echo_len())
        .expect("lumen::echo_len declares");
    let bytes = host
        .compile_bytecode(SOURCE, "module_fn.cdl")
        .expect("the registration folds into the prelude's own lumen block");

    let mut vm_host = CandelaVmHost::new(bytes);
    let err = vm_host
        .load("", "module_fn.cdl")
        .expect_err("nothing registered echo_len for this load, so it fails");
    let message = err.to_string();
    assert!(
        message.contains("echo_len"),
        "the failure names the missing function: {message}"
    );
}
