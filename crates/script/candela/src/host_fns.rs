//! The Lumen builtin surface, written once against a registration sink.
//!
//! candela binds a `host "lumen" { ... }` declaration to a Rust closure in two
//! places: [`candela::Engine`] binds while it compiles source, and
//! `candela_vm::HostRegistry` binds while it loads a `.cdlb` image. The two
//! take the same closures and derive the same signatures, so the builtin list
//! lives here, behind [`HostFnSink`], and each host registers it into whichever
//! surface it drives. A builtin added to
//! [`register_lumen_host_fns`] is reachable from a compiled program and a
//! precompiled artifact alike.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use candela_vm::{IntoHostFn, Value};
use lumen_script::{CommandFn, FileDialogKind, ScriptCommand, ScriptValue};

use crate::parse;
use crate::value::{array_to_rows, candela_value_to_script, script_value_to_candela};

/// The candela host namespace every Lumen builtin is registered under. Scripts
/// reach a builtin as `lumen::<name>(...)` after declaring it in a
/// `host "lumen" { ... }` block.
pub const HOST_NAMESPACE: &str = "lumen";

/// The candela host namespace embedder-exposed native functions are registered
/// under. candela has no global function namespace, so a native function the
/// C-ABI or the Rust SDK exposes lands here: the script declares
/// `host "native" { any my_fn(...); }` and calls `native::my_fn(...)`.
pub const NATIVE_NAMESPACE: &str = "native";

/// A surface that binds candela `host` declarations to Rust closures.
///
/// Implemented by `candela_vm::HostRegistry` (the artifact path) and, with the
/// `compiler` feature, by [`candela::Engine`] (the from-source path). Both
/// derive a closure's signature the same way and check it against the same
/// declaration, so a builtin registered through this trait behaves identically
/// whichever host runs it.
pub trait HostFnSink {
    /// Registers a typed host function under `namespace::name`. The closure's
    /// argument and return types are derived from its Rust signature.
    fn register_host_fn<Marker, F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: IntoHostFn<Marker>;

    /// Registers a variadic host function under `namespace::name`. The closure
    /// receives every argument as a slice, so a dynamically-shaped value
    /// crosses the boundary without a fixed signature; the declaration spells
    /// the argument list `...`.
    fn register_host_fn_variadic<F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: Fn(&[Value]) -> Value + 'static;
}

impl HostFnSink for candela_vm::HostRegistry {
    fn register_host_fn<Marker, F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: IntoHostFn<Marker>,
    {
        Self::register_host_fn(self, namespace, name, f);
    }

    fn register_host_fn_variadic<F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: Fn(&[Value]) -> Value + 'static,
    {
        Self::register_host_fn_variadic(self, namespace, name, f);
    }
}

#[cfg(feature = "compiler")]
impl HostFnSink for candela::Engine {
    fn register_host_fn<Marker, F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: IntoHostFn<Marker>,
    {
        Self::register_host_fn(self, namespace, name, f);
    }

    fn register_host_fn_variadic<F>(&mut self, namespace: &str, name: &str, f: F)
    where
        F: Fn(&[Value]) -> Value + 'static,
    {
        Self::register_host_fn_variadic(self, namespace, name, f);
    }
}

/// Register a `lumen`-namespace builtin whose whole body is a single
/// `sink.push(<command>)`. Keeps each builtin's argument list AND its
/// `ScriptCommand` construction inline at the call site (passed as macro
/// args, not hidden behind another layer) so a reviewer can still diff
/// the three script hosts builtin-by-builtin. Zero-arg builtins use the
/// `|| <command>` form (dedicated arm below).
macro_rules! enqueue {
    ($engine:expr, $sink:expr, $name:literal, || $build:expr $(,)?) => {{
        let sink = $sink.clone();
        $engine.register_host_fn(HOST_NAMESPACE, $name, move || {
            sink.lock().unwrap().push($build);
        });
    }};
    ($engine:expr, $sink:expr, $name:literal, |$($arg:ident : $ty:ty),+ $(,)?| $build:expr $(,)?) => {{
        let sink = $sink.clone();
        $engine.register_host_fn(HOST_NAMESPACE, $name, move |$($arg: $ty),+| {
            sink.lock().unwrap().push($build);
        });
    }};
}

/// Derivation registry: `name -> (dep signal names, recompute fn name)`.
/// Aliased to keep clippy's `type_complexity` lint quiet (mirrors the Lua
/// host's `DerivationMap`).
type DerivationMap = Arc<RwLock<HashMap<String, (Vec<String>, String)>>>;

/// Host-neutral registries shared between the registered candela host-fn
/// closures and the [`ScriptHost`](lumen_script::ScriptHost) surface. Cloned
/// into every closure at registration time; the same `Arc`s survive
/// `load`/`replace` so state persists across a hot reload exactly as the Rhai
/// host's do.
#[derive(Clone, Default)]
pub(crate) struct Registries {
    /// Commands queued by builtins since the last drain.
    pub(crate) sink: Arc<Mutex<Vec<ScriptCommand>>>,
    /// Host-side rich-typed mirror of the reactive signal store.
    pub(crate) mirror: Arc<Mutex<HashMap<String, ScriptValue>>>,
    /// Per-id handler registry: `(event, id) -> fn_name`, written by `on(...)`.
    pub(crate) handlers: Arc<RwLock<HashMap<(String, String), String>>>,
    /// Derivation registry: `name -> (dep signal names, recompute fn name)`,
    /// written by `derive(...)`. candela has no first-class closure value, so the
    /// recompute body is referenced by the script function's name - exactly
    /// what a host's `Closure` associated type models.
    pub(crate) derivations: DerivationMap,
    /// Names of derivations registered but never successfully evaluated; they
    /// all run on the next derivation pass regardless of dirt.
    pub(crate) pending: Arc<Mutex<HashSet<String>>>,
    /// Event handler registry: `token -> handler fn name`, written by
    /// `event_on(...)`. candela has no closure value, so the handler is
    /// referenced by name; the dispatcher looks the name up by token and calls
    /// it.
    pub(crate) event_handlers: Arc<RwLock<HashMap<u64, String>>>,
}

/// The registry-only half of the [`ScriptHost`](lumen_script::ScriptHost)
/// surface. Both candela hosts hold the same [`Registries`] and differ only in
/// how they load and call a program, so every method that touches nothing but
/// the registries is implemented once here and delegated to from both.
impl Registries {
    /// Take everything the builtins have queued since the last drain.
    pub(crate) fn drain(&self) -> Vec<ScriptCommand> {
        std::mem::take(&mut *self.sink.lock().unwrap())
    }

    /// Put `cmds` back at the FRONT of the queue, so re-stashed `on_start`
    /// commands keep their order ahead of anything queued afterward.
    pub(crate) fn push_front(&self, cmds: Vec<ScriptCommand>) {
        let mut sink = self.sink.lock().unwrap();
        let mut merged = cmds;
        merged.append(&mut sink);
        *sink = merged;
    }

    /// Queue one command.
    pub(crate) fn push(&self, cmd: ScriptCommand) {
        self.sink.lock().unwrap().push(cmd);
    }

    /// Read the host-side mirror of a signal.
    pub(crate) fn mirror_get(&self, name: &str) -> Option<ScriptValue> {
        self.mirror.lock().unwrap().get(name).cloned()
    }

    /// Write the host-side mirror of a signal, without queueing a command.
    pub(crate) fn mirror_set(&self, name: &str, value: ScriptValue) {
        self.mirror.lock().unwrap().insert(name.to_owned(), value);
    }

