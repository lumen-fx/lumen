//! `ScriptHost::register_script_fn` on the candela host: a native function
//! registered through the fork's host-fn API, callable from a `.cdl` script as
//! `<namespace>::<name>(...)`. Mirrors the Rhai/Lua hosts' extension point.
//!
//! candela resolves such a call through a `host` declaration, so the host
//! writes one for every namespace it bound; the tests below cover both that and
//! the app that brings its own block.

use lumen_script::{
    ScriptCommand, ScriptError, ScriptFn, ScriptHost, ScriptNs, ScriptTy, ScriptValue,
};
use lumen_script_candela::CandelaHost;

/// Join the marshalled args into a `Print` command so the test can observe
/// exactly what crossed the boundary. Lands in the `lumen` namespace, the one
/// the fixtures declare.
fn joining_script_fn() -> ScriptFn {
    ScriptFn::new("log_it")
        .ns(ScriptNs::Builtin)
        .variadic()
        .build(|cx| {
            let joined = cx
                .args()
                .iter()
                .map(ScriptValue::stringify)
                .collect::<Vec<_>>()
                .join(",");
            cx.emit(ScriptCommand::Print(joined));
            Ok(ScriptValue::Unit)
        })
}

/// The pin number a `gpio::level` call passed.
fn pin_number(args: &[ScriptValue]) -> i64 {
    match args.first() {
        Some(ScriptValue::I64(n)) => *n,
        _ => 0,
    }
}

fn prints(cmds: &[ScriptCommand]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_registered_fn_is_callable_from_script_and_emits_its_command() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn on_start() {
    lumen::log_it("tag", 42, true);
}
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");

    let outcome = host.call("on_start", &[]).expect("on_start");
    assert!(outcome.found);
    // The mixed-typed positional args marshalled string/int/bool into the fn.
    assert_eq!(prints(&outcome.commands), vec!["tag,42,true".to_owned()]);
}

#[test]
fn one_registration_serves_any_arity() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn none() { lumen::log_it(); }
fn one() { lumen::log_it("solo"); }
fn many() { lumen::log_it("a", "b", "c", "d"); }
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");

    assert_eq!(
        prints(&host.call("none", &[]).unwrap().commands),
        vec![String::new()]
    );
    assert_eq!(
        prints(&host.call("one", &[]).unwrap().commands),
        vec!["solo".to_owned()]
    );
    assert_eq!(
        prints(&host.call("many", &[]).unwrap().commands),
        vec!["a,b,c,d".to_owned()]
    );
}

/// A shared builtin binds as a typed host function, so a declaration that
/// disagrees with it fails the compile.
///
/// This is what the shape adapter buys: were the entry bound variadically, the
/// wrong declaration below would compile and the mismatch would surface as a
/// runtime surprise. candela checks the argument list at the boundary, not at
/// the call site, so this is where the check lands.
#[test]
fn a_shared_builtin_is_bound_under_its_declared_shape() {
    let good = r#"
host "lumen" {
    set_text(string, string);
}
fn on_start() { lumen::set_text("out", "hi"); }
fn main() {}
"#;
    let wrong_arity = r#"
host "lumen" {
    set_text(string);
}
fn on_start() { lumen::set_text("out"); }
fn main() {}
"#;
    let variadic = r#"
host "lumen" {
    any set_text(...);
}
fn on_start() { lumen::set_text("out", "hi"); }
fn main() {}
"#;

    let mut host = CandelaHost::new();
    host.load(good, "good.cdl")
        .expect("the declared shape compiles");

    for (src, name) in [(wrong_arity, "wrong arity"), (variadic, "variadic")] {
        let mut host = CandelaHost::new();
        assert!(
            host.load(src, "bad.cdl").is_err(),
            "a {name} declaration must not bind to a typed registration"
        );
    }
}

