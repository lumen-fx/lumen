//! The compiled-in shape: [`ArchivePlugin`] installed like any other plugin
//! on a headless app, driving the script surface in process.
//!
//! What these prove, once per concern:
//!
//! - `archive::extract` reaches a candela script through the generic
//!   `ScriptFnRegistry`, as the `host "archive"` block the host synthesizes
//!   from what the plugin registered;
//! - both paths resolve against the app directory, so an archive the app
//!   ships unpacks the same wherever the app was started from;
//! - the outcome arrives over the plugin-event bus: `on_archive_done` fires
//!   with the tag, the destination, and the count, and a per-tag
//!   `on("archive_done", tag, fn)` registration wins over it;
//! - a refused archive reports on `archive_error` instead, and so does a job
//!   the module would not take;
//! - without the plugin the function does not exist: the program fails to
//!   compile against the missing namespace, and the app keeps running.
//!
//! Every path a script here names is relative and spelled with forward
//! slashes. A host path put into script text would carry backslashes on
//! Windows, where a script lexer reads them as escape sequences and refuses
//! the whole program; keep paths out of the script and let the module resolve
//! them.

// The candela host, compiled in: the module registry answers the script's
// implied host dependency from it.
use lumen_candela_dev as _;

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

/// Build a headless app in `dir` running one candela script, with the given
/// plugin.
fn build_app(dir: &std::path::Path, source: &str, plugin: Option<ArchivePlugin>) -> EcsApp {
    lumen_core::plugin_events::discard_plugin_events();
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

/// The script takes the job, the archive lands beside the app, and the
/// fallback handler is called with the tag, the destination, and the file
/// count.
#[test]
fn a_script_unpacks_into_the_app_directory() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("unpack", "bundle.zip");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("taken", archive::extract("bundle.zip", "out", "bundle", Default::default()));
}

fn on_archive_done(tag: string, dest: string, count: int) {
    lumen::signal_set("done", tag);
    lumen::signal_set_int("count", count);
    lumen::signal_set("dest", dest);
}

fn on_archive_error(tag: string, message: string) {
    lumen::signal_set("failed", message);
}

fn main() {}
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
        r#"import "lumen.cdl";

fn on_start() {
    lumen::on("archive_done", "bundle", "bundle_ready");
    archive::extract("bundle.zip", "out", "bundle", Default::default());
}

fn bundle_ready(tag: string, dest: string, count: int) {
    lumen::signal_set_int("special", count);
}

fn on_archive_done(tag: string, dest: string, count: int) {
    lumen::signal_set("fallback", tag);
}

fn main() {}
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
        r#"import "lumen.cdl";

fn on_start() {
    archive::extract("hostile.zip", "out", "hostile", Default::default());
}

fn on_archive_done(tag: string, dest: string, count: int) {
    lumen::signal_set("done", tag);
}

fn on_archive_error(tag: string, message: string) {
    lumen::signal_set("tag", tag);
    lumen::signal_set("message", message);
}

fn main() {}
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
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("first", archive::extract("bundle.zip", "one", "same", Default::default()));
    lumen::signal_set_bool("again", archive::extract("bundle.zip", "two", "same", Default::default()));
    lumen::signal_set_bool("over", archive::extract("bundle.zip", "three", "other", Default::default()));
}

fn on_archive_error(tag: string, message: string) {
    lumen::signal_set("why_" + tag, message);
}

fn main() {}
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

/// `include` in the options keeps only the matching files, and the count
/// reports what was written.
#[test]
fn an_include_filter_keeps_only_the_matching_files() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("include", "bundle.zip");
    testkit::natives_jar(&dir.join("natives.jar")).expect("jar fixture");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    archive::extract(
        "natives.jar",
        "out",
        "natives",
        archive::ExtractOptions { include: ["*.so"] },
    );
}

fn on_archive_done(tag: string, dest: string, count: int) {
    lumen::signal_set_int("count", count);
}

fn on_archive_error(tag: string, message: string) {
    lumen::signal_set("failed", message);
}

fn main() {}
"#,
        Some(ArchivePlugin::default()),
    );

    assert!(
        tick_until(&mut app, 10.0, |app| signal(app, "count").is_some()),
        "on_archive_done must fire; failed={:?}",
        signal(&app, "failed")
    );
    assert_eq!(signal(&app, "count").as_deref(), Some("1"));
    assert!(
        dir.join("out/linux/x64/org/lwjgl/liblwjgl.so").is_file(),
        "the library was written"
    );
    assert!(
        !dir.join("out/META-INF").exists(),
        "the jar metadata was not"
    );
    assert!(
        !dir.join("out/linux/x64/org/lwjgl/liblwjgl.so.sha1")
            .exists(),
        "nor the checksum beside the library"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the function does not exist: the program fails to
/// compile against the missing `archive` namespace, nothing is unpacked, and
/// the app keeps ticking with the failure on record.
#[test]
fn without_the_plugin_the_function_does_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absent", "bundle.zip");
    let mut app = build_app(
        &dir,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool(
        "taken",
        archive::extract("bundle.zip", "out", "bundle", archive::ExtractOptions { include: [] }),
    );
}

fn main() {}
"#,
        None,
    );

    for _ in 0..10 {
        app.tick();
    }
    let failure = app
        .world
        .get_resource::<lumen_script::ScriptLoadFailure>()
        .expect("the program that names a missing namespace fails to load");
    assert!(
        failure.0.contains("`archive`"),
        "the failure names the namespace: {}",
        failure.0
    );
    assert_eq!(
        signal(&app, "taken"),
        None,
        "no module, no `archive` namespace, no value"
    );
    assert!(!dir.join("out").exists(), "nothing was unpacked");

    let _ = std::fs::remove_dir_all(&dir);
}
