//! The multi-argument OS builtins queue the command their description says.
//!
//! `notify_ex`, `tray_icon_menu`, and `pick_file_filtered` each carry four or
//! five arguments into one [`ScriptCommand`], assembled from one shared
//! description. The thing worth proving is that the assembled command has the
//! shape the OS side expects: the same field order, the same empty-tooltip-to-`None`
//! rule, the same boolean, and the same parsed filter list.
//!
//! A desktop is out of reach here, so the assertions stop at the command. The
//! script calls the builtin, [`ScriptHost::call`] hands back what the call
//! queued, and the test reads every field off it. That is the observation point
//! the runtime itself uses; the applier downstream is what turns these into an
//! OS call.

// The candela host, compiled in: the module registry answers the program's
// implied host dependency from it.
use lumen_candela_dev as _;
use lumen_candela_dev::CandelaHost;
use lumen_core::app::App as EcsApp;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{FileDialogKind, ScriptCommand, ScriptHost, builtin_script_fns};

/// An app publishes process-global registries, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build and tick a headless app whose candela program is `source`.
fn app_with(source: &str) -> EcsApp {
    let dir = std::env::temp_dir().join(format!(
        "lumen_script_fn_os_commands_{}_{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).expect("temp app dir");
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
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app.tick();
    app
}

/// Call `fn_name` in the app's script and return the commands it queued.
///
/// The sink is emptied first, so what comes back is what this call produced and
/// not what the two startup ticks left behind.
fn probe(app: &mut EcsApp, fn_name: &str) -> Vec<ScriptCommand> {
    let mut host = app.world.resource_mut::<CandelaHost>();
    host.drain_commands();
    let outcome = host
        .call(fn_name, &[])
        .unwrap_or_else(|e| panic!("`{fn_name}` ran: {e:?}"));
    assert!(outcome.found, "the script defines `{fn_name}`");
    outcome.commands
}

// -- the scripts ------------------------------------------------------
//
// One probe per builtin, each making every call that case needs, so the
// assertion reads the sequence back in the order the script wrote it.

const SOURCE: &str = r#"
import "lumen.cdl";

fn probe_notify() {
    lumen::notify_ex("save-done", "Saved", "Your notes were written.",
                     "icon:document-save|urgency:critical", "open:Open|dismiss:Dismiss");
    lumen::notify_ex("bare", "Saved", "Your notes were written.", "", "");
}

fn probe_tray() {
    lumen::tray_icon_menu("main", "icons/tray.png", "Lumen hotkeys demo",
                          "open:Open|-|quit:Quit", true);
    lumen::tray_icon_menu("main", "icons/tray.png", "", "quit:Quit", false);
}

fn probe_pick() {
    lumen::pick_file_filtered("open", "Images:png,jpg|All:*");
    lumen::pick_file_filtered("open", "  Images : png , jpg | Odd:a:b  ");
    lumen::pick_file_filtered("open", "Images:png|Documents");
    lumen::pick_file_filtered("open", "");
}

fn main() {}
"#;

// -- the assertions ---------------------------------------------------

/// The two `NotifyEx` commands `probe_notify` queues.
///
/// Argument order is `(id, title, body, options, actions)`, and the two string
/// specs travel verbatim: the notify backend owns their vocabulary, so an empty
/// `options` or `actions` reaches it as an empty string rather than as an
/// absent field.
fn assert_notify_commands(queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 2, "unexpected commands: {queued:?}");

    let ScriptCommand::NotifyEx {
        id,
        title,
        body,
        options,
        actions,
    } = &queued[0]
    else {
        panic!("expected NotifyEx, got {:?}", queued[0]);
    };
    assert_eq!(id, "save-done", "id");
    assert_eq!(title, "Saved", "title");
    assert_eq!(body, "Your notes were written.", "body");
    assert_eq!(options, "icon:document-save|urgency:critical", "options");
    assert_eq!(actions, "open:Open|dismiss:Dismiss", "actions");

    let ScriptCommand::NotifyEx {
        id,
        title,
        body,
        options,
        actions,
    } = &queued[1]
    else {
        panic!("expected NotifyEx, got {:?}", queued[1]);
    };
    assert_eq!(id, "bare", "id");
    assert_eq!(title, "Saved", "title");
    assert_eq!(body, "Your notes were written.", "body");
    assert_eq!(options, "", "an empty options spec stays empty");
    assert_eq!(actions, "", "an empty actions spec stays empty");
}

/// The two `RegisterTrayIcon` commands `probe_tray` queues.
///
/// The tooltip is the one argument that changes kind on the way through: a
/// non-empty string becomes `Some`, an empty one becomes `None`, which is how
/// a script says "no tooltip" to a command that has no other spelling for it.
fn assert_tray_commands(queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 2, "unexpected commands: {queued:?}");

    let ScriptCommand::RegisterTrayIcon {
        id,
        icon_path,
        tooltip,
        menu,
        template,
    } = &queued[0]
    else {
        panic!("expected RegisterTrayIcon, got {:?}", queued[0]);
    };
    assert_eq!(id, "main", "id");
    assert_eq!(icon_path, "icons/tray.png", "icon_path");
    assert_eq!(tooltip.as_deref(), Some("Lumen hotkeys demo"), "tooltip");
    assert_eq!(menu, "open:Open|-|quit:Quit", "menu");
    assert!(*template, "the template flag arrived as passed");

    let ScriptCommand::RegisterTrayIcon {
        id,
        icon_path,
        tooltip,
        menu,
        template,
    } = &queued[1]
    else {
        panic!("expected RegisterTrayIcon, got {:?}", queued[1]);
    };
    assert_eq!(id, "main", "id");
    assert_eq!(icon_path, "icons/tray.png", "icon_path");
    assert_eq!(
        *tooltip, None,
        "an empty tooltip is absent, not an empty string"
    );
    assert_eq!(menu, "quit:Quit", "menu");
    assert!(!*template, "the template flag arrived as passed");
}

