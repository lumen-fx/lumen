//! The builtin surface every script host shares, described once.
//!
//! A builtin whose whole body is "push one [`ScriptCommand`]" or "read one
//! process-global and return it" says nothing about the language calling it, so
//! it lives here as a [`ScriptFn`] rather than three times over in the host
//! crates. Each host binds this table through its own
//! [`ScriptHost::register_script_fn`](crate::ScriptHost::register_script_fn)
//! when it is constructed, filtered by the entry's [`HostSet`], so a bare host
//! in a test, in `lumenc check`, or in a server render carries the same
//! builtins a windowed app does.
//!
//! What stays in a host crate is what a host cannot share: the signal mirror
//! and its handle types, closure registries (`on`, `derive`, event handlers),
//! anything whose return shape differs per language, and the receiver-method
//! surfaces (`node.set_text(..)`) each engine spells its own way.

use crate::ScriptValue;
use crate::{
    FileDialogKind, HostSet, ScriptCommand, ScriptFn, ScriptFnCx, ScriptNs, ScriptTy as T,
};
use lumen_core::warn_line;

/// Describe a builtin whose whole effect is one queued [`ScriptCommand`].
fn emit<F>(name: &str, doc: &str, params: &[(&str, T)], build: F) -> ScriptFn
where
    F: Fn(&ScriptFnCx<'_>) -> ScriptCommand + Send + Sync + 'static,
{
    let mut f = ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .ret(T::Unit)
        .doc(doc);
    for (pname, ty) in params {
        f = f.param(*pname, ty.clone());
    }
    f.build(move |cx| {
        let cmd = build(cx);
        cx.emit(cmd);
        Ok(ScriptValue::Unit)
    })
}

/// Describe a builtin that returns a value and queues nothing.
fn value<F>(name: &str, doc: &str, params: &[(&str, T)], ret: T, body: F) -> ScriptFn
where
    F: Fn(&ScriptFnCx<'_>) -> ScriptValue + Send + Sync + 'static,
{
    let mut f = ScriptFn::new(name).ns(ScriptNs::Builtin).ret(ret).doc(doc);
    for (pname, ty) in params {
        f = f.param(*pname, ty.clone());
    }
    f.build(move |cx| Ok(body(cx)))
}

/// The whole shared builtin table, in registration order.
///
/// Every entry carries [`ScriptNs::Builtin`], so it lands in the host's global
/// namespace on Rhai and Lua and in candela's `lumen` namespace. An app's own
/// registration of the same name arrives later and shadows it.
pub fn builtin_script_fns() -> Vec<ScriptFn> {
    let mut fns = Vec::new();
    fns.extend(state_fns());
    fns.extend(timer_fns());
    fns.extend(os_fns());
    fns.extend(dialog_fns());
    fns.extend(navigation_fns());
    fns.extend(request_fns());
    fns.extend(audio_fns());
    fns.extend(misc_fns());
    fns.extend(crate::node_fns::node_script_fns());
    fns
}

/// Element and signal writes that address their target by id.
fn state_fns() -> Vec<ScriptFn> {
    vec![
        emit(
            "add_clicks",
            "Add to the demo click counter.",
            &[("n", T::Int)],
            |cx| ScriptCommand::AddClicks(cx.int_arg(0) as i32),
        ),
        emit(
            "set_string",
            "Write a string into the app's key-value state.",
            &[("key", T::Str), ("value", T::Str)],
            |cx| ScriptCommand::SetString {
                key: cx.str_arg(0),
                value: cx.str_arg(1),
            },
        ),
        emit(
            "set_text",
            "Replace the text content of the element with that id.",
            &[("target_id", T::Str), ("text", T::Str)],
            |cx| ScriptCommand::SetText {
                target_id: cx.str_arg(0),
                text: cx.str_arg(1),
            },
        ),
        // The runtime strips the old asset and queues a fresh decode. The path
        // is taken verbatim and resolved against the app directory by the
        // applier, so scripts pass app-relative paths like "icons/sun.png".
        emit(
            "set_src",
            "Swap the asset path of an <image> at run time.",
            &[("target_id", T::Str), ("path", T::Str)],
            |cx| ScriptCommand::SetSrc {
                target_id: cx.str_arg(0),
                path: cx.str_arg(1),
            },
        ),
        // The runtime detects a changed class list on the root and re-applies
        // CSS, so theme-token selectors light up live.
        emit(
            "set_class",
            "Replace the class list of the element with that id.",
            &[("id", T::Str), ("classes", T::Str)],
            |cx| ScriptCommand::SetClasses {
                target_id: cx.str_arg(0),
                classes: cx.str_arg(1),
            },
        ),
        emit(
            "set_root_class",
            "Replace the class list of the root element.",
            &[("classes", T::Str)],
            |cx| ScriptCommand::SetClasses {
                target_id: "<root>".to_string(),
                classes: cx.str_arg(0),
            },
        ),
        emit(
            "set_color_scheme",
            "Apply a color scheme: default, force-light, force-dark, \
             prefer-light, or prefer-dark.",
            &[("name", T::Str)],
            |cx| ScriptCommand::SetColorScheme {
                name: cx.str_arg(0),
            },
        ),
        // Menus are modeled as a reserved `__menu_open:<id>` signal the markup
        // binds its open state to.
        emit(
            "open_menu",
            "Open the menu with that id.",
            &[("id", T::Str)],
            |cx| ScriptCommand::SetSignal {
                name: menu_signal(&cx.str_arg(0)),
                value: "true".to_string(),
            },
        ),
        emit(
            "close_menu",
            "Close the menu with that id.",
            &[("id", T::Str)],
            |cx| ScriptCommand::SetSignal {
                name: menu_signal(&cx.str_arg(0)),
                value: "false".to_string(),
            },
        ),
    ]
}

/// The reserved signal name carrying a menu's open state.
fn menu_signal(id: &str) -> String {
    format!("__menu_open:{id}")
}

/// Timers. Both fire `on_timer(name)`; the interval keeps firing until
/// `cancel_timer`.
fn timer_fns() -> Vec<ScriptFn> {
    vec![
        emit(
            "set_timeout",
            "Fire on_timer(name) once, after that many milliseconds.",
            &[("name", T::Str), ("ms", T::Int)],
            |cx| ScriptCommand::SetTimer {
                name: cx.str_arg(0),
                millis: cx.int_arg(1).max(0) as u64,
                repeat: false,
            },
        ),
        emit(
            "set_interval",
            "Fire on_timer(name) every that many milliseconds.",
            &[("name", T::Str), ("ms", T::Int)],
            |cx| ScriptCommand::SetTimer {
                name: cx.str_arg(0),
                millis: cx.int_arg(1).max(0) as u64,
                repeat: true,
            },
        ),
        emit(
            "cancel_timer",
            "Stop the timer registered under that name.",
            &[("name", T::Str)],
            |cx| ScriptCommand::CancelTimer {
                name: cx.str_arg(0),
            },
        ),
    ]
}

/// Notifications, clipboard, tray, shell, power, and global hotkeys.
fn os_fns() -> Vec<ScriptFn> {
    vec![
        emit(
            "notify",
            "Post a desktop notification.",
            &[("title", T::Str), ("body", T::Str)],
            |cx| ScriptCommand::Notify {
                title: cx.str_arg(0),
                body: cx.str_arg(1),
            },
        ),
        emit(
            "notify_ex",
            "Post a desktop notification with an id, options, and actions.",
            &[
                ("id", T::Str),
                ("title", T::Str),
                ("body", T::Str),
                ("options", T::Str),
                ("actions", T::Str),
            ],
            |cx| ScriptCommand::NotifyEx {
                id: cx.str_arg(0),
                title: cx.str_arg(1),
                body: cx.str_arg(2),
                options: cx.str_arg(3),
                actions: cx.str_arg(4),
            },
        ),
        emit(
            "clipboard_write",
            "Put text on the system clipboard.",
            &[("text", T::Str)],
            |cx| ScriptCommand::ClipboardWrite {
                text: cx.str_arg(0),
            },
        ),
        emit(
            "clipboard_read",
            "Read the clipboard; delivers to on_clipboard(tag, text).",
            &[("tag", T::Str)],
            |cx| ScriptCommand::ClipboardRead { tag: cx.str_arg(0) },
        ),
        emit(
            "copy_image",
            "Put the image at that path on the clipboard.",
            &[("path", T::Str)],
            |cx| ScriptCommand::CopyImageToClipboard {
                path: cx.str_arg(0),
            },
        ),
        emit(
            "save_clipboard_image",
            "Write the clipboard image to that path.",
            &[("path", T::Str)],
            |cx| ScriptCommand::SaveClipboardImage {
                path: cx.str_arg(0),
            },
        ),
        emit(
            "tray_icon",
            "Register a tray icon.",
            &[("id", T::Str), ("icon_path", T::Str), ("tooltip", T::Str)],
            |cx| ScriptCommand::RegisterTrayIcon {
                id: cx.str_arg(0),
                icon_path: cx.str_arg(1),
                tooltip: non_empty(cx.str_arg(2)),
                menu: String::new(),
                template: false,
            },
        ),
        emit(
            "tray_icon_menu",
            "Register a tray icon carrying a menu.",
            &[
                ("id", T::Str),
                ("icon_path", T::Str),
                ("tooltip", T::Str),
                ("menu", T::Str),
                ("template", T::Bool),
            ],
            |cx| ScriptCommand::RegisterTrayIcon {
                id: cx.str_arg(0),
                icon_path: cx.str_arg(1),
                tooltip: non_empty(cx.str_arg(2)),
                menu: cx.str_arg(3),
                template: cx.bool_arg(4),
            },
        ),
        emit(
            "unregister_tray",
            "Remove the tray icon with that id.",
            &[("id", T::Str)],
            |cx| ScriptCommand::UnregisterTrayIcon { id: cx.str_arg(0) },
        ),
        emit(
            "open_url",
            "Open a URL in the system browser.",
            &[("url", T::Str)],
            |cx| ScriptCommand::OpenUrl { url: cx.str_arg(0) },
        ),
        emit(
            "open_path",
            "Open a path with its default application.",
            &[("path", T::Str)],
            |cx| ScriptCommand::OpenPath {
                path: cx.str_arg(0),
            },
        ),
        emit(
            "reveal_path",
            "Show a path in the system file manager.",
            &[("path", T::Str)],
            |cx| ScriptCommand::RevealPath {
                path: cx.str_arg(0),
            },
        ),
        emit(
            "keep_awake",
            "Hold a wake lock under that name until allow_sleep.",
            &[("name", T::Str), ("reason", T::Str)],
            |cx| ScriptCommand::KeepAwake {
                name: cx.str_arg(0),
                reason: cx.str_arg(1),
            },
        ),
        emit(
            "allow_sleep",
            "Release the wake lock held under that name.",
            &[("name", T::Str)],
            |cx| ScriptCommand::AllowSleep {
                name: cx.str_arg(0),
            },
        ),
        // Accelerator syntax follows the global-hotkey conventions:
        // "CommandOrControl+S", "Alt+Space", "F11".
        emit(
            "register_hotkey",
            "Bind an OS-level global accelerator; fires on_hotkey(name).",
            &[("name", T::Str), ("accelerator", T::Str)],
            |cx| ScriptCommand::RegisterHotkey {
                name: cx.str_arg(0),
                accelerator: cx.str_arg(1),
            },
        ),
        emit(
            "unregister_hotkey",
            "Release the global accelerator bound under that name.",
            &[("name", T::Str)],
            |cx| ScriptCommand::UnregisterHotkey {
                name: cx.str_arg(0),
            },
        ),
    ]
}

/// `Some(text)` unless it is empty.
fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// Native file dialogs. The runtime opens the dialog on the main thread and
/// fires `on_file_picked` / `on_files_picked` / `on_folder_picked` once the
/// user closes it; a cancelled dialog still fires once, with an empty path.
fn dialog_fns() -> Vec<ScriptFn> {
    let mut fns: Vec<ScriptFn> = [
        ("pick_file", FileDialogKind::Open, "Pick one file."),
        (
            "pick_files",
            FileDialogKind::OpenMulti,
            "Pick several files.",
        ),
        ("pick_folder", FileDialogKind::PickFolder, "Pick a folder."),
    ]
    .into_iter()
    .map(|(name, kind, doc)| {
        emit(name, doc, &[("tag", T::Str)], move |cx| {
            ScriptCommand::OpenFileDialog {
                kind,
                tag: cx.str_arg(0),
                filters: Vec::new(),
                default_name: None,
            }
        })
    })
    .collect();
    fns.push(emit(
        "save_file",
        "Ask for a save destination, offering that file name.",
        &[("tag", T::Str), ("default_name", T::Str)],
        |cx| ScriptCommand::OpenFileDialog {
            kind: FileDialogKind::Save,
            tag: cx.str_arg(0),
            filters: Vec::new(),
            default_name: Some(cx.str_arg(1)),
        },
    ));
    fns.push(emit(
        "pick_file_filtered",
        "Pick one file, restricted to a filter spec like \
         \"Images:png,jpg|All:*\".",
        &[("tag", T::Str), ("spec", T::Str)],
        |cx| ScriptCommand::OpenFileDialog {
            kind: FileDialogKind::Open,
            tag: cx.str_arg(0),
            filters: parse_dialog_filter_spec(&cx.str_arg(1)),
            default_name: None,
        },
    ));
    fns
}

/// Parse a `pick_file_filtered` spec like `"Images:png,jpg|All:*"` into the
/// `(label, [extensions])` list the dialog backend takes. A group with no `:`
/// is a label with no extensions, and a literal `*` extension is dropped (no
/// extension filter means "all files").
pub fn parse_dialog_filter_spec(spec: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for group in spec.split('|') {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let (label, exts) = match group.split_once(':') {
            Some((l, e)) => (l.trim().to_string(), e),
            None => (group.to_string(), ""),
        };
        let exts: Vec<String> = exts
            .split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty() && e != "*")
            .collect();
        out.push((label, exts));
    }
    out
}

/// File-based pages. Every entry rides the `lumen_core::nav` bus, the one an
/// `<a href>` click, the C ABI, and the Rust SDK write.
///
/// The shapes differ by language and so do the entries. Rhai and Lua resolve
/// `page()` with no argument as the reader and take the boolean a history step
/// reports; a candela host function is neither arity-overloaded nor allowed to
/// return a value its declaration does not name, so candela gets the
/// single-argument writer, the separate `page_current` reader, and unit-valued
/// steps.
fn navigation_fns() -> Vec<ScriptFn> {
    let read_or_navigate = |cx: &ScriptFnCx<'_>| match cx.arg(0) {
        ScriptValue::Unit => ScriptValue::Str(lumen_core::nav::current()),
        path => {
            lumen_core::nav::navigate(path.stringify());
            ScriptValue::Unit
        }
    };
    vec![
        ScriptFn::new("page")
            .ns(ScriptNs::Builtin)
            .param("path", T::Str)
            .min_arity(0)
            .doc("Navigate to a page, or read the current one when called with no argument.")
            .hosts(HostSet::RHAI | HostSet::LUA)
            .build(move |cx| Ok(read_or_navigate(cx))),
        ScriptFn::new("page")
            .ns(ScriptNs::Builtin)
            .param("path", T::Str)
            .ret(T::Unit)
            .doc("Navigate to a page.")
            .hosts(HostSet::CANDELA)
            .build(move |cx| Ok(read_or_navigate(cx))),
        value(
            "page_current",
            "The page the app is on.",
            &[],
            T::Str,
            |_| ScriptValue::Str(lumen_core::nav::current()),
        ),
        step(
            "page_back",
            "Step back through the page history.",
            lumen_core::nav::back,
        ),
        step(
            "page_forward",
            "Step forward through the page history.",
            lumen_core::nav::forward,
        ),
    ]
    .into_iter()
    .chain(
        [
            ("page_back", "Step back through the page history."),
            ("page_forward", "Step forward through the page history."),
        ]
        .into_iter()
        .map(|(name, doc)| {
            let forward = name == "page_forward";
            ScriptFn::new(name)
                .ns(ScriptNs::Builtin)
                .ret(T::Unit)
                .doc(doc)
                .hosts(HostSet::CANDELA)
                .build(move |_| {
                    if forward {
                        lumen_core::nav::forward();
                    } else {
                        lumen_core::nav::back();
                    }
                    Ok(ScriptValue::Unit)
                })
        }),
    )
    .collect()
}