    /// Type-preserving parse-back of a store string into the mirror: a scalar
    /// entry keeps its own type, a structured entry stays authoritative, and an
    /// absent or string entry takes the text verbatim. An unparseable string
    /// leaves a scalar untouched.
    pub(crate) fn mirror_sync_str(&self, name: &str, value: &str) {
        let mut mirror = self.mirror.lock().unwrap();
        let next: Option<ScriptValue> = match mirror.get(name) {
            None => Some(ScriptValue::Str(value.to_owned())),
            Some(ScriptValue::Str(cur)) => {
                (cur != value).then(|| ScriptValue::Str(value.to_owned()))
            }
            Some(ScriptValue::Bool(cur)) => match value {
                "true" | "1" => (!*cur).then_some(ScriptValue::Bool(true)),
                "false" | "0" => (*cur).then_some(ScriptValue::Bool(false)),
                _ => None,
            },
            Some(ScriptValue::I64(cur)) => value
                .parse::<i64>()
                .ok()
                .filter(|n| n != cur)
                .map(ScriptValue::I64),
            Some(ScriptValue::F64(cur)) => value
                .parse::<f64>()
                .ok()
                .filter(|n| n != cur)
                .map(ScriptValue::F64),
            // Unit / Array / Map: structured or empty - stay authoritative.
            Some(_) => None,
        };
        if let Some(next) = next {
            mirror.insert(name.to_owned(), next);
        }
    }

    /// Mirror `value` and queue the `SetSignal` that carries it to the store.
    pub(crate) fn set_signal(&self, name: &str, value: ScriptValue) {
        let text = value.stringify();
        self.mirror_set(name, value);
        self.push(ScriptCommand::SetSignal {
            name: name.to_owned(),
            value: text,
        });
    }

    /// Replace an array signal's items, mirroring them and queueing the
    /// `SetArray` that drives `<for each>` reconciliation next tick.
    pub(crate) fn set_array(&self, name: &str, items: Vec<ScriptValue>) {
        let rows = array_to_rows(&items);
        self.mirror_set(name, ScriptValue::Array(items));
        self.push(ScriptCommand::SetArray {
            name: name.to_owned(),
            items: rows,
        });
    }

    /// Current items of the named array signal; empty when it holds anything
    /// else.
    pub(crate) fn array_items(&self, name: &str) -> Vec<ScriptValue> {
        match self.mirror.lock().unwrap().get(name) {
            Some(ScriptValue::Array(a)) => a.clone(),
            _ => Vec::new(),
        }
    }

    /// The handler function registered for `(event, key)`, falling back to the
    /// template-suffix form: a handler for `save` also matches
    /// `user-card:save`.
    pub(crate) fn handler_for(&self, event: &str, key: &str) -> Option<String> {
        let handlers = self.handlers.read().ok()?;
        if let Some(f) = handlers.get(&(event.to_owned(), key.to_owned())) {
            return Some(f.clone());
        }
        if let Some(idx) = key.rfind(':') {
            let suffix = &key[idx + 1..];
            if let Some(f) = handlers.get(&(event.to_owned(), suffix.to_owned())) {
                return Some(f.clone());
            }
        }
        None
    }

    /// The derivations to recompute this pass: those awaiting their initial run
    /// and those whose deps went dirty. Snapshotted so no lock is held while
    /// the driver invokes the recompute bodies.
    pub(crate) fn derivations_matching(
        &self,
        dirty: &HashSet<&str>,
        pending: &HashSet<String>,
    ) -> Vec<(String, Vec<String>, String)> {
        self.derivations
            .read()
            .unwrap()
            .iter()
            .filter(|(name, (deps, _))| {
                pending.contains(name.as_str()) || deps.iter().any(|d| dirty.contains(d.as_str()))
            })
            .map(|(name, (deps, f))| (name.clone(), deps.clone(), f.clone()))
            .collect()
    }

    /// Derivations registered but never successfully evaluated.
    pub(crate) fn pending_initial(&self) -> HashSet<String> {
        self.pending.lock().unwrap().iter().cloned().collect()
    }

    /// Drop `evaluated` from the pending set.
    pub(crate) fn clear_pending(&self, evaluated: &[String]) {
        let mut pending = self.pending.lock().unwrap();
        for name in evaluated {
            pending.remove(name);
        }
    }

    /// The handler function bound to `token`, if any.
    pub(crate) fn event_handler(&self, token: u64) -> Option<String> {
        self.event_handlers.read().unwrap().get(&token).cloned()
    }

    /// Forget the handler bound to `token`.
    pub(crate) fn drop_event_handler(&self, token: u64) {
        self.event_handlers.write().unwrap().remove(&token);
    }

    /// Drop every registration and queued command, and clear the process-wide
    /// event bindings this host owns.
    pub(crate) fn reset(&self) {
        self.sink.lock().unwrap().clear();
        self.mirror.lock().unwrap().clear();
        self.handlers.write().unwrap().clear();
        self.event_handlers.write().unwrap().clear();
        lumen_script::event::clear_host_bindings();
    }

