//! The app-lifecycle builtins (recent files, autostart) queue their commands
//! and round-trip through the applier.
//!
//! `add_recent_file`, `list_recent_files`, `clear_recent_files`,
//! `set_autostart`, and `query_autostart` reach `lumen-os-lifecycle` through
//! the same `ScriptCommand` seam `notify_ex` / `keep_awake` use: a shared
//! builtin queues one command, `apply_os_script_commands` applies it against
//! the `RecentFilesService` / `AutostartService` resources
//! `register_os_lifecycle` installs, and a read (`list_recent_files` /
//! `query_autostart`) answers back on the plugin-event bus as a callback.
//!
//! `single_instance_gate` covering the socket / named-pipe exclusion is
//! `lumen-os-lifecycle`'s own `second_launch_forwards_args` test; a desktop
//! is out of reach here, headless like the rest of this crate's tests, so
//! this file stops at the recent-files and autostart round trip.

// The candela host, compiled in: the module registry answers the program's
// implied host dependency from it.
use lumen_candela_dev as _;
use lumen_candela_dev::CandelaHost;
use lumen_core::app::App as EcsApp;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptCommand, ScriptHost, builtin_script_fns};
use std::path::{Path, PathBuf};

/// An app publishes process-global registries, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A temp app directory carrying `lumen.toml` with the id under test - the
/// same id `register_os_lifecycle` scopes the recent-files / autostart
/// storage under, so the round-trip test below can find and clean up what
/// it wrote.
fn app_dir(name: &str) -> (PathBuf, String) {
    let id = format!(
        "lumen-lifecycle-cmd-test-{name}-{}-{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    );
    let dir = std::env::temp_dir().join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(dir.join("lumen.toml"), format!("[app]\nid = \"{id}\"\n"))
        .expect("write lumen.toml");
    (dir, id)
}

/// Build and tick a headless app in `dir` whose candela program is `source`.
fn app_with(dir: &Path, source: &str) -> EcsApp {
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
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app.tick();
    app
}

/// Call `fn_name` in the app's script and return the commands it queued.
fn probe(app: &mut EcsApp, fn_name: &str) -> Vec<ScriptCommand> {
    let mut host = app.world.resource_mut::<CandelaHost>();
    host.drain_commands();
    let outcome = host
        .call(fn_name, &[])
        .unwrap_or_else(|e| panic!("`{fn_name}` ran: {e:?}"));
    assert!(outcome.found, "the script defines `{fn_name}`");
    outcome.commands
}

// -- the scripts --------------------------------------------------------

const PROBE_SOURCE: &str = r#"
import "lumen.cdl";

fn probe_recent() {
    lumen::add_recent_file("notes.txt", "");
    lumen::list_recent_files("g");
    lumen::clear_recent_files();
}

fn probe_autostart() {
    lumen::set_autostart(true);
    lumen::query_autostart("g");
}

fn main() {}
"#;

// -- the assertions -------------------------------------------------------

fn assert_recent_commands(queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 3, "unexpected commands: {queued:?}");
    let ScriptCommand::AddRecentFile { path, label } = &queued[0] else {
        panic!("expected AddRecentFile, got {:?}", queued[0]);
    };
    assert_eq!(path, "notes.txt", "path");
    assert_eq!(label, "", "an empty label stays empty");

    let ScriptCommand::ListRecentFiles { tag } = &queued[1] else {
        panic!("expected ListRecentFiles, got {:?}", queued[1]);
    };
    assert_eq!(tag, "g", "tag");

    assert!(
        matches!(queued[2], ScriptCommand::ClearRecentFiles),
        "expected ClearRecentFiles, got {:?}",
        queued[2]
    );
}

fn assert_autostart_commands(queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 2, "unexpected commands: {queued:?}");
    let ScriptCommand::SetAutostart { on } = &queued[0] else {
        panic!("expected SetAutostart, got {:?}", queued[0]);
    };
    assert!(*on, "on");

    let ScriptCommand::QueryAutostart { tag } = &queued[1] else {
        panic!("expected QueryAutostart, got {:?}", queued[1]);
    };
    assert_eq!(tag, "g", "tag");
}

