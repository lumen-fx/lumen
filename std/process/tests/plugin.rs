//! The compiled-in shape: [`ProcessPlugin`] installed like any other plugin
//! on a headless app, running a real child in process.
//!
//! What these prove, once per concern:
//!
//! - `process::start` reaches a script through the generic `ScriptFnRegistry`,
//!   in Rhai and in candela;
//! - a child runs in the app directory, and a `cmd` carrying a separator names
//!   a program the app ships;
//! - output and exit arrive over the generic plugin-event bus, with the exit
//!   last, and a per-tag `on("process_exit", tag, fn)` registration winning
//!   over the `on_process_exit` fallback;
//! - a program that cannot start answers false and fires nothing at all;
//! - without the plugin the function does not exist, and the app keeps running
//!   after the script's own unknown-function error.

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
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

/// Build a headless app in `dir` running one script, with the given plugin.
fn build_app(
    dir: &std::path::Path,
    engine: &str,
    source: &str,
    plugin: Option<ProcessPlugin>,
) -> EcsApp {
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
        script_source: source.to_string(),
        scripts: vec![CompiledScript {
            engine: engine.to_string(),
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

/// The whole surface from Rhai: a program the app ships runs in the app
/// directory, its arguments reach it, both pipes arrive as lines, and the exit
/// comes last.
#[test]
fn rhai_runs_a_program_the_app_ships() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("rhai");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    signal("started", "").set(process::start("<child>", ["0", "one", "two"], "job"));
}
fn on_process_stdout(tag, line) {
    let s = signal("out", "");
    s.set(s.get() + tag + "/" + line + ";");
}
fn on_process_stderr(tag, line) { signal("err", "").set(tag + "/" + line); }
fn on_process_exit(tag, code) { signal("exit", "").set(tag + "/" + code); }
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
        "rhai",
        r#"
fn on_start() {
    process::start("<child>", ["4", "--lines", "40"], "flood");
}
fn on_process_stdout(tag, line) {
    let seen = signal("lines", 0);
    seen.set(seen.get() + 1);
}
fn on_process_exit(tag, code) {
    signal("at_exit", "").set(signal("lines", 0).get());
    signal("code", "").set(code);
}
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
        "rhai",
        r#"
fn on_start() {
    on("process_exit", "job", "job_ended");
    process::start("<child>", ["7"], "job");
}
fn job_ended(tag, code) { signal("special", "").set(tag + "/" + code); }
fn on_process_exit(tag, code) { signal("fallback", "").set(tag + "/" + code); }
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
        "rhai",
        r#"
fn on_start() {
    signal("started", "").set(process::start("sh", ["-c", "echo found"], "sh"));
}
fn on_process_stdout(tag, line) { signal("out", "").set(line); }
fn on_process_exit(tag, code) { signal("exit", "").set(code); }
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
        "rhai",
        r#"
fn on_start() {
    signal("started", "").set(process::start("no-such-program-8f2c", [], "gone"));
}
fn on_process_stdout(tag, line) { signal("out", "").set(line); }
fn on_process_stderr(tag, line) { signal("err", "").set(line); }
fn on_process_exit(tag, code) { signal("exit", "").set(code); }
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

/// candela reaches the same function through the `host "process"` block the
/// host synthesizes from what the plugin registered, and its handlers take the
/// tag and the value the event carries.
#[test]
fn candela_reaches_the_module_surface_through_its_namespace() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("candela");
    let mut app = build_app(
        &dir,
        "candela",
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("started", process::start("<child>", ["5", "hi"], "job"));
}

fn on_process_stdout(tag: string, line: string) {
    lumen::signal_set("out", lumen::signal_get("out") + tag + "/" + line + ";");
}

fn on_process_exit(tag: string, code: int) {
    lumen::signal_set("exit", tag);
    lumen::signal_set_int("code", code);
}

fn main() {}
"#,
        Some(ProcessPlugin),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "exit").is_some()),
        "the exit must reach candela; out={:?}",
        signal(&app, "out")
    );
    assert_eq!(signal(&app, "out").as_deref(), Some("job/5;job/hi;"));
    assert_eq!(signal(&app, "code").as_deref(), Some("5"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the function does not exist: the script's call fails
/// with the host's ordinary unknown-function error, no child runs, and the app
/// keeps ticking.
#[test]
fn without_the_plugin_the_function_does_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absent");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { signal("started", "").set(process::start("<child>", [], "job")); }
fn on_ready() { signal("alive", "").set("yes"); }
fn on_process_exit(tag, code) { signal("exit", "").set(code); }
"#,
        None,
    );

    for _ in 0..20 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        signal(&app, "alive").as_deref(),
        Some("yes"),
        "the app went on running past the failed call"
    );
    assert_eq!(
        signal(&app, "started"),
        None,
        "no module, no `process` namespace, no value"
    );
    assert_eq!(signal(&app, "exit"), None, "nothing ran");

    let _ = std::fs::remove_dir_all(&dir);
}