    /// Bridge an embedder-registered [`CommandFn`] onto `sink` as a variadic
    /// `lumen`-namespace builtin. One registration serves any arity; the script
    /// declares it with a `...` argument list.
    pub(crate) fn register_command_fn<S: HostFnSink>(
        &self,
        sink: &mut S,
        name: &str,
        f: CommandFn,
    ) {
        let queue = self.sink.clone();
        sink.register_host_fn_variadic(HOST_NAMESPACE, name, move |args: &[Value]| {
            let svs: Vec<ScriptValue> = args.iter().map(candela_value_to_script).collect();
            queue.lock().unwrap().extend(f(&svs));
            Value::Null
        });
    }
}
/// Register every Lumen builtin into `sink`, each closure closing over `r`'s
/// registries.
///
/// This is the one list. [`crate::engine_host::CandelaHost`] feeds it a
/// `candela::Engine`, which binds the declarations a fresh compile produced;
/// [`crate::vm_host::CandelaVmHost`] feeds it a `candela_vm::HostRegistry`,
/// which binds the declarations a `.cdlb` image recorded. A builtin added here
/// reaches both.
pub(crate) fn register_lumen_host_fns<S: HostFnSink>(engine: &mut S, r: &Registries) {
    // -- app-command emitters ----------------------------------------
    enqueue!(engine, r.sink, "add_clicks", |n: i64| {
        ScriptCommand::AddClicks(n as i32)
    });
    enqueue!(
        engine,
        r.sink,
        "set_string",
        |key: String, value: String| ScriptCommand::SetString { key, value }
    );
    enqueue!(
        engine,
        r.sink,
        "set_text",
        |target_id: String, text: String| ScriptCommand::SetText { target_id, text }
    );
    enqueue!(
        engine,
        r.sink,
        "set_src",
        |target_id: String, path: String| ScriptCommand::SetSrc { target_id, path }
    );

    // -- signals (string + typed scalars) ----------------------------
    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_get",
        move |name: String| -> String {
            m.lock()
                .unwrap()
                .get(&name)
                .map(ScriptValue::stringify)
                .unwrap_or_default()
        },
    );

    let reg = r.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_set",
        move |name: String, value: String| {
            reg.set_signal(&name, ScriptValue::Str(value));
        },
    );

    register_typed_signal_int(engine, r);
    register_typed_signal_float(engine, r);
    register_typed_signal_bool(engine, r);
    register_color_signals(engine, r);
    register_array_signals(engine, r);

    // `is_valid(id)`: the per-tick `valid:<id>` signal `apply_validation`
    // writes from the element's `Validation` component. An element with no
    // validation state has never written the signal, and reads as valid.
    let m = r.mirror.clone();
    engine.register_host_fn(HOST_NAMESPACE, "is_valid", move |id: String| -> bool {
        match m.lock().unwrap().get(&format!("valid:{id}")) {
            Some(ScriptValue::Bool(b)) => *b,
            Some(ScriptValue::Str(s)) => s == "true",
            Some(_) => false,
            None => true,
        }
    });

    // -- timers ------------------------------------------------------
    enqueue!(engine, r.sink, "set_timeout", |name: String, ms: i64| {
        ScriptCommand::SetTimer {
            name,
            millis: ms.max(0) as u64,
            repeat: false,
        }
    });
    enqueue!(engine, r.sink, "set_interval", |name: String, ms: i64| {
        ScriptCommand::SetTimer {
            name,
            millis: ms.max(0) as u64,
            repeat: true,
        }
    });
    enqueue!(engine, r.sink, "cancel_timer", |name: String| {
        ScriptCommand::CancelTimer { name }
    });

    // -- notifications / clipboard / tray ----------------------------
    enqueue!(engine, r.sink, "notify", |title: String, body: String| {
        ScriptCommand::Notify { title, body }
    });
    enqueue!(
        engine,
        r.sink,
        "notify_ex",
        |id: String, title: String, body: String, options: String, actions: String| {
            ScriptCommand::NotifyEx {
                id,
                title,
                body,
                options,
                actions,
            }
        }
    );
    enqueue!(engine, r.sink, "clipboard_write", |text: String| {
        ScriptCommand::ClipboardWrite { text }
    });
    enqueue!(engine, r.sink, "clipboard_read", |tag: String| {
        ScriptCommand::ClipboardRead { tag }
    });
    enqueue!(engine, r.sink, "open_url", |url: String| {
        ScriptCommand::OpenUrl { url }
    });
    enqueue!(engine, r.sink, "open_path", |path: String| {
        ScriptCommand::OpenPath { path }
    });
    enqueue!(engine, r.sink, "reveal_path", |path: String| {
        ScriptCommand::RevealPath { path }
    });
    enqueue!(
        engine,
        r.sink,
        "keep_awake",
        |name: String, reason: String| ScriptCommand::KeepAwake { name, reason }
    );
    enqueue!(engine, r.sink, "allow_sleep", |name: String| {
        ScriptCommand::AllowSleep { name }
    });
    enqueue!(engine, r.sink, "copy_image", |path: String| {
        ScriptCommand::CopyImageToClipboard { path }
    });
    enqueue!(engine, r.sink, "save_clipboard_image", |path: String| {
        ScriptCommand::SaveClipboardImage { path }
    });
    enqueue!(
        engine,
        r.sink,
        "tray_icon",
        |id: String, icon_path: String, tooltip: String| ScriptCommand::RegisterTrayIcon {
            id,
            icon_path,
            tooltip: (!tooltip.is_empty()).then_some(tooltip),
            menu: String::new(),
            template: false,
        }
    );
    enqueue!(
        engine,
        r.sink,
        "tray_icon_menu",
        |id: String, icon_path: String, tooltip: String, menu: String, template: bool| {
            ScriptCommand::RegisterTrayIcon {
                id,
                icon_path,
                tooltip: (!tooltip.is_empty()).then_some(tooltip),
                menu,
                template,
            }
        }
    );
    enqueue!(engine, r.sink, "unregister_tray", |id: String| {
        ScriptCommand::UnregisterTrayIcon { id }
    });

    // -- menus (modeled as `__menu_open:<id>` signals) ---------------
    let reg = r.clone();
    engine.register_host_fn(HOST_NAMESPACE, "open_menu", move |id: String| {
        reg.set_signal(&menu_signal(&id), ScriptValue::Bool(true));
    });
    let reg = r.clone();
    engine.register_host_fn(HOST_NAMESPACE, "close_menu", move |id: String| {
        reg.set_signal(&menu_signal(&id), ScriptValue::Bool(false));
    });

    // -- file dialogs ------------------------------------------------
    // pick_file / pick_files / pick_folder share one closure shape;
    // the loop keeps them from drifting apart (parity with the Rhai
    // host's identical trio loop).
    for (fname, kind) in [
        ("pick_file", FileDialogKind::Open),
        ("pick_files", FileDialogKind::OpenMulti),
        ("pick_folder", FileDialogKind::PickFolder),
    ] {
        let s = r.sink.clone();
        engine.register_host_fn(HOST_NAMESPACE, fname, move |tag: String| {
            s.lock().unwrap().push(file_dialog(kind, tag, None));
        });
    }
    enqueue!(
        engine,
        r.sink,
        "save_file",
        |tag: String, default_name: String| file_dialog(
            FileDialogKind::Save,
            tag,
            Some(default_name)
        )
    );
    enqueue!(
        engine,
        r.sink,
        "pick_file_filtered",
        |tag: String, spec: String| ScriptCommand::OpenFileDialog {
            kind: FileDialogKind::Open,
            tag,
            filters: parse_filter_spec(&spec),
            default_name: None,
        }
    );

    // -- hotkeys -----------------------------------------------------
    enqueue!(
        engine,
        r.sink,
        "register_hotkey",
        |name: String, accelerator: String| ScriptCommand::RegisterHotkey { name, accelerator }
    );
    enqueue!(engine, r.sink, "unregister_hotkey", |name: String| {
        ScriptCommand::UnregisterHotkey { name }
    });

    // -- classes -----------------------------------------------------
    enqueue!(
        engine,
        r.sink,
        "set_class",
        |id: String, classes: String| ScriptCommand::SetClasses {
            target_id: id,
            classes,
        }
    );
    enqueue!(engine, r.sink, "set_root_class", |classes: String| {
        ScriptCommand::SetClasses {
            target_id: "<root>".to_owned(),
            classes,
        }
    });
    enqueue!(engine, r.sink, "set_color_scheme", |name: String| {
        ScriptCommand::SetColorScheme { name }
    });

    // -- file-based pages --------------------------------------------
    // Navigation rides the host-neutral `lumen_core::nav` bus, the same
    // one an `<a href>` click and the Rust SDK write, so these need no
    // world access and register here rather than as an embedder hook.
    // A candela host fn cannot be arity-overloaded on one name, so the
    // no-arg reader Rhai and Lua spell `page()` is `page_current()` here.
    engine.register_host_fn(HOST_NAMESPACE, "page", |path: String| {
        lumen_core::nav::navigate(path);
    });
    engine.register_host_fn(HOST_NAMESPACE, "page_current", || -> String {
        lumen_core::nav::current()
    });
    engine.register_host_fn(HOST_NAMESPACE, "page_back", || {
        lumen_core::nav::back();
    });
    engine.register_host_fn(HOST_NAMESPACE, "page_forward", || {
        lumen_core::nav::forward();
    });

    // -- networking --------------------------------------------------
    enqueue!(engine, r.sink, "fetch", |url: String, tag: String| {
        ScriptCommand::Fetch { url, tag }
    });
    register_http(engine, r);

    // -- text parsers ------------------------------------------------
    // Both return a dynamically-shaped value, so both register
    // variadically and are declared `any name(...)`; see the crate docs.
    engine.register_host_fn_variadic(HOST_NAMESPACE, "parse_json", |args: &[Value]| {
        parse::json(&arg_text(args, 0))
    });
    engine.register_host_fn_variadic(HOST_NAMESPACE, "parse_markdown", |args: &[Value]| {
        parse::markdown(&arg_text(args, 0))
    });

    // -- template-local ids ------------------------------------------
    // `local_id("user-card:btn", "label")` is `"user-card:label"`: the
    // sibling id `suffix` inside the same template instance as `source`.
    // A source with no `:` prefix returns `suffix` unchanged.
    engine.register_host_fn(
        HOST_NAMESPACE,
        "local_id",
        |source: String, suffix: String| -> String {
            match source.rfind(':') {
                Some(colon) => format!("{}:{suffix}", &source[..colon]),
                None => suffix,
            }
        },
    );

    // -- diagnostics -------------------------------------------------
    // candela's own `print` writes to process stdout. `lumen::print`
    // routes through the command sink instead, so the text reaches the
    // same place the Rhai and Lua hosts' `print` does. Arguments are
    // stringified and joined with a space.
    {
        let sink = r.sink.clone();
        engine.register_host_fn_variadic(HOST_NAMESPACE, "print", move |args: &[Value]| {
            let line = args
                .iter()
                .map(|v| candela_value_to_script(v).stringify())
                .collect::<Vec<_>>()
                .join(" ");
            sink.lock().unwrap().push(ScriptCommand::Print(line));
            Value::Null
        });
    }

    // -- translation -------------------------------------------------
    // `lumen::t("key")` returns the string the app's active locale
    // carries for `key`, or `key` itself when no catalogue does - an
    // untranslated app still renders something readable. The catalogue
    // lives behind the process-wide `lumen_core::i18n` hook the runtime
    // installs, so this host links no Fluent/ICU code and needs no
    // world access. `tr` is Qt's spelling of the same call.
    engine.register_host_fn(HOST_NAMESPACE, "t", |key: String| -> String {
        lumen_core::i18n::translate(&key)
    });
    engine.register_host_fn(HOST_NAMESPACE, "tr", |key: String| -> String {
        lumen_core::i18n::translate(&key)
    });

    // -- filesystem --------------------------------------------------
    engine.register_host_fn(HOST_NAMESPACE, "read_file", |path: String| -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "write_file",
        |path: String, contents: String| -> bool { std::fs::write(path, contents).is_ok() },
    );

    // -- per-id handler routing --------------------------------------
    let h = r.handlers.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "on",
        move |event: String, id: String, handler: String| {
            h.write().unwrap().insert((event, id), handler);
        },
    );

    // -- derived signals ---------------------------------------------
    // `derive(name, deps, f)`: candela has no first-class closure value, so
    // the recompute body is passed by the script function's NAME (a plain
    // string) - the candela-idiomatic way to reference a function, and what
    // `apply_derivations` re-invokes via `call_closure`. The dep list is a
    // `string[]`, which marshals across the boundary natively.
    let d = r.derivations.clone();
    let p = r.pending.clone();
    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "derive",
        move |name: String, deps: Vec<String>, f: String| {
            d.write().unwrap().insert(name.clone(), (deps, f));
            p.lock().unwrap().insert(name.clone());
            m.lock().unwrap().entry(name).or_insert(ScriptValue::Unit);
        },
    );

    register_node_query(engine);
    register_node_mutators(engine, r);
    register_node_events(engine, r);
    register_web_namespaces(engine, r);

    register_audio(engine, r);
}

