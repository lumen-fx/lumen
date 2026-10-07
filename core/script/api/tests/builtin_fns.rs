//! The shared builtin table, exercised the way a host exercises it: call the
//! body with the arguments a script would pass and read back the command it
//! queued or the value it returned.
//!
//! Each host's own suite checks that its language resolves these names; this
//! one checks what they do once resolved.

use lumen_script::{ScriptCommand, ScriptFn, ScriptValue, builtin_script_fns};

/// Navigation rides a process-global bus, so the tests that read it run one at
/// a time.
fn nav_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The entry of that name every host binds.
fn builtin(name: &str) -> ScriptFn {
    builtin_script_fns()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no builtin `{name}`"))
}

/// The commands a call queued.
fn commands(f: &ScriptFn, args: &[ScriptValue]) -> Vec<ScriptCommand> {
    f.invoke(args).1
}

/// The value a call returned. Nothing in the shared table fails, so a failure
/// here is the test's own bug.
fn returns(f: &ScriptFn, args: &[ScriptValue]) -> ScriptValue {
    f.invoke(args).0.expect("a shared builtin does not fail")
}

fn text(s: &str) -> ScriptValue {
    ScriptValue::Str(s.to_string())
}

/// The scheme change rides the same command every host queues, applied by the
/// runtime's script-command applier.
#[test]
fn set_color_scheme_queues_the_command_that_carries_it() {
    let f = builtin("set_color_scheme");
    let queued = commands(&f, &[text("force-dark")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetColorScheme { name }] if name == "force-dark"),
        "unexpected commands: {queued:?}"
    );
    assert_eq!(
        returns(&f, &[text("force-dark")]),
        ScriptValue::Unit,
        "the builtin returns nothing to the script"
    );
}

/// An unknown name still queues: the applier owns the vocabulary and warns
/// there, so every host reports a typo the same way.
#[test]
fn set_color_scheme_leaves_an_unknown_name_to_the_applier() {
    let f = builtin("set_color_scheme");
    let queued = commands(&f, &[text("chartreuse")]);
    assert!(matches!(
        &queued[..],
        [ScriptCommand::SetColorScheme { .. }]
    ));
}

/// The page reader is shared: every host reads the current page the same way.
#[test]
fn page_current_reads_the_page_the_app_is_on() {
    let _guard = nav_guard();
    assert_eq!(
        returns(&builtin("page_current"), &[]),
        ScriptValue::Str(lumen_core::nav::current()),
    );
}

/// A menu's open state is a reserved signal, so the same command drives the
/// markup binding in every language.
#[test]
fn opening_a_menu_writes_the_reserved_signal() {
    let queued = commands(&builtin("open_menu"), &[text("file")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetSignal { name, value }]
            if name == "__menu_open:file" && value == "true"),
        "unexpected commands: {queued:?}"
    );
    let queued = commands(&builtin("close_menu"), &[text("file")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::SetSignal { value, .. }] if value == "false"),
        "unexpected commands: {queued:?}"
    );
}

/// A filtered pick carries the parsed filter list; the plain picks carry none.
#[test]
fn a_file_dialog_carries_its_kind_and_filters() {
    let queued = commands(&builtin("pick_folder"), &[text("dir")]);
    assert!(
        matches!(&queued[..], [ScriptCommand::OpenFileDialog { kind, tag, filters, .. }]
            if *kind == lumen_script::FileDialogKind::PickFolder
                && tag == "dir"
                && filters.is_empty()),
        "unexpected commands: {queued:?}"
    );

    let queued = commands(
        &builtin("pick_file_filtered"),
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
        &builtin("set_interval"),
        &[text("tick"), ScriptValue::I64(-5)],
    );
    assert!(
        matches!(&queued[..], [ScriptCommand::SetTimer { name, millis, repeat }]
            if name == "tick" && *millis == 0 && *repeat),
        "unexpected commands: {queued:?}"
    );
}

/// An integer reaches a float parameter: `seek(30)` is what an author
/// writes, and every host spells the literal that way.
#[test]
fn a_float_parameter_takes_an_integer_argument() {
    let seek = ScriptFn::new("seek")
        .param("secs", lumen_script::ScriptTy::Float)
        .ret(lumen_script::ScriptTy::Unit)
        .build(|cx| {
            cx.emit(ScriptCommand::Print(format!("{}", cx.float_arg(0))));
            Ok(ScriptValue::Unit)
        });
    assert!(seek.sig.check_args(&[ScriptValue::I64(30)]).is_ok());
    let queued = commands(&seek, &[ScriptValue::I64(30)]);
    assert!(
        matches!(&queued[..], [ScriptCommand::Print(secs)] if secs == "30"),
        "unexpected commands: {queued:?}"
    );
}

/// `local_id` resolves a sibling id inside the same template instance.
#[test]
fn local_id_swaps_the_suffix_under_the_same_prefix() {
    let f = builtin("local_id");
    assert_eq!(
        returns(&f, &[text("user-card:btn"), text("label")]),
        text("user-card:label")
    );
    assert_eq!(
        returns(&f, &[text("a:b:btn"), text("label")]),
        text("a:b:label"),
        "a multi-level prefix stacks"
    );
    assert_eq!(
        returns(&f, &[text("btn"), text("label")]),
        text("label"),
        "a source with no prefix gives the suffix back"
    );
}

/// Outside a server render the request surface is empty rather than absent, so
/// a script written for one still runs on the desktop.
#[test]
fn the_request_surface_reads_empty_off_a_server() {
    assert_eq!(
        returns(&builtin("request_header"), &[text("accept")]),
        text("")
    );
    assert_eq!(returns(&builtin("request_body"), &[]), text(""));
}
