//! The plugin that puts the filesystem into an app: the `files` script
//! namespace, and nothing else.
//!
//! The engine has no file surface of its own; everything an app observes
//! comes from here, through the one generic seam a plugin needs: the
//! functions register on the app's `ScriptFnRegistry`, so every host (Rhai,
//! Lua, candela) binds them before the program loads. There are no systems,
//! no resources, and no events, because a file call answers in place: the
//! body does the work and hands the value straight back to the script.
//!
//! Two rules shape the surface:
//!
//! - **Paths are the app's.** Every path goes through the runtime's own
//!   resolution, so a relative path names a file beside the app wherever the
//!   app was started from, and an absolute path is left alone. Saved state
//!   belongs under `files::data_dir()`, which is writable after install where
//!   the app directory is not.
//! - **A refusal degrades, it does not raise.** A call that cannot do what it
//!   was asked answers false, or empty, and explains itself in one `lumen-fs:`
//!   line on stderr. A script branches on what it got back instead of
//!   catching, which is the shape every host can write.

use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::warn_line;
use lumen_module::lumen_script::{ScriptFn, ScriptFnAppExt, ScriptNs, ScriptTy as T, ScriptValue};

use crate::ops;

/// The namespace the functions live in: `files::read(..)` in Rhai and candela,
/// `files.read(..)` in Lua.
const NAMESPACE: &str = "files";

/// How much of a file `files::read_bytes` hands back by default.
pub const DEFAULT_READ_BYTES_CAP: u64 = 8 * 1024 * 1024;

/// The smallest cap an app can ask for. Below this the call is useless rather
/// than careful.
pub const MIN_READ_BYTES_CAP: u64 = 1024;

/// The largest cap an app can ask for. A script value holds one integer per
/// byte, so the ceiling is what stops a single call from exhausting memory.
pub const MAX_READ_BYTES_CAP: u64 = 256 * 1024 * 1024;

/// The filesystem for a Lumen app: install it and the `files` functions exist.
///
/// Ships as the bundled `lumen-fs` runtime module (an app declares
/// `lumen-fs = { bundled = true }` under `[dependencies]`), and works the same
/// added as an ordinary plugin in a static build. Without it the functions do
/// not exist and a script call fails with the host's ordinary
/// unknown-function error.
pub struct FsPlugin {
    read_bytes_cap: u64,
}

impl FsPlugin {
    /// Build from the module's `config` table. `read_bytes_cap` is an integer
    /// number of bytes, clamped into the range the module supports; anything
    /// else leaves the default in place.
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        match config.int("read_bytes_cap") {
            Some(cap) => Self::with_read_bytes_cap(cap),
            None => Self::default(),
        }
    }

    /// Build with an explicit `files::read_bytes` cap, in bytes, clamped into
    /// the supported range. This is what a static build sets when it installs
    /// the plugin itself and has no `config` table to read.
    #[must_use]
    pub fn with_read_bytes_cap(cap: i64) -> Self {
        let cap = u64::try_from(cap).unwrap_or(MIN_READ_BYTES_CAP);
        Self {
            read_bytes_cap: cap.clamp(MIN_READ_BYTES_CAP, MAX_READ_BYTES_CAP),
        }
    }
}

impl Default for FsPlugin {
    fn default() -> Self {
        Self {
            read_bytes_cap: DEFAULT_READ_BYTES_CAP,
        }
    }
}

impl Plugin for FsPlugin {
    fn build(self, app: &mut App) {
        app.add_script_fns(script_fns(self.read_bytes_cap));
    }
}

/// Report a refusal and answer with what the script sees instead.
fn degrade<T>(outcome: ops::Outcome<T>, fallback: T) -> T {
    match outcome {
        Ok(value) => value,
        Err(message) => {
            warn_line!("lumen-fs: {message}");
            fallback
        }
    }
}

