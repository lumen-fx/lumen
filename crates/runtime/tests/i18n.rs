//! Headless proof that translation and formatting round-trip: an app
//! directory with a `locale/` catalogue starts in the locale `lumen.toml`
//! names, markup marked `translatable="key"` spawns with the translated
//! string, markup marked `format="<spec>"` spawns rendered for the locale,
//! and the translator and formatter every script host's builtins call
//! resolve the same catalogue and the same locale.
//!
//! Runs window-free through `build_headless_app`, the same path
//! `run_app_headless` takes.

use lumen_core::components::TextContent;
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use std::path::{Path, PathBuf};

/// The script-side translator and formatter hooks are process-global
/// singletons, so apps that install them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn label(text: Option<&str>, translatable: Option<&str>) -> Element {
    Element {
        tag: "label".to_string(),
        attrs: Attributes {
            text: text.map(str::to_string),
            translatable: translatable.map(str::to_string),
            ..Default::default()
        },
        children: Vec::new(),
        ..Default::default()
    }
}

fn formatted_label(text: &str, format: &str) -> Element {
    Element {
        tag: "label".to_string(),
        attrs: Attributes {
            text: Some(text.to_string()),
            format: Some(format.to_string()),
            ..Default::default()
        },
        children: Vec::new(),
        ..Default::default()
    }
}

fn app_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_i18n_{name}_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(dir.join("locale")).unwrap();
    dir
}

fn write_catalogue(dir: &Path, lang: &str, body: &str) {
    std::fs::write(dir.join("locale").join(format!("{lang}.ftl")), body).unwrap();
}

/// Build the app from a hand-made artifact so the test needs no parser.
fn build(dir: &Path, root: Element) -> lumen_core::app::App {
    let ir = LayoutIR {
        root,
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        script_source: String::new(),
        ..Default::default()
    })
    .expect("serialize artifact");
    let opts = RunOptions::new(dir).with_artifact_bytes(bytes);
    let (app, _) = build_headless_app(opts).expect("app builds headless");
    app
}

fn texts(app: &mut lumen_core::app::App) -> Vec<String> {
    app.world
        .query::<&TextContent>()
        .iter(&app.world)
        .map(|t| t.0.clone())
        .collect()
}

