//! The compiled-in shape: [`ProcessPlugin`] installed like any other plugin
//! on a headless app, running a real child in process.
//!
//! What these prove, once per concern:
//!
//! - `process::start` reaches a candela script through the generic
//!   `ScriptFnRegistry`, with its options as the `process::StartOptions`
//!   struct the module declares;
//! - a child runs in the app directory, and a `cmd` carrying a separator names
//!   a program the app ships;
//! - output and exit arrive over the generic plugin-event bus, with the exit
//!   last, and a per-tag `on("process_exit", tag, fn)` registration winning
//!   over the `on_process_exit` fallback;
//! - a program that cannot start answers false and fires nothing at all, and
//!   a start with an option the struct does not declare, or one of the wrong
//!   kind, is a compile error that starts nothing;
//! - the options set the child's directory, relative to the app, and its
//!   environment;
//! - `process::stop` ends a running child, whose exit still arrives, and
//!   answers false once nothing runs under the tag;
//! - dropping the app ends the children started with `end_at_exit: true` and
//!   leaves the others running;
//! - without the plugin the function does not exist: the script fails to
//!   compile, and the app keeps running without it.

// The candela host, compiled into the test binary: there is no shared engine
// to open the host module beside.
use lumen_candela_dev as _;
use lumen_core::app::App as EcsApp;
use lumen_core::plugin_events::{QueuedEvent, drain_plugin_events};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_module::lumen_script::{PluginEvent, ScriptLoadFailure, ScriptValue};
use lumen_process::ProcessPlugin;
use lumen_runtime::{RunOptions, build_headless_app};

/// The app directory, the DOM snapshot, the property store, and the
/// plugin-event bus are process-global, so the headless apps here run one at a
/// time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The test program, as the absolute path this build produced it at.
const CHILD: &str = env!("CARGO_BIN_EXE_lumen-process-test-child");

/// Where the copy of the test program sits in an app directory, spelled the
/// way a script names it: forward slashes, and the extension Windows needs to
/// run a program at all. A script string is script source, so a path in one
/// never carries a backslash; forward slashes name the same file on every
/// platform.
#[cfg(windows)]
const CHILD_IN_APP: &str = "tools/child.exe";
#[cfg(not(windows))]
const CHILD_IN_APP: &str = "tools/child";

/// A fresh app directory carrying `lumen.toml` and a copy of the test program,
/// so a script can name it the way an app names a program it ships.
fn app_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-process-plugin-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tools")).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-process-plugin-{name}\"\n"),
    )
    .expect("lumen.toml");
    std::fs::copy(CHILD, dir.join(CHILD_IN_APP)).expect("the test program is copied in");
    dir
}

/// Build a headless app in `dir` running one candela script, with the given
/// plugin.
fn build_app(dir: &std::path::Path, source: &str, plugin: Option<ProcessPlugin>) -> EcsApp {
    lumen_core::plugin_events::discard_plugin_events();
    // `<child>` in a script stands for the program the app ships.
    let source = &source.replace("<child>", &format!("./{CHILD_IN_APP}"));
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            module: "lumen-candela-dev".to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(dir).with_artifact_bytes(bytes);
    if let Some(plugin) = plugin {
        opts = opts.with_plugin(plugin);
    }
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app
}

/// One signal, as the string a bound label would read.
fn signal(app: &EcsApp, name: &str) -> Option<String> {
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

/// Tick with wall time between ticks (the child's threads need some) until
/// `pred` holds or the deadline passes.
fn tick_until(app: &mut EcsApp, secs: f64, pred: impl Fn(&EcsApp) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    loop {
        app.tick();
        if pred(app) {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// The whole surface: a program the app ships runs in the app directory, its
/// arguments reach it, both pipes arrive as lines, and the exit comes last.
/// `Default::default()` builds the options, and every handler takes the tag
/// and the value the event carries.
#[test]
fn a_script_runs_a_program_the_app_ships() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("ships");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("started", process::start("<child>", ["0", "one", "two"], "job", Default::default()));
}

fn on_process_stdout(tag: string, line: string) {
    lumen::signal_set("out", lumen::signal_get("out") + tag + "/" + line + ";");
}

fn on_process_stderr(tag: string, line: string) { lumen::signal_set("err", tag + "/" + line); }

fn on_process_exit(tag: string, code: int) { lumen::signal_set("exit", tag + "/" + str(code)); }

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "exit").is_some()),
        "the exit must arrive; out={:?} err={:?}",
        signal(&app, "out"),
        signal(&app, "err")
    );
    assert_eq!(
        signal(&app, "out").as_deref(),
        Some("job/0;job/one;job/two;"),
        "every argument was echoed back, in order, under the tag"
    );
    assert_eq!(signal(&app, "err").as_deref(), Some("job/child stderr"));
    assert_eq!(signal(&app, "exit").as_deref(), Some("job/0"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The exit is the last word for a tag: a chatty child's lines are all
/// delivered before the handler that says it ended.
#[test]
fn the_exit_is_the_last_event_for_a_tag() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("order");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    process::start("<child>", ["4", "--lines", "40"], "flood", Default::default());
}

fn on_process_stdout(tag: string, line: string) {
    lumen::signal_set_int("lines", lumen::signal_get_int("lines") + 1);
}

fn on_process_exit(tag: string, code: int) {
    lumen::signal_set_int("at_exit", lumen::signal_get_int("lines"));
    lumen::signal_set_int("code", code);
}

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "code").is_some()),
        "the exit must arrive; lines={:?}",
        signal(&app, "lines")
    );
    assert_eq!(
        signal(&app, "at_exit").as_deref(),
        Some("43"),
        "three echoed arguments and 40 flooded lines, all before the exit"
    );
    assert_eq!(
        signal(&app, "code").as_deref(),
        Some("4"),
        "the program's own exit code reaches the handler"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A per-tag `on("process_exit", tag, fn)` registration wins over the
/// `on_process_exit` fallback, like every other plugin event.
#[test]
fn a_per_tag_handler_wins_over_the_fallback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("routing");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::on("process_exit", "job", "job_ended");
    process::start("<child>", ["7"], "job", Default::default());
}

