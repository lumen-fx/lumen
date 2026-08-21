//! The script functions the runtime provides, exercised as the hosts call
//! them.
//!
//! `builtin_script_fns` describes `set_color_scheme` and the page navigation
//! family once, host-neutrally, and every host binds the same values from the
//! app's registry. So the bodies can be driven directly here, with
//! `ScriptValue` arguments standing in for whatever the host marshalled, and
//! the per-host suites in lumenc check that each language resolves the names.

use lumen_core::app::App;
use lumen_core::command::{Command, CommandQueue, CommandReceiver};
use lumen_core::components::ColorScheme;
use lumen_runtime::run::ColorSchemeIntent;
use lumen_script::{HostSet, ScriptFn, ScriptValue};

/// Navigation rides a process-global bus, so the tests that read it run one
/// at a time.
fn nav_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Look up one built-in by name.
fn builtin(fns: &[ScriptFn], name: &str) -> ScriptFn {
    fns.iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no built-in `{name}`"))
        .clone()
}

fn call(f: &ScriptFn, args: &[ScriptValue]) -> ScriptValue {
    f.invoke(args).0
}

/// An app whose command queue this test holds the receiving end of, so what
/// the built-in pushed can be read back.
fn app_with_queue(capacity: usize) -> (App, CommandReceiver) {
    let mut app = App::new();
    let (queue, rx) = CommandQueue::with_capacity(capacity);
    app.world.insert_resource(queue);
    (app, rx)
}

/// The scheme a recognised name selects reaches the queue as the typed intent
/// the command handler is registered for.
#[test]
fn set_color_scheme_queues_the_typed_intent() {
    let (app, mut rx) = app_with_queue(4);
    let fns = lumen_runtime::run::builtin_script_fns(&app);
    let set_scheme = builtin(&fns, "set_color_scheme");

    assert_eq!(
        call(&set_scheme, &[ScriptValue::Str("force-dark".into())]),
        ScriptValue::Unit,
        "the built-in returns nothing to the script"
    );

    let queued: Vec<Command> = rx.drain().collect();
    assert_eq!(queued.len(), 1, "one command should be waiting");
    let Command::Typed { type_id, payload } = queued.into_iter().next().unwrap() else {
        panic!("the scheme change rides as a typed command");
    };
    assert_eq!(
        type_id,
        std::any::TypeId::of::<ColorSchemeIntent>(),
        "routed to the color-scheme handler"
    );
    let intent = payload
        .downcast::<ColorSchemeIntent>()
        .expect("the payload is the intent its type id claims");
    assert_eq!(intent.0, ColorScheme::ForceDark);
}

/// A name that is not a scheme is dropped with a warning rather than queued,
/// so a typo in a script cannot push a command nothing can apply.
#[test]
fn set_color_scheme_ignores_an_unknown_name() {
    let (app, mut rx) = app_with_queue(4);
    let fns = lumen_runtime::run::builtin_script_fns(&app);
    let set_scheme = builtin(&fns, "set_color_scheme");

    assert_eq!(
        call(&set_scheme, &[ScriptValue::Str("chartreuse".into())]),
        ScriptValue::Unit
    );
    assert_eq!(
        call(&set_scheme, &[]),
        ScriptValue::Unit,
        "a call with no argument is the same non-event"
    );
    assert_eq!(
        rx.drain().count(),
        0,
        "neither call should have queued anything"
    );
}

/// A full queue drops the update instead of panicking or blocking the script
/// thread. A one-slot queue reaches the full state without having to push the
/// production capacity.
#[test]
fn set_color_scheme_drops_the_update_when_the_queue_is_full() {
    let (app, _rx) = app_with_queue(1);
    let fns = lumen_runtime::run::builtin_script_fns(&app);
    let set_scheme = builtin(&fns, "set_color_scheme");

    let sender = app.world.resource::<CommandQueue>().sender().clone();
    sender
        .try_send(Command::ScriptUpdate(Box::new(())))
        .expect("the one slot starts free");
    assert!(sender.is_full(), "the queue is now full");

    assert_eq!(
        call(&set_scheme, &[ScriptValue::Str("force-dark".into())]),
        ScriptValue::Unit,
        "the built-in still returns cleanly with nowhere to put the command"
    );
}

/// `page` takes its argument optionally: with one it navigates, without one it
/// reads. Hosts that pass a unit placeholder for a missing argument read as
/// well, which is what keeps the no-argument spelling working across
/// languages.
#[test]
fn page_navigates_with_an_argument_and_reads_without_one() {
    let _guard = nav_guard();
    let app = App::new();
    let fns = lumen_runtime::run::builtin_script_fns(&app);

    let page = builtin(&fns, "page");
    assert_eq!(
        page.sig.arity_range(),
        0..=1,
        "one description answers both call shapes"
    );

    assert_eq!(
        call(&page, &[ScriptValue::Str("settings".into())]),
        ScriptValue::Unit,
        "navigating returns nothing"
    );

    let current = lumen_core::nav::current();
    assert_eq!(
        call(&page, &[]),
        ScriptValue::Str(current.clone()),
        "no argument reads the active page"
    );
    assert_eq!(
        call(&page, &[ScriptValue::Unit]),
        ScriptValue::Str(current.clone()),
        "a unit placeholder reads too"
    );
    assert_eq!(
        call(&builtin(&fns, "page_current"), &[]),
        ScriptValue::Str(current),
        "page_current is the same reader under an unambiguous name"
    );
}

/// History steps report whether the request reached the navigation bus, so a
/// script can branch on the result.
#[test]
fn history_steps_report_that_they_were_queued() {
    let _guard = nav_guard();
    let app = App::new();
    let fns = lumen_runtime::run::builtin_script_fns(&app);

    assert_eq!(
        call(&builtin(&fns, "page_back"), &[]),
        ScriptValue::Bool(true),
        "page_back hands the script a boolean"
    );
    assert_eq!(
        call(&builtin(&fns, "page_forward"), &[]),
        ScriptValue::Bool(true),
        "page_forward does the same"
    );
}

/// candela declares these names in its own prelude, so the runtime's copies
/// stay out of its way; Rhai and Lua have no such declaration and take them.
#[test]
fn the_runtime_builtins_are_offered_to_rhai_and_lua_only() {
    let app = App::new();
    for f in lumen_runtime::run::builtin_script_fns(&app) {
        assert_eq!(
            f.hosts,
            HostSet::RHAI | HostSet::LUA,
            "`{}` must not reach candela",
            f.name
        );
    }
}