// -- free helpers ------------------------------------------------------------

/// Read positional argument `idx` of a variadic host call as text. A string
/// argument comes through verbatim; anything else takes its canonical
/// stringified form, and a missing argument is the empty string.
fn arg_text(args: &[Value], idx: usize) -> String {
    args.get(idx)
        .map(|v| candela_value_to_script(v).stringify())
        .unwrap_or_default()
}

/// Read positional argument `idx` of a variadic host call as a
/// [`ScriptValue`], or [`ScriptValue::Unit`] when it is absent.
fn arg_value(args: &[Value], idx: usize) -> ScriptValue {
    args.get(idx)
        .map_or(ScriptValue::Unit, candela_value_to_script)
}

/// Resolve a candela `int` node id (from the process-global side-table)
/// into the packed handle the host-neutral query surface consumes.
fn cd_id_to_packed(id: i64) -> Option<u64> {
    i32::try_from(id)
        .ok()
        .and_then(lumen_core::node::resolve_node)
        .map(|h| h.pack())
}

/// Intern a packed handle back into a candela `int` id (`0` for none).
fn cd_packed_to_id(packed: u64) -> i64 {
    match lumen_core::node::NodeHandle::unpack(packed) {
        Some(h) => lumen_core::node::intern_node(h.entity, h.generation) as i64,
        None => 0,
    }
}

/// Resolve a candela `int` id to its raw packed bits (a live handle OR a
/// reserved spawn token), for the mutation surface.
fn cd_id_to_raw(id: i64) -> Option<u64> {
    i32::try_from(id)
        .ok()
        .and_then(lumen_core::node::resolve_node_raw)
}

/// Intern any packed handle (real or reserved token) into a candela id.
fn cd_intern_raw(packed: u64) -> i64 {
    lumen_core::node::intern_node_raw(packed) as i64
}

/// Register the dynamic DOM read side. candela's value type is a small
/// integer, so a 64-bit handle cannot ride inside it; every node is an
/// `int` id interned in the process-global side-table
/// (`lumen_core::node`). Scripts call these procedurally
/// (`lumen::node_parent(h)`); the `impl Node`/`node.parent()` sugar waits
/// on user-struct methods landing in candela.
/// Register the phase-4 event surface (procedural). candela has no closure
/// value, so a handler is referenced by function name and the event object is
/// reached through free `event_*` accessors keyed by the event id passed to
/// the handler. The method-sugar form (`ev.target()`) waits on user-struct
/// impl methods landing in the pinned candela dep.
///
/// ```candela
/// let off = lumen::event_on(btn, "click", "on_save");   // returns a token
/// fn on_save(ev) {
///     let t = lumen::event_target(ev);
///     lumen::event_prevent_default(ev);
/// }
/// // later: lumen::event_off(off);
/// ```
fn register_node_events<S: HostFnSink>(engine: &mut S, r: &Registries) {
    use lumen_script::event as ev;

    // Bind: resolve the node id, stash the handler name by token, emit
    // BindEvent, and return the off token.
    for (fname, capture) in [("event_on", false), ("event_on_capture", true)] {
        let sink = r.sink.clone();
        let eh = r.event_handlers.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            fname,
            move |node: i64, event_type: String, handler: String| -> i64 {
                let Some(packed) = cd_id_to_raw(node) else {
                    return 0;
                };
                let token = ev::mint_event_token();
                eh.write().unwrap().insert(token, handler);
                sink.lock().unwrap().push(ScriptCommand::BindEvent {
                    node: packed,
                    event_type,
                    capture,
                    token,
                });
                token as i64
            },
        );
    }
    // Unbind by token.
    {
        let sink = r.sink.clone();
        let eh = r.event_handlers.clone();
        engine.register_host_fn(HOST_NAMESPACE, "event_off", move |token: i64| {
            let t = token as u64;
            eh.write().unwrap().remove(&t);
            sink.lock()
                .unwrap()
                .push(ScriptCommand::UnbindEvent { token: t });
        });
    }

    // Accessors. Each takes the event id (currently the token) and reads the
    // process-global current-event cell; the id is accepted for the
    // web-idiomatic `event_target(ev)` shape and to leave room for nested
    // dispatch later.
    engine.register_host_fn(HOST_NAMESPACE, "event_target", |_ev: i64| -> i64 {
        cd_packed_to_id(ev::event_target())
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_current_target", |_ev: i64| -> i64 {
        cd_packed_to_id(ev::event_current_target())
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_type", |_ev: i64| -> String {
        ev::event_type()
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_key", |_ev: i64| -> String {
        ev::event_key()
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_value", |_ev: i64| -> String {
        ev::event_value()
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_button", |_ev: i64| -> i64 {
        ev::event_button()
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_x", |_ev: i64| -> f64 {
        ev::event_position_local().0
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_y", |_ev: i64| -> f64 {
        ev::event_position_local().1
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_client_x", |_ev: i64| -> f64 {
        ev::event_position_client().0
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_client_y", |_ev: i64| -> f64 {
        ev::event_position_client().1
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_delta_x", |_ev: i64| -> f64 {
        ev::event_delta().0
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_delta_y", |_ev: i64| -> f64 {
        ev::event_delta().1
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_shift", |_ev: i64| -> bool {
        ev::event_modifiers().0
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_ctrl", |_ev: i64| -> bool {
        ev::event_modifiers().1
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_alt", |_ev: i64| -> bool {
        ev::event_modifiers().2
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_super", |_ev: i64| -> bool {
        ev::event_modifiers().3
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_prevent_default", |_ev: i64| {
        ev::event_prevent_default();
    });
    engine.register_host_fn(HOST_NAMESPACE, "event_stop_propagation", |_ev: i64| {
        ev::event_stop_propagation();
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "event_stop_immediate_propagation",
        |_ev: i64| {
            ev::event_stop_immediate_propagation();
        },
    );
}

