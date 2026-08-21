//! The shared builtin table, exercised the way a host exercises it: call the
//! body with the arguments a script would pass and read back the command it
//! queued or the value it returned.
//!
//! The per-host suites check that each language resolves these names; this one
//! checks what they do once resolved.

use lumen_script::{HostSet, ScriptCommand, ScriptFn, ScriptValue, builtin_script_fns};

/// Navigation rides a process-global bus, so the tests that read it run one at
/// a time.
fn nav_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The entry of that name a `lang` host would bind.
fn builtin(lang: &str, name: &str) -> ScriptFn {
    builtin_script_fns()
        .into_iter()
        .find(|f| f.name == name && f.visible_to(lang))
        .unwrap_or_else(|| panic!("no builtin `{name}` for {lang}"))
}

/// The commands a call queued.
fn commands(f: &ScriptFn, args: &[ScriptValue]) -> Vec<ScriptCommand> {
    f.invoke(args).1
}

fn text(s: &str) -> ScriptValue {
    ScriptValue::Str(s.to_string())
}

/// The scheme change rides the same command every host queues, applied by the
/// runtime's script-command applier.
#[test]
fn set_color_scheme_queues_the_command_that_carries_it() {
    let f = builtin("rhai", "set_color_scheme");
    let queued = commands(&f, &[text("force-dark")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetColorScheme { name }] if name == "force-dark"),
        "unexpected commands: {queued:?}"
    );
    assert_eq!(
        f.invoke(&[text("force-dark")]).0,
        ScriptValue::Unit,
        "the builtin returns nothing to the script"
    );
}

/// An unknown name still queues: the applier owns the vocabulary and warns
/// there, so every host reports a typo the same way.
#[test]
fn set_color_scheme_leaves_an_unknown_name_to_the_applier() {
    let f = builtin("lua", "set_color_scheme");
    let queued = commands(&f, &[text("chartreuse")]);
    assert!(matches!(
        &queued[..],
        [ScriptCommand::SetColorScheme { .. }]
    ));
}

/// `page` takes its argument optionally on the hosts that can resolve both
/// call shapes: with one it navigates, without one it reads.
#[test]
fn page_navigates_with_an_argument_and_reads_without_one() {
    let _guard = nav_guard();
    let page = builtin("rhai", "page");
    assert_eq!(page.sig.arity_range(), 0..=1);

    assert_eq!(
        page.invoke(&[text("settings")]).0,
        ScriptValue::Unit,
        "navigating returns nothing"
    );
    let current = lumen_core::nav::current();
    assert_eq!(page.invoke(&[]).0, ScriptValue::Str(current.clone()));
    assert_eq!(
        page.invoke(&[ScriptValue::Unit]).0,
        ScriptValue::Str(current.clone()),
        "a unit placeholder reads too"
    );
    assert_eq!(
        builtin("lua", "page_current").invoke(&[]).0,
        ScriptValue::Str(current),
        "page_current is the same reader under an unambiguous name"
    );
}

/// candela cannot overload a host function on arity or return a value its
/// declaration does not name, so it takes the writer-only `page` and
/// unit-valued history steps.
#[test]
fn candela_takes_the_declarable_shape_of_the_navigation_family() {
    let _guard = nav_guard();
    let page = builtin("candela", "page");
    assert_eq!(page.sig.arity_range(), 1..=1);
    assert_eq!(page.sig.ret, lumen_script::ScriptTy::Unit);

    let back = builtin("candela", "page_back");
    assert_eq!(back.sig.ret, lumen_script::ScriptTy::Unit);
    assert_eq!(back.invoke(&[]).0, ScriptValue::Unit);

    assert_eq!(
        builtin("rhai", "page_back").invoke(&[]).0,
        ScriptValue::Bool(true),
        "the hosts that read the result get the boolean"
    );
    assert_eq!(
        builtin("rhai", "page_forward").invoke(&[]).0,
        ScriptValue::Bool(true)
    );
}

