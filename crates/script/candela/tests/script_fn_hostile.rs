//! What a plugin can name its functions, and what happens when it names one
//! something candela cannot spell.
//!
//! The candela host writes a `host "<ns>" { .. }` declaration for every
//! namespace an embedder registered under and puts it in front of the app's own
//! source. That makes a plugin's choice of name part of the app's program: a
//! name the grammar rejects would fail the app's compile, with the diagnostic
//! pointing at a line the author never wrote. These pin the outcome instead;
//! the registration is refused, and the app compiles and runs without it.

use lumen_script::{ScriptError, ScriptFn, ScriptHost, ScriptNs, ScriptTy, ScriptValue};
use lumen_script_candela::CandelaHost;

/// A host with one function registered under `ns::name`, or the error that
/// refused it.
fn host_with(ns: &str, name: &str) -> Result<CandelaHost, ScriptError> {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::value(name, 0, |_| ScriptValue::I64(7)).with_ns(ScriptNs::Named(ns.to_owned())),
    )?;
    Ok(host)
}

/// A name candela cannot declare is refused, and the app still compiles.
///
/// Each of these reaches the declaration through a different part of the
/// grammar: an operator inside an identifier, a keyword where a function name
/// goes, a quote that closes the namespace string early, and the empty name
/// that leaves the return type standing alone.
#[test]
fn a_name_candela_cannot_declare_is_refused_rather_than_breaking_the_app() {
    for (ns, name) in [
        ("gpio", "my-fn"),
        ("gpio", "if"),
        ("gpio", ""),
        ("ev\"il", "level"),
        ("gpio", "a\"); } fn hacked() { return native::x("),
    ] {
        let err = host_with(ns, name)
            .err()
            .unwrap_or_else(|| panic!("`{ns}::{name}` must not be accepted"));
        let text = err.to_string();
        assert!(
            text.contains("cannot declare"),
            "the message has to say what was refused: {text}"
        );
        assert!(
            text.contains(ns),
            "the message has to name the namespace so the plugin is identifiable: {text}"
        );

        // The refusal is the whole cost: an app carrying no reference to the
        // function compiles and runs exactly as it would have.
        let mut host = CandelaHost::new();
        let _ = host.register_script_fn(
            &ScriptFn::value(name, 0, |_| ScriptValue::I64(7))
                .with_ns(ScriptNs::Named(ns.to_owned())),
        );
        host.load("fn go() { return 1; }\nfn main() {}\n", "app.cdl")
            .unwrap_or_else(|e| panic!("`{ns}::{name}` left the app uncompilable: {e}"));
        assert_eq!(
            host.call("go", &[]).expect("go runs").ret,
            Some(ScriptValue::I64(1))
        );
    }
}

/// A namespace no call can spell is still accepted.
///
/// A namespace is a string literal in the declaration, so candela takes one
/// with a hyphen or a keyword in it. No `my-plugin::level()` call parses, so
/// the function is unreachable, but nothing the app does is affected and there
/// is no reason to refuse the registration.
#[test]
fn a_namespace_no_call_can_spell_costs_the_app_nothing() {
    for ns in ["my-plugin", "if", ""] {
        let mut host = host_with(ns, "level").unwrap_or_else(|e| panic!("`{ns}` was refused: {e}"));
        host.load("fn go() { return 1; }\nfn main() {}\n", "app.cdl")
            .unwrap_or_else(|e| panic!("`{ns}` left the app uncompilable: {e}"));
    }
}

/// A plugin cannot displace a namespace the runtime owns.
///
/// The prelude declares `window`, `document` and `history`. A second block for
/// one of them compiled clean and then failed every call into it at run time,
/// so an app that imported the prelude lost the whole namespace to a plugin
/// that happened to pick the name. The prelude's block is now the one that
/// stands.
#[test]
fn a_plugin_namespace_does_not_displace_the_runtime_s_own() {
    let mut host = host_with("window", "thing").expect("register");
    let src = "import \"lumen.cdl\";\n\
               fn title() { return window::title(); }\n\
               fn dpr() { return window::dpr(); }\n\
               fn main() {}\n";
    host.load(src, "app.cdl").expect("load");

    assert_eq!(
        host.call("title", &[]).expect("title runs").ret,
        Some(ScriptValue::Str(String::new())),
        "the prelude's own `window` surface still resolves"
    );
    assert!(
        host.call("dpr", &[]).is_ok(),
        "every function in the namespace, not only the first"
    );
}

/// Without the prelude nothing else claims the name, so the plugin keeps it.
#[test]
fn a_runtime_namespace_is_the_plugin_s_when_the_app_does_not_import_the_prelude() {
    let mut host = host_with("window", "thing").expect("register");
    host.load(
        "fn go() { return window::thing(); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("load");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(7))
    );
}