fn register_node_query<S: HostFnSink>(engine: &mut S) {
    use lumen_script::node_query;

    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_query",
        |selector: String| -> Vec<i64> {
            node_query::run_query(&selector)
                .map(|q| q.nodes.iter().map(|&p| cd_packed_to_id(p)).collect())
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_get_by_id", |id: String| -> i64 {
        node_query::run_get_by_id(&id)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_document", || -> i64 {
        node_query::run_document().map(cd_packed_to_id).unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_parent", |h: i64| -> i64 {
        cd_id_to_packed(h)
            .and_then(node_query::node_parent)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_first_child", |h: i64| -> i64 {
        cd_id_to_packed(h)
            .and_then(node_query::node_first_child)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_last_child", |h: i64| -> i64 {
        cd_id_to_packed(h)
            .and_then(node_query::node_last_child)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_next", |h: i64| -> i64 {
        cd_id_to_packed(h)
            .and_then(node_query::node_next)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_prev", |h: i64| -> i64 {
        cd_id_to_packed(h)
            .and_then(node_query::node_prev)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_children", |h: i64| -> Vec<i64> {
        cd_id_to_packed(h)
            .map(|p| {
                node_query::node_children(p)
                    .iter()
                    .map(|&x| cd_packed_to_id(x))
                    .collect()
            })
            .unwrap_or_default()
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_closest",
        |h: i64, selector: String| -> i64 {
            cd_id_to_packed(h)
                .and_then(|p| node_query::node_closest(p, &selector).ok().flatten())
                .map(cd_packed_to_id)
                .unwrap_or(0)
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_valid", |h: i64| -> bool {
        cd_id_to_packed(h)
            .map(node_query::node_valid)
            .unwrap_or(false)
    });
}