/// One label-and-extensions filter group.
fn group(label: &str, exts: &[&str]) -> (String, Vec<String>) {
    (
        label.to_string(),
        exts.iter().map(|e| (*e).to_string()).collect(),
    )
}

/// The four `OpenFileDialog` commands `probe_pick` queues.
///
/// The interesting half is the spec parser: `|` separates groups, `:` splits a
/// group's label from its extensions, `,` separates the extensions, a literal
/// `*` extension is dropped because no extension filter is what "all files"
/// means to the dialog backend, and surrounding whitespace is trimmed off both
/// halves. There is no escape, so a second `:` inside a group belongs to the
/// extension text.
fn assert_pick_commands(queued: &[ScriptCommand]) {
    assert_eq!(queued.len(), 4, "unexpected commands: {queued:?}");

    let expected = [
        vec![group("Images", &["png", "jpg"]), group("All", &[])],
        vec![group("Images", &["png", "jpg"]), group("Odd", &["a:b"])],
        vec![group("Images", &["png"]), group("Documents", &[])],
        Vec::new(),
    ];
    for (i, want) in expected.iter().enumerate() {
        let ScriptCommand::OpenFileDialog {
            kind,
            tag,
            filters,
            default_name,
        } = &queued[i]
        else {
            panic!("expected OpenFileDialog, got {:?}", queued[i]);
        };
        assert_eq!(*kind, FileDialogKind::Open, "call {i} kind");
        assert_eq!(tag, "open", "call {i} tag");
        assert_eq!(filters, want, "call {i} filters");
        assert_eq!(*default_name, None, "a pick carries no save-file name");
    }
}

// -- the cases --------------------------------------------------------

/// candela reaches the three builtins through the prelude's typed
/// declarations, so this also covers the shape adapter carrying a five-argument
/// call whose last argument is a boolean.
#[test]
fn candela_queues_the_os_commands_field_for_field() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = app_with(SOURCE);
    assert_notify_commands(&probe(&mut app, "probe_notify"));
    assert_tray_commands(&probe(&mut app, "probe_tray"));
    assert_pick_commands(&probe(&mut app, "probe_pick"));
}

/// The shared table describes all three builtins, and each takes every
/// argument it declares, so every case above is a case the table carries.
#[test]
fn the_shared_table_describes_the_three_os_builtins() {
    for name in ["notify_ex", "tray_icon_menu", "pick_file_filtered"] {
        let f = builtin_script_fns()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("the shared table describes `{name}`"));
        assert_eq!(
            f.sig.min_arity,
            f.sig.params.len(),
            "`{name}` takes every argument it declares; none is optional"
        );
    }
}
