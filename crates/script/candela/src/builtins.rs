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
    // Dynamic DOM write side. `node_spawn` / `node_clone_deep` mint a
    // reserved-token id valid for the rest of the tick.
    BuiltinFn {
        name: "node_spawn",
        params: &[p("tag", "string")],
        ret: "int",
        doc: "Create a detached element; the handle is valid for the rest of the tick.",
    },
    BuiltinFn {
        name: "node_clone_deep",
        params: &[p("source", "int")],
        ret: "int",
        doc: "Deep-clone a subtree into a fresh detached element.",
    },
    BuiltinFn {
        name: "node_set_attr",
        params: &[p("node", "int"), p("name", "string"), p("value", "string")],
        ret: "()",
        doc: "Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map.",
    },
    BuiltinFn {
        name: "node_remove_attr",
        params: &[p("node", "int"), p("name", "string")],
        ret: "()",
        doc: "Remove an attribute.",
    },
    BuiltinFn {
        name: "node_set_id",
        params: &[p("node", "int"), p("id", "string")],
        ret: "()",
        doc: "Set the `id` attribute.",
    },
    BuiltinFn {
        name: "node_set_text",
        params: &[p("node", "int"), p("text", "string")],
        ret: "()",
        doc: "Replace the text content.",
    },
    BuiltinFn {
        name: "node_set_inner_markup",
        params: &[p("node", "int"), p("markup", "string")],
        ret: "()",
        doc: "Replace the children with a parsed markup fragment. A no-op when the app runs from a precompiled artifact, which links no parser.",
    },
    BuiltinFn {
        name: "node_class_add",
        params: &[p("node", "int"), p("class", "string")],
        ret: "()",
        doc: "Add one class.",
    },
    BuiltinFn {
        name: "node_class_remove",
        params: &[p("node", "int"), p("class", "string")],
        ret: "()",
        doc: "Remove one class.",
    },
    BuiltinFn {
        name: "node_class_toggle",
        params: &[p("node", "int"), p("class", "string")],
        ret: "()",
        doc: "Toggle one class.",
    },
    BuiltinFn {
        name: "node_set_class",
        params: &[p("node", "int"), p("classes", "string")],
        ret: "()",
        doc: "Replace the whole class list.",
    },
    BuiltinFn {
        name: "node_set_style",
        params: &[p("node", "int"), p("name", "string"), p("value", "string")],
        ret: "()",
        doc: "Set one inline style property.",
    },
    BuiltinFn {
        name: "node_style_remove",
        params: &[p("node", "int"), p("name", "string")],
        ret: "()",
        doc: "Remove one inline style property.",
    },
    BuiltinFn {
        name: "node_remove",
        params: &[p("node", "int")],
        ret: "()",
        doc: "Detach and despawn the element and its subtree.",
    },
    BuiltinFn {
        name: "node_append",
        params: &[p("parent", "int"), p("child", "int")],
        ret: "()",
        doc: "Append `child` under `parent`.",
    },
    BuiltinFn {
        name: "node_insert_before",
        params: &[p("parent", "int"), p("child", "int"), p("reference", "int")],
        ret: "()",
        doc: "Insert `child` before `reference` under `parent`; a `reference` of 0 appends.",
    },
    BuiltinFn {
        name: "node_set_parent",
        params: &[p("node", "int"), p("parent", "int")],
        ret: "()",
        doc: "Reparent `node` under `parent`.",
    },
    BuiltinFn {
        name: "node_move_to",
        params: &[p("node", "int"), p("parent", "int")],
        ret: "()",
        doc: "Same as `node_set_parent`.",
    },
    BuiltinFn {
        name: "node_replace_with",
        params: &[p("old", "int"), p("new", "int")],
        ret: "()",
        doc: "Replace `old` with `new`, despawning `old`'s subtree.",
    },
    // Read-backs on a single node.
    BuiltinFn {
        name: "node_get_attr",
        params: &[p("node", "int"), p("name", "string")],
        ret: "string",
        doc: "One attribute value; empty when absent.",
    },
    BuiltinFn {
        name: "node_text",
        params: &[p("node", "int")],
        ret: "string",
        doc: "Text content.",
    },
    BuiltinFn {
        name: "node_id",
        params: &[p("node", "int")],
        ret: "string",
        doc: "The `id` attribute.",
    },
    BuiltinFn {
        name: "node_class_contains",
        params: &[p("node", "int"), p("class", "string")],
        ret: "bool",
        doc: "Whether the class list contains `class`.",
    },
    BuiltinFn {
        name: "node_style_get",
        params: &[p("node", "int"), p("prop", "string")],
        ret: "string",
        doc: "One inline style override.",
    },
    BuiltinFn {
        name: "node_computed_style",
        params: &[p("node", "int"), p("prop", "string")],
        ret: "string",
        doc: "One resolved style property after the cascade.",
    },
    BuiltinFn {
        name: "node_computed_style_all",
        params: &[p("node", "int")],
        ret: "{string: string}",
        doc: "Every resolved style property.",
    },
    BuiltinFn {
        name: "node_inline_style",
        params: &[p("node", "int")],
        ret: "{string: string}",
        doc: "Every inline style override.",
    },
    BuiltinFn {
        name: "node_attrs",
        params: &[p("node", "int")],
        ret: "{string: string}",
        doc: "Every attribute.",
    },
    BuiltinFn {
        name: "node_classes",
        params: &[p("node", "int")],
        ret: "string[]",
        doc: "The class list.",
    },
    // Low-level introspection over the per-tick snapshot.
    BuiltinFn {
        name: "node_rect",
        params: &[p("node", "int")],
        ret: "{string: float}",
        doc: "Post-layout border box: `x`, `y`, `width`, `height`, `client_x`, `client_y`.",
    },
    BuiltinFn {
        name: "node_content_rect",
        params: &[p("node", "int")],
        ret: "{string: float}",
        doc: "Same keys as `node_rect`, for the content box (padding and border removed).",
    },
    BuiltinFn {
        name: "node_scroll",
        params: &[p("node", "int")],
        ret: "{string: float}",
        doc: "Scroll offsets and extents: `x`, `y`, `max_x`, `max_y`.",
    },
    BuiltinFn {
        name: "node_is_visible",
        params: &[p("node", "int")],
        ret: "bool",
        doc: "Effective visibility.",
    },
    BuiltinFn {
        name: "node_z_index",
        params: &[p("node", "int")],
        ret: "int",
        doc: "Resolved stacking order.",
    },
    BuiltinFn {
        name: "node_entity_id",
        params: &[p("node", "int")],
        ret: "{string: int}",
        doc: "`index` and `generation` of the backing entity.",
    },
    BuiltinFn {
        name: "node_components",
        params: &[p("node", "int")],
        ret: "string[]",
        doc: "Names of the introspectable components on the element.",
    },
    BuiltinFn {
        name: "node_component",
        params: &[p("node", "int"), p("name", "string")],
        ret: "{string: string}",
        doc: "Field map of one component; empty for an absent or non-introspectable name.",
    },
    BuiltinFn {
        name: "node_outer_markup",
        params: &[p("node", "int")],
        ret: "string",
        doc: "The subtree serialized to markup text.",
    },
    BuiltinFn {
        name: "node_inner_markup",
        params: &[p("node", "int")],
        ret: "string",
        doc: "The children serialized to markup text.",
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
    // Element events: bind by handler name, then read the current event
    // through the accessors keyed by the id the handler receives.
    BuiltinFn {
        name: "event_on",
        params: &[
            p("node", "int"),
            p("event_type", "string"),
            p("handler", "string"),
        ],
        ret: "int",
        doc: "Bind the script fn named `handler` for the bubble phase; returns the off token, or 0 for an unknown node.",
    },
    BuiltinFn {
        name: "event_on_capture",
        params: &[
            p("node", "int"),
            p("event_type", "string"),
            p("handler", "string"),
        ],
        ret: "int",
        doc: "Same as `event_on`, for the capture phase.",
    },
    BuiltinFn {
        name: "event_off",
        params: &[p("token", "int")],
        ret: "()",
        doc: "Unbind the handler an `event_on` / `event_on_capture` token names.",
    },
    BuiltinFn {
        name: "event_target",
        params: &[p("ev", "int")],
        ret: "int",
        doc: "The element the event originated on.",
    },
    BuiltinFn {
        name: "event_current_target",
        params: &[p("ev", "int")],
        ret: "int",
        doc: "The element whose handler is running.",
    },
    BuiltinFn {
        name: "event_type",
        params: &[p("ev", "int")],
        ret: "string",
        doc: "Event type name.",
    },
    BuiltinFn {
        name: "event_key",
        params: &[p("ev", "int")],
        ret: "string",
        doc: "Key name for keyboard events.",
    },
    BuiltinFn {
        name: "event_value",
        params: &[p("ev", "int")],
        ret: "string",
        doc: "Text value for `input` / `change` / `submit`.",
    },
    BuiltinFn {
        name: "event_button",
        params: &[p("ev", "int")],
        ret: "int",
        doc: "Pointer button: 0 primary, 1 middle, 2 secondary.",
    },
    BuiltinFn {
        name: "event_x",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Pointer x relative to the target.",
    },
    BuiltinFn {
        name: "event_y",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Pointer y relative to the target.",
    },
    BuiltinFn {
        name: "event_client_x",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Pointer x in window coordinates.",
    },
    BuiltinFn {
        name: "event_client_y",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Pointer y in window coordinates.",
    },
    BuiltinFn {
        name: "event_delta_x",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Horizontal wheel delta.",
    },
    BuiltinFn {
        name: "event_delta_y",
        params: &[p("ev", "int")],
        ret: "float",
        doc: "Vertical wheel delta.",
    },
    BuiltinFn {
        name: "event_shift",
        params: &[p("ev", "int")],
        ret: "bool",
        doc: "Whether Shift was held.",
    },
    BuiltinFn {
        name: "event_ctrl",
        params: &[p("ev", "int")],
        ret: "bool",
        doc: "Whether Control was held.",
    },
    BuiltinFn {
        name: "event_alt",
        params: &[p("ev", "int")],
        ret: "bool",
        doc: "Whether Alt was held.",
    },
    BuiltinFn {
        name: "event_super",
        params: &[p("ev", "int")],
        ret: "bool",
        doc: "Whether the Super / Command key was held.",
    },
    BuiltinFn {
        name: "event_prevent_default",
        params: &[p("ev", "int")],
        ret: "()",
        doc: "Cancel the default action.",
    },
    BuiltinFn {
        name: "event_stop_propagation",
        params: &[p("ev", "int")],
        ret: "()",
        doc: "Stop the event reaching further elements.",
    },
    BuiltinFn {
        name: "event_stop_immediate_propagation",
        params: &[p("ev", "int")],
        ret: "()",
        doc: "Stop the event entirely, including other handlers on this element.",
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