/// A reset drops the program, not the embedder's registrations.
#[test]
fn a_registered_function_survives_a_reset() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn shout() { lumen::log_it("after"); }
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");
    ScriptHost::reset(&mut host);
    host.load(src, "cmd.cdl").expect("load again");

    assert_eq!(
        prints(&host.call("shout", &[]).expect("shout runs").commands),
        vec!["after".to_owned()]
    );
}

/// A registered function needs no declaration in the app.
///
/// The host knows every signature it bound, so it writes the `host` block the
/// call resolves through. This is what lets a plugin add a function an app
/// calls without the author repeating its shape.
#[test]
fn a_registered_fn_is_declared_by_the_host() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&ScriptFn::value("answer", 0, |_| ScriptValue::I64(42)))
        .expect("register");

    let src = "fn ask() { return native::answer(); }\nfn main() {}\n";
    host.load(src, "auto.cdl")
        .expect("the registered fn is declared for the app");
    assert_eq!(
        host.call("ask", &[]).expect("ask runs").ret,
        Some(ScriptValue::I64(42))
    );
}

/// A namespace of the plugin's choosing is declared the same way.
#[test]
fn a_named_namespace_is_declared_by_the_host() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::value("level", 1, |args| ScriptValue::I64(pin_number(args) * 2))
            .with_ns(ScriptNs::Named("gpio".to_owned())),
    )
    .expect("register");

    host.load(
        "fn go() { return gpio::level(21); }\nfn main() {}\n",
        "ns.cdl",
    )
    .expect("the named namespace is declared for the app");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(42))
    );
}

/// An app that declares the namespace itself keeps its own block.
///
/// A `.cdl` written before the host declared anything, and a `.cdlb` built from
/// one, both carry a hand-written block; a second block for the same namespace
/// would stop them compiling.
#[test]
fn a_hand_written_block_is_left_alone() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn().with_ns(ScriptNs::Extension))
        .expect("register");

    let src = r#"
host "native" {
    any log_it(...);
}
fn shout() { native::log_it("hand-written"); }
fn main() {}
"#;
    host.load(src, "hand.cdl")
        .expect("the app's own block still compiles");
    assert_eq!(
        prints(&host.call("shout", &[]).expect("shout runs").commands),
        vec!["hand-written".to_owned()]
    );
}

/// What the host puts in front of the source costs the author nothing: an
/// error still reports the line it is on.
#[test]
fn the_synthesized_blocks_do_not_shift_line_numbers() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&ScriptFn::value("answer", 0, |_| ScriptValue::I64(42)))
        .expect("register");

    // A wrapper runs to several lines, so the offset it costs is what the
    // author would otherwise see added to every diagnostic.
    host.add_prelude(
        "native",
        "fn helper_one() { return 1; }\nfn helper_two() { return 2; }\n",
    );

    let src = "import \"lumen.cdl\";\nfn main() {}\nfn broken( {}\n";
    match host
        .load(src, "lines.cdl")
        .expect_err("line 3 is malformed")
    {
        ScriptError::Compile { line, col, uri, .. } => {
            assert_eq!((line, col), (3, 12));
            assert_eq!(uri, "lines.cdl");
        }
        other => panic!("expected a compile error, got {other:?}"),
    }
}

/// A plugin's own `.cdl` compiles ahead of the app, so a script calls the
/// method form of what the plugin registered.
#[test]
fn a_plugin_wrapper_is_compiled_ahead_of_the_app() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::value("level", 1, |args| ScriptValue::I64(pin_number(args) * 2))
            .with_ns(ScriptNs::Named("gpio".to_owned())),
    )
    .expect("register");
    host.add_prelude(
        "gpio",
        r#"
struct Pin { number: int }
fn pin(number) { return Pin { number: number }; }
impl Pin {
    fn level(self) { return gpio::level(self.number); }
}
"#,
    );

    host.load(
        "fn go() { return pin(21).level(); }\nfn main() {}\n",
        "sugar.cdl",
    )
    .expect("the wrapper compiles ahead of the app");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(42))
    );
}

