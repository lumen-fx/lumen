//! The `import "lumen.cdl";` prelude: a `.cdl` app pulls in the entire Lumen
//! host surface with one line instead of a hand-written `host "lumen" { ... }`
//! block. Proven headless through `CandelaHost` load + dispatch (no window), the
//! same path `run_app_headless` drives via `ScriptPlugin`.

use lumen_script::{ScriptCommand, ScriptError, ScriptHost, ScriptValue};
use lumen_script_candela::{BUILTINS, CandelaHost, PRELUDE_SOURCE};

/// The prelude import alone grants the full builtin surface: an app declaring
/// no `host` block can still call `signal_set` / `on` / `add_clicks`, `on_start`
/// runs, its commands drain, and a routed handler dispatches - end to end.
#[test]
fn prelude_import_grants_builtins_without_host_block() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "hi");
    lumen::on("click", "bump", "handle_bump");
    lumen::add_clicks(2);
}

fn handle_bump(id) {
    lumen::signal_set("greeting", "clicked");
}

fn main() {}
"#;
    host.load(src, "prelude_app.cdl")
        .expect("a prelude-importing app must compile without a host block");

    let outcome = host.call("on_start", &[]).expect("on_start ok");
    assert!(outcome.found, "on_start exists");
    assert!(
        outcome
            .commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::AddClicks(2))),
        "add_clicks reached the sink"
    );
    assert!(
        outcome.commands.iter().any(|c| matches!(
            c,
            ScriptCommand::SetSignal { name, value } if name == "greeting" && value == "hi"
        )),
        "signal_set reached the sink"
    );

    // `on(...)` routed the click; dispatch the handler like the runtime would.
    assert_eq!(
        host.handler_for("click", "bump"),
        Some("handle_bump".to_owned())
    );
    let out2 = host
        .call("handle_bump", &[ScriptValue::Str("bump".to_owned())])
        .expect("handler ok");
    assert!(out2.commands.iter().any(|c| matches!(
        c,
        ScriptCommand::SetSignal { name, value } if name == "greeting" && value == "clicked"
    )));
}

/// Without the import (and without a host block) the builtins stay opt-in:
/// candela resolves host fns lazily, so the source loads, but *calling*
/// `lumen::signal_set` errors ("lumen is not a valid namespace") and emits no
/// command - the builtin surface is unreachable until explicitly imported.
#[test]
fn without_import_builtins_stay_opt_in() {
    let mut host = CandelaHost::new();
    host.load(
        "fn on_start() { lumen::signal_set(\"g\", \"v\"); }\nfn main() {}\n",
        "no_prelude.cdl",
    )
    .expect("candela resolves host fns lazily, so load itself succeeds");

    let err = host
        .call("on_start", &[])
        .expect_err("an unimported builtin call must error at runtime");
    assert!(
        matches!(err, ScriptError::Runtime(_)),
        "expected a runtime namespace error, got {err:?}"
    );
}

/// A typed builtin reached only through the prelude still type-checks and
/// round-trips: `signal_get_int` returns an `int`, usable in arithmetic.
#[test]
fn prelude_typed_signal_roundtrips() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn seed() { lumen::signal_set_int("count", 41); }
fn bumped() { return lumen::signal_get_int("count") + 1; }
fn main() {}
"#;
    host.load(src, "typed.cdl").expect("compiles via prelude");
    host.call("seed", &[]).expect("seed ok");
    let b = host.call("bumped", &[]).expect("bumped ok");
    assert_eq!(b.ret, Some(ScriptValue::I64(42)));
}

/// The single-line prelude splice keeps user line numbers intact: a syntax
/// error two lines below the import reports line 3 (via `compile_check`, whose
/// diagnostics map against the resolved source).
#[test]
fn prelude_splice_preserves_user_line_numbers() {
    let host = CandelaHost::new();
    let src = "import \"lumen.cdl\";\nfn main() {}\nfn broken( {}\n";
    let err = host
        .compile_check(src, "lines.cdl")
        .expect_err("the malformed fn on line 3 must fail");
    match err {
        ScriptError::Compile { line, .. } => {
            assert_eq!(line, 3, "prelude splice must not shift user line numbers");
        }
        other => panic!("expected a compile error, got {other:?}"),
    }
}

