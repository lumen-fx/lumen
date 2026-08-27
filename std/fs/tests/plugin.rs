//! The compiled-in shape: [`FsPlugin`] installed like any other plugin on a
//! headless app, driving the whole script surface in process.
//!
//! What these prove, once per concern:
//!
//! - the `files` functions reach a script through the generic
//!   `ScriptFnRegistry`, in Rhai and in candela;
//! - every path resolves against the app directory rather than the process
//!   working directory, so a script names a file the same way wherever the
//!   app was started from;
//! - `read_bytes` honours the cap the app configured;
//! - without the plugin the functions do not exist, and the app keeps running
//!   after the script's own unknown-function error.

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_fs::FsPlugin;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// The app directory, the DOM snapshot, and the property store are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh app directory carrying `lumen.toml` with an id of its own, so
/// `files::data_dir()` names something this test can recognize.
fn app_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-fs-plugin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-fs-plugin-{name}\"\n"),
    )
    .expect("lumen.toml");
    dir
}

/// Build a headless app in `dir` running one script, with the given plugin.
fn build_app(
    dir: &std::path::Path,
    engine: &str,
    source: &str,
    plugin: Option<FsPlugin>,
) -> EcsApp {
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

/// The whole surface from Rhai, against the app directory: every path in the
/// script is relative, and every file it names lands beside the app.
#[test]
fn rhai_drives_the_whole_surface_against_the_app_directory() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("rhai");
    let app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() {
    signal("wrote", "").set(files::write("notes.txt", "hello"));
    signal("read", "").set(files::read("notes.txt"));
    signal("absent_read", "").set(files::read("never-saved.txt"));
    signal("exists", "").set(files::exists("notes.txt"));
    signal("absent", "").set(files::exists("never-saved.txt"));
    signal("made", "").set(files::mkdir("sub/deep"));
    signal("is_dir", "").set(files::is_dir("sub/deep"));
    signal("file_is_dir", "").set(files::is_dir("notes.txt"));
    signal("copied", "").set(files::copy("notes.txt", "sub/deep/copy.txt"));
    let names = files::list("sub/deep");
    signal("listed", "").set(names.len());
    signal("first", "").set(names[0]);
    signal("wrote_bytes", "").set(files::write_bytes("raw.bin", [104, 105]));
    signal("read_back", "").set(files::read("raw.bin"));
    let bytes = files::read_bytes("notes.txt");
    signal("byte_count", "").set(bytes.len());
    signal("first_byte", "").set(bytes[0]);
    signal("removed", "").set(files::remove("raw.bin"));
    signal("remove_absent", "").set(files::remove("raw.bin"));
    signal("remove_full", "").set(files::remove("sub/deep"));
    signal("data", "").set(files::data_dir());
}
"#,
        Some(FsPlugin::default()),
    );

    assert_eq!(signal(&app, "wrote").as_deref(), Some("true"));
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).ok(),
        Some("hello".to_string()),
        "a relative write lands in the app directory, not the working one"
    );
    assert_eq!(signal(&app, "read").as_deref(), Some("hello"));
    assert_eq!(signal(&app, "absent_read").as_deref(), Some(""));
    assert_eq!(signal(&app, "exists").as_deref(), Some("true"));
    assert_eq!(signal(&app, "absent").as_deref(), Some("false"));
    assert_eq!(signal(&app, "made").as_deref(), Some("true"));
    assert_eq!(signal(&app, "is_dir").as_deref(), Some("true"));
    assert_eq!(signal(&app, "file_is_dir").as_deref(), Some("false"));
    assert_eq!(signal(&app, "copied").as_deref(), Some("true"));
    assert_eq!(signal(&app, "listed").as_deref(), Some("1"));
    assert_eq!(signal(&app, "first").as_deref(), Some("copy.txt"));
    assert_eq!(signal(&app, "wrote_bytes").as_deref(), Some("true"));
    assert_eq!(signal(&app, "read_back").as_deref(), Some("hi"));
    assert_eq!(signal(&app, "byte_count").as_deref(), Some("5"));
    assert_eq!(signal(&app, "first_byte").as_deref(), Some("104"));
    assert_eq!(signal(&app, "removed").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "remove_absent").as_deref(),
        Some("false"),
        "removing what is already gone answers false"
    );
    assert_eq!(
        signal(&app, "remove_full").as_deref(),
        Some("false"),
        "a directory holding a file is refused"
    );
    let data = signal(&app, "data").expect("data_dir answers");
    assert!(
        std::path::Path::new(&data).is_dir(),
        "{data} was not created"
    );
    assert!(
        data.ends_with("lumen-fs-plugin-rhai") || data == dir.to_string_lossy(),
        "the data directory carries the app id: {data}"
    );
    if data != dir.to_string_lossy() {
        let _ = std::fs::remove_dir(&data);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A host path, spelled the way a script has to spell it.
///
/// A backslash starts an escape sequence in every host's string syntax, so a
/// Windows path spliced into a literal as it comes off `Path` (`C:\Users\..`)
/// is a parse error and the whole script fails to load. Windows takes a
/// forward slash in a path just as well, so that is what goes in.
fn as_script_path(path: &std::path::Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// An absolute path is left alone: it names exactly the file it spells, and
/// the app directory does not come into it.
#[test]
fn an_absolute_path_is_left_alone() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absolute");
    // A directory of its own, outside the app, so landing in the right place
    // is visible rather than inferred.
    let elsewhere = std::env::temp_dir().join(format!("lumen-fs-elsewhere-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).expect("a directory outside the app");
    let target = elsewhere.join("kept.txt");
    let spelled = as_script_path(&target);
    let app = build_app(
        &dir,
        "rhai",
        &format!(
            r#"
fn on_start() {{
    signal("wrote", "").set(files::write("{spelled}", "kept"));
    signal("read", "").set(files::read("{spelled}"));
}}
"#
        ),
        Some(FsPlugin::default()),
    );

    assert_eq!(signal(&app, "wrote").as_deref(), Some("true"));
    assert_eq!(signal(&app, "read").as_deref(), Some("kept"));
    assert_eq!(
        std::fs::read_to_string(&target).ok(),
        Some("kept".to_string()),
        "the write landed at the path the script spelled"
    );
    assert!(
        !dir.join("kept.txt").exists(),
        "an absolute path is never resolved against the app directory"
    );

    let _ = std::fs::remove_dir_all(&elsewhere);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The cap the app configured is what `read_bytes` enforces: a file inside it
/// reads, and one past it comes back empty.
#[test]
fn the_configured_cap_bounds_what_read_bytes_hands_back() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("cap");
    // The smallest cap the module supports, and the two files either side of
    // it: one exactly that size, one a byte over.
    let cap = i64::try_from(lumen_fs::MIN_READ_BYTES_CAP).expect("the cap fits an i64");
    let size = usize::try_from(lumen_fs::MIN_READ_BYTES_CAP).expect("the cap fits a usize");
    std::fs::write(dir.join("small.bin"), vec![1u8; size]).expect("small file");
    std::fs::write(dir.join("large.bin"), vec![1u8; size + 1]).expect("large file");
    let source = r#"
fn on_start() {
    signal("small", "").set(files::read_bytes("small.bin").len());
    signal("large", "").set(files::read_bytes("large.bin").len());
}
"#;

    let app = build_app(
        &dir,
        "rhai",
        source,
        Some(FsPlugin::with_read_bytes_cap(cap)),
    );
    assert_eq!(
        signal(&app, "small").as_deref(),
        Some(size.to_string().as_str()),
        "a file of exactly the cap reads"
    );
    assert_eq!(
        signal(&app, "large").as_deref(),
        Some("0"),
        "a file past the cap reads as no bytes"
    );

    // The default cap is far above either file, so both read.
    let app = build_app(&dir, "rhai", source, Some(FsPlugin::default()));
    assert_eq!(
        signal(&app, "small").as_deref(),
        Some(size.to_string().as_str())
    );
    assert_eq!(
        signal(&app, "large").as_deref(),
        Some((size + 1).to_string().as_str())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A cap outside the supported range is clamped rather than taken: a call
/// still reads something.
#[test]
fn a_cap_outside_the_range_is_clamped() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("clamp");
    std::fs::write(dir.join("blob.bin"), vec![2u8; 900]).expect("file");
    let app = build_app(
        &dir,
        "rhai",
        r#"fn on_start() { signal("count", "").set(files::read_bytes("blob.bin").len()); }"#,
        Some(FsPlugin::with_read_bytes_cap(0)),
    );

    assert_eq!(
        signal(&app, "count").as_deref(),
        Some("900"),
        "a zero cap clamps up to the smallest supported one"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// candela reaches the same functions through the `host "files"` block the host
/// synthesizes from what the plugin registered.
#[test]
fn candela_reaches_the_module_surface_through_its_namespace() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("candela");
    let app = build_app(
        &dir,
        "candela",
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("wrote", files::write("from-candela.txt", "written"));
    lumen::signal_set("read", files::read("from-candela.txt"));
    lumen::signal_set_bool("exists", files::exists("from-candela.txt"));
    lumen::signal_set_bool("made", files::mkdir("sub"));
    let names = files::list(".");
    lumen::signal_set("first", names[0]);
    let bytes = files::read_bytes("from-candela.txt");
    lumen::signal_set_int("first_byte", bytes[0]);
    lumen::signal_set_bool("wrote_bytes", files::write_bytes("raw.bin", [104, 105]));
    lumen::signal_set("raw", files::read("raw.bin"));
}

fn main() {}
"#,
        Some(FsPlugin::default()),
    );

    assert_eq!(signal(&app, "wrote").as_deref(), Some("true"));
    assert_eq!(signal(&app, "read").as_deref(), Some("written"));
    assert_eq!(signal(&app, "exists").as_deref(), Some("true"));
    assert_eq!(signal(&app, "made").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "first").as_deref(),
        Some("from-candela.txt"),
        "the listing is sorted and names the app's own files"
    );
    assert_eq!(signal(&app, "first_byte").as_deref(), Some("119"));
    assert_eq!(signal(&app, "wrote_bytes").as_deref(), Some("true"));
    assert_eq!(signal(&app, "raw").as_deref(), Some("hi"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the plugin the functions do not exist: the script's call fails
/// with the host's ordinary unknown-function error, nothing is written, and
/// the app keeps ticking.
#[test]
fn without_the_plugin_the_functions_do_not_exist() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("absent");
    let mut app = build_app(
        &dir,
        "rhai",
        r#"
fn on_start() { signal("read", "").set(files::read("notes.txt")); }
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
        signal(&app, "read"),
        None,
        "no module, no `files` namespace, no value"
    );
    assert!(
        !dir.join("notes.txt").exists(),
        "nothing touched the filesystem"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