/// The later registration of a namespace and name is the one a call reaches.
///
/// Two plugins can want the same name, and one declaration is all candela gets,
/// so the store keeps one entry per name and the last body registered is behind
/// it.
#[test]
fn the_last_registration_of_a_name_is_the_one_bound() {
    let mut host = CandelaHost::new();
    for value in [1i64, 2] {
        host.register_script_fn(
            &ScriptFn::value("level", 0, move |_| ScriptValue::I64(value))
                .with_ns(ScriptNs::Named("gpio".to_owned())),
        )
        .expect("register");
    }

    host.load(
        "fn go() { return gpio::level(); }\nfn main() {}\n",
        "app.cdl",
    )
    .expect("one declaration, not two");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(2))
    );
}

/// A name candela types from its own library is refused unless the type agrees.
///
/// candela reads the bare function name out of a namespaced call and answers
/// the type from a table of its own, so `gpio::int(..)` is an `int` however the
/// plugin declared it. Assigning the result then reports a type error, and
/// using it arithmetically trips an assertion inside the VM and takes the
/// process down. Refusing the registration is what keeps that unreachable.
#[test]
fn a_name_candela_types_from_its_own_library_is_refused() {
    for name in ["int", "float", "bool", "exists", "write", "range", "append"] {
        let mut host = CandelaHost::new();
        let Err(err) = host.register_script_fn(
            &ScriptFn::new(name)
                .ns(ScriptNs::Named("gpio".to_owned()))
                .param("a", ScriptTy::Str)
                .ret(ScriptTy::Str)
                .build(|_| ScriptValue::Str("from the plugin".into())),
        ) else {
            panic!("`gpio::{name}` returns a string where candela forces another type");
        };
        let err = err.to_string();
        assert!(
            err.contains(name) && err.contains("name candela's own library does not take"),
            "the message has to say what to do about it: {err}"
        );
    }
}

/// The same name is fine when the type candela forces is the one it returns.
///
/// `read` is a string in candela's library and a string here, so nothing
/// disagrees and there is no reason to refuse it.
#[test]
fn a_library_name_whose_type_already_agrees_is_left_alone() {
    for name in ["read", "str", "input"] {
        let mut host = CandelaHost::new();
        host.register_script_fn(
            &ScriptFn::new(name)
                .ns(ScriptNs::Named("gpio".to_owned()))
                .param("a", ScriptTy::Str)
                .ret(ScriptTy::Str)
                .build(|_| ScriptValue::Str("from the plugin".into())),
        )
        .unwrap_or_else(|e| panic!("`{name}` returns what candela expects: {e}"));

        let src = format!(
            "fn go() -> string {{ let v = gpio::{name}(\"x\"); return v; }}\nfn main() {{}}\n"
        );
        host.load(&src, "app.cdl")
            .unwrap_or_else(|e| panic!("`gpio::{name}` did not compile: {e}"));
        assert_eq!(
            host.call("go", &[]).expect("go runs").ret,
            Some(ScriptValue::Str("from the plugin".into())),
            "`gpio::{name}` reached the plugin's body"
        );
    }
}

/// A name candela's library does not take is reached through its namespace.
#[test]
fn a_name_candela_s_library_does_not_take_is_reached_through_its_namespace() {
    for name in ["len", "push", "abs", "format", "level"] {
        let mut host = CandelaHost::new();
        host.register_script_fn(
            &ScriptFn::new(name)
                .ns(ScriptNs::Named("gpio".to_owned()))
                .param("a", ScriptTy::Str)
                .ret(ScriptTy::Str)
                .build(|_| ScriptValue::Str("from the plugin".into())),
        )
        .expect("register");

        let src = format!(
            "fn go() -> string {{ let v = gpio::{name}(\"x\"); return v; }}\nfn main() {{}}\n"
        );
        host.load(&src, "app.cdl")
            .unwrap_or_else(|e| panic!("`gpio::{name}` did not compile: {e}"));
        assert_eq!(
            host.call("go", &[]).expect("go runs").ret,
            Some(ScriptValue::Str("from the plugin".into())),
            "`gpio::{name}` reached the plugin's body"
        );
    }
}

/// An app function of the same bare name does not capture the namespaced call.
#[test]
fn an_app_function_of_the_same_name_does_not_capture_the_namespaced_call() {
    let mut host = CandelaHost::new();
    host.register_script_fn(
        &ScriptFn::value("level", 1, |_| ScriptValue::I64(99))
            .with_ns(ScriptNs::Named("gpio".to_owned())),
    )
    .expect("register");

    let src = "fn level(x) { return 1; }\n\
               fn go() { return gpio::level(1); }\n\
               fn main() {}\n";
    host.load(src, "app.cdl").expect("load");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(99))
    );
}