/// Every `.cdl` file in an app states the import it depends on, and the runtime
/// concatenates them into one candela module before it reaches the host. The
/// prelude lands once for the whole module, so the second file's import costs
/// nothing and both halves call the builtins.
#[test]
fn every_file_in_an_app_may_import_the_prelude() {
    let mut host = CandelaHost::new();
    // What `grouped_script_sources` hands the host for a two-file app.
    let helper =
        "import \"lumen.cdl\";\n\nfn greet() { lumen::signal_set(\"greeting\", \"hi\"); }\n";
    let main = r#"import "lumen.cdl";

fn on_start() {
    greet();
    lumen::add_clicks(1);
}

fn main() {}
"#;
    let src = format!("{helper}\n{main}");

    host.compile_check(&src, "two_files.lmn")
        .expect("a second importing file must not redefine the prelude");
    host.load(&src, "two_files.lmn")
        .expect("a second importing file must not redefine the prelude");

    let outcome = host.call("on_start", &[]).expect("on_start ok");
    assert!(
        outcome.commands.iter().any(|c| matches!(
            c,
            ScriptCommand::SetSignal { name, value } if name == "greeting" && value == "hi"
        )),
        "the first file's builtin call reached the sink"
    );
    assert!(
        outcome
            .commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::AddClicks(1))),
        "the second file's builtin call reached the sink"
    );
}

/// The prelude alone grants the DOM write side: a list-building app with no
/// hand-written `host` block spawns elements, sets text/class, and appends them,
/// and every mutation reaches the command sink. This is the surface that used to
/// force a hand-written block because the prelude omitted it.
#[test]
fn prelude_grants_dom_write_side() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn build() {
    let root = lumen::node_spawn("column");
    let row = lumen::node_spawn("row");
    lumen::node_class_add(row, "track");
    lumen::node_set_text(row, "Reference Tone");
    lumen::node_set_attr(row, "id", "tr|1");
    lumen::node_append(root, row);
}
fn main() {}
"#;
    host.load(src, "dom_app.cdl")
        .expect("a DOM app compiles from the prelude alone, no host block");
    let out = host.call("build", &[]).expect("build ok");
    let n = |pred: fn(&ScriptCommand) -> bool| out.commands.iter().filter(|c| pred(c)).count();
    assert_eq!(
        n(|c| matches!(c, ScriptCommand::Spawn { .. })),
        2,
        "two node_spawn calls -> two Spawn commands"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::ClassAdd { class, .. } if class == "track")),
        "node_class_add reached the sink"
    );
    assert!(
        out.commands.iter().any(
            |c| matches!(c, ScriptCommand::SetNodeText { text, .. } if text == "Reference Tone")
        ),
        "node_set_text reached the sink"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::SetAttr { name, value, .. } if name == "id" && value == "tr|1")),
        "node_set_attr reached the sink"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::Insert { .. })),
        "node_append reached the sink"
    );
}

/// The `lm_append` prelude helper collapses the five-call element-build into one
/// call and returns the new node id, so a follow-up mutation can target it.
#[test]
fn prelude_lm_append_helper_builds_and_returns() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn build() {
    let root = lumen::node_spawn("column");
    let row = lm_append(root, "row", "track", "Cipher");
    lumen::node_set_attr(row, "id", "tr|cipher");
}
fn main() {}
"#;
    host.load(src, "helper_app.cdl")
        .expect("the lm_append helper compiles via the prelude");
    let out = host.call("build", &[]).expect("build ok");
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::ClassAdd { class, .. } if class == "track")),
        "helper added the class"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::SetNodeText { text, .. } if text == "Cipher")),
        "helper set the text"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::Insert { .. })),
        "helper appended the element"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::SetAttr { value, .. } if value == "tr|cipher")),
        "the helper's returned id is a valid mutation target"
    );
}

