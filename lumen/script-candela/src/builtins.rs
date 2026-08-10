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
//! The surface is scalar-marshallable apart from `derive`, whose `deps`
//! argument is a `string[]` (candela marshals homogeneous arrays natively). The
//! Rhai builtins that pass or return maps / heterogeneous `any` values
//! (`signal`, `signal_array`, `http`, `parse_json`, `parse_markdown`,
//! `signal_get_color`) still have no entry - see the crate-level docs for the
//! exact upstream gap.
//!
//! The metadata *types* are host-neutral and live in
//! [`lumen_script::builtins`]; only this candela table is host-specific.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Look up a builtin by exact name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    BUILTINS.iter().find(|b| b.name == name)
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

    #[test]
    fn every_param_type_is_marshallable() {
        // candela's embedding marshalling handles scalars plus homogeneous arrays
        // of scalars; the table must never grow a `map` / `fn` / `any` param or
        // a non-scalar return type.
        fn is_scalar(ty: &str) -> bool {
            matches!(ty, "int" | "float" | "bool" | "string")
        }
        for b in BUILTINS {
            for param in b.params {
                let ok = is_scalar(param.ty) || param.ty.strip_suffix("[]").is_some_and(is_scalar);
                assert!(
                    ok,
                    "builtin {} has non-marshallable param type {}",
                    b.name, param.ty
                );
            }
            let ret_ok = matches!(b.ret, "int" | "float" | "bool" | "string" | "()")
                || b.ret.strip_suffix("[]").is_some_and(is_scalar);
            assert!(
                ret_ok,
                "builtin {} has non-marshallable return type {}",
                b.name, b.ret
            );
        }
    }
}