fn job_ended(tag: string, code: int) { lumen::signal_set("special", tag + "/" + str(code)); }

fn on_process_exit(tag: string, code: int) { lumen::signal_set("fallback", tag + "/" + str(code)); }

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "special").is_some()),
        "the per-tag handler must fire"
    );
    assert_eq!(signal(&app, "special").as_deref(), Some("job/7"));
    assert_eq!(signal(&app, "fallback"), None, "the fallback must not fire");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A bare `cmd` is looked up on `PATH` rather than beside the app.
#[cfg(unix)]
#[test]
fn a_bare_command_is_looked_up_on_the_path() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("path");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("started", process::start("sh", ["-c", "echo found"], "sh", Default::default()));
}

fn on_process_stdout(tag: string, line: string) { lumen::signal_set("out", line); }

fn on_process_exit(tag: string, code: int) { lumen::signal_set_int("exit", code); }

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "exit").is_some()),
        "the shell must run and end"
    );
    assert_eq!(signal(&app, "out").as_deref(), Some("found"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A program that is not there answers false and fires nothing: the tag never
/// named a running program, so a script branches on the value it got back.
#[test]
fn a_program_that_cannot_start_answers_false_and_fires_nothing() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("missing");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("started", process::start("no-such-program-8f2c", [], "gone", Default::default()));
}

fn on_process_stdout(tag: string, line: string) { lumen::signal_set("out", line); }

fn on_process_stderr(tag: string, line: string) { lumen::signal_set("err", line); }

fn on_process_exit(tag: string, code: int) { lumen::signal_set_int("exit", code); }

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("false"));
    for _ in 0..20 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(signal(&app, "out"), None);
    assert_eq!(signal(&app, "err"), None);
    assert_eq!(
        signal(&app, "exit"),
        None,
        "a start that failed has no exit to report"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the function does not exist: there is no `process`
/// namespace to declare, so the program fails to compile and the failure names
/// it. No child runs, and the app keeps ticking without its script.
#[test]
fn without_the_plugin_the_function_does_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absent");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("started", process::start("<child>", ["0"], "job", process::StartOptions::default()));
}

fn on_process_exit(tag: string, code: int) { lumen::signal_set_int("exit", code); }

fn main() {}
"#,
        None,
    );

    for _ in 0..20 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let failure = app
        .world
        .get_resource::<ScriptLoadFailure>()
        .expect("the program failed to load");
    assert!(
        failure.0.contains("no `process::"),
        "the failure names the missing namespace: {}",
        failure.0
    );
    assert_eq!(
        signal(&app, "started"),
        None,
        "no module, no `process` namespace, no value"
    );
    assert_eq!(signal(&app, "exit"), None, "nothing ran");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The options reach the child, written as the struct the module declares: a
/// relative `cwd` is a directory inside the app, `env` is laid over the
/// inherited environment, and the fields the script leaves out come from
/// `..Default::default()`, so `end_at_exit` keeps its default.
#[test]
fn the_options_set_the_directory_and_the_environment() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("options");
    std::fs::create_dir_all(dir.join("instances/a")).expect("instance dir");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    let opts = process::StartOptions {
        cwd: "instances/a",
        env: {"LUMEN_PROCESS_INSTANCE": "a"},
        ..Default::default()
    };
    lumen::signal_set_bool("started", process::start(
        "<child>", ["0", "--cwd", "--env", "LUMEN_PROCESS_INSTANCE"], "job", opts));
}

fn on_process_stdout(tag: string, line: string) {
    if line.len() < 4 { return; }
    let key = line[0..4];
    let rest = line[4..line.len()];
    if key == "cwd=" { lumen::signal_set("cwd", rest); }
    if key == "env=" { lumen::signal_set("env", rest); }
}

