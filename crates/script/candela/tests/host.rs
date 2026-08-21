//! Integration tests for the candela [`ScriptHost`] backend.
//!
//! Mirrors the shape of `lumen-script-rhai`'s tests: compile a small candela
//! script, drive a handler / lifecycle fn, and assert the scalar builtins and
//! host-neutral registries round-trip. candela reaches builtins through a typed
//! `host "lumen" { ... }` block, so every script here declares the builtins it
//! calls.

use lumen_script::{CallOutcome, ScriptCommand, ScriptError, ScriptHost, ScriptValue};
use lumen_script_candela::{BUILTINS, CandelaHost};

/// A script header declaring the handful of builtins the tests below call.
const HOST_BLOCK: &str = r#"
host "lumen" {
    string signal_get(string);
    signal_set(string, string);
    int signal_get_int(string);
    signal_set_int(string, int);
    add_clicks(int);
    on(string, string, string);
    audio_play(string);
    set_class(string, string);
    set_timeout(string, int);
}
"#;

fn load(host: &mut CandelaHost, body: &str) {
    let src = format!("{HOST_BLOCK}\n{body}\n");
    host.load(&src, "test.cdl")
        .unwrap_or_else(|e| panic!("script should compile: {e}"));
}

#[test]
fn on_start_dispatches_and_drains_commands() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn on_start() {
    lumen::add_clicks(3);
    lumen::signal_set("greeting", "hi");
    lumen::audio_play("track.wav");
    lumen::set_class("box", "active");
}
fn main() {}
"#,
    );

    let CallOutcome {
        commands,
        found,
        ret,
    } = host.call("on_start", &[]).expect("on_start ok");

    assert!(found, "on_start exists so found must be true");
    assert_eq!(ret, Some(ScriptValue::Unit));

    // Every scalar builtin enqueued its command.
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::AddClicks(3)))
    );
    assert!(commands.iter().any(|c| matches!(
        c,
        ScriptCommand::AudioPlay { path } if path == "track.wav"
    )));
    assert!(commands.iter().any(|c| matches!(
        c,
        ScriptCommand::SetClasses { target_id, classes } if target_id == "box" && classes == "active"
    )));
    assert!(commands.iter().any(|c| matches!(
        c,
        ScriptCommand::SetSignal { name, value } if name == "greeting" && value == "hi"
    )));
}

#[test]
fn missing_handler_is_silent_success() {
    let mut host = CandelaHost::new();
    load(&mut host, "fn main() {}");

    let outcome = host
        .call("on_definitely_not_here", &[])
        .expect("a missing fn is not an error");
    assert!(!outcome.found, "missing fn reports found = false");
    assert!(outcome.ret.is_none());
    assert!(outcome.commands.is_empty());
}

#[test]
fn signal_scalar_roundtrips_through_the_mirror() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn seed() {
    lumen::signal_set("greeting", "hello");
    lumen::signal_set_int("count", 41);
}
fn greeting() { return lumen::signal_get("greeting"); }
fn bumped() { return lumen::signal_get_int("count") + 1; }
fn main() {}
"#,
    );

    host.call("seed", &[]).expect("seed ok");

    let g = host.call("greeting", &[]).expect("greeting ok");
    assert_eq!(g.ret, Some(ScriptValue::Str("hello".to_owned())));

    let b = host.call("bumped", &[]).expect("bumped ok");
    assert_eq!(b.ret, Some(ScriptValue::I64(42)));

    // The host-side mirror sees the writes too.
    assert_eq!(
        host.mirror_get("greeting"),
        Some(ScriptValue::Str("hello".to_owned()))
    );
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(41)));
}

#[test]
fn handler_registration_and_suffix_fallback() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn on_start() {
    lumen::on("click", "save", "handle_save");
}
fn main() {}
"#,
    );
    // Nothing registered until on_start runs.
    assert_eq!(host.handler_for("click", "save"), None);

    host.call("on_start", &[]).expect("on_start ok");

    assert_eq!(
        host.handler_for("click", "save"),
        Some("handle_save".to_owned())
    );
    // Template-suffix fallback: `user-card:save` matches the `save` handler.
    assert_eq!(
        host.handler_for("click", "user-card:save"),
        Some("handle_save".to_owned())
    );
    assert_eq!(host.handler_for("click", "other"), None);
}

