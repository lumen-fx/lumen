//! The shared builtin table, exercised the way a host exercises it: call the
//! body with the arguments a script would pass and read back the command it
//! queued or the value it returned.
//!
//! The per-host suites check that each language resolves these names; this one
//! checks what they do once resolved.

use lumen_script::{HostSet, ScriptCommand, ScriptFn, ScriptValue, builtin_script_fns, builtins};

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
    let f = builtin("rhai", "set_color_scheme");
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
        returns(&page, &[text("settings")]),
        ScriptValue::Unit,
        "navigating returns nothing"
    );
    let current = lumen_core::nav::current();
    assert_eq!(returns(&page, &[]), ScriptValue::Str(current.clone()));
    assert_eq!(
        returns(&page, &[ScriptValue::Unit]),
        ScriptValue::Str(current.clone()),
        "a unit placeholder reads too"
    );
    assert_eq!(
        returns(&builtin("lua", "page_current"), &[]),
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
    assert_eq!(returns(&back, &[]), ScriptValue::Unit);

    assert_eq!(
        returns(&builtin("rhai", "page_back"), &[]),
        ScriptValue::Bool(true),
        "the hosts that read the result get the boolean"
    );
    assert_eq!(
        returns(&builtin("rhai", "page_forward"), &[]),
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
    let f = builtin("rhai", "local_id");
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

/// The file surface reports failure rather than raising: a script branches on
/// what it got back.
#[test]
fn the_file_builtins_report_failure_through_their_return_value() {
    let missing = "/nonexistent-lumen-test-dir/nope.txt";
    assert_eq!(
        returns(&builtin("rhai", "read_file"), &[text(missing)]),
        text(""),
    );
    // A directory is the other kind of failure, the one still worth a
    // warning; the return value stays the same empty string.
    assert_eq!(
        returns(&builtin("rhai", "read_file"), &[text("/")]),
        text("")
    );
    assert_eq!(
        returns(&builtin("rhai", "write_file"), &[text(missing), text("x")]),
        ScriptValue::Bool(false),
    );
}

/// A path an app author writes is app-relative, so a file the app ships is
/// named the same way wherever the app was started from, and saved state has a
/// per-app home the working directory cannot move.
#[test]
fn the_file_builtins_resolve_against_the_app_and_its_data_directory() {
    let app = std::env::temp_dir().join(format!("lumen-builtin-files-{}", std::process::id()));
    std::fs::create_dir_all(&app).expect("app dir");
    lumen_core::app_paths::set_app(&app, "lumen-builtin-files-test");

    let wrote = returns(
        &builtin("rhai", "write_file"),
        &[text("notes.txt"), text("hello")],
    );
    assert_eq!(wrote, ScriptValue::Bool(true));
    assert_eq!(
        std::fs::read_to_string(app.join("notes.txt")).ok(),
        Some("hello".to_string()),
        "a relative write lands in the app directory"
    );
    assert_eq!(
        returns(&builtin("rhai", "read_file"), &[text("notes.txt")]),
        text("hello"),
        "and the matching read finds it"
    );

    let absolute = app.join("absolute.txt");
    let absolute = absolute.to_string_lossy().into_owned();
    assert_eq!(
        returns(
            &builtin("lua", "write_file"),
            &[text(&absolute), text("kept")]
        ),
        ScriptValue::Bool(true),
        "an absolute path is left alone"
    );
    assert_eq!(
        returns(&builtin("lua", "read_file"), &[text(&absolute)]),
        text("kept")
    );

    let ScriptValue::Str(data) = returns(&builtin("candela", "data_dir"), &[]) else {
        panic!("data_dir returns a path");
    };
    let data = std::path::PathBuf::from(data);
    assert!(data.is_dir(), "{} was not created", data.display());
    assert!(
        data.ends_with("lumen-builtin-files-test") || data == app,
        "the data directory carries the app id: {}",
        data.display()
    );
    if data != app {
        let _ = std::fs::remove_dir(&data);
    }
    let _ = std::fs::remove_dir_all(&app);
}

/// `write_file` goes through a temp-file-and-rename, not a truncating write
/// in place, so a reader racing it never sees a partial file (#154). Check
/// the two observable ends of that: the final file holds the new contents,
/// and no temp file is left behind next to it.
#[test]
fn write_file_leaves_only_the_finished_file_behind() {
    let dir = std::env::temp_dir().join(format!(
        "lumen-write-file-atomic-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("out.txt");

    assert_eq!(
        returns(
            &builtin("rhai", "write_file"),
            &[text(target.to_str().unwrap()), text("first")]
        ),
        ScriptValue::Bool(true),
    );
    assert_eq!(
        returns(
            &builtin("rhai", "write_file"),
            &[
                text(target.to_str().unwrap()),
                text("second, longer contents")
            ]
        ),
        ScriptValue::Bool(true),
    );

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "second, longer contents"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(
        leftovers.len(),
        1,
        "a temp file was left behind: {leftovers:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Outside a server render the request surface is empty rather than absent, so
/// a script written for one still runs on the desktop.
#[test]
fn the_request_surface_reads_empty_off_a_server() {
    assert_eq!(
        returns(&builtin("lua", "request_header"), &[text("accept")]),
        text("")
    );
    assert_eq!(returns(&builtin("lua", "request_body"), &[]), text(""));
}

/// Every host is offered the shared surface, apart from two deliberate
/// exceptions: the navigation family, where a language's own shape wins, and
/// the free-function DOM surface, which exists for the language that has no
/// receiver methods to reach it through.
#[test]
fn the_table_reaches_every_host() {
    let split = ["page", "page_back", "page_forward"];
    for f in builtin_script_fns() {
        if split.contains(&f.name.as_str()) {
            continue;
        }
        let expected = if f.name.starts_with("node_") || f.name.starts_with("event_") {
            HostSet::CANDELA
        } else {
            HostSet::ALL
        };
        assert_eq!(f.hosts, expected, "`{}` reaches the wrong hosts", f.name);
    }
}

/// Editor tooling reads the per-host metadata tables, so a function a host
/// binds from the shared table has a row there for that host.
///
/// The two sides are built from different files: the bodies from this crate's
/// source, the rows from `builtins.ron`. A function moved into the shared table
/// without its row would work in every language and be invisible in every
/// editor.
#[test]
fn every_shared_entry_has_a_metadata_row_for_each_host_that_sees_it() {
    let tables = [
        ("rhai", builtins::RHAI_BUILTINS),
        ("lua", builtins::LUA_BUILTINS),
        ("candela", builtins::CANDELA_BUILTINS),
    ];

    let mut missing: Vec<String> = Vec::new();
    for (lang, table) in tables {
        let rows: std::collections::HashSet<&str> = table.iter().map(|b| b.name).collect();
        for f in builtin_script_fns() {
            if f.visible_to(lang) && !rows.contains(f.name.as_str()) {
                missing.push(format!("{lang}::{}", f.name));
            }
        }
    }
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "these shared builtins have no row in builtins.ron for the host that sees them, so the \
         LSP cannot offer them: {missing:?}"
    );
}

/// A metadata row's parameter count matches the signature behind it, so hover
/// text describes the call the body accepts.
///
/// A variadic or optional entry is exempt: one registration serves a range of
/// arities and the row spells the shape an author writes.
#[test]
fn a_metadata_row_matches_the_signature_behind_it() {
    let mut wrong: Vec<String> = Vec::new();
    for b in builtins::CANDELA_BUILTINS {
        let Some(f) = builtin_script_fns()
            .into_iter()
            .find(|f| f.name == b.name && f.visible_to("candela"))
        else {
            continue;
        };
        if f.sig.variadic || f.sig.min_arity != f.sig.params.len() {
            continue;
        }
        if f.sig.params.len() != b.params.len() {
            wrong.push(format!(
                "{}: the table takes {} argument(s), the row spells {}",
                b.name,
                f.sig.params.len(),
                b.params.len()
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// A candela row spells the types the candela host declares.
///
/// candela is the language that has to name a type in a `host` block, so its
/// rows and the generated declarations are read side by side. An entry the
/// generator declares `string node_text(int)` and the LSP describes as taking a
/// `node` is the same function twice over, and the two spellings have to agree.
#[test]
fn a_candela_row_spells_the_types_the_declaration_names() {
    /// How a candela row spells a declared type.
    fn spelling(ty: &lumen_script::ScriptTy) -> String {
        use lumen_script::ScriptTy as T;
        match ty {
            T::Int => "int".to_string(),
            T::Float => "float".to_string(),
            T::Bool => "bool".to_string(),
            T::Str => "string".to_string(),
            T::Unit => "()".to_string(),
            T::Any => "any".to_string(),
            T::Array(inner) => format!("{}[]", spelling(inner)),
            T::Map(value) => format!("{{string: {}}}", spelling(value)),
        }
    }

    let mut drifted: Vec<String> = Vec::new();
    for b in builtins::CANDELA_BUILTINS {
        let Some(f) = builtin_script_fns()
            .into_iter()
            .find(|f| f.name == b.name && f.visible_to("candela"))
        else {
            continue;
        };
        if spelling(&f.sig.ret) != b.ret {
            drifted.push(format!(
                "{}: returns {}, the row says {}",
                b.name,
                spelling(&f.sig.ret),
                b.ret
            ));
        }
        if f.sig.variadic || f.sig.min_arity != f.sig.params.len() {
            continue;
        }
        for (param, row) in f.sig.params.iter().zip(b.params) {
            if spelling(&param.ty) != row.ty {
                drifted.push(format!(
                    "{}: `{}` is {}, the row says {}",
                    b.name,
                    param.name,
                    spelling(&param.ty),
                    row.ty
                ));
            }
        }
    }
    drifted.sort_unstable();
    assert!(drifted.is_empty(), "{drifted:#?}");
}

/// The free-function DOM surface is candela's, and no other host offers it:
/// Rhai and Lua reach the same reads and writes through node receivers.
#[test]
fn the_free_function_dom_surface_is_candela_only() {
    let node_fns: Vec<ScriptFn> = builtin_script_fns()
        .into_iter()
        .filter(|f| f.name.starts_with("node_") || f.name.starts_with("event_"))
        .collect();
    assert!(
        node_fns.len() > 50,
        "the DOM surface is much larger than {}",
        node_fns.len()
    );
    for f in &node_fns {
        assert!(!f.visible_to("rhai"), "`{}` reached Rhai", f.name);
        assert!(!f.visible_to("lua"), "`{}` reached Lua", f.name);
    }
}
