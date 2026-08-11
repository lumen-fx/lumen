//! Single source of truth for the Lumen script builtins exposed on the
//! candela [`Engine`](candela::Engine) by [`crate::CandelaHost`].
//!
//! Every host function registered via `engine.register_host_fn("lumen", ...)`
//! in [`crate::CandelaHost::build_engine`] has a matching entry in [`BUILTINS`].
//! Unlike the Rhai host (where builtins are bare global functions), candela
//! reaches them through a typed `host "lumen" { ... }` block the script
//! declares; the declaration is type-checked against the registered closure
//! at compile time. The table is consumed by:
//!
//! - the Lumen LSP for completion / hover / signature help, and
//! - the `builtins_parity` integration test, which synthesizes a `host`
//!   block from this table and compiles it - proving every entry is
//!   registered with a matching scalar signature.
//!
//! Most entries have a concrete signature: scalars, homogeneous arrays
//! (`string[]`), and string-keyed maps of one value type (`{string: int}`).
//! An entry that names `any` in a parameter or its return carries a value with
//! no single concrete shape; those register variadically and are declared
//! `name(...)` in the prelude, with the `any` return type where they return
//! one. [`is_variadic`] is the single place that rule lives.
//!
//! The metadata *types* are host-neutral and live in
//! [`lumen_script::builtins`]; only this candela table is host-specific.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Look up a builtin by exact name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Whether `b` is registered variadically, which is true exactly when it names
/// `any` in a parameter or its return type. Such a builtin is declared
/// `name(...)` in a `host` block; every other entry keeps its concrete
/// signature.
#[must_use]
pub fn is_variadic(b: &BuiltinFn) -> bool {
    b.ret == "any" || b.params.iter().any(|p| p.ty == "any")
}

// Shorthand param constructor keeps the table below readable.
const fn p(name: &'static str, ty: &'static str) -> BuiltinParam {
    BuiltinParam { name, ty }
}

