//! The Lumen script builtins a candela program can call.
//!
//! Every host function the crate registers under the `lumen` namespace has a
//! matching entry in [`BUILTINS`]. Both candela hosts register the same list,
//! so the table describes what a compiled program and a `.cdlb` artifact each
//! reach. Unlike the Rhai host (where builtins are bare global
//! functions), candela reaches them through a typed `host "lumen" { ... }`
//! block the script declares; the declaration is type-checked against the
//! registered closure at compile time. The table is consumed by:
//!
//! - the Lumen LSP for completion / hover / signature help,
//! - the `builtins_parity` integration test, which synthesizes a `host`
//!   block from this table and compiles it - proving every entry is
//!   registered with a matching scalar signature, and
//! - `every_registered_lumen_fn_is_tabled`, which scans the host source for
//!   registrations and proves the other direction.
//!
//! Most entries have a concrete signature: scalars, homogeneous arrays
//! (`string[]`), and string-keyed maps of one value type (`{string: int}`).
//! An entry that names `any` in a parameter or its return carries a value with
//! no single concrete shape; those register variadically and are declared
//! `name(...)` in the prelude, with the `any` return type where they return
//! one. [`is_variadic`] is the single place that rule lives.
//!
//! The table is long because the dynamic DOM and event surface is free
//! functions over an `int` handle. Add a builtin by registering it and adding
//! its entry below, with the types spelled the way a `host` block declares
//! them.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Every scalar Lumen builtin registered on the candela engine under the
/// `lumen` host namespace.
pub const BUILTINS: &[BuiltinFn] = &[
    BuiltinFn {
        name: "add_clicks",
        params: &[BuiltinParam {
            name: "n",
            ty: "int",
        }],
        ret: "()",
        doc: "Increment the app's click counter by `n`.",
    },
    BuiltinFn {
        name: "dump_tree",
        params: &[],
        ret: "string",
        doc: "Whole-tree structural dump for debugging.",
    },
    BuiltinFn {
        name: "pointer_state",
        params: &[],
        ret: "{string: string}",
        doc: "Pointer position, buttons, and modifiers: `x`, `y`, `inside`, `buttons`, `shift`, `ctrl`, `alt`, `super`, stringified.",
    },
    BuiltinFn {
        name: "frame_info",
        params: &[],
        ret: "{string: float}",
        doc: "Per-frame counters `frame`, `dt_ms`, `dirty_count`.",
    },
    BuiltinFn {
        name: "signals_all",
        params: &[],
        ret: "{string: string}",
        doc: "The whole signal set as a name-to-value map.",
    },
    BuiltinFn {
        name: "set_string",
        params: &[
            BuiltinParam {
                name: "key",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Set an app-side string key to `value`.",
    },
    BuiltinFn {
        name: "set_text",
        params: &[
            BuiltinParam {
                name: "target_id",
                ty: "string",
            },
            BuiltinParam {
                name: "text",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Replace the text content of the element with id `target_id`.",
    },
    BuiltinFn {
        name: "set_src",
        params: &[
            BuiltinParam {
                name: "target_id",
                ty: "string",
            },
            BuiltinParam {
                name: "path",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Swap the asset path of the `<image id=target_id>` at runtime (app-relative path).",
    },
    BuiltinFn {
        name: "signal_set_int",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Deprecated: prefer `signal<int>(\"name\").set(v)`. Write a typed i64 signal.",
    },
    BuiltinFn {
        name: "signal_get_int",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "int",
        doc: "Read a typed i64 signal; `0` on miss or non-numeric value.",
    },
    BuiltinFn {
        name: "signal_set_float",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "float",
            },
        ],
        ret: "()",
        doc: "Deprecated: prefer `signal<float>(\"name\").set(v)`. Write a typed f64 signal.",
    },
    BuiltinFn {
        name: "signal_get_float",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "float",
        doc: "Read a typed f64 signal; `0.0` on miss or non-numeric value.",
    },
    BuiltinFn {
        name: "signal_set_bool",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "bool",
            },
        ],
        ret: "()",
        doc: "Deprecated: prefer `signal<bool>(\"name\").set(v)`. Write a typed bool signal.",
    },
    BuiltinFn {
        name: "signal_get_bool",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "bool",
        doc: "Read a typed bool signal; `false` on miss or unparseable value.",
    },
    BuiltinFn {
        name: "signal_set_color",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "hex",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Deprecated: prefer `signal<Color>(\"name\").set(hex)`. Write a `#rrggbb` / `#rrggbbaa` color signal; unparseable input is ignored.",
    },
    BuiltinFn {
        name: "signal_get_color",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "{string: int}",
        doc: "Read a color signal as an `{ r, g, b, a }` map of 0-255 channels; empty when the signal holds no color.",
    },
    BuiltinFn {
        name: "is_valid",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
        ret: "bool",
        doc: "Whether the element with id `id` currently passes validation. An element with no validation state reads as valid.",
    },
    BuiltinFn {
        name: "set_timeout",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "ms",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Schedule a one-shot timer firing `on_timer(name)` after `ms` milliseconds.",
    },
    BuiltinFn {
        name: "set_interval",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "ms",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Schedule a repeating timer firing `on_timer(name)` every `ms` milliseconds.",
    },
    BuiltinFn {
        name: "cancel_timer",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "()",
        doc: "Cancel a timer previously created with `set_timeout`/`set_interval`.",
    },
    BuiltinFn {
        name: "request_frame",
        params: &[],
        ret: "()",
        doc: "Ask for one `on_frame(dt)` call on the next tick; call it again from the handler to keep animating.",
    },
    BuiltinFn {
        name: "notify",
        params: &[
            BuiltinParam {
                name: "title",
                ty: "string",
            },
            BuiltinParam {
                name: "body",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Show an OS notification with `title` and `body`.",
    },
    BuiltinFn {
        name: "notify_ex",
        params: &[
            BuiltinParam {
                name: "id",
                ty: "string",
            },
            BuiltinParam {
                name: "title",
                ty: "string",
            },
            BuiltinParam {
                name: "body",
                ty: "string",
            },
            BuiltinParam {
                name: "options",
                ty: "string",
            },
            BuiltinParam {
                name: "actions",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Show an OS notification. `options` is `icon:name-or-path|urgency:critical`, `actions` is `id:Label|id2:Label2`; a press fires `on_notification_action(id, action_id)`.",
    },
    BuiltinFn {
        name: "clipboard_write",
        params: &[BuiltinParam {
            name: "text",
            ty: "string",
        }],
        ret: "()",
        doc: "Put `text` on the system clipboard.",
    },
    BuiltinFn {
        name: "clipboard_read",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Request the clipboard text; fires `on_clipboard(tag, text)` next tick.",
    },
    BuiltinFn {
        name: "open_url",
        params: &[BuiltinParam {
            name: "url",
            ty: "string",
        }],
        ret: "()",
        doc: "Open `url` with the user's default browser or mail client.",
    },
    BuiltinFn {
        name: "open_path",
        params: &[BuiltinParam {
            name: "path",
            ty: "string",
        }],
        ret: "()",
        doc: "Open `path` (app-relative) with the platform's default application.",
    },
    BuiltinFn {
        name: "reveal_path",
        params: &[BuiltinParam {
            name: "path",
            ty: "string",
        }],
        ret: "()",
        doc: "Show `path` (app-relative) in the platform's file manager.",
    },
    BuiltinFn {
        name: "keep_awake",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "reason",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Hold off the screensaver and system sleep under `name` until `allow_sleep(name)`.",
    },
    BuiltinFn {
        name: "allow_sleep",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "()",
        doc: "Release the sleep inhibit registered under `name`.",
    },
    BuiltinFn {
        name: "copy_image",
        params: &[BuiltinParam {
            name: "path",
            ty: "string",
        }],
        ret: "()",
        doc: "Copy the image at `path` (app-relative) to the system clipboard.",
    },
    BuiltinFn {
        name: "save_clipboard_image",
        params: &[BuiltinParam {
            name: "path",
            ty: "string",
        }],
        ret: "()",
        doc: "Write the current clipboard image to `path` as PNG.",
    },
    BuiltinFn {
        name: "tray_icon",
        params: &[
            BuiltinParam {
                name: "id",
                ty: "string",
            },
            BuiltinParam {
                name: "icon_path",
                ty: "string",
            },
            BuiltinParam {
                name: "tooltip",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Register or replace a system tray icon; clicks fire `on_tray(id)`. Empty tooltip disables it.",
    },
    BuiltinFn {
        name: "tray_icon_menu",
        params: &[
            BuiltinParam {
                name: "id",
                ty: "string",
            },
            BuiltinParam {
                name: "icon_path",
                ty: "string",
            },
            BuiltinParam {
                name: "tooltip",
                ty: "string",
            },
            BuiltinParam {
                name: "menu",
                ty: "string",
            },
            BuiltinParam {
                name: "template",
                ty: "bool",
            },
        ],
        ret: "()",
        doc: "Register a tray icon with a context menu `id:Label|-|id2:Label2` (a pick fires `on_menu(id)`) and the macOS template-image flag.",
    },
    BuiltinFn {
        name: "unregister_tray",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
        ret: "()",
        doc: "Remove a previously registered tray icon.",
    },
    BuiltinFn {
        name: "open_menu",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
        ret: "()",
        doc: "Open the menu `id` (sets the `__menu_open:id` signal to true).",
    },
    BuiltinFn {
        name: "close_menu",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
        ret: "()",
        doc: "Close the menu `id` (sets the `__menu_open:id` signal to false).",
    },
    BuiltinFn {
        name: "pick_file",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Open a native open-file dialog; fires `on_file_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "pick_files",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Open a native multi-select dialog; fires `on_files_picked(tag, paths)`.",
    },
    BuiltinFn {
        name: "pick_folder",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Open a native folder-picker dialog; fires `on_folder_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "save_file",
        params: &[
            BuiltinParam {
                name: "tag",
                ty: "string",
            },
            BuiltinParam {
                name: "default_name",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Open a native save-file dialog seeded with `default_name`; fires `on_file_picked(tag, path)`.",
    },
    BuiltinFn {
        name: "pick_file_filtered",
        params: &[
            BuiltinParam {
                name: "tag",
                ty: "string",
            },
            BuiltinParam {
                name: "spec",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Open a filtered open-file dialog. `spec` is `Label:ext1,ext2|All:*`.",
    },
    BuiltinFn {
        name: "register_hotkey",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "accelerator",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Register a global OS hotkey (e.g. `CommandOrControl+S`); fires `on_hotkey(name)`.",
    },
    BuiltinFn {
        name: "unregister_hotkey",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "()",
        doc: "Remove a previously registered global hotkey.",
    },
    BuiltinFn {
        name: "add_recent_file",
        params: &[
            BuiltinParam {
                name: "path",
                ty: "string",
            },
            BuiltinParam {
                name: "label",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Record path as recently opened; an empty label derives one from the path.",
    },
    BuiltinFn {
        name: "list_recent_files",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Request the recent-files list; fires `on_recent_files(tag, paths)`, paths joined by `|`.",
    },
    BuiltinFn {
        name: "clear_recent_files",
        params: &[],
        ret: "()",
        doc: "Remove every entry from the recent-files list.",
    },
    BuiltinFn {
        name: "set_autostart",
        params: &[BuiltinParam {
            name: "on",
            ty: "bool",
        }],
        ret: "()",
        doc: "Enable or disable launching this app at login.",
    },
    BuiltinFn {
        name: "query_autostart",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Request the autostart state; fires `on_autostart_enabled(tag)` or `on_autostart_disabled(tag)`.",
    },
    BuiltinFn {
        name: "set_class",
        params: &[
            BuiltinParam {
                name: "id",
                ty: "string",
            },
            BuiltinParam {
                name: "classes",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Replace the CSS classes on the element with id `id`.",
    },
    BuiltinFn {
        name: "set_root_class",
        params: &[BuiltinParam {
            name: "classes",
            ty: "string",
        }],
        ret: "()",
        doc: "Replace the CSS classes on the `<root>` element (drives theme-token selectors).",
    },
    BuiltinFn {
        name: "fetch",
        params: &[
            BuiltinParam {
                name: "url",
                ty: "string",
            },
            BuiltinParam {
                name: "tag",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Issue an HTTP GET; fires `on_fetch(tag, body)` when the response lands.",
    },
    BuiltinFn {
        name: "http",
        params: &[BuiltinParam {
            name: "request",
            ty: "any",
        }],
        ret: "()",
        doc: "Issue an HTTP request `{ method, url, headers, body, timeout_ms, tag }`; fires `on_http(tag, response)` with `{ ok, status, headers, body, error }`.",
    },
    BuiltinFn {
        name: "request_header",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "string",
        doc: "The named header of the request being rendered for; empty string when there is none.",
    },
    BuiltinFn {
        name: "request_cookie",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "string",
        doc: "The named cookie of the request being rendered for; empty string when there is none.",
    },
    BuiltinFn {
        name: "request_body",
        params: &[],
        ret: "string",
        doc: "The body of the request being rendered for; empty string when there is none.",
    },
    BuiltinFn {
        name: "response_status",
        params: &[BuiltinParam {
            name: "status",
            ty: "int",
        }],
        ret: "()",
        doc: "Answer the request with HTTP status `status`, clamped to 100..=599.",
    },
    BuiltinFn {
        name: "response_header",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Set header `name` to `value` on the response; setting the same name twice replaces it.",
    },
    BuiltinFn {
        name: "redirect",
        params: &[BuiltinParam {
            name: "location",
            ty: "string",
        }],
        ret: "()",
        doc: "Answer the request with a redirect to `location` instead of a document.",
    },
    BuiltinFn {
        name: "parse_json",
        params: &[BuiltinParam {
            name: "json",
            ty: "string",
        }],
        ret: "any",
        doc: "Parse a JSON string into a map, list, or scalar; null on a parse error.",
    },
    BuiltinFn {
        name: "derive",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "deps",
                ty: "string[]",
            },
            BuiltinParam {
                name: "f",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Register a computed signal `name` recomputed by the script fn named `f` whenever any of `deps` changes; `f` receives the dep values in order.",
    },
    BuiltinFn {
        name: "on",
        params: &[
            BuiltinParam {
                name: "event",
                ty: "string",
            },
            BuiltinParam {
                name: "id",
                ty: "string",
            },
            BuiltinParam {
                name: "handler",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Route `event` on element `id` to the script function named `handler`.",
    },
    BuiltinFn {
        name: "local_id",
        params: &[
            BuiltinParam {
                name: "source",
                ty: "string",
            },
            BuiltinParam {
                name: "suffix",
                ty: "string",
            },
        ],
        ret: "string",
        doc: "The sibling id `suffix` inside the same template instance as `source`.",
    },
    BuiltinFn {
        name: "parse_markdown",
        params: &[BuiltinParam {
            name: "src",
            ty: "string",
        }],
        ret: "any",
        doc: "Parse markdown into a block list of `{ id, kind, level, text, lang }` records.",
    },
    BuiltinFn {
        name: "t",
        params: &[BuiltinParam {
            name: "key",
            ty: "string",
        }],
        ret: "string",
        doc: "Translate `key` in the active locale; returns the key itself when untranslated.",
    },
    BuiltinFn {
        name: "tr",
        params: &[BuiltinParam {
            name: "key",
            ty: "string",
        }],
        ret: "string",
        doc: "Alias for `t(key)`.",
    },
    BuiltinFn {
        name: "set_locale",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "()",
        doc: "Switch the app to a BCP-47 locale (`\"de-DE\"`); marked text, placeholders, tooltips, `format` output and the writing direction follow.",
    },
    BuiltinFn {
        name: "locale",
        params: &[],
        ret: "string",
        doc: "The BCP-47 tag of the locale the app is running in.",
    },
    BuiltinFn {
        name: "format_number",
        params: &[BuiltinParam {
            name: "n",
            ty: "float",
        }],
        ret: "string",
        doc: "Write `n` the way the active locale writes numbers.",
    },
    BuiltinFn {
        name: "format_currency",
        params: &[
            BuiltinParam {
                name: "amount",
                ty: "float",
            },
            BuiltinParam {
                name: "currency",
                ty: "string",
            },
        ],
        ret: "string",
        doc: "Write `amount` as money in the ISO-4217 code `currency`, for the active locale.",
    },
    BuiltinFn {
        name: "format_date",
        params: &[BuiltinParam {
            name: "iso",
            ty: "string",
        }],
        ret: "string",
        doc: "Write the date in `iso` (`YYYY-MM-DD`, time optional) for the active locale.",
    },
    BuiltinFn {
        name: "format_time",
        params: &[BuiltinParam {
            name: "iso",
            ty: "string",
        }],
        ret: "string",
        doc: "Write the time in `iso` (`YYYY-MM-DDTHH:MM[:SS]`) for the active locale.",
    },
    BuiltinFn {
        name: "format_datetime",
        params: &[BuiltinParam {
            name: "iso",
            ty: "string",
        }],
        ret: "string",
        doc: "Write the date and time in `iso` for the active locale.",
    },
    BuiltinFn {
        name: "format_relative",
        params: &[BuiltinParam {
            name: "seconds",
            ty: "int",
        }],
        ret: "string",
        doc: "Write `seconds` from now as the locale says it; past is negative.",
    },
    BuiltinFn {
        name: "signal_get",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "string",
        doc: "Read the named signal as a string; empty string when never written.",
    },
    BuiltinFn {
        name: "signal_set",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Write the named signal to the string `value` and mirror it into the reactive store.",
    },
    BuiltinFn {
        name: "signal_array_set",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "items",
                ty: "any",
            },
        ],
        ret: "()",
        doc: "Replace the named array signal with `items`, a list of records.",
    },
    BuiltinFn {
        name: "signal_array_push",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "item",
                ty: "any",
            },
        ],
        ret: "()",
        doc: "Append one record to the named array signal.",
    },
    BuiltinFn {
        name: "signal_array_get",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "index",
                ty: "int",
            },
        ],
        ret: "any",
        doc: "Read one record by zero-based index; null when out of range.",
    },
    BuiltinFn {
        name: "signal_array_all",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "any",
        doc: "Every record in the named array signal, as a list.",
    },
    BuiltinFn {
        name: "signal_array_len",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "int",
        doc: "Number of records in the named array signal.",
    },
    BuiltinFn {
        name: "signal_array_remove",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "index",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Drop the record at `index`; an out-of-range index does nothing.",
    },
    BuiltinFn {
        name: "signal_array_clear",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "()",
        doc: "Empty the named array signal.",
    },
    BuiltinFn {
        name: "node_query",
        params: &[BuiltinParam {
            name: "selector",
            ty: "string",
        }],
        ret: "int[]",
        doc: "Run a CSS selector; returns the matching node ids in document order.",
    },
    BuiltinFn {
        name: "node_get_by_id",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
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
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "Parent node id, or 0.",
    },
    BuiltinFn {
        name: "node_first_child",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "First child node id, or 0.",
    },
    BuiltinFn {
        name: "node_last_child",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "Last child node id, or 0.",
    },
    BuiltinFn {
        name: "node_next",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "Next sibling node id, or 0.",
    },
    BuiltinFn {
        name: "node_prev",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "Previous sibling node id, or 0.",
    },
    BuiltinFn {
        name: "node_children",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int[]",
        doc: "Child node ids in document order.",
    },
    BuiltinFn {
        name: "node_closest",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "selector",
                ty: "string",
            },
        ],
        ret: "int",
        doc: "Nearest ancestor-or-self matching the selector; node id or 0.",
    },
    BuiltinFn {
        name: "node_valid",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "bool",
        doc: "Whether the node id is present in the current snapshot.",
    },
    BuiltinFn {
        name: "node_spawn",
        params: &[BuiltinParam {
            name: "tag",
            ty: "string",
        }],
        ret: "int",
        doc: "Create a detached element; the handle is valid for the rest of the tick.",
    },
    BuiltinFn {
        name: "fragment_spawn",
        params: &[
            BuiltinParam {
                name: "key",
                ty: "string",
            },
            BuiltinParam {
                name: "args",
                ty: "string[]",
            },
            BuiltinParam {
                name: "children",
                ty: "int[]",
            },
        ],
        ret: "int",
        doc: "Instantiate the compiled fragment `key` into a detached node. `args` is flattened name/value pairs; `children` are the nodes its slots take, in slot order. What an `lmn!` block expands to.",
    },
    BuiltinFn {
        name: "mount",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "()",
        doc: "Put `node` at the app root.",
    },
    BuiltinFn {
        name: "node_clone_deep",
        params: &[BuiltinParam {
            name: "source",
            ty: "int",
        }],
        ret: "int",
        doc: "Deep-clone a subtree into a fresh detached element.",
    },
    BuiltinFn {
        name: "node_set_attr",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map.",
    },
    BuiltinFn {
        name: "node_remove_attr",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Remove an attribute.",
    },
    BuiltinFn {
        name: "node_set_id",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "id",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Set the `id` attribute.",
    },
    BuiltinFn {
        name: "node_set_text",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "text",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Replace the text content.",
    },
    BuiltinFn {
        name: "node_set_inner_markup",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "markup",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Replace the children with a parsed markup fragment. A no-op when the app runs from a precompiled artifact, which links no parser.",
    },
    BuiltinFn {
        name: "node_class_add",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "class",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Add one class.",
    },
    BuiltinFn {
        name: "node_class_remove",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "class",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Remove one class.",
    },
    BuiltinFn {
        name: "node_class_toggle",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "class",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Toggle one class.",
    },
    BuiltinFn {
        name: "node_set_class",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "classes",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Replace the whole class list.",
    },
    BuiltinFn {
        name: "node_set_style",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "value",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Set one inline style property.",
    },
    BuiltinFn {
        name: "node_style_remove",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
        ],
        ret: "()",
        doc: "Remove one inline style property.",
    },
    BuiltinFn {
        name: "node_remove",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "()",
        doc: "Detach and despawn the element and its subtree.",
    },
    BuiltinFn {
        name: "node_append",
        params: &[
            BuiltinParam {
                name: "parent",
                ty: "int",
            },
            BuiltinParam {
                name: "child",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Append `child` under `parent`.",
    },
    BuiltinFn {
        name: "node_insert_before",
        params: &[
            BuiltinParam {
                name: "parent",
                ty: "int",
            },
            BuiltinParam {
                name: "child",
                ty: "int",
            },
            BuiltinParam {
                name: "reference",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Insert `child` before `reference` under `parent`; a `reference` of 0 appends.",
    },
    BuiltinFn {
        name: "node_set_parent",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "parent",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Reparent `node` under `parent`.",
    },
    BuiltinFn {
        name: "node_move_to",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "parent",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Same as `node_set_parent`.",
    },
    BuiltinFn {
        name: "node_replace_with",
        params: &[
            BuiltinParam {
                name: "old",
                ty: "int",
            },
            BuiltinParam {
                name: "new",
                ty: "int",
            },
        ],
        ret: "()",
        doc: "Replace `old` with `new`, despawning `old`'s subtree.",
    },
    BuiltinFn {
        name: "node_get_attr",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
        ],
        ret: "string",
        doc: "One attribute value; empty when absent.",
    },
    BuiltinFn {
        name: "node_text",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string",
        doc: "Text content.",
    },
    BuiltinFn {
        name: "node_id",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string",
        doc: "The `id` attribute.",
    },
    BuiltinFn {
        name: "node_class_contains",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "class",
                ty: "string",
            },
        ],
        ret: "bool",
        doc: "Whether the class list contains `class`.",
    },
    BuiltinFn {
        name: "node_style_get",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "prop",
                ty: "string",
            },
        ],
        ret: "string",
        doc: "One inline style override.",
    },
    BuiltinFn {
        name: "node_computed_style",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "prop",
                ty: "string",
            },
        ],
        ret: "string",
        doc: "One resolved style property after the cascade.",
    },
    BuiltinFn {
        name: "node_computed_style_all",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: string}",
        doc: "Every resolved style property.",
    },
    BuiltinFn {
        name: "node_inline_style",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: string}",
        doc: "Every inline style override.",
    },
    BuiltinFn {
        name: "node_attrs",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: string}",
        doc: "Every attribute.",
    },
    BuiltinFn {
        name: "node_classes",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string[]",
        doc: "The class list.",
    },
    BuiltinFn {
        name: "node_rect",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: float}",
        doc: "Post-layout border box: `x`, `y`, `width`, `height`, `client_x`, `client_y`.",
    },
    BuiltinFn {
        name: "node_content_rect",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: float}",
        doc: "Same keys as `node_rect`, for the content box (padding and border removed).",
    },
    BuiltinFn {
        name: "node_scroll",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: float}",
        doc: "Scroll offsets and extents: `x`, `y`, `max_x`, `max_y`.",
    },
    BuiltinFn {
        name: "node_is_visible",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "bool",
        doc: "Effective visibility.",
    },
    BuiltinFn {
        name: "node_z_index",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "int",
        doc: "Resolved stacking order.",
    },
    BuiltinFn {
        name: "node_entity_id",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "{string: int}",
        doc: "`index` and `generation` of the backing entity.",
    },
    BuiltinFn {
        name: "node_components",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string[]",
        doc: "Names of the introspectable components on the element.",
    },
    BuiltinFn {
        name: "node_component",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "name",
                ty: "string",
            },
        ],
        ret: "{string: string}",
        doc: "Field map of one component; empty for an absent or non-introspectable name.",
    },
    BuiltinFn {
        name: "node_outer_markup",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string",
        doc: "The subtree serialized to markup text.",
    },
    BuiltinFn {
        name: "node_inner_markup",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "string",
        doc: "The children serialized to markup text.",
    },
    BuiltinFn {
        name: "event_on",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "event_type",
                ty: "string",
            },
            BuiltinParam {
                name: "handler",
                ty: "string",
            },
        ],
        ret: "int",
        doc: "Bind the script fn named `handler` for the bubble phase; returns the off token, or 0 for an unknown node.",
    },
    BuiltinFn {
        name: "event_on_capture",
        params: &[
            BuiltinParam {
                name: "node",
                ty: "int",
            },
            BuiltinParam {
                name: "event_type",
                ty: "string",
            },
            BuiltinParam {
                name: "handler",
                ty: "string",
            },
        ],
        ret: "int",
        doc: "Same as `event_on`, for the capture phase.",
    },
    BuiltinFn {
        name: "event_off",
        params: &[BuiltinParam {
            name: "token",
            ty: "int",
        }],
        ret: "()",
        doc: "Unbind the handler an `event_on` / `event_on_capture` token names.",
    },
    BuiltinFn {
        name: "event_target",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "int",
        doc: "The element the event originated on.",
    },
    BuiltinFn {
        name: "event_current_target",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "int",
        doc: "The element whose handler is running.",
    },
    BuiltinFn {
        name: "event_type",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "string",
        doc: "Event type name.",
    },
    BuiltinFn {
        name: "event_key",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "string",
        doc: "Key name for keyboard events.",
    },
    BuiltinFn {
        name: "event_value",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "string",
        doc: "Text value for `input` / `change` / `submit`.",
    },
    BuiltinFn {
        name: "event_button",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "int",
        doc: "Pointer button: 0 primary, 1 middle, 2 secondary.",
    },
    BuiltinFn {
        name: "event_x",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Pointer x relative to the target.",
    },
    BuiltinFn {
        name: "event_y",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Pointer y relative to the target.",
    },
    BuiltinFn {
        name: "event_client_x",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Pointer x in window coordinates.",
    },
    BuiltinFn {
        name: "event_client_y",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Pointer y in window coordinates.",
    },
    BuiltinFn {
        name: "event_delta_x",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Horizontal wheel delta.",
    },
    BuiltinFn {
        name: "event_delta_y",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "float",
        doc: "Vertical wheel delta.",
    },
    BuiltinFn {
        name: "event_shift",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "bool",
        doc: "Whether Shift was held.",
    },
    BuiltinFn {
        name: "event_ctrl",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "bool",
        doc: "Whether Control was held.",
    },
    BuiltinFn {
        name: "event_alt",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "bool",
        doc: "Whether Alt was held.",
    },
    BuiltinFn {
        name: "event_super",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "bool",
        doc: "Whether the Super / Command key was held.",
    },
    BuiltinFn {
        name: "event_prevent_default",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "()",
        doc: "Cancel the default action.",
    },
    BuiltinFn {
        name: "event_stop_propagation",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "()",
        doc: "Stop the event reaching further elements.",
    },
    BuiltinFn {
        name: "event_stop_immediate_propagation",
        params: &[BuiltinParam {
            name: "ev",
            ty: "int",
        }],
        ret: "()",
        doc: "Stop the event entirely, including other handlers on this element.",
    },
    BuiltinFn {
        name: "set_color_scheme",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "()",
        doc: "Switch the color scheme: \"default\" (follow the OS), \"force-light\", \"force-dark\", \"prefer-light\", \"prefer-dark\".",
    },
    BuiltinFn {
        name: "page",
        params: &[BuiltinParam {
            name: "path",
            ty: "string",
        }],
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
        name: "matched_rules",
        params: &[BuiltinParam {
            name: "node",
            ty: "int",
        }],
        ret: "any",
        doc: "The stylesheet rules that matched `node`, ascending in cascade order.",
    },
    BuiltinFn {
        name: "print",
        params: &[BuiltinParam {
            name: "args",
            ty: "any",
        }],
        ret: "()",
        doc: "Emit a print command carrying the arguments, stringified and joined with a space.",
    },
];

/// Look up a builtin by exact name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    lumen_script::builtins::lookup_in(BUILTINS, name)
}

/// Whether `b` is registered variadically, which is true exactly when it names
/// `any` in a parameter or its return type. Such a builtin is declared
/// `name(...)` in a `host` block; every other entry keeps its concrete
/// signature.
#[must_use]
pub fn is_variadic(b: &BuiltinFn) -> bool {
    b.ret == "any" || b.params.iter().any(|p| p.ty == "any")
}

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
