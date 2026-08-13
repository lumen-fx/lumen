//! Single source of truth for the Lumen script builtins exposed on the
//! Lua [`Lua`](mlua::Lua) engine.
//!
//! Every free function registered as a Lua global in
//! [`crate::LuaHost::new`] has a matching entry in [`BUILTINS`]. The
//! table is consumed by:
//!
//! - the Lumen LSP (`lumen-lsp`) for completion, hover, and signature
//!   help, and
//! - the `builtins_parity` integration test, which asserts every name
//!   in the table resolves to a Lua function global on a fresh host
//!   (guarding against the table drifting away from the registration
//!   code).
//!
//! The name/param/doc surface is deliberately **identical** to the Rhai
//! host's table so an app author sees the same builtins regardless of
//! the selected engine. Custom-type *methods* (`Signal:get` /
//! `ArraySignal:push` / the `signals.foo.set(v)` chained accessors) are
//! intentionally not listed here: they dispatch on a receiver, which the
//! text-only LSP cannot resolve. Only top-level free functions belong in
//! the table.
//!
//! The metadata *types* are host-neutral and live in
//! [`lumen_script::builtins`]; only this table is host-specific.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Look up a builtin by exact name.
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    BUILTINS.iter().find(|b| b.name == name)
}

// Shorthand param constructor keeps the table below readable.
const fn p(name: &'static str, ty: &'static str) -> BuiltinParam {
    BuiltinParam { name, ty }
}

