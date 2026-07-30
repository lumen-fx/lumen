// =================================================================
// Lumen Rhai builtin stubs.
//
// These functions are *injected by the lumen-script-rhai runtime* -
// they're not part of the Rhai standard library, so external Rhai
// language servers / linters (rhai-lang-server, tree-sitter-rhai
// based tools) flag them as "unknown function" by default. Use this
// file as either:
//
//   - A reference for what's available inside <script> blocks.
//   - A stubs file your Rhai LSP can be pointed at to silence
//     "unknown function" warnings (most Rhai LSP configurations
//     accept a list of extra-signature files).
//
// All bodies are empty / return-typed placeholders - they exist
// only for tooling. The real implementations live in
// crates/lumen-script-rhai/src/lib.rs.
// =================================================================

// ---- Signals (reactive scalar store) ----------------------------

/// Return a handle to a named scalar signal, auto-initialising the
/// host-local mirror with `default` if the signal is unset. Call
/// `.get()` / `.set(v)` on the returned handle (or use `.value` for
/// Vue-flavoured `s.value = 1; let x = s.value`).
///
/// ```rhai
/// let count = signal("clicks", 0);
/// count.set(count.get() + 1);
/// // or:
/// count.value = count.value + 1;
/// ```
fn signal(name, default) {}

/// Return a handle to a named reactive array. Backs `<for each="name">`
/// markup. Handle methods: `.set(arr)`, `.push(item)`, `.len()`,
/// `.get(i)`.
///
/// ```rhai
/// let rows = signal_array("rows");
/// rows.set([#{ id: "1", label: "A" }]);
/// rows.push(#{ id: "2", label: "B" });
/// ```
fn signal_array(name) {}

// ---- DOM-style imperative text mutator --------------------------

/// Replace the `TextContent` of the entity whose `id="..."` matches.
/// Mostly superseded by `bind-text="<signal>"` + `signal_set`.
fn set_text(target_id, text) {}

// ---- Event router (per-id handler registry) ---------------------

/// Register a per-id event handler. When `<event>` fires on entity
/// `id`, the named function is called with the entity id instead of
/// the global `on_<event>(id)` fallback.
fn on(event, id, fn_name) {}

// ---- Timers (off-thread, fires on the main tick) ----------------

/// One-shot timer. After `ms` milliseconds the script's
/// `on_timer(name)` runs.
fn set_timeout(name, ms) {}

/// Repeating timer. Fires `on_timer(name)` every `ms` milliseconds
/// until `cancel_timer` is called.
fn set_interval(name, ms) {}

/// Cancel a pending or repeating timer.
fn cancel_timer(name) {}

// ---- HTTP fetch (off-thread, fires on next tick) ----------------

/// Issue an HTTP GET. The runtime spawns a worker thread; when the
/// response lands `on_fetch(tag, body)` runs. Errors fire
/// `on_fetch_error(tag, message)`.
fn fetch(url, tag) {}

/// Parse a JSON string into a Rhai Dynamic (map / array / scalar).
fn parse_json(body) {}

// ---- Diagnostics ------------------------------------------------

/// Append a line to the host's diagnostic output. Equivalent to
/// Rhai's `print` but captured by the host so it shows in the
/// `lumenc` stderr stream instead of stdout.
fn add_clicks(n) {}

/// Free-form key/value setter (app-interpreted; the host emits
/// the token without semantics).
fn set_string(key, value) {}
