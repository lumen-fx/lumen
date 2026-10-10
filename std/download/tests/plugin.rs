//! The compiled-in shape: [`DownloadPlugin`] installed like any other plugin
//! on a headless app, driving the whole script surface in process against the
//! crate's own loopback server.
//!
//! What these prove, once per concern:
//!
//! - `download::to_file` reaches a candela script through the generic
//!   `ScriptFnRegistry`, under the `download` namespace, and the file it names
//!   lands beside the app;
//! - progress, completion, and failure arrive over the plugin-event bus, and a
//!   per-tag `on("download_done", tag, fn)` registration wins over the
//!   `on_download_done` fallback;
//! - a reply that is not 2xx is a failure the script hears about, not a saved
//!   error page;
//! - a tag with a transfer already running is refused rather than superseded,
//!   and a call past the configured concurrency is refused too;
//! - without the plugin the function does not exist: the script fails to
//!   compile, and the app keeps running without it.

// The candela host, compiled into the test binary: there is no shared engine
// to open the host module beside.
use lumen_candela_dev as _;
use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_download::DownloadPlugin;
use lumen_download::testkit::{BODY, TestServer};
use lumen_download::transfer::Limits;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// The app directory, the property store, and the plugin-event bus are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh app directory carrying a `lumen.toml` with an id of its own.
fn app_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-download-plugin-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-download-plugin-{name}\"\n"),
    )
    .expect("lumen.toml");
    dir
}

/// Build a headless app in `dir` running one candela script, with or without
/// the plugin.
fn build_app(dir: &std::path::Path, source: &str, plugin: Option<DownloadPlugin>) -> EcsApp {
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
            source_map: Default::default(),
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

/// Tick with wall time between ticks (the transfer thread needs some) until
/// `pred` holds or the deadline passes.
///
/// The deadline is generous because a finished transfer is flushed to disk
/// before it is renamed into place, and an `fsync` on a loaded machine takes
/// far longer than the transfer it follows. A run that is not stuck returns as
/// soon as the predicate holds, so the headroom costs nothing.
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

/// The whole good path: the call is accepted, progress reports the declared
/// size, the file lands beside the app, and `on_download_done` carries the
/// path it was written to.
#[test]
fn a_script_downloads_a_verified_file_beside_the_app() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("verified");
    let sum = {
        use sha2::{Digest, Sha256};
        lumen_download::transfer::hex(Sha256::digest(BODY).as_slice())
    };
    let url = server.url("/fixed");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("started", download::to_file("{url}", "payload.bin", "art", "sha256:{sum}"));
}}

fn on_download_progress(tag: string, received: int, total: int) {{
    lumen::signal_set("progress", tag + ":" + str(received) + "/" + str(total));
}}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("done", tag + ":" + path); }}

fn on_download_error(tag: string, message: string) {{ lumen::signal_set("error", message); }}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::default()),
    );

    assert_eq!(
        signal(&app, "started").as_deref(),
        Some("true"),
        "the call answers true once the transfer is running"
    );
    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "done").is_some()),
        "on_download_done must fire; error={:?} progress={:?}",
        signal(&app, "error"),
        signal(&app, "progress")
    );

    let dest = dir.join("payload.bin");
    assert_eq!(
        std::fs::read(&dest).expect("the file"),
        BODY,
        "a relative destination names a file beside the app"
    );
    assert_eq!(
        signal(&app, "done").as_deref(),
        Some(format!("art:{}", dest.display()).as_str()),
        "the done handler carries the tag and the path as written"
    );
    assert_eq!(
        signal(&app, "progress").as_deref(),
        Some(format!("art:{}/{}", BODY.len(), BODY.len()).as_str()),
        "the last progress report reaches the declared total"
    );
    assert_eq!(signal(&app, "error"), None, "nothing failed");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A per-tag `on("download_done", tag, fn)` registration wins over the
/// `on_download_done` fallback, like every other plugin event.
#[test]
fn a_per_tag_handler_wins_over_the_fallback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("routed");
    let url = server.url("/fixed");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::on("download_done", "art", "art_arrived");
    download::to_file("{url}", "payload.bin", "art", "");
}}