/// A history step for the hosts that read its result: it reports whether the
/// request reached the navigation bus, so a script can branch on it.
fn step<F>(name: &str, doc: &str, go: F) -> ScriptFn
where
    F: Fn() -> bool + Send + Sync + 'static,
{
    ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .ret(T::Bool)
        .doc(doc)
        .hosts(HostSet::RHAI | HostSet::LUA)
        .build(move |_| Ok(ScriptValue::Bool(go())))
}

/// The request being rendered for, and the response being built.
///
/// The headers, the cookies, and the body are too large to publish as signals,
/// so they stay in the per-thread request context and a script asks for one
/// part at a time. Outside a server render nothing is installed, every reader
/// gives back an empty string, and every response command is drained and
/// dropped.
fn request_fns() -> Vec<ScriptFn> {
    vec![
        value(
            "request_header",
            "The value of that request header, or an empty string.",
            &[("name", T::Str)],
            T::Str,
            |cx| ScriptValue::Str(lumen_core::request::header(&cx.str_arg(0))),
        ),
        value(
            "request_cookie",
            "The value of that request cookie, or an empty string.",
            &[("name", T::Str)],
            T::Str,
            |cx| ScriptValue::Str(lumen_core::request::cookie(&cx.str_arg(0))),
        ),
        value(
            "request_body",
            "The request body, or an empty string.",
            &[],
            T::Str,
            |_| ScriptValue::Str(lumen_core::request::body()),
        ),
        emit(
            "response_status",
            "Answer with that HTTP status.",
            &[("status", T::Int)],
            |cx| ScriptCommand::SetResponseStatus {
                status: cx.int_arg(0).clamp(100, 599) as u16,
            },
        ),
        emit(
            "response_header",
            "Set a header on the response.",
            &[("name", T::Str), ("value", T::Str)],
            |cx| ScriptCommand::SetResponseHeader {
                name: cx.str_arg(0),
                value: cx.str_arg(1),
            },
        ),
        emit(
            "redirect",
            "Answer with a redirect to that location.",
            &[("location", T::Str)],
            |cx| ScriptCommand::Redirect {
                location: cx.str_arg(0),
            },
        ),
        emit(
            "fetch",
            "Request a URL; delivers to on_fetch(tag, body).",
            &[("url", T::Str), ("tag", T::Str)],
            |cx| ScriptCommand::Fetch {
                url: cx.str_arg(0),
                tag: cx.str_arg(1),
            },
        ),
    ]
}

