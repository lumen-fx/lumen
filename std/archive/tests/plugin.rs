//! The compiled-in shape: [`ArchivePlugin`] installed like any other plugin
//! on a headless app, driving the script surface in process.
//!
//! What these prove, once per concern:
//!
//! - `archive::extract` reaches a script through the generic
//!   `ScriptFnRegistry`, in Rhai and in candela;
//! - both paths resolve against the app directory, so an archive the app
//!   ships unpacks the same wherever the app was started from;
//! - the outcome arrives over the plugin-event bus: `on_archive_done` fires
//!   with the tag, the destination, and the count, and a per-tag
//!   `on("archive_done", tag, fn)` registration wins over it;
//! - a refused archive reports on `archive_error` instead, and so does a job
//!   the module would not take;
//! - without the plugin the function does not exist, and the app keeps
//!   running after the script's own unknown-function error.
//!
//! Every path a script here names is relative and spelled with forward
//! slashes. A host path put into script text would carry backslashes on
//! Windows, where a script lexer reads them as escape sequences and refuses
//! the whole program; keep paths out of the script and let the module resolve
//! them.

use lumen_archive::{ArchivePlugin, testkit};
use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// The app directory, the DOM snapshot, the property store, and the
/// plugin-event bus are process-global, so the headless apps here run one at
/// a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh app directory carrying `lumen.toml` and one archive to unpack.
fn app_dir(name: &str, archive: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-archive-plugin-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-archive-plugin-{name}\"\n"),
    )
    .expect("lumen.toml");
    testkit::normal_zip(&dir.join(archive)).expect("archive fixture");
    dir
}

/// Build a headless app in `dir` running one script, with the given plugin.
fn build_app(
    dir: &std::path::Path,
    engine: &str,
    source: &str,
    plugin: Option<ArchivePlugin>,
) -> EcsApp {
    lumen_core::plugin_events::discard_plugin_events();
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
    // One tick, so `on_start` has run and whatever it queued has been picked
    // up by the time a test reads a signal.
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

/// Tick with a little wall time between ticks (the extraction runs on
/// another thread) until `pred` holds or the deadline passes.
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

/// Rhai takes the job, the archive lands beside the app, and the fallback
/// handler is called with the tag, the destination, and the file count.
#[test]
fn rhai_unpacks_into_the_app_directory() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("rhai", "bundle.zip");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    signal("taken", "").set(archive::extract("bundle.zip", "out", "bundle"));
}
fn on_archive_done(tag, dest, count) {
    signal("done", "").set(tag);
    signal("count", "").set(count);
    signal("dest", "").set(dest);
}
fn on_archive_error(tag, message) { signal("failed", "").set(message); }
"#,
        Some(ArchivePlugin::default()),
    );

    assert_eq!(
        signal(&app, "taken").as_deref(),
        Some("true"),
        "the call answers straight away with the job being taken"
    );
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "done").is_some()),
        "on_archive_done must fire; failed={:?}",
        signal(&app, "failed")
    );
    assert_eq!(signal(&app, "done").as_deref(), Some("bundle"));
    assert_eq!(signal(&app, "count").as_deref(), Some("3"));
    assert_eq!(signal(&app, "failed"), None);
    let dest = signal(&app, "dest").expect("the destination reaches the handler");
    assert_eq!(
        std::path::Path::new(&dest),
        dir.join("out"),
        "a relative destination named a directory beside the app"
    );
    for (member, body) in testkit::MEMBERS {
        assert_eq!(
            std::fs::read_to_string(dir.join("out").join(member)).ok(),
            Some(body.to_string()),
            "{member}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A per-tag `on("archive_done", tag, fn)` registration wins over the
/// `on_archive_done` fallback, like every other plugin event.
#[test]
fn a_per_tag_handler_wins_over_the_fallback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("per-tag", "bundle.zip");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    on("archive_done", "bundle", "bundle_ready");
    archive::extract("bundle.zip", "out", "bundle");
}
fn bundle_ready(tag, dest, count) { signal("special", "").set(count); }
fn on_archive_done(tag, dest, count) { signal("fallback", "").set(tag); }
"#,
        Some(ArchivePlugin::default()),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "special").is_some()),
        "the per-tag handler must fire"
    );
    assert_eq!(signal(&app, "special").as_deref(), Some("3"));
    assert_eq!(signal(&app, "fallback"), None, "the fallback must not fire");

    let _ = std::fs::remove_dir_all(&dir);
}