#[test]
fn handler_call_receives_string_arg() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn handle_save(id) {
    lumen::signal_set("last_saved", id);
}
fn main() {}
"#,
    );

    // The runtime dispatches a handler with the element id as a string arg.
    let outcome = host
        .call("handle_save", &[ScriptValue::Str("doc-1".to_owned())])
        .expect("handler ok");
    assert!(outcome.found);
    assert!(outcome.commands.iter().any(|c| matches!(
        c,
        ScriptCommand::SetSignal { name, value } if name == "last_saved" && value == "doc-1"
    )));
}

#[test]
fn timer_command_carries_delay_and_repeat() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn arm() { lumen::set_timeout("tick", 250); }
fn main() {}
"#,
    );
    let outcome = host.call("arm", &[]).expect("arm ok");
    assert!(outcome.commands.iter().any(|c| matches!(
        c,
        ScriptCommand::SetTimer { name, millis, repeat }
            if name == "tick" && *millis == 250 && !*repeat
    )));
}

#[test]
fn mirror_sync_str_parses_back_into_scalar_type() {
    let mut host = CandelaHost::new();
    // Seed a typed int mirror entry, then push a store string: it must parse
    // back into an i64, not overwrite with a string (the section 1.3 policy).
    host.mirror_set("count", ScriptValue::I64(1));
    host.mirror_sync_str("count", "5");
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(5)));

    // An unparseable string leaves the scalar untouched.
    host.mirror_sync_str("count", "not-a-number");
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(5)));

    // An absent entry takes the string verbatim.
    host.mirror_sync_str("fresh", "verbatim");
    assert_eq!(
        host.mirror_get("fresh"),
        Some(ScriptValue::Str("verbatim".to_owned()))
    );
}

#[test]
fn reset_drops_program_and_state() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn on_start() { lumen::on("click", "save", "h"); }
fn main() {}
"#,
    );
    host.call("on_start", &[]).expect("on_start ok");
    assert!(host.handler_for("click", "save").is_some());

    host.reset();
    assert!(host.handler_for("click", "save").is_none());
    // With no program loaded, calls are silent misses.
    let outcome = host.call("on_start", &[]).expect("miss ok");
    assert!(!outcome.found);
}

#[test]
fn compile_error_is_structured() {
    let mut host = CandelaHost::new();
    let err = host
        .load("fn main( { }", "broken.cdl")
        .expect_err("malformed source must fail");
    match err {
        ScriptError::Compile { uri, .. } => assert_eq!(uri, "broken.cdl"),
        other => panic!("expected a compile error, got {other:?}"),
    }
}

#[test]
fn compile_check_is_side_effect_free() {
    let mut host = CandelaHost::new();
    load(&mut host, "fn main() {}");

    // A check compiles + runs `main` on a throwaway engine, so a builtin call
    // inside `main` must not leak commands into the live sink.
    host.compile_check(
        &format!("{HOST_BLOCK}\nfn main() {{ lumen::add_clicks(9); }}\n"),
        "check.cdl",
    )
    .expect("valid source checks");
    assert!(
        host.drain_commands().is_empty(),
        "compile_check must not touch the live command sink"
    );
}

/// Parity guard: synthesize a `host \"lumen\" { ... }` block from every entry in
/// [`BUILTINS`] and compile it. candela validates each declared host fn against
/// its registered closure (arity, types, and fixed-versus-variadic), so a clean
/// compile proves the table and the registrations agree - the candela analogue
/// of the Rhai host's `gen_fn_signatures` parity test. An entry naming `any` is
/// registered variadically and declares a `...` argument list.
#[test]
fn builtins_parity() {
    let mut block = String::from("host \"lumen\" {\n");
    for b in BUILTINS {
        let args = if lumen_script_candela::builtins::is_variadic(b) {
            "...".to_owned()
        } else {
            b.params.iter().map(|p| p.ty).collect::<Vec<_>>().join(", ")
        };
        if b.ret == "()" {
            block.push_str(&format!("    {}({});\n", b.name, args));
        } else {
            block.push_str(&format!("    {} {}({});\n", b.ret, b.name, args));
        }
    }
    block.push_str("}\nfn main() {}\n");

    let mut host = CandelaHost::new();
    host.load(&block, "parity.cdl").unwrap_or_else(|e| {
        panic!("every BUILTINS entry must be a registered host fn: {e}\n{block}")
    });
}

