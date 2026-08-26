//! The compiled-in shape: [`AudioPlugin`] installed like any other plugin on
//! a headless app, driving the whole script surface in process.
//!
//! What these prove, once per concern:
//!
//! - the `audio_*` functions reach a script through the generic
//!   `ScriptFnRegistry`, in Rhai and (through the prelude's `host "lumen"`
//!   block) in candela;
//! - `audio_play` resolves an app-relative path against the app directory,
//!   not the process working directory, headless included;
//! - the position signals are written into the shared `PropertyStore`;
//! - end of track arrives over the generic plugin-event bus: the
//!   `on_audio_end(path)` fallback fires, and a per-key
//!   `on("audio_end", path, fn)` registration wins over it;
//! - without the plugin the functions simply do not exist, and the app
//!   keeps running after the script's own unknown-function error.

use lumen_audio::{AudioPlugin, synth};
use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// Nav, the DOM snapshot, the property store, and the plugin-event bus are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh app directory holding a generated wav under `name`.
fn app_dir_with_wav(name: &str, secs: f32) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_audio_plugin_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).expect("temp app dir");
    synth::write_wav(&dir.join(name), &synth::sine(440.0, secs)).expect("wav written");
    dir
}

/// Build a headless app in `dir` running one script, with or without the
/// plugin.
fn build_app(dir: &std::path::Path, engine: &str, source: &str, with_plugin: bool) -> EcsApp {
    build_app_with_assets(dir, engine, source, with_plugin, None)
}

/// [`build_app`] plus an optional `.lpak` archive served through the asset
/// server, the way `lumenc run --assets` installs one.
fn build_app_with_assets(
    dir: &std::path::Path,
    engine: &str,
    source: &str,
    with_plugin: bool,
    assets: Option<&std::path::Path>,
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
    if let Some(lpak) = assets {
        opts = opts.with_assets(lpak);
    }
    if with_plugin {
        // The deviceless shape: everything but sound, and no device probe on
        // the test machine.
        opts = opts.with_plugin(AudioPlugin::inert());
    }
    opts.bounded = true;
    let (app, _window) = build_headless_app(opts).expect("build headless app");
    app
}

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

/// Tick with wall time between ticks (the loader thread needs some) until
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

fn parsed(app: &EcsApp, name: &str) -> f64 {
    signal(app, name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// Rhai calls the registered functions; the app-relative path resolves
/// against the app directory; duration decodes and the position advances.
#[test]
fn rhai_drives_playback_through_the_registry() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 5.0);
    let mut app = build_app(
        &dir,
        "rhai",
        r#"fn on_start() { audio_volume(0.5); audio_play("tone.wav"); }"#,
        true,
    );

    assert!(
        tick_until(&mut app, 3.0, |app| parsed(app, "audio_duration") > 4.0),
        "the decoded duration must reach the audio_duration signal; got {:?}",
        signal(&app, "audio_duration")
    );
    assert_eq!(signal(&app, "audio_playing").as_deref(), Some("true"));
    assert!(
        tick_until(&mut app, 2.0, |app| parsed(app, "audio_position") > 0.0),
        "the playhead must advance; got {:?}",
        signal(&app, "audio_position")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A track that exists only inside the app's `.lpak` plays: the loader
/// resolves the app-relative path through the bundle the way images do,
/// instead of reading the filesystem directly.
#[test]
fn a_bundle_packed_track_plays() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let staging = app_dir_with_wav("tone.wav", 5.0);
    let lpak = std::env::temp_dir().join(format!("lumen_audio_lpak_{}.lpak", std::process::id()));
    lumen_assets::LumenBundle::pack_dir(&staging, &lpak).expect("pack lpak");
    // The app dir holds no loose wav: the bundle is the only source.
    std::fs::remove_file(staging.join("tone.wav")).expect("drop the loose wav");
    let mut app = build_app_with_assets(
        &staging,
        "rhai",
        r#"fn on_start() { audio_play("tone.wav"); }"#,
        true,
        Some(&lpak),
    );

    assert!(
        tick_until(&mut app, 3.0, |app| parsed(app, "audio_duration") > 4.0),
        "the bundled track must decode through the source chain; got {:?}",
        signal(&app, "audio_duration")
    );
    assert_eq!(signal(&app, "audio_playing").as_deref(), Some("true"));

    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&lpak);
}

/// A `lumen://app/...` URI names the same bundled track directly, with no
/// app-relative resolution in between.
#[test]
fn a_lumen_uri_track_plays() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let staging = app_dir_with_wav("tone.wav", 5.0);
    let lpak = std::env::temp_dir().join(format!("lumen_audio_uri_{}.lpak", std::process::id()));
    lumen_assets::LumenBundle::pack_dir(&staging, &lpak).expect("pack lpak");
    std::fs::remove_file(staging.join("tone.wav")).expect("drop the loose wav");
    let mut app = build_app_with_assets(
        &staging,
        "rhai",
        r#"fn on_start() { audio_play("lumen://app/tone.wav"); }"#,
        true,
        Some(&lpak),
    );

    assert!(
        tick_until(&mut app, 3.0, |app| parsed(app, "audio_duration") > 4.0),
        "the URI-addressed track must decode; got {:?}",
        signal(&app, "audio_duration")
    );

    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&lpak);
}