/// `[app] locale` selects the catalogue, a marked element spawns
/// translated, and a key the active locale lacks falls through to en-US.
#[test]
fn marked_markup_spawns_translated() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("markup");
    std::fs::write(dir.join("lumen.toml"), "[app]\nlocale = \"de-DE\"\n").unwrap();
    write_catalogue(&dir, "en-US", "greet = Hello!\nbye = Goodbye!\n");
    write_catalogue(&dir, "de-DE", "greet = Hallo!\n");

    let root = Element {
        tag: "root".to_string(),
        attrs: Attributes::default(),
        children: vec![
            label(Some("Hello!"), Some("greet")),
            // Only en-US carries this one: the fallback chain resolves it.
            label(Some("Goodbye!"), Some("bye")),
        ],
        ..Default::default()
    };
    let mut app = build(&dir, root);
    let texts = texts(&mut app);
    assert!(texts.contains(&"Hallo!".to_string()), "{texts:?}");
    assert!(texts.contains(&"Goodbye!".to_string()), "{texts:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Fallback order for a key no catalogue carries: the authored text wins,
/// and an element with no text at all renders the key rather than nothing.
#[test]
fn untranslated_falls_back_to_authored_text_then_key() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("fallback");
    write_catalogue(&dir, "en-US", "known = Known!\n");

    let root = Element {
        tag: "root".to_string(),
        attrs: Attributes::default(),
        children: vec![
            label(Some("Source text"), Some("absent-key")),
            label(None, Some("no-text-key")),
        ],
        ..Default::default()
    };
    let mut app = build(&dir, root);
    let texts = texts(&mut app);
    assert!(texts.contains(&"Source text".to_string()), "{texts:?}");
    assert!(texts.contains(&"no-text-key".to_string()), "{texts:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The hook every script host's `t()` builtin calls resolves the app's
/// catalogue, and reports a miss so callers can fall back.
#[test]
fn script_translator_hook_resolves_the_catalogue() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("script");
    std::fs::write(dir.join("lumen.toml"), "[app]\nlocale = \"de-DE\"\n").unwrap();
    write_catalogue(&dir, "de-DE", "greet = Hallo!\n");

    let _app = build(
        &dir,
        Element {
            tag: "root".to_string(),
            attrs: Attributes::default(),
            children: Vec::new(),
            ..Default::default()
        },
    );
    assert_eq!(lumen_core::i18n::translate("greet"), "Hallo!");
    assert_eq!(lumen_core::i18n::translate("unknown"), "unknown");
    assert_eq!(lumen_core::i18n::try_translate("unknown"), None);

    lumen_core::i18n::clear_translator();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A catalogue reload replaces the strings the shared registry serves, so a
/// hot edit under `locale/` reaches both markup and scripts.
#[test]
fn catalogue_reload_replaces_strings() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("reload");
    write_catalogue(&dir, "en-US", "greet = Hello!\n");

    let app = build(
        &dir,
        Element {
            tag: "root".to_string(),
            attrs: Attributes::default(),
            children: Vec::new(),
            ..Default::default()
        },
    );
    let shared = app.world.resource::<lumen_i18n::SharedI18n>().clone();
    assert_eq!(shared.t("greet"), "Hello!");

    write_catalogue(&dir, "en-US", "greet = Hi again!\n");
    shared
        .write()
        .load_dir(&dir.join("locale"), |p| std::fs::read(p))
        .unwrap();
    assert_eq!(shared.t("greet"), "Hi again!");
    assert_eq!(lumen_core::i18n::translate("greet"), "Hi again!");

    lumen_core::i18n::clear_translator();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `format="<spec>"` markup spawns rendered for the app's locale, and text
/// the spec cannot read spawns as it was authored rather than blank.
#[test]
fn formatted_markup_spawns_for_the_locale() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("format");
    std::fs::write(dir.join("lumen.toml"), "[app]\nlocale = \"de-DE\"\n").unwrap();

    let root = Element {
        tag: "root".to_string(),
        attrs: Attributes::default(),
        children: vec![
            formatted_label("1234.5", "currency:EUR"),
            formatted_label("2024-06-15T09:30:00Z", "date"),
            formatted_label("not a number", "number"),
            formatted_label("hello", "wat"),
        ],
        ..Default::default()
    };
    let mut app = build(&dir, root);
    let texts = texts(&mut app);
    assert!(
        texts.iter().any(|t| t.contains("1.234,50")),
        "the euro amount reads in German: {texts:?}"
    );
    assert!(texts.contains(&"15.06.2024".to_string()), "{texts:?}");
    assert!(texts.contains(&"not a number".to_string()), "{texts:?}");
    assert!(texts.contains(&"hello".to_string()), "{texts:?}");

    lumen_core::i18n::clear_formatter();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `format` on a `bind-text` element renders every value the signal
/// writes, not only the one the element spawned with.
#[test]
fn a_bound_signal_is_formatted_on_every_write() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("bound");
    std::fs::write(dir.join("lumen.toml"), "[app]\nlocale = \"de-DE\"\n").unwrap();

    let mut live = formatted_label("0", "number");
    live.attrs.bind = Some(lumen_ir::layout_ir::BindSpec {
        kind: lumen_ir::layout_ir::BindKind::Text,
        name: "count".to_string(),
    });
    let root = Element {
        tag: "root".to_string(),
        attrs: Attributes::default(),
        children: vec![live],
        ..Default::default()
    };
    let mut app = build(&dir, root);
    app.world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("count", "12345.678");
    app.tick();
    let texts = texts(&mut app);
    assert!(texts.contains(&"12.345,678".to_string()), "{texts:?}");

    lumen_core::i18n::clear_formatter();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The hook every script host's `format_*` builtin calls renders for the
/// app's locale, and declines a spec or a value it cannot read.
#[test]
fn script_formatter_hook_reads_the_app_locale() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("scriptformat");
    std::fs::write(dir.join("lumen.toml"), "[app]\nlocale = \"de-DE\"\n").unwrap();

    let _app = build(
        &dir,
        Element {
            tag: "root".to_string(),
            attrs: Attributes::default(),
            children: Vec::new(),
            ..Default::default()
        },
    );
    assert_eq!(
        lumen_core::i18n::format("number", "12345.678").as_deref(),
        Some("12.345,678")
    );
    assert!(
        lumen_core::i18n::format("currency:EUR", "1234.5")
            .expect("a euro amount formats")
            .contains("1.234,50")
    );
    assert_eq!(lumen_core::i18n::format("wat", "hello"), None);
    assert_eq!(lumen_core::i18n::format("number", "hello"), None);

    lumen_core::i18n::clear_formatter();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.ftl` whose filename is not a BCP-47 tag fails the app load naming
/// the problem, rather than being skipped and leaving strings untranslated.
#[test]
fn a_bad_catalogue_filename_fails_the_load() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("badname");
    write_catalogue(&dir, "not a tag", "greet = Hello!\n");

    let ir = LayoutIR::default();
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        script_source: String::new(),
        ..Default::default()
    })
    .unwrap();
    let err = match build_headless_app(RunOptions::new(&dir).with_artifact_bytes(bytes)) {
        Err(e) => e,
        Ok(_) => panic!("an unparseable locale filename must fail the load"),
    };
    assert!(err.to_string().contains("i18n"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}