/// The other direction: every builtin the host registers under the `lumen`
/// namespace must have a [`BUILTINS`] entry, so the LSP and the reference page
/// see the whole surface. `builtins_parity` above proves the table is a subset
/// of the registrations; this proves it is not a strict one.
///
/// The registrations come from two places and so does this list. The shared
/// table is read structurally, by name, which is exact. What is left in the
/// host's own source is found by scanning it for the forms it registers
/// through: a `register_host_fn` / `register_host_fn_variadic` call whose
/// namespace argument is `HOST_NAMESPACE`, and a `mutate!` invocation. Both
/// name the builtin with a string literal. Because that half reads source
/// text, a registration form added later is invisible to it: extend
/// `registered_names` when one appears.
#[test]
fn every_registered_lumen_fn_is_tabled() {
    /// The builtin list, scanned for registration sites. One file: both hosts
    /// register from it.
    const SRC: &str = include_str!("../src/host_fns.rs");

    /// Builtins registered from a loop over a `fname` variable rather than a
    /// string literal, so the scan cannot see them.
    const LOOP_REGISTERED: &[&str] = &["event_on", "event_on_capture"];

    /// The contents of the first string literal at or after `at`, or `None`
    /// when the argument in that position is not a literal.
    fn literal_at(src: &str, at: usize) -> Option<&str> {
        let rest = src.get(at..)?;
        let open = rest.find('"')?;
        // A `)` or `;` before the quote means this argument was a variable.
        if rest[..open].contains(')') || rest[..open].contains(';') {
            return None;
        }
        let body = &rest[open + 1..];
        let close = body.find('"')?;
        Some(&body[..close])
    }

    /// The offset just past the next non-whitespace character, when it is `c`.
    fn skip_to(src: &str, from: usize, c: char) -> Option<usize> {
        let rest = src.get(from..)?;
        let off = rest.find(|ch: char| !ch.is_whitespace())?;
        (rest[off..].starts_with(c)).then_some(from + off + c.len_utf8())
    }

    let mut names: Vec<String> = LOOP_REGISTERED.iter().map(|n| (*n).to_string()).collect();

    // The shared table, read by name rather than scanned: these bind through
    // the shape adapter, which takes the name from the entry.
    names.extend(
        lumen_script::builtin_script_fns()
            .iter()
            .filter(|f| f.visible_to("candela"))
            .map(|f| f.name.clone()),
    );

    // `register_host_fn(HOST_NAMESPACE, "name", ..)` and its variadic sibling.
    // Every other occurrence of the constant (its own declaration, the macro
    // bodies that take the name as `$name`) fails one of the two shape checks.
    for (idx, _) in SRC.match_indices("HOST_NAMESPACE") {
        let after = idx + "HOST_NAMESPACE".len();
        let Some(comma) = skip_to(SRC, after, ',') else {
            continue;
        };
        if let Some(name) = literal_at(SRC, comma) {
            names.push(name.to_string());
        }
    }

    // `mutate!("name", ..)`: the first literal in the invocation is the name.
    for (idx, _) in SRC.match_indices("mutate!(") {
        if let Some(name) = literal_at(SRC, idx + "mutate!(".len()) {
            names.push(name.to_string());
        }
    }

    assert!(
        names.len() > 100,
        "the source scan found only {} registrations - the registration form \
         probably changed and the scan needs updating",
        names.len()
    );

    let tabled: std::collections::HashSet<&str> = BUILTINS.iter().map(|b| b.name).collect();
    let mut missing: Vec<String> = names
        .into_iter()
        .filter(|n| !tabled.contains(n.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "these builtins are registered on the engine but absent from \
         builtins::BUILTINS: {missing:?}"
    );
}