fn art_arrived(tag: string, path: string) {{ lumen::signal_set("routed", tag); }}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("fallback", tag); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::default()),
    );

    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "routed").is_some()),
        "the per-tag handler must fire"
    );
    assert_eq!(signal(&app, "routed").as_deref(), Some("art"));
    assert_eq!(signal(&app, "fallback"), None, "the fallback must not fire");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A reply that is not 2xx fails the download: the error handler hears the
/// status, and nothing is written where the file would have gone.
#[test]
fn a_missing_file_reaches_the_error_handler() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("missing");
    let url = server.url("/missing");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{ download::to_file("{url}", "payload.bin", "art", ""); }}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("done", path); }}

fn on_download_error(tag: string, message: string) {{ lumen::signal_set("error", tag + ":" + message); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::default()),
    );

    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "error").is_some()),
        "on_download_error must fire"
    );
    let message = signal(&app, "error").expect("the error");
    assert!(message.starts_with("art:"), "keyed by the tag: {message}");
    assert!(
        message.contains("HTTP 404"),
        "the status is named: {message}"
    );
    assert_eq!(signal(&app, "done"), None, "nothing completed");
    assert!(
        !dir.join("payload.bin").exists(),
        "the 404 page was not saved as the file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A tag with a transfer already running is refused rather than superseded:
/// one tag means one download, because both would report under the same key.
#[test]
fn a_tag_already_downloading_is_refused() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("duplicate");
    // A body that arrives in pieces, so the second call is made while the
    // first transfer is still running.
    let slow = server.url("/drip");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("first", download::to_file("{slow}", "payload.bin", "art", ""));
    lumen::signal_set_bool("second", download::to_file("{slow}", "other.bin", "art", ""));
}}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("done", path); }}

fn on_download_error(tag: string, message: string) {{ lumen::signal_set("error", message); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::default()),
    );

    assert_eq!(signal(&app, "first").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "second").as_deref(),
        Some("false"),
        "the second call is refused while the first runs"
    );
    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "error").is_some()),
        "the refusal is reported to the tag"
    );
    let message = signal(&app, "error").expect("the error");
    assert!(
        message.contains("already running"),
        "the refusal says why: {message}"
    );

    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "done").is_some()),
        "the first transfer still finishes"
    );
    assert_eq!(
        std::fs::read(dir.join("payload.bin")).expect("the file"),
        BODY
    );
    assert!(
        !dir.join("other.bin").exists(),
        "the refused call downloaded nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The concurrency the app configured is the number of transfers that run at
/// once: with one, a second tag waits rather than joining in, and hears why.
#[test]
fn the_configured_concurrency_bounds_what_runs_at_once() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("concurrency");
    let slow = server.url("/drip");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("first", download::to_file("{slow}", "one.bin", "a", ""));
    lumen::signal_set_bool("second", download::to_file("{slow}", "two.bin", "b", ""));
}}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("done", tag); }}

fn on_download_error(tag: string, message: string) {{ lumen::signal_set("error", tag + ":" + message); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::with_limits(Limits::default(), 1)),
    );

    assert_eq!(signal(&app, "first").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "second").as_deref(),
        Some("false"),
        "the second tag is over the limit of one"
    );
    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "error").is_some()),
        "the refusal is reported to the tag that asked"
    );
    let message = signal(&app, "error").expect("the error");
    assert!(
        message.starts_with("b:"),
        "keyed by the refused tag: {message}"
    );
    assert!(
        message.contains("which is the limit"),
        "the refusal names the limit: {message}"
    );

    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "done").is_some()),
        "the transfer that did start finishes"
    );
    assert_eq!(signal(&app, "done").as_deref(), Some("a"));
    assert_eq!(std::fs::read(dir.join("one.bin")).expect("the file"), BODY);
    assert!(
        !dir.join("two.bin").exists(),
        "the refused tag wrote nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `download::cancel` stops a transfer stuck on a quiet server: the tag hears
/// `cancelled` through `download_error`, and both the tag and its concurrency
/// slot are free at once, so the error handler can start the next transfer
/// under the same tag with a limit of one. Cancelling a tag with nothing
/// running answers false.
#[test]
fn a_cancelled_download_frees_its_tag_and_its_slot() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("cancel");
    let stall = server.url("/stall");
    let fixed = server.url("/fixed");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("idle_cancel", download::cancel("nothing"));
    lumen::signal_set_bool("started", download::to_file("{stall}", "big.bin", "big", ""));
    lumen::set_timeout("stop", 200);
}}