/// The `files` surface, described once for every host. Names, parameters, and
/// docs are the contract a script writes against.
fn script_fns(read_bytes_cap: u64) -> Vec<ScriptFn> {
    let f = |name: &str, doc: &str| {
        ScriptFn::new(name)
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(doc)
    };
    vec![
        f("exists", "Whether anything exists at that path.")
            .param("path", T::Str)
            .ret(T::Bool)
            .build(|cx| Ok(ScriptValue::Bool(ops::exists(&resolve(cx.str_arg(0)))))),
        f("is_dir", "Whether that path is a directory.")
            .param("path", T::Str)
            .ret(T::Bool)
            .build(|cx| Ok(ScriptValue::Bool(ops::is_dir(&resolve(cx.str_arg(0)))))),
        f(
            "list",
            "The names of the entries in that directory, sorted.",
        )
        .param("path", T::Str)
        .ret(T::Array(Box::new(T::Str)))
        .build(|cx| {
            let names = degrade(ops::list(&resolve(cx.str_arg(0))), Vec::new());
            Ok(ScriptValue::Array(
                names.into_iter().map(ScriptValue::Str).collect(),
            ))
        }),
        f(
            "mkdir",
            "Create that directory and any above it; true when it is there.",
        )
        .param("path", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            Ok(ScriptValue::Bool(degrade(
                ops::mkdir(&resolve(cx.str_arg(0))),
                false,
            )))
        }),
        f(
            "remove",
            "Remove that file, or that directory when it is empty.",
        )
        .param("path", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            Ok(ScriptValue::Bool(degrade(
                ops::remove(&resolve(cx.str_arg(0))),
                false,
            )))
        }),
        f("copy", "Copy that file to that destination.")
            .param("src", T::Str)
            .param("dest", T::Str)
            .ret(T::Bool)
            .build(|cx| {
                let (src, dest) = (resolve(cx.str_arg(0)), resolve(cx.str_arg(1)));
                Ok(ScriptValue::Bool(degrade(ops::copy(&src, &dest), false)))
            }),
        f("read", "The contents of that file, or an empty string.")
            .param("path", T::Str)
            .ret(T::Str)
            .build(|cx| {
                let text = degrade(ops::read(&resolve(cx.str_arg(0))), String::new());
                Ok(ScriptValue::Str(text))
            }),
        f(
            "write",
            "Write contents to that path; false when the write failed.",
        )
        .param("path", T::Str)
        .param("contents", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            let path = resolve(cx.str_arg(0));
            Ok(ScriptValue::Bool(degrade(
                ops::write(&path, &cx.str_arg(1)),
                false,
            )))
        }),
        f("read_bytes", "The bytes of that file, as integers.")
            .param("path", T::Str)
            .ret(T::Array(Box::new(T::Int)))
            .build(move |cx| {
                let bytes = degrade(
                    ops::read_bytes(&resolve(cx.str_arg(0)), read_bytes_cap),
                    Vec::new(),
                );
                Ok(ScriptValue::Array(
                    bytes
                        .into_iter()
                        .map(|b| ScriptValue::I64(i64::from(b)))
                        .collect(),
                ))
            }),
        f("write_bytes", "Write those bytes to that path.")
            .param("path", T::Str)
            .param("bytes", T::Array(Box::new(T::Int)))
            .ret(T::Bool)
            .build(|cx| {
                let path = resolve(cx.str_arg(0));
                let values: Vec<i64> = match cx.arg_ref(1) {
                    ScriptValue::Array(items) => items.iter().map(int_of).collect(),
                    _ => Vec::new(),
                };
                Ok(ScriptValue::Bool(degrade(
                    ops::write_bytes(&path, &values),
                    false,
                )))
            }),
        f(
            "data_dir",
            "The directory this app saves data in, created if missing.",
        )
        .ret(T::Str)
        .build(|_| {
            Ok(ScriptValue::Str(
                app_paths::data_dir().to_string_lossy().into_owned(),
            ))
        }),
    ]
}

/// A path an app author wrote, against the app: relative names a file beside
/// the app, absolute is left alone.
fn resolve(path: String) -> std::path::PathBuf {
    app_paths::resolve(path)
}

/// One array element as an integer, with the coercions every host argument
/// gets. A value that is no number at all lands outside the byte range, which
/// is what `write_bytes` refuses on.
fn int_of(value: &ScriptValue) -> i64 {
    match value {
        ScriptValue::I64(v) => *v,
        ScriptValue::F64(v) => *v as i64,
        ScriptValue::Bool(b) => i64::from(*b),
        ScriptValue::Str(s) => s.trim().parse().unwrap_or(-1),
        _ => -1,
    }
}
