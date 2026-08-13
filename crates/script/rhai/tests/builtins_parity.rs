//! Parity guard: every entry in `builtins::BUILTINS` must be a function
//! actually registered on a fresh `RhaiHost` engine, and no new bare
//! free-function builtin should be registered without a table entry.
//!
//! Uses `Engine::gen_fn_signatures`, which requires rhai's `metadata`
//! feature - enabled here through this crate's dev-dependency so the
//! feature never leaks into normal builds.

use lumen_script_rhai::RhaiHost;
use lumen_script_rhai::builtins::BUILTINS;
use std::collections::HashSet;

/// Extract the function name (text before the first `(`) from a rhai
/// signature line such as `set_timeout(name: string, ms: i64) -> ()`.
fn sig_name(sig: &str) -> &str {
    sig.split('(').next().unwrap_or(sig).trim()
}

fn registered_names() -> HashSet<String> {
    let mut host = RhaiHost::new();
    // `false` = exclude the standard operator/package signatures so we
    // only see functions registered by `RhaiHost::new`.
    host.engine_mut()
        .gen_fn_signatures(false)
        .into_iter()
        .map(|s| sig_name(&s).to_string())
        .collect()
}

#[test]
fn every_table_entry_is_registered() {
    let registered = registered_names();
    for b in BUILTINS {
        assert!(
            registered.contains(b.name),
            "builtin `{}` is in builtins::BUILTINS but not registered on the engine",
            b.name
        );
    }
}

#[test]
fn no_untabled_free_functions() {
    // Custom-type methods / indexers / property accessors dispatch on a
    // receiver and are deliberately excluded from the table. Everything
    // else registered on the engine must have a table entry.
    let method_allowlist: HashSet<&str> = [
        "get",
        "set",
        "push",
        "len",
        "all",
        "value",
        "set_color",
        // Node / NodeQuery receiver methods (dispatch on a Node / NodeQuery
        // handle, so they are intentionally not table free-functions).
        "parent",
        "first_child",
        "last_child",
        "next",
        "prev",
        "children",
        "closest",
        "exists",
        "valid",
        "handle",
        "single",
        "get_single",
        "first",
        "nth",
        "iter",
        "collect",
        "is_empty",
        // Node mutators / read-backs (phases 2 + 3) - receiver methods on a
        // Node handle, not free-function builtins.
        "set_attr",
        "remove_attr",
        "set_id",
        "add_class",
        "remove_class",
        "toggle_class",
        "set_style",
        "style_set",
        "style_remove",
        "set_parent",
        "move_to",
        "append",
        "insert_before",
        "replace_with",
        "remove",
        "clone_deep",
        "get_attr",
        "id",
        "text",
        "has_class",
        "style_get",
        "computed_style",
        "computed_style_all",
        // Low-level introspection (phase 5) - receiver methods on a Node.
        "rect",
        "content_rect",
        "scroll",
        "is_visible",
        "z_index",
        "inline_style",
        "attrs",
        "classes",
        "matched_rules",
        "entity_id",
        "components",
        "component",
        "outer_markup",
        // Guarded markup injection (phase 6) - receiver methods on a Node.
        "inner_markup",
        "set_inner_markup",
        // `window` / `document` / `history` / `location` namespace methods
        // (section 4.8) - dispatch on the namespace constants.
        "set_href",
        "href",
        "reload",
        "title",
        "set_title",
        "size",
        "set_size",
        "dpr",
        "path",
        "hash",
        "back",
        "forward",
        "go",
        "focused",
        "hovered",
        "root",
        "create",
        // Event handle accessors (phase 4) - receiver methods on `Event`.
        "target",
        "current_target",
        "event_type",
        "key",
        "button",
        "x",
        "y",
        "client_x",
        "client_y",
        "delta_x",
        "delta_y",
        "position",
        "modifiers",
        "prevent_default",
        "stop_propagation",
        "stop_immediate_propagation",
        "on_capture",
        // Internal unbind the off token curries (phase 4) - not user-facing.
        "__lumen_off",
    ]
    .into_iter()
    .collect();

    let table: HashSet<&str> = BUILTINS.iter().map(|b| b.name).collect();
    for name in registered_names() {
        // Property getters/setters and indexers register under synthetic
        // `get$value` / `index$get$` style names - never bare builtins.
        if name.contains('$') {
            continue;
        }
        if method_allowlist.contains(name.as_str()) || table.contains(name.as_str()) {
            continue;
        }
        panic!(
            "engine registers `{name}` but it is neither in builtins::BUILTINS \
             nor the method allowlist - add a table entry or update the allowlist"
        );
    }
}