/// An absolute path keeps working exactly as before: straight to the
/// filesystem, no bundle root involved.
#[test]
fn an_absolute_path_still_plays() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 5.0);
    let abs = dir
        .join("tone.wav")
        .display()
        .to_string()
        .replace('\\', "/");
    let mut app = build_app(
        &dir,
        "rhai",
        &format!(r#"fn on_start() {{ audio_play("{abs}"); }}"#),
        true,
    );

    assert!(
        tick_until(&mut app, 3.0, |app| parsed(app, "audio_duration") > 4.0),
        "the absolute path must decode; got {:?}",
        signal(&app, "audio_duration")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A path nothing holds - no bundle entry, no source, no file - reports the
/// per-track load error and leaves the transport idle instead of wedging it.
#[test]
fn a_missing_track_leaves_the_transport_idle() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 1.0);
    let mut app = build_app(
        &dir,
        "rhai",
        r#"fn on_start() { audio_play("no-such-track.wav"); }"#,
        true,
    );

    assert!(
        !tick_until(&mut app, 1.0, |app| parsed(app, "audio_duration") > 0.0),
        "a missing track must never produce a duration; got {:?}",
        signal(&app, "audio_duration")
    );
    assert_eq!(signal(&app, "audio_playing").as_deref(), Some("false"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// candela reaches the same functions through the prelude's `host "lumen"`
/// block, which the host extends with what the plugin registered.
#[test]
fn candela_reaches_the_module_surface_through_the_prelude() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 5.0);
    let mut app = build_app(
        &dir,
        "candela",
        r#"import "lumen.cdl";
fn on_start() { lumen::audio_play("tone.wav"); }
fn main() {}
"#,
        true,
    );

    assert!(
        tick_until(&mut app, 3.0, |app| parsed(app, "audio_duration") > 4.0),
        "the candela call must start playback; got {:?}",
        signal(&app, "audio_duration")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A finished track calls `on_audio_end(path)` through the plugin-event bus.
#[test]
fn end_of_track_fires_the_fallback_handler() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("short.wav", 0.2);
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { audio_play("short.wav"); }
fn on_audio_end(path) { signal("ended_path", "").set(path); }
"#,
        true,
    );

    assert!(
        tick_until(&mut app, 5.0, |app| signal(app, "ended_path").is_some()),
        "on_audio_end must fire after the 0.2s track; playing={:?} pos={:?}",
        signal(&app, "audio_playing"),
        signal(&app, "audio_position")
    );
    assert_eq!(signal(&app, "ended_path").as_deref(), Some("short.wav"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A per-key `on("audio_end", path, fn)` registration wins over the
/// `on_audio_end` fallback, like every other plugin event.
#[test]
fn a_per_key_handler_wins_over_the_fallback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("short.wav", 0.2);
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    on("audio_end", "short.wav", "special_end");
    audio_play("short.wav");
}
fn special_end(path) { signal("special", "").set(path); }
fn on_audio_end(path) { signal("fallback", "").set(path); }
"#,
        true,
    );

    assert!(
        tick_until(&mut app, 5.0, |app| signal(app, "special").is_some()),
        "the per-key handler must fire"
    );
    assert_eq!(signal(&app, "special").as_deref(), Some("short.wav"));
    assert_eq!(signal(&app, "fallback"), None, "the fallback must not fire");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the functions do not exist: the script's call fails
/// with the host's ordinary unknown-function error, the app keeps ticking,
/// and no audio signal ever appears.
#[test]
fn without_the_plugin_the_functions_do_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 1.0);
    let mut app = build_app(
        &dir,
        "rhai",
        r#"fn on_start() { audio_play("tone.wav"); signal("alive", "").set("yes"); }"#,
        false,
    );

    for _ in 0..20 {
        app.tick();
    }
    assert_eq!(
        signal(&app, "audio_duration"),
        None,
        "no module, no audio signals"
    );
    assert_eq!(
        signal(&app, "audio_playing"),
        None,
        "no module, no audio signals"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The candela shape of the same absence: the prelude's `host "lumen"` block
/// no longer declares the function, the call fails with candela's own
/// resolution error (the module-shape suite asserts the printed message), and
/// the app still boots and ticks with no audio signal ever appearing.
#[test]
fn candela_without_the_plugin_has_no_audio_surface() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir_with_wav("tone.wav", 1.0);
    let mut app = build_app(
        &dir,
        "candela",
        r#"import "lumen.cdl";
fn on_start() { lumen::audio_play("tone.wav"); }
fn main() {}
"#,
        false,
    );

    for _ in 0..10 {
        app.tick();
    }
    assert_eq!(
        signal(&app, "audio_duration"),
        None,
        "no module, no audio signals"
    );
    assert_eq!(
        signal(&app, "audio_playing"),
        None,
        "no module, no audio signals"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