#[test]
fn candela_queues_the_lifecycle_commands_field_for_field() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _id) = app_dir("candela");
    let mut app = app_with(&dir, PROBE_SOURCE);
    assert_recent_commands(&probe(&mut app, "probe_recent"));
    assert_autostart_commands(&probe(&mut app, "probe_autostart"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shared_table_describes_the_five_lifecycle_builtins() {
    for name in [
        "add_recent_file",
        "list_recent_files",
        "clear_recent_files",
        "set_autostart",
        "query_autostart",
    ] {
        assert!(
            builtin_script_fns().iter().any(|f| f.name == name),
            "the shared table describes `{name}`"
        );
    }
}

/// Full round trip for all five lifecycle commands, past the queued
/// command and back out again through a script callback:
///
/// - `add_recent_file` / `set_autostart` reach the SAME `RecentFilesService`
///   / `AutostartService` resources `register_os_lifecycle` installed, so
///   their effect is there to read straight back off the resource.
/// - `query_autostart` is asked once before enabling (exercises the
///   `on_autostart_disabled(tag)` reply) and once after (exercises
///   `on_autostart_enabled(tag)`); `list_recent_files` is asked in between
///   (exercises `on_recent_files(tag, paths)`); `clear_recent_files` then
///   empties the list. Each callback records that it ran by adding its own
///   marker entry, so what survives to the end proves every reply arrived
///   with the right tag: the original `notes.txt` was cleared, but the
///   three markers - written by callbacks that only run after the clear's
///   command already applied - are still there.
///
/// All headless: `RecentFilesService` is a plain JSON file and
/// `AutostartService` writes the login-item entry the same way `write_file`
/// writes any other file, neither needs a display or a GPU.
#[test]
fn recent_files_and_autostart_round_trip_through_the_applier() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, id) = app_dir("round-trip");
    const SOURCE: &str = r#"
import "lumen.cdl";

fn on_ready() {
    lumen::add_recent_file("notes.txt", "Notes");
    lumen::query_autostart("before");
    lumen::set_autostart(true);
    lumen::query_autostart("after");
    lumen::list_recent_files("listing");
    lumen::clear_recent_files();
}

fn on_recent_files(tag: string, paths: string) {
    lumen::add_recent_file("marker-recent-" + tag + ".txt", "");
}

fn on_autostart_enabled(tag: string) {
    lumen::add_recent_file("marker-autostart-enabled-" + tag + ".txt", "");
}

fn on_autostart_disabled(tag: string) {
    lumen::add_recent_file("marker-autostart-disabled-" + tag + ".txt", "");
}

fn main() {}
"#;
    let mut app = app_with(&dir, SOURCE);
    // `on_ready` fires on mount within `app_with`'s own ticks; the first of
    // these applies its six queued commands, later ones let the resulting
    // recent-files and autostart events dispatch to their callbacks and
    // those callbacks' own `add_recent_file` calls apply in turn.
    for _ in 0..4 {
        app.tick();
    }

    let recent = app
        .world
        .resource::<lumen_os_lifecycle::RecentFilesService>()
        .list(10);
    let paths: Vec<String> = recent
        .iter()
        .map(|e| e.path.display().to_string())
        .collect();
    assert!(
        !paths.iter().any(|p| p.ends_with("notes.txt")),
        "clear_recent_files removed the original entry: {paths:?}"
    );
    for marker in [
        "marker-recent-listing.txt",
        "marker-autostart-enabled-after.txt",
        "marker-autostart-disabled-before.txt",
    ] {
        assert!(
            paths.iter().any(|p| p.ends_with(marker)),
            "{marker} missing from the recorded callbacks: {paths:?}"
        );
    }

    assert_eq!(
        app.world
            .resource::<lumen_os_lifecycle::AutostartService>()
            .is_enabled(),
        Some(true),
        "set_autostart(true) reached the same AutostartService the runtime installed"
    );

    // Clean up what the round trip wrote: the autostart entry (by platform
    // path) and the whole per-app data directory the recent-files list
    // landed under.
    app.world
        .resource::<lumen_os_lifecycle::AutostartService>()
        .set_enabled(false);
    let data_dir = lumen_core::app_paths::data_dir_for(&id);
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The other half of the single-instance pipeline: `lumen-os-lifecycle`'s
/// own tests cover the socket mechanism and `poll_second_instance` turning a
/// forwarded argv into its script event; this covers that event reaching the
/// script as `on_second_instance(args)`. The event is written directly rather
/// than through a live socket - a second process forwarding real argv is what
/// the crate-level test already exercises, headless the same way this one is.
#[test]
fn second_instance_launch_reaches_the_script_callback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, id) = app_dir("second-instance");
    const SOURCE: &str = r#"
import "lumen.cdl";

fn on_second_instance(args: string) {
    lumen::add_recent_file(args, "");
}

fn main() {}
"#;
    let mut app = app_with(&dir, SOURCE);
    app.world
        .write_message(lumen_os_lifecycle::second_instance_event(&[
            "--open".to_string(),
            "report.pdf".to_string(),
        ]));
    app.tick();
    app.tick();

    let recent = app
        .world
        .resource::<lumen_os_lifecycle::RecentFilesService>()
        .list(10);
    assert_eq!(
        recent.len(),
        1,
        "on_second_instance's add_recent_file landed"
    );
    assert!(
        recent[0].path.ends_with("--open|report.pdf"),
        "args arrive joined by |: {}",
        recent[0].path.display()
    );

    let data_dir = lumen_core::app_paths::data_dir_for(&id);
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&dir);
}