/// A menu's open state is a reserved signal, so the same command drives the
/// markup binding in every language.
#[test]
fn opening_a_menu_writes_the_reserved_signal() {
    let queued = commands(&builtin("candela", "open_menu"), &[text("file")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetSignal { name, value }]
            if name == "__menu_open:file" && value == "true"),
        "unexpected commands: {queued:?}"
    );
    let queued = commands(&builtin("candela", "close_menu"), &[text("file")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetSignal { value, .. }] if value == "false"),
        "unexpected commands: {queued:?}"
    );
}

/// A filtered pick carries the parsed filter list; the plain picks carry none.
#[test]
fn a_file_dialog_carries_its_kind_and_filters() {
    let queued = commands(&builtin("rhai", "pick_folder"), &[text("dir")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::OpenFileDialog { kind, tag, filters, .. }]
            if *kind == lumen_script::FileDialogKind::PickFolder
                && tag == "dir"
                && filters.is_empty()),
        "unexpected commands: {queued:?}"
    );

    let queued = commands(
        &builtin("rhai", "pick_file_filtered"),
        &[text("open"), text("Images:png,jpg")],
    );
    let [ScriptCommand::OpenFileDialog { filters, .. }] = &queued[..] else {
        panic!("unexpected commands: {queued:?}");
    };
    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0].1, vec!["png".to_string(), "jpg".to_string()]);
}

/// A timer carries its repeat flag and clamps a negative delay.
#[test]
fn a_timer_carries_its_repeat_flag() {
    let queued = commands(
        &builtin("lua", "set_interval"),
        &[text("tick"), ScriptValue::I64(-5)],
    );
    assert!(
        matches!(&queued[..], [ScriptCommand::SetTimer { name, millis, repeat }]
            if name == "tick" && *millis == 0 && *repeat),
        "unexpected commands: {queued:?}"
    );
}

/// An integer reaches a float parameter: `audio_seek(30)` is what an author
/// writes, and every host spells the literal that way.
#[test]
fn a_float_parameter_takes_an_integer_argument() {
    let seek = builtin("candela", "audio_seek");
    assert!(seek.sig.check_args(&[ScriptValue::I64(30)]).is_ok());
    let queued = commands(&seek, &[ScriptValue::I64(30)]);
    assert!(
        matches!(&queued[..], [ScriptCommand::AudioSeek { secs }] if *secs == 30.0),
        "unexpected commands: {queued:?}"
    );
}

/// `local_id` resolves a sibling id inside the same template instance.
#[test]
fn local_id_swaps_the_suffix_under_the_same_prefix() {
    let f = builtin("rhai", "local_id");
    assert_eq!(
        f.invoke(&[text("user-card:btn"), text("label")]).0,
        text("user-card:label")
    );
    assert_eq!(
        f.invoke(&[text("a:b:btn"), text("label")]).0,
        text("a:b:label"),
        "a multi-level prefix stacks"
    );
    assert_eq!(
        f.invoke(&[text("btn"), text("label")]).0,
        text("label"),
        "a source with no prefix gives the suffix back"
    );
}

/// The file surface reports failure rather than raising: a script branches on
/// what it got back.
#[test]
fn the_file_builtins_report_failure_through_their_return_value() {
    let missing = "/nonexistent-lumen-test-dir/nope.txt";
    assert_eq!(
        builtin("rhai", "read_file").invoke(&[text(missing)]).0,
        text(""),
    );
    assert_eq!(
        builtin("rhai", "write_file")
            .invoke(&[text(missing), text("x")])
            .0,
        ScriptValue::Bool(false),
    );
}

/// Outside a server render the request surface is empty rather than absent, so
/// a script written for one still runs on the desktop.
#[test]
fn the_request_surface_reads_empty_off_a_server() {
    assert_eq!(
        builtin("lua", "request_header").invoke(&[text("accept")]).0,
        text("")
    );
    assert_eq!(builtin("lua", "request_body").invoke(&[]).0, text(""));
}

/// Every host is offered the same table apart from the navigation family,
/// which is the one place a language's own shape wins.
#[test]
fn the_table_reaches_every_host() {
    let split: Vec<&str> = ["page", "page_back", "page_forward"].to_vec();
    for f in builtin_script_fns() {
        if split.contains(&f.name.as_str()) {
            continue;
        }
        assert_eq!(
            f.hosts,
            HostSet::ALL,
            "`{}` is offered to some hosts only",
            f.name
        );
    }
}