fn on_timer(name: string) {{ lumen::signal_set_bool("cancelled", download::cancel("big")); }}

fn on_download_error(tag: string, message: string) {{
    lumen::signal_set("error", tag + ":" + message);
    lumen::signal_set_bool("retry", download::to_file("{fixed}", "small.bin", "big", ""));
}}

fn on_download_done(tag: string, path: string) {{ lumen::signal_set("done", tag); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::with_limits(Limits::default(), 1)),
    );

    assert_eq!(signal(&app, "idle_cancel").as_deref(), Some("false"));
    assert_eq!(signal(&app, "started").as_deref(), Some("true"));
    let started = std::time::Instant::now();
    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "done").is_some()),
        "the retry under the freed tag finishes; error={:?} retry={:?}",
        signal(&app, "error"),
        signal(&app, "retry")
    );
    assert!(
        started.elapsed() < lumen_download::testkit::STALL,
        "the stalled transfer was waited out rather than cancelled"
    );
    assert_eq!(signal(&app, "cancelled").as_deref(), Some("true"));
    assert_eq!(signal(&app, "error").as_deref(), Some("big:cancelled"));
    assert_eq!(signal(&app, "retry").as_deref(), Some("true"));
    assert_eq!(
        std::fs::read(dir.join("small.bin")).expect("the file"),
        BODY
    );
    assert!(
        !dir.join("big.bin").exists(),
        "the cancelled transfer wrote nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A checksum the module cannot read is that tag's error, and no request is
/// made at all.
#[test]
fn an_unreadable_checksum_is_refused_before_the_request() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("checksum");
    let url = server.url("/fixed");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("started", download::to_file("{url}", "payload.bin", "art", "md5:abcdef"));
}}

fn on_download_error(tag: string, message: string) {{ lumen::signal_set("error", message); }}

fn on_download_progress(tag: string, received: int, total: int) {{}}

fn main() {{}}
"#
        ),
        Some(DownloadPlugin::default()),
    );

    assert_eq!(signal(&app, "started").as_deref(), Some("false"));
    assert!(
        tick_until(&mut app, 30.0, |app| signal(app, "error").is_some()),
        "the refusal reaches the error handler"
    );
    assert!(
        signal(&app, "error")
            .expect("the error")
            .contains("unsupported checksum format"),
        "{:?}",
        signal(&app, "error")
    );
    assert!(!dir.join("payload.bin").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the function does not exist: there is no `download`
/// namespace to declare, so the program fails to compile and the failure
/// names it. Nothing is downloaded, and the app keeps ticking without its
/// script.
#[test]
fn without_the_plugin_the_function_does_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = TestServer::start();
    let dir = app_dir("absent");
    let url = server.url("/fixed");
    let mut app = build_app(
        &dir,
        &format!(
            r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("started", download::to_file("{url}", "payload.bin", "art", ""));
}}

fn main() {{}}
"#
        ),
        None,
    );

    for _ in 0..10 {
        app.tick();
    }
    let failure = app
        .world
        .get_resource::<lumen_script::ScriptLoadFailure>()
        .expect("the program failed to load");
    assert!(
        failure.0.contains("no `download` namespace"),
        "the failure names the missing namespace: {}",
        failure.0
    );
    assert_eq!(
        signal(&app, "started"),
        None,
        "no module, no `download` namespace, no value"
    );
    assert!(
        !dir.join("payload.bin").exists(),
        "nothing reached the network"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
