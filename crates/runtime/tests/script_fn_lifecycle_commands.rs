//! The app-lifecycle builtins (recent files, autostart) queue and round-trip
//! the same way in every language.
//!
//! `add_recent_file`, `list_recent_files`, `clear_recent_files`,
//! `set_autostart`, and `query_autostart` reach `lumen-os-lifecycle` through
//! the same `ScriptCommand` seam `notify_ex` / `keep_awake` use: a shared
//! builtin queues one command, `apply_os_script_commands` applies it against
//! the `RecentFilesService` / `AutostartService` resources
//! `register_os_lifecycle` installs, and a read (`list_recent_files` /
//! `query_autostart`) answers back on a message the script sees as a
//! callback.
//!
//! `single_instance_gate` covering the socket / named-pipe exclusion is
//! `lumen-os-lifecycle`'s own `second_launch_forwards_args` test; a desktop
//! is out of reach here, headless like the rest of this crate's tests, so
//! this file stops at the recent-files and autostart round trip.

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::Resource;
use lumen_core::app::App as EcsApp;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptCommand, ScriptHost, builtin_script_fns};
use lumen_script_candela::CandelaHost;
use lumen_script_lua::LuaHost;
use lumen_script_rhai::RhaiHost;
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

/// Build and tick a headless app whose script is `source` in `engine`, in `dir`.
fn app_with(dir: &Path, engine: &str, source: &str) -> EcsApp {
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
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app.tick();
    app
}

/// Call `fn_name` in the app's script and return the commands it queued.
fn probe<H>(app: &mut EcsApp, fn_name: &str) -> Vec<ScriptCommand>
where
    H: ScriptHost + Resource<Mutability = Mutable>,
{
    let mut host = app.world.resource_mut::<H>();
    host.drain_commands();
    let outcome = host
        .call(fn_name, &[])
        .unwrap_or_else(|e| panic!("`{fn_name}` ran: {e:?}"));
    assert!(outcome.found, "the script defines `{fn_name}`");
    outcome.commands
}

// -- the scripts --------------------------------------------------------

const RHAI_SOURCE: &str = r#"
fn probe_recent() {
    add_recent_file("notes.txt", "");
    list_recent_files("g");
    clear_recent_files();
}

fn probe_autostart() {
    set_autostart(true);
    query_autostart("g");
}
"#;

const LUA_SOURCE: &str = r#"
function probe_recent()
    add_recent_file("notes.txt", "")
    list_recent_files("g")
    clear_recent_files()
end

function probe_autostart()
    set_autostart(true)
    query_autostart("g")
end
"#;

const CANDELA_SOURCE: &str = r#"
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

fn assert_recent_commands(lang: &str, queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 3, "{lang}: unexpected commands: {queued:?}");
    let ScriptCommand::AddRecentFile { path, label } = &queued[0] else {
        panic!("{lang}: expected AddRecentFile, got {:?}", queued[0]);
    };
    assert_eq!(path, "notes.txt", "{lang}: path");
    assert_eq!(label, "", "{lang}: an empty label stays empty");

    let ScriptCommand::ListRecentFiles { tag } = &queued[1] else {
        panic!("{lang}: expected ListRecentFiles, got {:?}", queued[1]);
    };
    assert_eq!(tag, "g", "{lang}: tag");

    assert!(
        matches!(queued[2], ScriptCommand::ClearRecentFiles),
        "{lang}: expected ClearRecentFiles, got {:?}",
        queued[2]
    );
}

fn assert_autostart_commands(lang: &str, queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 2, "{lang}: unexpected commands: {queued:?}");
    let ScriptCommand::SetAutostart { on } = &queued[0] else {
        panic!("{lang}: expected SetAutostart, got {:?}", queued[0]);
    };
    assert!(*on, "{lang}: on");

    let ScriptCommand::QueryAutostart { tag } = &queued[1] else {
        panic!("{lang}: expected QueryAutostart, got {:?}", queued[1]);
    };
    assert_eq!(tag, "g", "{lang}: tag");
}

#[test]
fn rhai_queues_the_lifecycle_commands_field_for_field() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _id) = app_dir("rhai");
    let mut app = app_with(&dir, "rhai", RHAI_SOURCE);
    assert_recent_commands("rhai", &probe::<RhaiHost>(&mut app, "probe_recent"));
    assert_autostart_commands("rhai", &probe::<RhaiHost>(&mut app, "probe_autostart"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lua_queues_the_lifecycle_commands_field_for_field() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _id) = app_dir("lua");
    let mut app = app_with(&dir, "lua", LUA_SOURCE);
    assert_recent_commands("lua", &probe::<LuaHost>(&mut app, "probe_recent"));
    assert_autostart_commands("lua", &probe::<LuaHost>(&mut app, "probe_autostart"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn candela_queues_the_lifecycle_commands_field_for_field() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _id) = app_dir("candela");
    let mut app = app_with(&dir, "candela", CANDELA_SOURCE);
    assert_recent_commands("candela", &probe::<CandelaHost>(&mut app, "probe_recent"));
    assert_autostart_commands(
        "candela",
        &probe::<CandelaHost>(&mut app, "probe_autostart"),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_host_sees_the_five_lifecycle_builtins() {
    for name in [
        "add_recent_file",
        "list_recent_files",
        "clear_recent_files",
        "set_autostart",
        "query_autostart",
    ] {
        let f = builtin_script_fns()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("the shared table describes `{name}`"));
        for lang in ["rhai", "lua", "candela"] {
            assert!(f.visible_to(lang), "`{name}` is hidden from {lang}");
        }
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
fn on_ready() {
    add_recent_file("notes.txt", "Notes");
    query_autostart("before");
    set_autostart(true);
    query_autostart("after");
    list_recent_files("listing");
    clear_recent_files();
}

fn on_recent_files(tag, paths) {
    add_recent_file("marker-recent-" + tag + ".txt", "");
}

fn on_autostart_enabled(tag) {
    add_recent_file("marker-autostart-enabled-" + tag + ".txt", "");
}

fn on_autostart_disabled(tag) {
    add_recent_file("marker-autostart-disabled-" + tag + ".txt", "");
}
"#;
    let mut app = app_with(&dir, "rhai", SOURCE);
    // `on_ready` fires on mount within `app_with`'s own ticks; the first of
    // these applies its six queued commands, later ones let the resulting
    // `RecentFilesRead` / `AutostartRead` messages dispatch to their
    // callbacks and those callbacks' own `add_recent_file` calls apply in
    // turn.
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
/// own tests cover the socket mechanism and `poll_second_instance` draining
/// into a `SecondInstanceLaunched` message; this covers that message
/// reaching the script as `on_second_instance(args)`. The message is
/// written directly rather than through a live socket - a second process
/// forwarding real argv is what the crate-level test already exercises,
/// headless the same way this one is.
#[test]
fn second_instance_launch_reaches_the_script_callback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, id) = app_dir("second-instance");
    const SOURCE: &str = r#"
fn on_second_instance(args) {
    add_recent_file(args, "");
}
"#;
    let mut app = app_with(&dir, "rhai", SOURCE);
    app.world
        .write_message(lumen_core::input::SecondInstanceLaunched {
            args: vec!["--open".to_string(), "report.pdf".to_string()],
        });
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