/// Register the dynamic DOM write side (phases 2 + 3) for candela.
///
/// The pinned candela dep predates user-struct impl-block methods, so the
/// mutators are procedural under the `lumen` namespace
/// (`lumen::node_set_attr(h, name, value)`), not `node.set_attr(..)`
/// method sugar, and there is no fluent chaining; each call is a separate
/// statement. A future candela-dep bump enables the method + chaining form
/// the rhai / lua hosts already expose. Handles are the same `int` ids the
/// read side uses; `node_spawn` / `node_clone_deep` mint a reserved-token
/// id valid for the whole tick.
fn register_node_mutators<S: HostFnSink>(engine: &mut S, r: &Registries) {
    use lumen_script::node_query;

    /// Register a mutator whose body resolves the node id and pushes one
    /// command into the sink.
    macro_rules! mutate {
        ($name:literal, |$node:ident $(, $arg:ident : $ty:ty)*| $build:expr) => {{
            let sink = r.sink.clone();
            engine.register_host_fn(
                HOST_NAMESPACE,
                $name,
                move |$node: i64 $(, $arg: $ty)*| {
                    if let Some($node) = cd_id_to_raw($node) {
                        sink.lock().unwrap().push($build);
                    }
                },
            );
        }};
    }

    mutate!("node_set_attr", |node, name: String, value: String| {
        ScriptCommand::SetAttr { node, name, value }
    });
    mutate!("node_remove_attr", |node, name: String| {
        ScriptCommand::RemoveAttr { node, name }
    });
    mutate!("node_set_id", |node, id: String| {
        ScriptCommand::SetAttr {
            node,
            name: "id".to_string(),
            value: id,
        }
    });
    mutate!("node_set_text", |node, text: String| {
        ScriptCommand::SetNodeText { node, text }
    });
    // Guarded markup injection (design 4.4). Do not feed untrusted content.
    mutate!("node_set_inner_markup", |node, markup: String| {
        ScriptCommand::SetInnerMarkup { node, markup }
    });
    mutate!("node_class_add", |node, class: String| {
        ScriptCommand::ClassAdd { node, class }
    });
    mutate!("node_class_remove", |node, class: String| {
        ScriptCommand::ClassRemove { node, class }
    });
    mutate!("node_class_toggle", |node, class: String| {
        ScriptCommand::ClassToggle { node, class }
    });
    mutate!("node_set_class", |node, classes: String| {
        ScriptCommand::SetAttr {
            node,
            name: "class".to_string(),
            value: classes,
        }
    });
    mutate!("node_set_style", |node, name: String, value: String| {
        ScriptCommand::SetStyleProp { node, name, value }
    });
    mutate!("node_style_remove", |node, name: String| {
        ScriptCommand::RemoveStyleProp { node, name }
    });
    mutate!("node_remove", |node| ScriptCommand::RemoveNode { node });

    // Two-handle structural ops.
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_append",
            move |parent: i64, child: i64| {
                if let (Some(parent), Some(child)) = (cd_id_to_raw(parent), cd_id_to_raw(child)) {
                    sink.lock().unwrap().push(ScriptCommand::Insert {
                        parent,
                        node: child,
                        before: 0,
                    });
                }
            },
        );
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_insert_before",
            move |parent: i64, child: i64, reference: i64| {
                if let (Some(parent), Some(child)) = (cd_id_to_raw(parent), cd_id_to_raw(child)) {
                    let before = cd_id_to_raw(reference).unwrap_or(0);
                    sink.lock().unwrap().push(ScriptCommand::Insert {
                        parent,
                        node: child,
                        before,
                    });
                }
            },
        );
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_set_parent",
            move |node: i64, parent: i64| {
                if let (Some(node), Some(parent)) = (cd_id_to_raw(node), cd_id_to_raw(parent)) {
                    sink.lock().unwrap().push(ScriptCommand::Insert {
                        parent,
                        node,
                        before: 0,
                    });
                }
            },
        );
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_move_to",
            move |node: i64, parent: i64| {
                if let (Some(node), Some(parent)) = (cd_id_to_raw(node), cd_id_to_raw(parent)) {
                    sink.lock().unwrap().push(ScriptCommand::Insert {
                        parent,
                        node,
                        before: 0,
                    });
                }
            },
        );
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_replace_with",
            move |old: i64, new: i64| {
                if let (Some(old), Some(new)) = (cd_id_to_raw(old), cd_id_to_raw(new)) {
                    sink.lock()
                        .unwrap()
                        .push(ScriptCommand::ReplaceWith { old, new });
                }
            },
        );
    }

    // Create verbs -> return the new node's id.
    {
        let sink = r.sink.clone();
        engine.register_host_fn(HOST_NAMESPACE, "node_spawn", move |tag: String| -> i64 {
            let (handle, cmd) = node_query::build_spawn(&tag);
            sink.lock().unwrap().push(cmd);
            cd_intern_raw(handle)
        });
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn(
            HOST_NAMESPACE,
            "node_clone_deep",
            move |source: i64| -> i64 {
                let Some(source) = cd_id_to_raw(source) else {
                    return 0;
                };
                let (handle, cmd) = node_query::build_clone(source);
                sink.lock().unwrap().push(cmd);
                cd_intern_raw(handle)
            },
        );
    }

    // Read-backs.
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_get_attr",
        |node: i64, name: String| -> String {
            cd_id_to_raw(node)
                .and_then(|h| node_query::node_get_attr(h, &name))
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_text", |node: i64| -> String {
        cd_id_to_raw(node)
            .and_then(node_query::node_text)
            .unwrap_or_default()
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_id", |node: i64| -> String {
        cd_id_to_raw(node)
            .and_then(node_query::node_id)
            .unwrap_or_default()
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_class_contains",
        |node: i64, class: String| -> bool {
            cd_id_to_raw(node)
                .map(|h| node_query::node_class_contains(h, &class))
                .unwrap_or(false)
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_style_get",
        |node: i64, prop: String| -> String {
            cd_id_to_raw(node)
                .and_then(|h| node_query::node_style_get(h, &prop))
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_computed_style",
        |node: i64, prop: String| -> String {
            cd_id_to_raw(node)
                .and_then(|h| node_query::node_computed_style(h, &prop))
                .unwrap_or_default()
        },
    );
    register_node_introspection(engine);
}

/// Register the phase-5 low-level introspection procedural surface for
/// candela: geometry, full computed style / provenance, typed component
/// reads, and global runtime state. Value maps marshal as candela
/// `{string: T}` maps; a node argument is the interned `int` id. Absent /
/// unknown reads yield an empty map (candela host fns surface no error).
fn register_node_introspection<S: HostFnSink>(engine: &mut S) {
    use lumen_script::introspect as ins;
    use std::collections::HashMap;

    fn rect_map(r: ins::NodeRect) -> HashMap<String, f64> {
        HashMap::from([
            ("x".to_string(), r.x as f64),
            ("y".to_string(), r.y as f64),
            ("width".to_string(), r.width as f64),
            ("height".to_string(), r.height as f64),
            ("client_x".to_string(), r.client_x as f64),
            ("client_y".to_string(), r.client_y as f64),
        ])
    }

    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_rect",
        |node: i64| -> HashMap<String, f64> {
            cd_id_to_raw(node)
                .and_then(ins::node_rect)
                .map(rect_map)
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_content_rect",
        |node: i64| -> HashMap<String, f64> {
            cd_id_to_raw(node)
                .and_then(ins::node_content_rect)
                .map(rect_map)
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_scroll",
        |node: i64| -> HashMap<String, f64> {
            cd_id_to_raw(node)
                .and_then(ins::node_scroll)
                .map(|s| {
                    HashMap::from([
                        ("x".to_string(), s.x as f64),
                        ("y".to_string(), s.y as f64),
                        ("max_x".to_string(), s.max_x as f64),
                        ("max_y".to_string(), s.max_y as f64),
                    ])
                })
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_is_visible", |node: i64| -> bool {
        cd_id_to_raw(node)
            .map(ins::node_is_visible)
            .unwrap_or(false)
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_z_index", |node: i64| -> i64 {
        cd_id_to_raw(node)
            .map(|h| ins::node_z_index(h) as i64)
            .unwrap_or(0)
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_computed_style_all",
        |node: i64| -> HashMap<String, String> {
            cd_id_to_raw(node)
                .map(|h| ins::node_computed_style_map(h).into_iter().collect())
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_inline_style",
        |node: i64| -> HashMap<String, String> {
            cd_id_to_raw(node)
                .map(|h| ins::node_inline_style(h).into_iter().collect())
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_attrs",
        |node: i64| -> HashMap<String, String> {
            cd_id_to_raw(node)
                .map(|h| ins::node_attrs(h).into_iter().collect())
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_classes", |node: i64| -> Vec<String> {
        cd_id_to_raw(node)
            .map(ins::node_classes)
            .unwrap_or_default()
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_entity_id",
        |node: i64| -> HashMap<String, i64> {
            match cd_id_to_raw(node).and_then(ins::node_entity_id) {
                Some((index, generation)) => HashMap::from([
                    ("index".to_string(), index as i64),
                    ("generation".to_string(), generation as i64),
                ]),
                None => HashMap::new(),
            }
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_components",
        |node: i64| -> Vec<String> {
            cd_id_to_raw(node)
                .map(ins::node_components)
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(
        HOST_NAMESPACE,
        "node_component",
        |node: i64, name: String| -> HashMap<String, String> {
            cd_id_to_raw(node)
                .and_then(|h| ins::node_component(h, &name).ok().flatten())
                .map(|m| m.into_iter().collect())
                .unwrap_or_default()
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "node_outer_markup", |node: i64| -> String {
        cd_id_to_raw(node)
            .map(ins::outer_markup)
            .unwrap_or_default()
    });
    engine.register_host_fn(HOST_NAMESPACE, "node_inner_markup", |node: i64| -> String {
        cd_id_to_raw(node)
            .map(ins::inner_markup)
            .unwrap_or_default()
    });

    // Global runtime state (no node argument).
    engine.register_host_fn(
        HOST_NAMESPACE,
        "pointer_state",
        || -> HashMap<String, String> {
            let p = ins::pointer_state();
            HashMap::from([
                ("x".to_string(), p.x.to_string()),
                ("y".to_string(), p.y.to_string()),
                ("inside".to_string(), p.inside.to_string()),
                ("buttons".to_string(), p.buttons.to_string()),
                ("shift".to_string(), p.shift.to_string()),
                ("ctrl".to_string(), p.ctrl.to_string()),
                ("alt".to_string(), p.alt.to_string()),
                ("super".to_string(), p.super_.to_string()),
            ])
        },
    );
    engine.register_host_fn(HOST_NAMESPACE, "frame_info", || -> HashMap<String, f64> {
        let f = ins::frame_info();
        HashMap::from([
            ("frame".to_string(), f.frame as f64),
            ("dt_ms".to_string(), f.dt_ms),
            ("dirty_count".to_string(), f.dirty_count as f64),
        ])
    });
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signals_all",
        || -> HashMap<String, String> { ins::signals_all().into_iter().collect() },
    );
    engine.register_host_fn(HOST_NAMESPACE, "dump_tree", || -> String {
        ins::dump_tree()
    });

    // `matched_rules(node)`: the stylesheet rules that matched, in ascending
    // cascade order (last wins). Each entry is
    // `{ selector, specificity, source, source_order, declarations }`, mixing
    // strings, an int list, and a nested map, so it registers variadically and
    // is declared `any matched_rules(...)`.
    engine.register_host_fn_variadic(HOST_NAMESPACE, "matched_rules", |args: &[Value]| {
        let Some(handle) = args.first().and_then(Value::as_i64).and_then(cd_id_to_raw) else {
            return Value::Array(Vec::new());
        };
        Value::Array(
            ins::node_matched_rules(handle)
                .into_iter()
                .map(|rule| {
                    Value::Map(std::collections::BTreeMap::from([
                        ("selector".to_owned(), Value::String(rule.selector)),
                        (
                            "specificity".to_owned(),
                            Value::Array(vec![
                                Value::Int(i64::from(rule.specificity.0)),
                                Value::Int(i64::from(rule.specificity.1)),
                                Value::Int(i64::from(rule.specificity.2)),
                            ]),
                        ),
                        ("source".to_owned(), Value::String(rule.source)),
                        (
                            "source_order".to_owned(),
                            Value::Int(rule.source_order as i64),
                        ),
                        (
                            "declarations".to_owned(),
                            Value::Map(
                                rule.declarations
                                    .into_iter()
                                    .map(|(k, v)| (k, Value::String(v)))
                                    .collect(),
                            ),
                        ),
                    ]))
                })
                .collect(),
        )
    });
}

/// Register the `window` / `document` / `history` namespaces (section 4.8)
/// for candela. Unlike per-node handles, candela namespaces
/// (`window.set_href(..)`) compile today, so these bind natively. Each is a
/// separate host namespace the app declares with its own `host "..." { ... }`
/// block.
fn register_web_namespaces<S: HostFnSink>(engine: &mut S, r: &Registries) {
    use lumen_script::node_query;

    // window navigation + state.
    engine.register_host_fn("window", "set_href", |path: String| {
        lumen_core::nav::navigate(path);
    });
    engine.register_host_fn("window", "href", || -> String {
        lumen_core::nav::current()
    });
    engine.register_host_fn("window", "reload", || {
        lumen_core::nav::navigate(lumen_core::nav::current());
    });
    engine.register_host_fn("window", "title", || -> String {
        lumen_core::window_state::title()
    });
    engine.register_host_fn("window", "dpr", || -> f64 {
        lumen_core::window_state::dpr() as f64
    });
    // `[width, height]` in logical pixels; a two-element list because a candela
    // host fn returns one value.
    engine.register_host_fn("window", "size", || -> Vec<f64> {
        let (width, height) = lumen_core::window_state::size();
        vec![f64::from(width), f64::from(height)]
    });
    {
        let sink = r.sink.clone();
        engine.register_host_fn("window", "set_title", move |title: String| {
            sink.lock()
                .unwrap()
                .push(ScriptCommand::WindowSetTitle { title });
        });
    }
    {
        let sink = r.sink.clone();
        engine.register_host_fn("window", "set_size", move |w: f64, h: f64| {
            sink.lock().unwrap().push(ScriptCommand::WindowSetSize {
                width: w as f32,
                height: h as f32,
            });
        });
    }
    // window.location parts as flat `location_*` fns (candela has no nested
    // namespace value). path only; query / hash are untracked.
    engine.register_host_fn("window", "location_path", || -> String {
        lumen_core::nav::current()
    });
    engine.register_host_fn("window", "location_query", || -> String { String::new() });
    engine.register_host_fn("window", "location_hash", || -> String { String::new() });

    // history.
    engine.register_host_fn("history", "back", || {
        lumen_core::nav::back();
    });
    engine.register_host_fn("history", "forward", || {
        lumen_core::nav::forward();
    });
    engine.register_host_fn("history", "go", |delta: i64| {
        for _ in 0..delta.unsigned_abs() {
            if delta < 0 {
                lumen_core::nav::back();
            } else {
                lumen_core::nav::forward();
            }
        }
    });

    // document entry points.
    engine.register_host_fn("document", "root", || -> i64 {
        node_query::run_document().map(cd_packed_to_id).unwrap_or(0)
    });
    engine.register_host_fn("document", "query", |selector: String| -> Vec<i64> {
        node_query::run_query(&selector)
            .map(|q| q.nodes.iter().map(|&p| cd_packed_to_id(p)).collect())
            .unwrap_or_default()
    });
    engine.register_host_fn("document", "get_by_id", |id: String| -> i64 {
        node_query::run_get_by_id(&id)
            .map(cd_packed_to_id)
            .unwrap_or(0)
    });
    engine.register_host_fn("document", "focused", || -> i64 {
        node_query::focused_node().map(cd_packed_to_id).unwrap_or(0)
    });
    engine.register_host_fn("document", "hovered", || -> i64 {
        node_query::hovered_node().map(cd_packed_to_id).unwrap_or(0)
    });
    {
        let sink = r.sink.clone();
        engine.register_host_fn("document", "create", move |tag: String| -> i64 {
            let (handle, cmd) = node_query::build_spawn(&tag);
            sink.lock().unwrap().push(cmd);
            cd_intern_raw(handle)
        });
    }
}

/// Register the typed `signal_get_int` / `signal_set_int` pair.
fn register_typed_signal_int<S: HostFnSink>(engine: &mut S, r: &Registries) {
    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_get_int",
        move |name: String| -> i64 {
            match m.lock().unwrap().get(&name) {
                Some(ScriptValue::I64(i)) => *i,
                Some(ScriptValue::F64(f)) => *f as i64,
                Some(ScriptValue::Str(s)) => s.parse().unwrap_or(0),
                _ => 0,
            }
        },
    );
    let reg = r.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_set_int",
        move |name: String, value: i64| {
            reg.set_signal(&name, ScriptValue::I64(value));
        },
    );
}

/// Register the typed `signal_get_float` / `signal_set_float` pair.
fn register_typed_signal_float<S: HostFnSink>(engine: &mut S, r: &Registries) {
    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_get_float",
        move |name: String| -> f64 {
            match m.lock().unwrap().get(&name) {
                Some(ScriptValue::F64(f)) => *f,
                Some(ScriptValue::I64(i)) => *i as f64,
                Some(ScriptValue::Str(s)) => s.parse().unwrap_or(0.0),
                _ => 0.0,
            }
        },
    );
    let reg = r.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_set_float",
        move |name: String, value: f64| {
            reg.set_signal(&name, ScriptValue::F64(value));
        },
    );
}

/// Register the typed `signal_get_bool` / `signal_set_bool` pair.
fn register_typed_signal_bool<S: HostFnSink>(engine: &mut S, r: &Registries) {
    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_get_bool",
        move |name: String| -> bool {
            match m.lock().unwrap().get(&name) {
                Some(ScriptValue::Bool(b)) => *b,
                Some(ScriptValue::Str(s)) => matches!(s.as_str(), "true" | "1"),
                Some(ScriptValue::I64(i)) => *i != 0,
                _ => false,
            }
        },
    );
    let reg = r.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_set_bool",
        move |name: String, value: bool| {
            reg.set_signal(&name, ScriptValue::Bool(value));
        },
    );
}

/// Register the array-signal builtins: the reactive lists `<for each="name">`
/// renders.
///
/// Rhai and Lua hand back an `ArraySignal` handle object; candela has no
/// user-defined value type, so the surface is name-keyed free functions and the
/// prelude's `ArraySignal` struct wraps a name to give the same
/// `rows.push(item)` reading. Items are records - string-keyed maps whose
/// fields `<for>` binds by name - and a non-record item is carried as a
/// one-field `value` row, matching the other hosts.
///
/// The item-carrying entries register variadically because a record mixes
/// value types; the rest have concrete signatures. Indices are zero-based.
fn register_array_signals<S: HostFnSink>(engine: &mut S, r: &Registries) {
    // set(name, items): replace the whole array.
    let reg = r.clone();
    engine.register_host_fn_variadic(
        HOST_NAMESPACE,
        "signal_array_set",
        move |args: &[Value]| {
            let next = match arg_value(args, 1) {
                ScriptValue::Array(a) => a,
                ScriptValue::Unit => Vec::new(),
                other => vec![other],
            };
            reg.set_array(&arg_text(args, 0), next);
            Value::Null
        },
    );

    // push(name, item): append one record.
    let reg = r.clone();
    engine.register_host_fn_variadic(
        HOST_NAMESPACE,
        "signal_array_push",
        move |args: &[Value]| {
            let name = arg_text(args, 0);
            let mut next = reg.array_items(&name);
            next.push(arg_value(args, 1));
            reg.set_array(&name, next);
            Value::Null
        },
    );

    // get(name, index) -> the record, or null when out of range.
    let reg = r.clone();
    engine.register_host_fn_variadic(
        HOST_NAMESPACE,
        "signal_array_get",
        move |args: &[Value]| {
            let index = args.get(1).and_then(Value::as_i64).unwrap_or(-1);
            let Ok(index) = usize::try_from(index) else {
                return Value::Null;
            };
            reg.array_items(&arg_text(args, 0))
                .get(index)
                .map_or(Value::Null, script_value_to_candela)
        },
    );

    // all(name) -> every record, as a list.
    let reg = r.clone();
    engine.register_host_fn_variadic(
        HOST_NAMESPACE,
        "signal_array_all",
        move |args: &[Value]| {
            script_value_to_candela(&ScriptValue::Array(reg.array_items(&arg_text(args, 0))))
        },
    );

    // len(name) -> item count.
    let reg = r.clone();
    engine.register_host_fn(HOST_NAMESPACE, "signal_array_len", move |name: String| {
        reg.array_items(&name).len() as i64
    });

    // remove(name, index): drop one record; out-of-range indices no-op.
    let reg = r.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_array_remove",
        move |name: String, index: i64| {
            let mut next = reg.array_items(&name);
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            if index >= next.len() {
                return;
            }
            next.remove(index);
            reg.set_array(&name, next);
        },
    );

    // clear(name): empty the array.
    let reg = r.clone();
    engine.register_host_fn(HOST_NAMESPACE, "signal_array_clear", move |name: String| {
        reg.set_array(&name, Vec::new());
    });
}

/// Register the color-signal pair. `signal_set_color` pushes a typed
/// `Color` cell onto the property bus (so CSS-facing consumers see a color,
/// not a string) and mirrors the channels; `signal_get_color` reads them back
/// as an `{ r, g, b, a }` map of 0-255 ints. An unparseable hex string is
/// ignored, and a signal that holds no color reads as an empty map.
fn register_color_signals<S: HostFnSink>(engine: &mut S, r: &Registries) {
    use lumen_core::property_store::{PropertyKey, PropertyValue, push_external_property};

    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_set_color",
        move |name: String, hex: String| {
            let Some((red, green, blue, alpha)) = parse_hex_color(&hex) else {
                return;
            };
            m.lock().unwrap().insert(
                name.clone(),
                ScriptValue::Map(HashMap::from([
                    ("r".to_owned(), ScriptValue::I64(i64::from(red))),
                    ("g".to_owned(), ScriptValue::I64(i64::from(green))),
                    ("b".to_owned(), ScriptValue::I64(i64::from(blue))),
                    ("a".to_owned(), ScriptValue::I64(i64::from(alpha))),
                ])),
            );
            let color = lumen_core::components::Color::rgba(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
                f32::from(alpha) / 255.0,
            );
            push_external_property(
                PropertyKey::Global(Arc::<str>::from(name.as_str())),
                PropertyValue::Color(color),
            );
        },
    );

    let m = r.mirror.clone();
    engine.register_host_fn(
        HOST_NAMESPACE,
        "signal_get_color",
        move |name: String| -> HashMap<String, i64> {
            let channels = |r: u8, g: u8, b: u8, a: u8| {
                HashMap::from([
                    ("r".to_owned(), i64::from(r)),
                    ("g".to_owned(), i64::from(g)),
                    ("b".to_owned(), i64::from(b)),
                    ("a".to_owned(), i64::from(a)),
                ])
            };
            match m.lock().unwrap().get(&name) {
                Some(ScriptValue::Map(map)) => map
                    .iter()
                    .filter_map(|(k, v)| match v {
                        ScriptValue::I64(n) => Some((k.clone(), *n)),
                        _ => None,
                    })
                    .collect(),
                Some(ScriptValue::Str(s)) => parse_hex_color(s)
                    .map(|(r, g, b, a)| channels(r, g, b, a))
                    .unwrap_or_default(),
                _ => HashMap::new(),
            }
        },
    );
}

/// Register `http(request)`: one general HTTP request, mirroring the Rhai and
/// Lua form. Only `url` and `tag` are required; `method` defaults to `GET`, and
/// `body`, `timeout_ms`, and headers are optional. The reply lands on
/// `on_http(tag, response)`.
///
/// It registers variadically because the request carries a map, which a fixed
/// signature cannot name alongside the rest of the surface.
///
/// A candela map literal holds one value type, so the request a script writes
/// by hand is a flat string map and each header rides on a `header:<Name>` key:
///
/// ```candela
/// lumen::http({
///     "method": "POST",
///     "url": "https://example.test/items",
///     "header:Accept": "application/json",
///     "timeout_ms": "2500",
///     "tag": "items"
/// });
/// ```
///
/// A request that did not come from a literal, one built by `parse_json` for
/// instance, may instead carry a nested `headers` map and an int `timeout_ms`,
/// which is the shape the Rhai and Lua hosts take. Both are accepted.
fn register_http<S: HostFnSink>(engine: &mut S, r: &Registries) {
    /// Header name prefix for the flat form.
    const HEADER_PREFIX: &str = "header:";

    let sink = r.sink.clone();
    engine.register_host_fn_variadic(HOST_NAMESPACE, "http", move |args: &[Value]| {
        let Some(Value::Map(req)) = args.first() else {
            return Value::Null;
        };
        let text = |v: &Value| candela_value_to_script(v).stringify();
        let field = |key: &str| req.get(key).map(&text);

        // Headers: the nested `headers` map first, then the flat
        // `header:<Name>` keys, so a request can use either or both.
        let mut headers: Vec<(String, String)> = match req.get("headers") {
            Some(Value::Map(m)) => m.iter().map(|(k, v)| (k.clone(), text(v))).collect(),
            _ => Vec::new(),
        };
        headers.extend(req.iter().filter_map(|(k, v)| {
            k.strip_prefix(HEADER_PREFIX)
                .map(|name| (name.to_owned(), text(v)))
        }));

        sink.lock().unwrap().push(ScriptCommand::Http {
            method: field("method")
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "GET".to_owned()),
            url: field("url").unwrap_or_default(),
            headers,
            body: match req.get("body") {
                Some(Value::Null) | None => None,
                Some(v) => Some(text(v)),
            },
            // Accepts an int or a numeric string; anything else, and any
            // non-positive value, means no client-imposed deadline.
            timeout_ms: req
                .get("timeout_ms")
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                })
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0),
            tag: field("tag").unwrap_or_default(),
        });
        Value::Null
    });
}