/// Audio transport. Each entry queues a command the embedder's applier runs
/// against the audio service, so every binding drives the same seam. The
/// read-backs (position, duration, playing) are signals, not builtins.
fn audio_fns() -> Vec<ScriptFn> {
    vec![
        emit(
            "audio_play",
            "Play the audio file at that path.",
            &[("path", T::Str)],
            |cx| ScriptCommand::AudioPlay {
                path: cx.str_arg(0),
            },
        ),
        emit("audio_pause", "Pause playback.", &[], |_| {
            ScriptCommand::AudioPause
        }),
        emit("audio_resume", "Resume playback.", &[], |_| {
            ScriptCommand::AudioResume
        }),
        emit("audio_stop", "Stop playback.", &[], |_| {
            ScriptCommand::AudioStop
        }),
        emit(
            "audio_seek",
            "Seek to that position, in seconds.",
            &[("secs", T::Float)],
            |cx| ScriptCommand::AudioSeek {
                secs: cx.float_arg(0),
            },
        ),
        emit(
            "audio_volume",
            "Set the output volume, 0.0 to 1.0.",
            &[("level", T::Float)],
            |cx| ScriptCommand::AudioVolume {
                level: cx.float_arg(0) as f32,
            },
        ),
    ]
}

/// Write `contents` to `path` without a truncated-read window.
///
/// `std::fs::write` truncates the file before writing, so a reader racing
/// the write (a file watcher, another process, the app reloading its own
/// save file) can see a zero-length or partial file. Writing to a sibling
/// temp file and renaming it into place avoids that: rename is atomic on a
/// given filesystem, so a concurrent reader always sees either the old
/// contents or the new ones.
fn write_file_atomic(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    let path = std::path::Path::new(path);
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "write_file: path has no file name",
        )
    })?;

    // A per-process counter so two writes racing the same path in the same
    // tick never pick the same temp name.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    })();

    match write_result {
        Ok(()) => std::fs::rename(&tmp_path, path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Translation, the filesystem, template-local ids, and the tree dump.
fn misc_fns() -> Vec<ScriptFn> {
    vec![
        // The catalogue lives behind the process-wide i18n hook the runtime
        // installs, so no host links Fluent or ICU itself. `tr` is Qt's
        // spelling of the same call.
        value(
            "t",
            "The active locale's string for that key, or the key itself.",
            &[("key", T::Str)],
            T::Str,
            |cx| ScriptValue::Str(lumen_core::i18n::translate(&cx.str_arg(0))),
        ),
        value(
            "tr",
            "The active locale's string for that key, or the key itself.",
            &[("key", T::Str)],
            T::Str,
            |cx| ScriptValue::Str(lumen_core::i18n::translate(&cx.str_arg(0))),
        ),
        value(
            "read_file",
            "The contents of that file, or an empty string.",
            &[("path", T::Str)],
            T::Str,
            |cx| {
                let path = cx.str_arg(0);
                match std::fs::read_to_string(&path) {
                    Ok(text) => ScriptValue::Str(text),
                    Err(e) => {
                        // A missing file is the expected way for a script to
                        // probe for optional state (a config that has not
                        // been saved yet, say), not a fault to report; only
                        // warn about the errors that mean something went
                        // wrong reading a file that is there.
                        if e.kind() != std::io::ErrorKind::NotFound {
                            warn_line!("read_file({path}): {e}");
                        }
                        ScriptValue::Str(String::new())
                    }
                }
            },
        ),
        value(
            "write_file",
            "Write contents to that path; false when the write failed.",
            &[("path", T::Str), ("contents", T::Str)],
            T::Bool,
            |cx| {
                let path = cx.str_arg(0);
                match write_file_atomic(&path, &cx.str_arg(1)) {
                    Ok(()) => ScriptValue::Bool(true),
                    Err(e) => {
                        warn_line!("write_file({path}): {e}");
                        ScriptValue::Bool(false)
                    }
                }
            },
        ),
        // A template instance prefixes the ids inside it. Given `user-card:btn`
        // as the source, `local_id(source, "label")` is `user-card:label`; a
        // source with no `:` gives the suffix back unchanged, and multi-level
        // prefixes stack.
        value(
            "local_id",
            "The id of a sibling inside the same template instance.",
            &[("source", T::Str), ("suffix", T::Str)],
            T::Str,
            |cx| {
                let source = cx.str_arg(0);
                let suffix = cx.str_arg(1);
                ScriptValue::Str(match source.rfind(':') {
                    Some(colon) => format!("{}:{suffix}", &source[..colon]),
                    None => suffix,
                })
            },
        ),
        value(
            "dump_tree",
            "The element tree, rendered for debugging.",
            &[],
            T::Str,
            |_| ScriptValue::Str(crate::introspect::dump_tree()),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every entry is a builtin with a doc line and declared parameters: what
    /// the hosts, the editor tooling, and the candela declaration generator
    /// all read.
    ///
    /// The return type is only pinned for the entries candela sees, because
    /// candela is the language that has to name it in a declaration. Rhai and
    /// Lua resolve `page()` and `page(path)` through one entry whose result
    /// depends on which one the script called.
    #[test]
    fn every_entry_is_a_documented_builtin() {
        for f in builtin_script_fns() {
            assert_eq!(f.ns, ScriptNs::Builtin, "{}", f.name);
            assert!(!f.sig.doc.is_empty(), "{} has no doc line", f.name);
            assert!(!f.sig.variadic, "{} must declare its parameters", f.name);
            for p in &f.sig.params {
                assert_ne!(
                    p.ty,
                    T::Any,
                    "{}: parameter `{}` is untyped",
                    f.name,
                    p.name
                );
            }
            if f.visible_to("candela") {
                assert_ne!(f.sig.ret, T::Any, "{}: return type is untyped", f.name);
            }
        }
    }

    /// A name may appear twice only when the two entries reach different
    /// languages, which is how the navigation family carries one shape for
    /// Rhai and Lua and another for candela.
    #[test]
    fn a_repeated_name_splits_by_language() {
        let fns = builtin_script_fns();
        for lang in ["rhai", "lua", "candela"] {
            let mut seen: HashSet<&str> = HashSet::new();
            for f in fns.iter().filter(|f| f.visible_to(lang)) {
                assert!(
                    seen.insert(f.name.as_str()),
                    "{lang}: `{}` is registered twice",
                    f.name
                );
            }
        }
    }

    #[test]
    fn a_filter_spec_parses_into_labelled_extension_groups() {
        assert_eq!(
            parse_dialog_filter_spec("Images:png,jpg|All:*"),
            vec![
                (
                    "Images".to_string(),
                    vec!["png".to_string(), "jpg".to_string()]
                ),
                ("All".to_string(), Vec::new()),
            ]
        );
    }
}