/// Every scalar Lumen builtin registered on the candela engine under the
/// `lumen` host namespace.
///
/// Keep this in sync with the `register_host_fn` calls in
/// [`crate::CandelaHost::build_engine`]; the `builtins_parity` test enforces it
/// by compiling a `host "lumen" { ... }` block generated from this table.
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
        name: "signal_get",
        params: &[p("name", "string")],
        ret: "string",
        doc: "Read the named signal as a string; empty string when never written.",
    },
    BuiltinFn {
        name: "signal_set",
        params: &[p("name", "string"), p("value", "string")],
        ret: "()",
        doc: "Write the named signal to the string `value` and mirror it into the reactive store.",
    },
    BuiltinFn {
        name: "signal_get_int",
        params: &[p("name", "string")],
        ret: "int",
        doc: "Read a typed i64 signal; `0` on miss or non-numeric value.",
    },
    BuiltinFn {
        name: "signal_set_int",
        params: &[p("name", "string"), p("value", "int")],
        ret: "()",
        doc: "Write a typed i64 signal.",
    },
    BuiltinFn {
        name: "signal_get_float",
        params: &[p("name", "string")],
        ret: "float",
        doc: "Read a typed f64 signal; `0.0` on miss or non-numeric value.",
    },
    BuiltinFn {
        name: "signal_set_float",
        params: &[p("name", "string"), p("value", "float")],
        ret: "()",
        doc: "Write a typed f64 signal.",
    },
    BuiltinFn {
        name: "signal_get_bool",
        params: &[p("name", "string")],
        ret: "bool",
        doc: "Read a typed bool signal; `false` on miss or unparseable value.",
    },
    BuiltinFn {
        name: "signal_set_bool",
        params: &[p("name", "string"), p("value", "bool")],
        ret: "()",
        doc: "Write a typed bool signal.",
    },
    // Array signals: the reactive lists `<for each="name">` renders. Items are
    // records (string-keyed maps), so the item-carrying entries are `any`.
    BuiltinFn {
        name: "signal_array_set",
        params: &[p("name", "string"), p("items", "any")],
        ret: "()",
        doc: "Replace the named array signal with `items`, a list of records.",
    },
    BuiltinFn {
        name: "signal_array_push",
        params: &[p("name", "string"), p("item", "any")],
        ret: "()",
        doc: "Append one record to the named array signal.",
    },
    BuiltinFn {
        name: "signal_array_get",
        params: &[p("name", "string"), p("index", "int")],
        ret: "any",
        doc: "Read one record by zero-based index; null when out of range.",
    },
    BuiltinFn {
        name: "signal_array_all",
        params: &[p("name", "string")],
        ret: "any",
        doc: "Every record in the named array signal, as a list.",
    },
    BuiltinFn {
        name: "signal_array_len",
        params: &[p("name", "string")],
        ret: "int",
        doc: "Number of records in the named array signal.",
    },
    BuiltinFn {
        name: "signal_array_remove",
        params: &[p("name", "string"), p("index", "int")],
        ret: "()",
        doc: "Drop the record at `index`; an out-of-range index does nothing.",
    },
    BuiltinFn {
        name: "signal_array_clear",
        params: &[p("name", "string")],
        ret: "()",
        doc: "Empty the named array signal.",
    },
    BuiltinFn {
        name: "signal_set_color",
        params: &[p("name", "string"), p("hex", "string")],
        ret: "()",
        doc: "Write a `#rrggbb` / `#rrggbbaa` color signal; unparseable input is ignored.",
    },
    BuiltinFn {
        name: "signal_get_color",
        params: &[p("name", "string")],
        ret: "{string: int}",
        doc: "Read a color signal as an `{ r, g, b, a }` map of 0-255 channels; empty when the signal holds no color.",
    },
    BuiltinFn {
        name: "is_valid",
        params: &[p("id", "string")],
        ret: "bool",
        doc: "Whether the element with id `id` currently passes validation. An element with no validation state reads as valid.",
    },
    BuiltinFn {
        name: "local_id",
        params: &[p("source", "string"), p("suffix", "string")],
        ret: "string",
        doc: "The sibling id `suffix` inside the same template instance as `source`.",
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
        name: "node_query",
        params: &[p("selector", "string")],
        ret: "int[]",
        doc: "Run a CSS selector; returns the matching node ids in document order.",
    },
    BuiltinFn {
        name: "node_get_by_id",
        params: &[p("id", "string")],
        ret: "int",
        doc: "Fast id lookup; returns the node id or 0.",
    },
    BuiltinFn {
        name: "node_document",
        params: &[],
        ret: "int",
        doc: "Return the document root node id.",
    },
    BuiltinFn {
        name: "node_parent",
        params: &[p("node", "int")],
        ret: "int",
        doc: "Parent node id, or 0.",
    },
    BuiltinFn {
        name: "node_first_child",
        params: &[p("node", "int")],
        ret: "int",
        doc: "First child node id, or 0.",
    },
    BuiltinFn {
        name: "node_last_child",
        params: &[p("node", "int")],
        ret: "int",
        doc: "Last child node id, or 0.",
    },
    BuiltinFn {
        name: "node_next",
        params: &[p("node", "int")],
        ret: "int",
        doc: "Next sibling node id, or 0.",
    },
    BuiltinFn {
        name: "node_prev",
        params: &[p("node", "int")],
        ret: "int",
        doc: "Previous sibling node id, or 0.",
    },
    BuiltinFn {
        name: "node_children",
        params: &[p("node", "int")],
        ret: "int[]",
        doc: "Child node ids in document order.",
    },
    BuiltinFn {
        name: "node_closest",
        params: &[p("node", "int"), p("selector", "string")],
        ret: "int",
        doc: "Nearest ancestor-or-self matching the selector; node id or 0.",
    },
    BuiltinFn {
        name: "node_valid",
        params: &[p("node", "int")],
        ret: "bool",
        doc: "Whether the node id is present in the current snapshot.",
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
        name: "set_color_scheme",
        params: &[p("name", "string")],
        ret: "()",
        doc: "Switch the color scheme: \"default\" (follow the OS), \"force-light\", \"force-dark\", \"prefer-light\", \"prefer-dark\".",
    },
    BuiltinFn {
        name: "page",
        params: &[p("path", "string")],
        ret: "()",
        doc: "Navigate to a page path (`\"settings\"`, `\"/user/7\"`, `\"/\"`).",
    },
    BuiltinFn {
        name: "page_current",
        params: &[],
        ret: "string",
        doc: "The active page key. Spelled apart from `page(path)` because a host fn takes one arity per name.",
    },
    BuiltinFn {
        name: "page_back",
        params: &[],
        ret: "()",
        doc: "Step one entry back in the in-memory page history.",
    },
    BuiltinFn {
        name: "page_forward",
        params: &[],
        ret: "()",
        doc: "Step one entry forward in the in-memory page history.",
    },
    BuiltinFn {
        name: "fetch",
        params: &[p("url", "string"), p("tag", "string")],
        ret: "()",
        doc: "Issue an HTTP GET; fires `on_fetch(tag, body)` when the response lands.",
    },
    BuiltinFn {
        name: "http",
        params: &[p("request", "any")],
        ret: "()",
        doc: "Issue an HTTP request `{ method, url, headers, body, timeout_ms, tag }`; fires `on_http(tag, response)` with `{ ok, status, headers, body, error }`.",
    },
    BuiltinFn {
        name: "parse_json",
        params: &[p("json", "string")],
        ret: "any",
        doc: "Parse a JSON string into a map, list, or scalar; null on a parse error.",
    },
    BuiltinFn {
        name: "parse_markdown",
        params: &[p("src", "string")],
        ret: "any",
        doc: "Parse markdown into a block list of `{ id, kind, level, text, lang }` records.",
    },
    BuiltinFn {
        name: "matched_rules",
        params: &[p("node", "int")],
        ret: "any",
        doc: "The stylesheet rules that matched `node`, ascending in cascade order.",
    },
    BuiltinFn {
        name: "print",
        params: &[p("args", "any")],
        ret: "()",
        doc: "Emit a print command carrying the arguments, stringified and joined with a space.",
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
    // candela has no first-class closure value, so the recompute body `f` is the
    // NAME of a script function (a string) rather than a `fn`/closure literal;
    // `deps` is a `string[]` of signal names. See the crate docs.
    BuiltinFn {
        name: "derive",
        params: &[p("name", "string"), p("deps", "string[]"), p("f", "string")],
        ret: "()",
        doc: "Register a computed signal `name` recomputed by the script fn named `f` whenever any of `deps` changes; `f` receives the dep values in order.",
    },
    // Audio transport. The `position` / `duration` / `playing` read-backs are
    // host-written signals, not builtins, consumed via `bind-*`.
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
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(b.name), "duplicate builtin {}", b.name);
        }
    }

    /// Whether `ty` is a type a fixed host-fn signature can name: a scalar, a
    /// homogeneous array of scalars, or a string-keyed map of one scalar.
    fn is_concrete(ty: &str) -> bool {
        fn is_scalar(ty: &str) -> bool {
            matches!(ty, "int" | "float" | "bool" | "string")
        }
        is_scalar(ty)
            || ty.strip_suffix("[]").is_some_and(is_scalar)
            || ty
                .strip_prefix("{string: ")
                .and_then(|rest| rest.strip_suffix('}'))
                .is_some_and(is_scalar)
    }

    #[test]
    fn every_non_variadic_type_is_concrete() {
        // A fixed host-fn signature names one concrete type per position. An
        // entry that needs a dynamically-shaped value says so with `any`, which
        // makes it variadic; everything else must stay concrete.
        for b in BUILTINS {
            if is_variadic(b) {
                continue;
            }
            for param in b.params {
                assert!(
                    is_concrete(param.ty),
                    "builtin {} has non-marshallable param type {}",
                    b.name,
                    param.ty
                );
            }
            assert!(
                b.ret == "()" || is_concrete(b.ret),
                "builtin {} has non-marshallable return type {}",
                b.name,
                b.ret
            );
        }
    }

    #[test]
    fn variadic_entries_are_the_ones_naming_any() {
        for b in BUILTINS {
            let names_any = b.ret == "any" || b.params.iter().any(|p| p.ty == "any");
            assert_eq!(
                is_variadic(b),
                names_any,
                "builtin {} disagrees with the `any` marker",
                b.name
            );
        }
    }
}