/// An archive holding an entry that climbs out of the destination fails the
/// whole extraction, and the message names the entry.
#[test]
fn a_hostile_archive_reports_on_the_error_event() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("hostile", "bundle.zip");
    testkit::escaping_zip(&dir.join("hostile.zip")).expect("hostile fixture");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { archive::extract("hostile.zip", "out", "hostile"); }
fn on_archive_done(tag, dest, count) { signal("done", "").set(tag); }
fn on_archive_error(tag, message) {
    signal("tag", "").set(tag);
    signal("message", "").set(message);
}
"#,
        Some(ArchivePlugin::default()),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "message").is_some()),
        "on_archive_error must fire"
    );
    assert_eq!(signal(&app, "tag").as_deref(), Some("hostile"));
    let message = signal(&app, "message").expect("a message");
    assert!(
        message.contains(testkit::ESCAPING_ENTRY),
        "the message names the entry: {message}"
    );
    assert_eq!(signal(&app, "done"), None, "no job finished");
    assert!(
        !dir.join("escape.txt").exists(),
        "nothing was written outside the destination"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A job the module will not take answers false in the call and explains
/// itself on `archive_error`: a tag already running, and one job past the
/// configured limit.
#[test]
fn a_refused_job_answers_false_and_reports() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("refused", "bundle.zip");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    signal("first", "").set(archive::extract("bundle.zip", "one", "same"));
    signal("again", "").set(archive::extract("bundle.zip", "two", "same"));
    signal("over", "").set(archive::extract("bundle.zip", "three", "other"));
}
fn on_archive_error(tag, message) { signal("why_" + tag, "").set(message); }
"#,
        // One at a time, so the third call is one past the limit while the
        // first is still queued.
        Some(ArchivePlugin::with_max_concurrent(1)),
    );

    assert_eq!(signal(&app, "first").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "again").as_deref(),
        Some("false"),
        "a tag already in flight is refused"
    );
    assert_eq!(
        signal(&app, "over").as_deref(),
        Some("false"),
        "a job past the limit is refused"
    );
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "why_other").is_some()),
        "the refusal reaches the error handler"
    );
    let same = signal(&app, "why_same").expect("the duplicate tag reported");
    assert!(same.contains("already running"), "{same}");
    let other = signal(&app, "why_other").expect("the limit reported");
    assert!(other.contains("limit"), "{other}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// candela reaches the same function through the `host "archive"` block the
/// host synthesizes from what the plugin registered.
#[test]
fn candela_reaches_the_module_surface_through_its_namespace() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("candela", "bundle.zip");
    let mut app = build_app(
        &dir,
        "candela",
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("taken", archive::extract("bundle.zip", "out", "bundle"));
}

fn on_archive_done(tag: string, dest: string, count: int) {
    lumen::signal_set("done", tag);
    lumen::signal_set_int("count", count);
}

fn on_archive_error(tag: string, message: string) {
    lumen::signal_set("failed", message);
}

fn main() {}
"#,
        Some(ArchivePlugin::default()),
    );

    assert_eq!(signal(&app, "taken").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "done").is_some()),
        "on_archive_done must fire; failed={:?}",
        signal(&app, "failed")
    );
    assert_eq!(signal(&app, "done").as_deref(), Some("bundle"));
    assert_eq!(signal(&app, "count").as_deref(), Some("3"));
    assert_eq!(
        std::fs::read_to_string(dir.join("out/top.txt")).ok(),
        Some("top".to_string())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the function does not exist: the script's call fails
/// with the host's ordinary unknown-function error, nothing is unpacked, and
/// the app keeps ticking.
#[test]
fn without_the_plugin_the_function_does_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absent", "bundle.zip");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { signal("taken", "").set(archive::extract("bundle.zip", "out", "bundle")); }
fn on_ready() { signal("alive", "").set("yes"); }
"#,
        None,
    );

    for _ in 0..10 {
        app.tick();
    }
    assert_eq!(
        signal(&app, "alive").as_deref(),
        Some("yes"),
        "the app went on running past the failed call"
    );
    assert_eq!(
        signal(&app, "taken"),
        None,
        "no module, no `archive` namespace, no value"
    );
    assert!(!dir.join("out").exists(), "nothing was unpacked");

    let _ = std::fs::remove_dir_all(&dir);
}