fn on_process_exit(tag: string, code: int) {
    lumen::signal_set_int("exit", code);
}

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "exit").is_some()),
        "the exit must arrive"
    );
    let cwd = signal(&app, "cwd").expect("the child reported its directory");
    assert_eq!(
        std::fs::canonicalize(cwd).expect("reported dir"),
        std::fs::canonicalize(dir.join("instances/a")).expect("instance dir"),
    );
    assert_eq!(signal(&app, "env").as_deref(), Some("a"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// An option the struct does not declare, or one of the wrong kind, is a
/// compile error naming it: the program never loads, no child runs, and no
/// event fires.
#[test]
fn a_bad_option_is_a_script_error_and_starts_nothing() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (name, options, needle) in [
        ("bad-option", r#"cdw: "x""#, "no field cdw"),
        ("bad-value", r#"end_at_exit: "never""#, "end_at_exit"),
    ] {
        let dir = app_dir(name);
        let mut app = build_app(
            &dir,
            &format!(
                r#"import "lumen.cdl";

fn on_start() {{
    let opts = process::StartOptions {{ {options}, ..Default::default() }};
    lumen::signal_set_bool("started", process::start("<child>", ["0"], "job", opts));
}}

fn on_process_stdout(tag: string, line: string) {{ lumen::signal_set("out", line); }}

fn on_process_exit(tag: string, code: int) {{ lumen::signal_set_int("exit", code); }}

fn main() {{}}
"#
            ),
            Some(ProcessPlugin),
        );

        for _ in 0..20 {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let failure = app
            .world
            .get_resource::<ScriptLoadFailure>()
            .expect("the program failed to load");
        assert!(
            failure.0.contains(needle),
            "the failure names the option: {}",
            failure.0
        );
        assert_eq!(signal(&app, "started"), None, "the call never ran");
        assert_eq!(signal(&app, "out"), None);
        assert_eq!(signal(&app, "exit"), None, "nothing started, nothing ended");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `process::stop` ends a child mid-sleep; its exit still arrives, and once it
/// has, the tag names nothing to stop.
#[test]
fn stop_ends_a_running_child_and_its_exit_still_arrives() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("stop");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("unknown", process::stop("sleeper"));
    process::start("<child>", ["0", "--sleep", "60000"], "sleeper", Default::default());
}

fn on_process_stdout(tag: string, line: string) {
    if line == "0" { lumen::signal_set_bool("stopped", process::stop(tag)); }
}

fn on_process_exit(tag: string, code: int) {
    lumen::signal_set_int("code", code);
    lumen::signal_set_bool("again", process::stop(tag));
}

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(
        signal(&app, "unknown").as_deref(),
        Some("false"),
        "no child runs under the tag yet"
    );
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "code").is_some()),
        "the stopped child's exit must arrive; stopped={:?}",
        signal(&app, "stopped")
    );
    assert_eq!(signal(&app, "stopped").as_deref(), Some("true"));
    assert_ne!(signal(&app, "code").as_deref(), Some("0"));
    #[cfg(unix)]
    assert_eq!(
        signal(&app, "code").as_deref(),
        Some("143"),
        "128 plus SIGTERM"
    );
    assert_eq!(
        signal(&app, "again").as_deref(),
        Some("false"),
        "an ended child is no longer running under its tag"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The exit codes the bus carries for `process_exit`, by tag, from the events
/// pushed since the last drain.
fn exits_on_the_bus(into: &mut Vec<(String, i64)>) {
    for event in drain_plugin_events() {
        let QueuedEvent::Value(value) = event else {
            continue;
        };
        let Ok(event) = value.downcast::<PluginEvent>() else {
            continue;
        };
        if let PluginEvent::Call {
            event, key, args, ..
        } = *event
            && event == "process_exit"
            && let Some(ScriptValue::I64(code)) = args.first()
        {
            into.push((key, *code));
        }
    }
}

/// Dropping the app ends a child started with `end_at_exit: true` before the
/// drop returns, and leaves a child started with the default to finish on
/// its own.
#[test]
fn the_app_exit_ends_the_children_that_asked_for_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("on-exit");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    process::start("<child>", ["0", "--sleep", "60000"], "helper",
        process::StartOptions { end_at_exit: true, ..Default::default() });
    process::start("<child>", ["0", "--sleep", "1500"], "keeper", Default::default());
}

fn on_process_stdout(tag: string, line: string) { lumen::signal_set(tag, "running"); }

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| {
            signal(app, "helper").is_some() && signal(app, "keeper").is_some()
        }),
        "both children must be running before the app exits"
    );
    exits_on_the_bus(&mut Vec::new());

    let dropped = std::time::Instant::now();
    drop(app);
    assert!(
        dropped.elapsed() < std::time::Duration::from_secs(10),
        "the drop ended the helper rather than waiting out its sleep"
    );

    let mut exits = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while exits.len() < 2 && std::time::Instant::now() < deadline {
        exits_on_the_bus(&mut exits);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let code = |tag: &str| exits.iter().find(|(t, _)| t == tag).map(|(_, c)| *c);
    assert!(
        code("helper").is_some_and(|c| c != 0),
        "the helper was ended with the app: {exits:?}"
    );
    assert_eq!(
        code("keeper"),
        Some(0),
        "the keeper outlived the app and finished its own sleep: {exits:?}"
    );
    assert_eq!(
        exits.first().map(|(t, _)| t.as_str()),
        Some("helper"),
        "the helper ended first, with the app"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