/// The prelude also grants the `window` / `document` namespaces in the same
/// import, each declared as its own host block. A prelude-only app can drive
/// window state without a hand-written block.
#[test]
fn prelude_grants_web_namespaces() {
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn go() {
    window::set_title("Waveform");
    let doc = document::create("row");
}
fn main() {}
"#;
    host.load(src, "web_app.cdl")
        .expect("the window/document namespaces compile via the prelude");
    let out = host.call("go", &[]).expect("go ok");
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::WindowSetTitle { title } if title == "Waveform")),
        "window::set_title reached the sink"
    );
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::Spawn { .. })),
        "document::create reached the sink"
    );
}

/// The prelude also grants the color scheme and the file-based page
/// navigation. Scheme changes ride the command sink; navigation rides the
/// host-neutral `lumen_core::nav` bus, so `page(path)` is observable through
/// the request the resolver reads, and `page_current()` reads the active page
/// key back.
#[test]
fn prelude_grants_color_scheme_and_page_navigation() {
    use lumen_core::nav::{NavOp, REQUEST_SIGNAL, parse_request};
    use lumen_core::property_store::{PropertyKey, PropertyValue, external_property_snapshot};

    /// The navigation op sitting on the external bus, if any.
    fn pending_nav() -> Option<NavOp> {
        let key = PropertyKey::Global(std::sync::Arc::from(REQUEST_SIGNAL));
        match external_property_snapshot().get(&key) {
            Some(PropertyValue::Str(raw)) => parse_request(raw).map(|(_, op)| op),
            _ => None,
        }
    }

    lumen_core::property_store::init_external_properties();
    let mut host = CandelaHost::new();
    let src = r#"
import "lumen.cdl";
fn go() {
    lumen::set_color_scheme("force-dark");
    lumen::page("/about");
    return lumen::page_current();
}
fn step_back() { lumen::page_back(); }
fn main() {}
"#;
    host.load(src, "nav.cdl")
        .expect("the scheme + page entries compile via the prelude, no host block");

    let out = host.call("go", &[]).expect("go ok");
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, ScriptCommand::SetColorScheme { name } if name == "force-dark")),
        "set_color_scheme reached the sink"
    );
    assert_eq!(
        out.ret,
        Some(ScriptValue::Str(lumen_core::nav::current())),
        "page_current reads the active page key"
    );
    assert_eq!(
        pending_nav(),
        Some(NavOp::Navigate("/about".to_owned())),
        "page(path) queued a navigation on the shared bus"
    );

    host.call("step_back", &[]).expect("step_back ok");
    assert_eq!(
        pending_nav(),
        Some(NavOp::Back),
        "page_back queued a back step on the same bus"
    );
}

/// An embedder can still replace any of those with its own closure: a later
/// `register_host_fn` under the same namespace and name wins, which is how the
/// runtime swaps in implementations that need world access.
#[test]
fn embedder_registration_overrides_a_builtin() {
    let mut host = CandelaHost::new();
    host.engine_mut()
        .register_host_fn("lumen", "page_current", || -> String {
            "/from-embedder".to_owned()
        });
    let src = r#"
import "lumen.cdl";
fn where_am_i() { return lumen::page_current(); }
fn main() {}
"#;
    host.load(src, "override.cdl").expect("compiles");
    let out = host.call("where_am_i", &[]).expect("call ok");
    assert_eq!(out.ret, Some(ScriptValue::Str("/from-embedder".to_owned())));
}

/// Anti-drift: every tabled builtin must be declared in the embedded prelude,
/// so a new `BUILTINS` entry that forgets it fails CI. The trailing `(`
/// disambiguates prefix names (`signal_get` vs `signal_get_int`).
#[test]
fn prelude_declares_every_builtin() {
    for b in BUILTINS {
        let needle = format!("{}(", b.name);
        assert!(
            PRELUDE_SOURCE.contains(&needle),
            "the prelude is missing a declaration for builtin `{}`; refresh it with\n    \
             UPDATE_PRELUDE=1 cargo test -p lumen-script-candela --test prelude_generated",
            b.name
        );
    }
}