/// An error inside a wrapper is the plugin's, and the message says so rather
/// than pointing at a line the author never wrote.
#[test]
fn an_error_in_a_wrapper_names_the_plugin() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::value("level", 1, |_| ScriptValue::I64(0))
            .with_ns(ScriptNs::Named("gpio".to_owned())),
    )
    .expect("register");
    host.add_prelude("gpio", "fn broken( {}\n");

    match host
        .load("fn main() {}\n", "app.cdl")
        .expect_err("the wrapper is malformed")
    {
        ScriptError::Compile { uri, .. } => {
            assert!(
                uri.contains("gpio"),
                "the message must name the plugin: {uri}"
            );
        }
        other => panic!("expected a compile error, got {other:?}"),
    }
}

/// `compile_check` checks against what the app will run.
///
/// candela binds every `host` block while it compiles, so a check on an engine
/// carrying only Lumen's own builtins fails on a source declaring
/// `host "native" { .. }` while the app it rejects runs fine. The host replays
/// its registrations into the scratch engine to keep the two verdicts the same.
#[test]
fn compile_check_sees_the_registered_native_functions() {
    let src = r#"
host "native" {
    any answer(...);
}
fn on_start() { let n = native::answer(); }
fn main() {}
"#;

    let bare = CandelaHost::new();
    assert!(
        bare.compile_check(src, "native.cdl").is_err(),
        "with nothing registered the declaration has nothing behind it"
    );

    let mut host = CandelaHost::new();
    // The default namespace is `native`, the one the source declares.
    host.register_script_fn(&ScriptFn::value("answer", 0, |_| ScriptValue::I64(42)))
        .expect("register");
    host.compile_check(src, "native.cdl")
        .expect("the declaration binds to the registered fn");

    // And a check sees what a load would put in front of the source, so an app
    // that declares nothing gets the same verdict either way.
    host.add_prelude(
        "native",
        "fn answer_twice() { return native::answer() * 2; }\n",
    );
    host.compile_check(
        "fn ask() { return answer_twice(); }\nfn main() {}\n",
        "auto.cdl",
    )
    .expect("the check sees the synthesized block and the wrapper");
}

/// A plugin function that fails raises where it was called.
///
/// The message reaches the script whole, under candela's own `host_fn_error`
/// kind and naming the function, so an app can tell which plugin refused and
/// the runtime reports it as an ordinary script error rather than dying.
#[test]
fn a_failing_plugin_fn_raises_at_the_call_site() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::new("read")
            .ns(ScriptNs::Named("gpio".to_owned()))
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .build(|cx| match cx.int_arg(0) {
                21 => Ok(ScriptValue::I64(1)),
                pin => Err(format!("pin {pin} is not wired")),
            }),
    )
    .expect("register");
    host.load(
        "fn level(pin) { return gpio::read(pin); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("load");

    let ScriptError::Runtime(message) = host
        .call("level", &[ScriptValue::I64(7)])
        .expect_err("the plugin refused")
    else {
        panic!("a failing plugin function is a runtime error");
    };
    assert!(
        message.contains("gpio::read") && message.contains("pin 7 is not wired"),
        "the message has to name the function and carry what it said: {message}"
    );

    // The app is still alive, and the call that does not fail still answers.
    assert_eq!(
        host.call("level", &[ScriptValue::I64(21)])
            .expect("21 is wired")
            .ret,
        Some(ScriptValue::I64(1))
    );
}

/// The failure is an ordinary candela runtime error, so a script catches it.
#[test]
fn a_script_catches_a_failing_plugin_fn() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::new("read")
            .ns(ScriptNs::Named("gpio".to_owned()))
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .build(|_| Err("the bus is down".to_owned())),
    )
    .expect("register");
    host.load(
        "fn level(pin) {\n\
         \x20   try { return gpio::read(pin); }\n\
         \x20   catch \"host_fn_error\" { return -1; }\n\
         }\n\
         fn main() {}\n",
        "app.cdl",
    )
    .expect("load");

    assert_eq!(
        host.call("level", &[ScriptValue::I64(7)])
            .expect("caught")
            .ret,
        Some(ScriptValue::I64(-1))
    );
}

