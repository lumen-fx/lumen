//! The Lumen script builtins exposed on the Lua [`Lua`](mlua::Lua) engine.
//!
//! Every free function registered as a Lua global in
//! [`crate::LuaHost::new`] has a matching entry in [`BUILTINS`]. The
//! table is what the Lumen LSP (`lumen-lsp`) reads for completion, hover,
//! and signature help.
//!
//! Custom-type *methods* (`Signal:get` / `ArraySignal:push` / the
//! `signals.foo.set(v)` chained accessors) are intentionally not listed
//! here: they dispatch on a receiver, which the text-only LSP cannot
//! resolve. Only top-level free functions belong in the table.
//!
//! Add a builtin by registering it in the host and adding its entry below.

pub use lumen_script::builtins::{BuiltinFn, BuiltinParam};

/// Every Lumen free-function builtin registered as a Lua global.
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
        name: "query",
        params: &[BuiltinParam {
            name: "selector",
            ty: "string",
        }],
        ret: "NodeQuery",
        doc: "Run a CSS selector against the live tree; returns a NodeQuery result set.",
    },
    BuiltinFn {
        name: "get_by_id",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
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
        name: "signal",
        params: &[
            BuiltinParam {
                name: "name",
                ty: "string",
            },
            BuiltinParam {
                name: "default",
                ty: "any",
            },
        ],
        ret: "Signal",
        doc: "Return a handle to the named scalar signal, initialising it to `default` the first time.",
    },
    BuiltinFn {
        name: "signal_array",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "ArraySignal",
        doc: "Return a handle to the named reactive array driving `<for each=\"name\">`.",
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
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed i64 signal.",
    },
    BuiltinFn {
        name: "signal_get_int",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "int",
        doc: "Read a typed i64 signal; `nil` on miss or wrong type.",
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
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed f64 signal.",
    },
    BuiltinFn {
        name: "signal_get_float",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "float",
        doc: "Read a typed f64 signal; `nil` on miss or wrong type.",
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
        doc: "Deprecated: prefer `signals.name.set(v)`. Write a typed bool signal.",
    },
    BuiltinFn {
        name: "signal_get_bool",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "bool",
        doc: "Read a typed bool signal; `nil` on miss or wrong type.",
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
        doc: "Deprecated: prefer `signals.name.set_color(hex)`. Write a `#rrggbb`/`#rrggbbaa` color signal.",
    },
    BuiltinFn {
        name: "signal_get_color",
        params: &[BuiltinParam {
            name: "name",
            ty: "string",
        }],
        ret: "map",
        doc: "Read a color signal as a `{ r, g, b, a }` table; `nil` on miss.",
    },
    BuiltinFn {
        name: "is_valid",
        params: &[BuiltinParam {
            name: "id",
            ty: "string",
        }],
        ret: "bool",
        doc: "True when the element with id `id` currently passes validation.",
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
            ty: "map",
        }],
        ret: "()",
        doc: "Issue an HTTP request `{method,url,headers,body,timeout_ms,tag}`; fires `on_http(tag, response)` with `{ok,status,headers,body,error}`.",
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
        doc: "Parse a JSON string into a Lua table/array/scalar; `nil` on parse error.",
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
                ty: "array",
            },
            BuiltinParam {
                name: "f",
                ty: "fn",
            },
        ],
        ret: "Signal",
        doc: "Register a computed signal recomputed from `deps` via `f`; returns the derived `Signal`.",
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
        doc: "Return the sibling id `suffix` inside the same template instance as `source`.",
    },
    BuiltinFn {
        name: "parse_markdown",
        params: &[BuiltinParam {
            name: "src",
            ty: "string",
        }],
        ret: "array",
        doc: "Parse markdown into a block list (`{ id, kind, level, text, lang }` tables) for `<for>`.",
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
        ret: "string",
        doc: "Navigate to a page path; called with no argument, the active page key.",
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
        ret: "bool",
        doc: "Step one entry back in the page history; false when there was nowhere to go.",
    },
    BuiltinFn {
        name: "page_forward",
        params: &[],
        ret: "bool",
        doc: "Step one entry forward in the page history; false when there was nowhere to go.",
    },
];

/// Look up a builtin by exact name.
pub fn lookup(name: &str) -> Option<&'static BuiltinFn> {
    lumen_script::builtins::lookup_in(BUILTINS, name)
}

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