/// Parse a `"#rrggbb"` or `"#rrggbbaa"` hex color into RGBA bytes. The leading
/// `#` is optional. `None` when the input matches neither shape.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8, u8)> {
    let s = s.strip_prefix('#').unwrap_or(s);
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(s.get(range)?, 16).ok();
    match s.len() {
        6 => Some((channel(0..2)?, channel(2..4)?, channel(4..6)?, 0xff)),
        8 => Some((
            channel(0..2)?,
            channel(2..4)?,
            channel(4..6)?,
            channel(6..8)?,
        )),
        _ => None,
    }
}

/// Register the `audio_*` transport builtins, each a thin enqueue onto the
/// shared command sink.
fn register_audio<S: HostFnSink>(engine: &mut S, r: &Registries) {
    enqueue!(engine, r.sink, "audio_play", |path: String| {
        ScriptCommand::AudioPlay { path }
    });
    enqueue!(engine, r.sink, "audio_pause", || ScriptCommand::AudioPause);
    enqueue!(engine, r.sink, "audio_resume", || {
        ScriptCommand::AudioResume
    });
    enqueue!(engine, r.sink, "audio_stop", || ScriptCommand::AudioStop);
    enqueue!(engine, r.sink, "audio_seek", |secs: f64| {
        ScriptCommand::AudioSeek { secs }
    });
    enqueue!(engine, r.sink, "audio_volume", |level: f64| {
        ScriptCommand::AudioVolume {
            level: level as f32,
        }
    });
}

/// The signal a menu's open state lives on.
fn menu_signal(id: &str) -> String {
    format!("__menu_open:{id}")
}

/// Build an unfiltered [`ScriptCommand::OpenFileDialog`].
fn file_dialog(kind: FileDialogKind, tag: String, default_name: Option<String>) -> ScriptCommand {
    ScriptCommand::OpenFileDialog {
        kind,
        tag,
        filters: Vec::new(),
        default_name,
    }
}

/// Parse a `Label:ext1,ext2|All:*` filter spec into `(label, extensions)`
/// pairs. `*` extensions are dropped (they mean "no filter").
fn parse_filter_spec(spec: &str) -> Vec<(String, Vec<String>)> {
    spec.split('|')
        .filter_map(|group| {
            let (label, exts) = group.split_once(':')?;
            let exts: Vec<String> = exts
                .split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty() && *e != "*")
                .map(str::to_owned)
                .collect();
            Some((label.trim().to_owned(), exts))
        })
        .collect()
}