/// A wrapper may call the prelude it is spliced in front of.
///
/// The wrappers go ahead of the app's source, and the prelude expands where the
/// app's `import` line stood, so a wrapper's `lumen::` call is written above the
/// block that declares it. candela resolves a host call through the whole
/// program rather than what precedes it, which is what makes the sugar a plugin
/// ships able to build on the runtime's own surface.
#[test]
fn a_wrapper_may_call_the_prelude_spliced_below_it() {
    let mut host = CandelaHost::new();
    host.add_prelude(
        "gpio",
        "fn announce(text: string) { lumen::print(text); return 7; }\n",
    );
    host.load(
        "import \"lumen.cdl\";\nfn go() { return announce(\"ready\"); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("the wrapper compiles above the prelude it calls");

    let outcome = host.call("go", &[]).expect("go runs");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(7)));
    assert_eq!(prints(&outcome.commands), vec!["ready".to_string()]);
}

/// Two plugins may ship sugar for the same namespace, and both compile.
///
/// One plugin registering the functions and another wrapping them is a
/// composition worth allowing, so the host keeps both sources and splices them
/// in registration order.
#[test]
fn two_wrappers_for_one_namespace_both_compile() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::new("level")
            .ns(ScriptNs::Named("gpio".to_owned()))
            .ret(ScriptTy::Int)
            .build(|_| Ok(ScriptValue::I64(3))),
    )
    .expect("register");
    host.add_prelude("gpio", "fn doubled() { return gpio::level() * 2; }\n");
    host.add_prelude("gpio", "fn tripled() { return gpio::level() * 3; }\n");
    host.load(
        "fn go() { return doubled() + tripled(); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("both wrappers compile");

    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(15))
    );
}

/// A shape no builtin uses is declared with its types, and the call is checked
/// against them.
///
/// The signature travels to candela as data, so what a plugin can declare is
/// not drawn from a list of shapes the host happens to carry. The result is
/// typed as the declaration says, and a call passing the wrong types or the
/// wrong number of arguments is refused naming the parameter.
#[test]
fn an_unusual_typed_shape_is_declared_and_checked() {
    let label = || {
        ScriptFn::new("label")
            .ns(ScriptNs::Named("gauge".to_owned()))
            .param("unit", ScriptTy::Str)
            .param("scale", ScriptTy::Int)
            .ret(ScriptTy::Str)
            .build(|cx| {
                Ok(ScriptValue::Str(format!(
                    "{}/{}",
                    cx.str_arg(0),
                    cx.int_arg(1)
                )))
            })
    };

    let mut host = CandelaHost::new();
    host.register_script_fn(&label()).expect("register");
    // Concatenating the result is what proves the declared `string` return
    // reached the compiler: an `any` would not concatenate.
    host.load(
        "fn go() -> string { return gauge::label(\"mm\", 4) + \"!\"; }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("the declared shape is the call the app makes");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::Str("mm/4!".into()))
    );

    for (what, call) in [
        (
            "the arguments the wrong way round",
            "gauge::label(4, \"mm\")",
        ),
        ("one argument short", "gauge::label(\"mm\")"),
        ("one argument too many", "gauge::label(\"mm\", 4, 9)"),
    ] {
        let mut host = CandelaHost::new();
        host.register_script_fn(&label()).expect("register");
        host.load(
            &format!("fn go() {{ return {call}; }}\nfn main() {{}}\n"),
            "app.cdl",
        )
        .expect("load");
        let err = host
            .call("go", &[])
            .expect_err("the declaration does not describe this call")
            .to_string();
        assert!(
            err.contains("label"),
            "{what}: the message has to name the function: {err}"
        );
    }
}
