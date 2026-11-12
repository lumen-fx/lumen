//! The operations every surface reaches: the `commands` script namespace,
//! the C ABI through `lumen_call`, and the [`Commands`] system parameter a
//! Rust app uses. One implementation; each surface is a thin call into it.

use std::sync::Arc;

use bevy_ecs::system::{Res, SystemParam};
use lumen_module::lumen_core::chord::Chord;
use lumen_module::lumen_script::{ScriptFn, ScriptFnBody, ScriptFnCx, ScriptValue};

use crate::model::{Command, CommandError, Core, PaletteAsk, Shared, chords};

/// Register `id` from a call: a command made at run time.
fn register(core: &mut Core, id: &str, label: &str, keys: &str) -> Result<(), CommandError> {
    let defaults = chords(keys)?;
    let label = if label.is_empty() { id } else { label };
    core.register(Command {
        id: id.to_string(),
        label: label.to_string(),
        label_key: None,
        defaults,
        enabled: true,
        from_markup: false,
    })
}

/// Report a failed call on the console and hand back `fallback`.
fn report<T>(name: &str, r: Result<T, CommandError>, fallback: T) -> T {
    r.unwrap_or_else(|e| {
        lumen_module::lumen_core::warn_line!("commands::{name}: {e}");
        fallback
    })
}

type Op = Box<dyn Fn(&mut Core, &ScriptFnCx<'_>) -> ScriptValue + Send + Sync>;

/// The `commands` namespace's functions, bound to `shared`.
///
/// # Errors
///
/// The bodies here and the functions the web half declares disagree.
pub fn script_fns(shared: &Shared) -> Result<Vec<ScriptFn>, String> {
    let s = shared.clone();
    let body = move |f: Op| -> ScriptFnBody {
        let s = s.clone();
        Arc::new(move |cx| {
            let mut core = s.lock();
            Ok(f(&mut core, cx))
        })
    };
    let b = ScriptValue::Bool;
    let bodies: Vec<(String, ScriptFnBody)> = vec![
        (
            "register".into(),
            body(Box::new(move |c, cx| {
                let r = register(c, &cx.str_arg(0), &cx.str_arg(1), &cx.str_arg(2));
                b(report("register", r.map(|()| true), false))
            })),
        ),
        (
            "unregister".into(),
            body(Box::new(move |c, cx| b(c.unregister(&cx.str_arg(0))))),
        ),
        (
            "run".into(),
            body(Box::new(move |c, cx| b(c.run(&cx.str_arg(0))))),
        ),
        (
            "set_enabled".into(),
            body(Box::new(move |c, cx| {
                let r = c.set_enabled(&cx.str_arg(0), cx.bool_arg(1));
                b(report("set_enabled", r.map(|()| true), false))
            })),
        ),
        (
            "keys".into(),
            body(Box::new(|c, cx| {
                ScriptValue::Str(c.keys_text(&cx.str_arg(0)))
            })),
        ),
        (
            "bind".into(),
            body(Box::new(move |c, cx| {
                let id = cx.str_arg(0);
                let r = chords(&cx.str_arg(1)).and_then(|k| c.bind(&id, k));
                b(report("bind", r.map(|()| true), false))
            })),
        ),
        (
            "reset".into(),
            body(Box::new(move |c, cx| b(c.reset(&cx.str_arg(0))))),
        ),
        (
            "record".into(),
            body(Box::new(move |c, cx| {
                let r = c.record(&cx.str_arg(0));
                b(report("record", r.map(|()| true), false))
            })),
        ),
        (
            "open_palette".into(),
            body(Box::new(|c, _| {
                c.palette = Some(PaletteAsk::Open);
                ScriptValue::Unit
            })),
        ),
    ];
    lumen_module::web_half_fns(crate::plugin::NAME, crate::plugin::DESCRIPTOR, bodies)
}

/// The command registry from a Rust system: the same operations the
/// `commands` script namespace has.
///
/// ```ignore
/// fn setup(commands: lumenui_commands::Commands) {
///     commands.register("view.zoom_in", "Zoom in", "Ctrl+=").ok();
/// }
/// ```
///
/// A command runs as a `command` event on the document; a Rust app hears it
/// with `App::on(EventKind::Custom("command"), ...)` or reads
/// [`CommandRun`](crate::CommandRun) messages.
#[derive(SystemParam)]
pub struct Commands<'w> {
    shared: Res<'w, Shared>,
}

impl Commands<'_> {
    /// Add a command made at run time, or replace the one with the same id.
    /// `keys` is a chord or a comma-separated list (`"Ctrl+S, F2"`).
    ///
    /// # Errors
    ///
    /// An empty id, or a chord that does not read.
    pub fn register(&self, id: &str, label: &str, keys: &str) -> Result<(), CommandError> {
        register(&mut self.shared.lock(), id, label, keys)
    }

    /// Drop the command `id`; whether there was one.
    pub fn unregister(&self, id: &str) -> bool {
        self.shared.lock().unregister(id)
    }

    /// Run `id` on the next tick; whether it will (it exists and is
    /// enabled).
    pub fn run(&self, id: &str) -> bool {
        self.shared.lock().run(id)
    }

    /// Turn `id` on or off.
    ///
    /// # Errors
    ///
    /// No command has the id.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CommandError> {
        self.shared.lock().set_enabled(id, enabled)
    }

    /// Whether `id` is a command that runs.
    pub fn is_enabled(&self, id: &str) -> bool {
        self.shared.lock().get(id).is_some_and(|c| c.enabled)
    }

    /// The chords that run `id` now.
    pub fn keys(&self, id: &str) -> Vec<Chord> {
        self.shared.lock().keys(id)
    }

    /// Bind `id` to `keys` in place of what it had; a chord another command
    /// has moves to this one. Kept between runs with `persist = "auto"`.
    ///
    /// # Errors
    ///
    /// No command has the id.
    pub fn bind(&self, id: &str, keys: Vec<Chord>) -> Result<(), CommandError> {
        self.shared.lock().bind(id, keys)
    }

    /// Give `id` its declared chords back; whether the user had bound it.
    pub fn reset(&self, id: &str) -> bool {
        self.shared.lock().reset(id)
    }

    /// Bind the next chord the user presses to `id`; Escape cancels.
    ///
    /// # Errors
    ///
    /// No command has the id.
    pub fn record(&self, id: &str) -> Result<(), CommandError> {
        self.shared.lock().record(id)
    }

    /// Open the command palette.
    pub fn open_palette(&self) {
        self.shared.lock().palette = Some(PaletteAsk::Open);
    }

    /// Every command id, in the order they were declared.
    pub fn ids(&self) -> Vec<String> {
        self.shared
            .lock()
            .commands
            .iter()
            .map(|c| c.id.clone())
            .collect()
    }

    /// The label of `id` in the app's language.
    pub fn label(&self, id: &str) -> Option<String> {
        self.shared.lock().get(id).map(Command::label)
    }
}