/// Every Lumen free-function builtin registered as a Lua global.
///
/// Keep this in sync with the registrations in [`crate::LuaHost::new`];
/// the `builtins_parity` test enforces it.
pub const BUILTINS: &[BuiltinFn] = &[
    BuiltinFn {
        name: "add_clicks",
        params: &[p("n", "int")],
        ret: "()",
        doc: "Increment the app's click counter by `n`.",
    },
    BuiltinFn {
        name: "set_string",
        params: &[p("key", "string"), p("value", "string")],
        ret: "()",
        doc: "Set an app-side string key to `value`.",
    },
    BuiltinFn {
        name: "set_text",
        params: &[p("target_id", "string"), p("text", "string")],
        ret: "()",
        doc: "Replace the text content of the element with id `target_id`.",
    },
    BuiltinFn {
        name: "set_src",
        params: &[p("target_id", "string"), p("path", "string")],
        ret: "()",
        doc: "Swap the asset path of the `<image id=target_id>` at runtime (app-relative path).",
    },
    BuiltinFn {
        name: "query",
        params: &[p("selector", "string")],
        ret: "NodeQuery",
        doc: "Run a CSS selector against the live tree; returns a NodeQuery result set.",
    },
    BuiltinFn {
        name: "get_by_id",
        params: &[p("id", "string")],
        ret: "Node",
        doc: "Fast id lookup; returns the matching Node or nil.",
    },
    BuiltinFn {
        name: "document",
        params: &[],
        ret: "Node",
        doc: "Return the document root Node.",
    },
    BuiltinFn {
        name: "dump_tree",
        params: &[],
        ret: "string",
        doc: "Whole-tree structural dump (id / tag / classes / rect) for debugging.",
    },
    BuiltinFn {
        name: "pointer_state",
        params: &[],
        ret: "map",
        doc: "Pointer position, buttons, and modifiers as a map.",
    },
    BuiltinFn {
        name: "frame_info",
        params: &[],
        ret: "map",
        doc: "Per-frame counters {frame, dt_ms, dirty_count} as a map.",
    },
    BuiltinFn {
        name: "signals_all",
        params: &[],
        ret: "map",
        doc: "The whole signal set as a name -> value map (inspection call).",
    },
    BuiltinFn {
        name: "signal",
        params: &[p("name", "string"), p("default", "any")],
        ret: "Signal",
        doc: "Return a handle to the named scalar signal, initialising it to `default` the first time.",
    },
    BuiltinFn {
        name: "signal_array",
        params: &[p("name", "string")],
        ret: "ArraySignal",
        doc: "Return a handle to the named reactive array driving `<for each=\"name\">`.",
    },
    BuiltinFn {
        name: "signal_set_int",
        params: &[p("name", "string"), p("value", "int")],
        ret: "()",
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed i64 signal.",
    },
    BuiltinFn {
        name: "signal_get_int",
        params: &[p("name", "string")],
        ret: "int",
        doc: "Read a typed i64 signal; `nil` on miss or wrong type.",
    },
    BuiltinFn {
        name: "signal_set_float",
        params: &[p("name", "string"), p("value", "float")],
        ret: "()",
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed f64 signal.",
    },
    BuiltinFn {
        name: "signal_get_float",
        params: &[p("name", "string")],
        ret: "float",
        doc: "Read a typed f64 signal; `nil` on miss or wrong type.",
    },
    BuiltinFn {
        name: "signal_set_bool",
        params: &[p("name", "string"), p("value", "bool")],
        ret: "()",
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed bool signal.",
    },
    BuiltinFn {
        name: "signal_get_bool",
        params: &[p("name", "string")],
        ret: "bool",
        doc: "Read a typed bool signal; `nil` on miss or wrong type.",
    },
    BuiltinFn {
        name: "signal_set_color",
        params: &[p("name", "string"), p("hex", "string")],
        ret: "()",
        doc: "Deprecated: prefer `signals.name.set_color(hex)`. Write a `#rrggbb`/`#rrggbbaa` color signal.",
    },
    BuiltinFn {
        name: "signal_get_color",
        params: &[p("name", "string")],
        ret: "map",
        doc: "Read a color signal as a `{ r, g, b, a }` table; `nil` on miss.",
    },
    BuiltinFn {
        name: "is_valid",
        params: &[p("id", "string")],
        ret: "bool",
        doc: "True when the element with id `id` currently passes validation.",
    },
    BuiltinFn {
        name: "set_timeout",
        params: &[p("name", "string"), p("ms", "int")],
        ret: "()",
        doc: "Schedule a one-shot timer firing `on_timer(name)` after `ms` milliseconds.",
    },
    BuiltinFn {
        name: "set_interval",
        params: &[p("name", "string"), p("ms", "int")],
        ret: "()",
        doc: "Schedule a repeating timer firing `on_timer(name)` every `ms` milliseconds.",
    },
    BuiltinFn {
        name: "cancel_timer",
        params: &[p("name", "string")],
        ret: "()",
        doc: "Cancel a timer previously created with `set_timeout`/`set_interval`.",
    },
    BuiltinFn {
        name: "notify",
        params: &[p("title", "string"), p("body", "string")],
        ret: "()",
        doc: "Show an OS notification with `title` and `body`.",
    },
    BuiltinFn {
        name: "notify_ex",
        params: &[
            p("id", "string"),
            p("title", "string"),
            p("body", "string"),
            p("options", "string"),
            p("actions", "string"),
        ],
        ret: "()",
        doc: "Show an OS notification. `options` is `icon:name-or-path|urgency:critical`, `actions` is `id:Label|id2:Label2`; a press fires `on_notification_action(id, action_id)`.",
    },
    BuiltinFn {
        name: "clipboard_write",
        params: &[p("text", "string")],
        ret: "()",
        doc: "Put `text` on the system clipboard.",
    },
    BuiltinFn {
        name: "clipboard_read",
        params: &[p("tag", "string")],
        ret: "()",
        doc: "Request the clipboard text; fires `on_clipboard(tag, text)` next tick.",
    },
    BuiltinFn {
        name: "open_url",
        params: &[p("url", "string")],
        ret: "()",
        doc: "Open `url` with the user's default browser or mail client.",
    },
    BuiltinFn {
        name: "open_path",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Open `path` (app-relative) with the platform's default application.",
    },
    BuiltinFn {
        name: "reveal_path",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Show `path` (app-relative) in the platform's file manager.",
    },
    BuiltinFn {
        name: "keep_awake",
        params: &[p("name", "string"), p("reason", "string")],
        ret: "()",
        doc: "Hold off the screensaver and system sleep under `name` until `allow_sleep(name)`.",
    },
    BuiltinFn {
        name: "allow_sleep",
        params: &[p("name", "string")],
        ret: "()",
        doc: "Release the sleep inhibit registered under `name`.",
    },
    BuiltinFn {
        name: "copy_image",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Copy the image at `path` (app-relative) to the system clipboard.",
    },
    BuiltinFn {
        name: "save_clipboard_image",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Write the current clipboard image to `path` as PNG.",
    },
    BuiltinFn {
        name: "tray_icon",
        params: &[
            p("id", "string"),
            p("icon_path", "string"),
            p("tooltip", "string"),
        ],
        ret: "()",
        doc: "Register or replace a system tray icon; clicks fire `on_tray(id)`. Empty tooltip disables it.",
    },
    BuiltinFn {
        name: "tray_icon_menu",
        params: &[
            p("id", "string"),
            p("icon_path", "string"),
            p("tooltip", "string"),
            p("menu", "string"),
            p("template", "bool"),
        ],
        ret: "()",
        doc: "Register a tray icon with a context menu `id:Label|-|id2:Label2` (a pick fires `on_menu(id)`) and the macOS template-image flag.",
    },
    BuiltinFn {
        name: "unregister_tray",
        params: &[p("id", "string")],
        ret: "()",
        doc: "Remove a previously registered tray icon.",
    },
    BuiltinFn {
        name: "open_menu",
        params: &[p("id", "string")],
        ret: "()",
        doc: "Open the menu `id` (sets the `__menu_open:id` signal to true).",
    },
    BuiltinFn {
        name: "close_menu",
        params: &[p("id", "string")],
        ret: "()",
        doc: "Close the menu `id` (sets the `__menu_open:id` signal to false).",
    },
    BuiltinFn {
        name: "pick_file",
        params: &[p("tag", "string")],
        ret: "()",
        doc: "Open a native open-file dialog; fires `on_file_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "pick_files",
        params: &[p("tag", "string")],
        ret: "()",
        doc: "Open a native multi-select dialog; fires `on_files_picked(tag, paths)`.",
    },
    BuiltinFn {
        name: "pick_folder",
        params: &[p("tag", "string")],
        ret: "()",
        doc: "Open a native folder-picker dialog; fires `on_folder_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "save_file",
        params: &[p("tag", "string"), p("default_name", "string")],
        ret: "()",
        doc: "Open a native save-file dialog seeded with `default_name`; fires `on_file_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "pick_file_filtered",
        params: &[p("tag", "string"), p("spec", "string")],
        ret: "()",
        doc: "Open a filtered open-file dialog. `spec` is `Label:ext1,ext2|All:*`.",
    },
    BuiltinFn {
        name: "register_hotkey",
        params: &[p("name", "string"), p("accelerator", "string")],
        ret: "()",
        doc: "Register a global OS hotkey (e.g. `CommandOrControl+S`); fires `on_hotkey(name)`.",
    },
    BuiltinFn {
        name: "unregister_hotkey",
        params: &[p("name", "string")],
        ret: "()",
        doc: "Remove a previously registered global hotkey.",
    },
    BuiltinFn {
        name: "set_class",
        params: &[p("id", "string"), p("classes", "string")],
        ret: "()",
        doc: "Replace the CSS classes on the element with id `id`.",
    },
    BuiltinFn {
        name: "set_root_class",
        params: &[p("classes", "string")],
        ret: "()",
        doc: "Replace the CSS classes on the `<root>` element (drives theme-token selectors).",
    },
    BuiltinFn {
        name: "fetch",
        params: &[p("url", "string"), p("tag", "string")],
        ret: "()",
        doc: "Issue an HTTP GET; fires `on_fetch(tag, body)` when the response lands.",
    },
    BuiltinFn {
        name: "http",
        params: &[p("request", "map")],
        ret: "()",
        doc: "Issue an HTTP request `{method,url,headers,body,timeout_ms,tag}`; fires `on_http(tag, response)` with `{ok,status,headers,body,error}`.",
    },
    BuiltinFn {
        name: "parse_json",
        params: &[p("json", "string")],
        ret: "any",
        doc: "Parse a JSON string into a Lua table/array/scalar; `nil` on parse error.",
    },
    BuiltinFn {
        name: "derive",
        params: &[p("name", "string"), p("deps", "array"), p("f", "fn")],
        ret: "Signal",
        doc: "Register a computed signal recomputed from `deps` via `f`; returns the derived `Signal`.",
    },
    BuiltinFn {
        name: "on",
        params: &[
            p("event", "string"),
            p("id", "string"),
            p("handler", "string"),
        ],
        ret: "()",
        doc: "Route `event` on element `id` to the script function named `handler`.",
    },
    BuiltinFn {
        name: "local_id",
        params: &[p("source", "string"), p("suffix", "string")],
        ret: "string",
        doc: "Return the sibling id `suffix` inside the same template instance as `source`.",
    },
    BuiltinFn {
        name: "parse_markdown",
        params: &[p("src", "string")],
        ret: "array",
        doc: "Parse markdown into a block list (`{ id, kind, level, text, lang }` tables) for `<for>`.",
    },
    BuiltinFn {
        name: "t",
        params: &[p("key", "string")],
        ret: "string",
        doc: "Translate `key` in the active locale; returns the key itself when untranslated.",
    },
    BuiltinFn {
        name: "tr",
        params: &[p("key", "string")],
        ret: "string",
        doc: "Alias for `t(key)`.",
    },
    BuiltinFn {
        name: "read_file",
        params: &[p("path", "string")],
        ret: "string",
        doc: "Read a file to a string; empty string on error.",
    },
    BuiltinFn {
        name: "write_file",
        params: &[p("path", "string"), p("contents", "string")],
        ret: "bool",
        doc: "Write `contents` to `path`; returns true on success.",
    },
    // Audio transport (registered in `crate::audio`). The `position` /
    // `duration` / `playing` read-backs are host-written signals, not
    // builtins, so they are consumed via `bind-*` / `derive()`.
    BuiltinFn {
        name: "audio_play",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Load and play the audio track at `path` (app-relative wav/ogg); resets position to 0.",
    },
    BuiltinFn {
        name: "audio_pause",
        params: &[],
        ret: "()",
        doc: "Pause the audio transport, holding its position.",
    },
    BuiltinFn {
        name: "audio_resume",
        params: &[],
        ret: "()",
        doc: "Resume a paused audio transport.",
    },
    BuiltinFn {
        name: "audio_stop",
        params: &[],
        ret: "()",
        doc: "Stop the audio transport and rewind to 0.",
    },
    BuiltinFn {
        name: "audio_seek",
        params: &[p("secs", "float")],
        ret: "()",
        doc: "Seek the audio transport to `secs` seconds (clamped to the track duration).",
    },
    BuiltinFn {
        name: "audio_volume",
        params: &[p("level", "float")],
        ret: "()",
        doc: "Set audio output volume in 0.0..=1.0.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_render() {
        let b = lookup("set_timeout").unwrap();
        assert_eq!(b.signature(), "set_timeout(name: string, ms: int) -> ()");
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
        }
    }
}
