//! Scripting backend trait (v2) + the `ScriptCommand` enum scripts emit.
//!
//! `lumen-core` does not depend on this crate. Scripts run inside
//! host-generic systems provided by `lumen-script`; those systems
//! drain the [`ScriptCommand`]s the host produced this tick and forward
//! them onto the ECS message bus for the embedder's applier.
//!
//! Concrete implementations (`lumen-script-rhai`, future `-candela`) provide
//! a [`ScriptHost`] that compiles + executes whatever source language they
//! speak, exposing the host-neutral registries (command sink, signal
//! mirror, per-id handlers, derivations) the generic runtime drives.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod builtins;

/// Host-neutral read side of the dynamic DOM API: `query` / `get_by_id` /
/// traversal over the per-tick [`lumen_core::node::DomIndex`] snapshot.
pub mod node_query;

/// Host-neutral event object, binding registry, and propagation driver
/// (phase 4): `n.on(type, handler)` / `off()`, the current-event cell, and
/// the capture -> target -> bubble dispatch.
pub mod event;

/// Host-neutral low-level introspection read surface (phase 5): post-layout
/// geometry, full computed style + provenance, typed component reads, tree
/// serialization, and global runtime state.
pub mod introspect;

/// Host-generic runtime event dispatch (phase 4): input messages ->
/// DOM events -> propagation. See [`dom_events::dispatch_pointer_and_key_events`].
pub mod dom_events;

/// Script-facing drag-and-drop event dispatchers (`on_drop` /
/// `on_drag_start`). See the module docs.
pub mod dnd;
/// Host-generic ECS driver: dispatchers, derivation fixed-point, mirror
/// sync, timers, HTTP fetch, and [`runtime::ScriptPlugin`]. Formerly the
/// `lumen-script-runtime` crate; folded in with its flat surface
/// re-exported at the crate root for source compatibility.
pub mod runtime;

use std::collections::HashSet;
use thiserror::Error;

pub use builtins::{BuiltinFn, BuiltinParam};
pub use dnd::{dispatch_drag_start_to_script, dispatch_drops_to_script};
pub use dom_events::{dispatch_pointer_and_key_events, dispatch_state_events};
pub use runtime::*;

/// Errors a [`ScriptHost`] can surface from `load` or `tick`.
///
/// Structured compile errors carry the source URI and the offending
/// `(line, col)` so editor / LSP layers can place a squiggle without
/// re-parsing the error message. `Runtime` is still a free-form string;
/// Rhai's runtime errors don't always carry positions and the
/// trait-level shape is "best effort".
#[derive(Debug, Error)]
pub enum ScriptError {
    /// Source failed to parse / compile.
    #[error("script compile error at {uri}:{line}:{col}: {message}")]
    Compile {
        /// Origin URI (e.g. `"main.rhai"` or `"lumen://app/main.rhai"`);
        /// `"<inline>"` when the source has no associated URI.
        uri: String,
        /// 1-based line number reported by the parser. `0` when the
        /// position is unknown.
        line: u32,
        /// 1-based column number reported by the parser. `0` when the
        /// position is unknown.
        col: u32,
        /// Stringified parser message (sans position prefix).
        message: String,
    },
    /// Source compiled but execution failed.
    #[error("script runtime error: {0}")]
    Runtime(String),
}

impl ScriptError {
    /// Build a `Compile` with no specific position info. Equivalent to
    /// the previous `Compile(String)` shape; used by backends that
    /// can't surface line/col yet.
    pub fn compile(message: impl Into<String>) -> Self {
        Self::Compile {
            uri: "<inline>".to_string(),
            line: 0,
            col: 0,
            message: message.into(),
        }
    }
}

/// One side-effect a script wants to apply to the app this tick.
///
/// V1 is intentionally small: just enough to demo end-to-end. Future
/// variants will cover `SetComponent`, `SpawnEntity`, `DespawnEntity`,
/// `RegisterTimer`, etc., keyed by an entity id type that's stable across
/// the script ABI boundary.
#[derive(Debug, Clone)]
pub enum ScriptCommand {
    /// Append a line to the app's diagnostic output.
    Print(String),
    /// Add `n` to the app's click counter (consumed in app-defined ways -
    /// the host doesn't know about Clicks, it just emits this token).
    AddClicks(i32),
    /// Free-form key/value the app can interpret. Strings only for now.
    SetString {
        /// Key (e.g. `"title"`, `"label"`).
        key: String,
        /// Value.
        value: String,
    },
    /// Replace the `TextContent` of the entity whose `LumenId` matches
    /// `target_id` (set in markup via `id="..."`).
    SetText {
        /// Target entity id.
        target_id: String,
        /// New text content.
        text: String,
    },
    /// Replace the asset path on an `<image>` entity. The runtime
    /// strips the old `LoadedImage` / `LoadedSvg` / `ImageLoadFailed`
    /// component, installs a fresh `ImageSource`, and the asset
    /// pipeline re-decodes on the next tick. Used by scripts to swap
    /// icons in response to runtime state (e.g. weather code ->
    /// `icons/sun.png`).
    SetSrc {
        /// Target entity id.
        target_id: String,
        /// New asset path. Resolved relative to the app directory at
        /// load time on the parser side; runtime mutations pass paths
        /// through verbatim, so authors should compose relative paths
        /// against their own app root (e.g. `"icons/sun.png"`).
        path: String,
    },
    /// Schedule a named timer. The runtime calls the script's
    /// `on_timer(name)` handler when it fires.
    SetTimer {
        /// Timer name, also passed as the `on_timer` argument.
        name: String,
        /// Delay until first fire, in milliseconds.
        millis: u64,
        /// If true, reschedule with the same interval after each fire.
        repeat: bool,
    },
    /// Cancel a pending or repeating timer by name. No-op if absent.
    CancelTimer {
        /// Timer name to cancel.
        name: String,
    },
    /// Issue an HTTP GET. The runtime spawns the request off-thread and
    /// fires `on_fetch(tag, body)` when the response arrives.
    ///
    /// This is the simple sugar; [`Self::Http`] is the general form and
    /// shares the same off-thread transport and completion channel.
    Fetch {
        /// URL to GET.
        url: String,
        /// Identifier the script gets back in `on_fetch(tag, body)`.
        tag: String,
    },
    /// Issue a general HTTP request. The runtime performs it off-thread
    /// (one worker per request) and, once the reply is marshalled back
    /// onto the ECS/UI thread, fires `on_http(tag, response)` where
    /// `response` is a map `#{ ok, status, headers, body, error }`.
    ///
    /// Request/reply shape is modeled on Qt's
    /// `QNetworkRequest`/`QNetworkReply` (method + url + headers + body
    /// in; status + headers + body + error out); the worker->UI-thread
    /// marshalling before any signal is touched mirrors Slint's
    /// `invoke_from_event_loop` discipline. `web`-`fetch`-like: a 4xx /
    /// 5xx is a *completed* reply the script can branch on (via
    /// `response.ok` / `response.status`), not an error.
    Http {
        /// HTTP method (`GET`, `POST`, `PUT`, `DELETE`, ...). Case-insensitive.
        method: String,
        /// Target URL.
        url: String,
        /// Request headers as ordered key/value pairs.
        headers: Vec<(String, String)>,
        /// Optional request body. `None` sends no body.
        body: Option<String>,
        /// Optional per-request timeout in milliseconds (`None` = no
        /// client-imposed deadline).
        timeout_ms: Option<u64>,
        /// Identifier echoed back in `on_http(tag, response)`.
        tag: String,
    },
    /// Write `value` into the named slot of the reactive [`Signals`]
    /// map. Any entity carrying a `Bind*` component pointing at this name
    /// re-renders next tick. Strings only at this layer; numeric / bool
    /// signals are stringified before storage.
    SetSignal {
        /// Signal name.
        name: String,
        /// New value.
        value: String,
    },
    /// Typed write into [`lumen_core::property_store::PropertyStore`].
    /// Unlike [`Self::SetSignal`], this variant carries a typed
    /// [`PropertyValue`] (i64 / f64 / bool / color / vec2 / arc-str /
    /// custom) and lands directly in the typed cell - no stringify, no
    /// [`Signals`] mirror. Used by typed Rhai builtins
    /// (`signal_set_int` / `_float` / `_bool` / `_color`) and the C
    /// FFI typed setters when they want to enqueue through the script
    /// command bus instead of the cross-thread `push_external_property`
    /// channel. The Rhai host today prefers the channel path
    /// (immediate, no tick round-trip); this variant is the
    /// equivalent for hosts that want to defer until the next
    /// `apply_script_commands` drain (gives the system one
    /// frame to coalesce typed writes).
    SetProperty {
        /// Property key (global or entity-scoped).
        key: lumen_core::property_store::PropertyKey,
        /// New typed value.
        value: lumen_core::property_store::PropertyValue,
    },
    /// Replace the named entry in the reactive `ArraySignals` map with a
    /// fresh ordered vector of records. Each record's field values are
    /// pre-stringified at the script boundary so the receiving system
    /// doesn't need to deserialize per-frame. Any `<for each="<name>">`
    /// reconciler then spawns / despawns child markup to match.
    SetArray {
        /// Array name.
        name: String,
        /// Items, each a flat map of field -> stringified value.
        items: Vec<std::collections::HashMap<String, String>>,
    },
    /// Show a native desktop notification. Cross-platform via
    /// `notify-rust` (libnotify on Linux, NSUserNotification on macOS,
    /// Toast on Windows).
    Notify {
        /// Notification summary / title.
        title: String,
        /// Body text.
        body: String,
    },
    /// Reads the PNG at `path` and writes its pixels to the system clipboard.
    /// `path` is resolved relative to the app directory by the runtime handler.
    CopyImageToClipboard {
        /// Source PNG path.
        path: String,
    },
    /// Pulls the current clipboard image (when present) and writes it as PNG to `path`. Failures log to stderr.
    SaveClipboardImage {
        /// Destination PNG path.
        path: String,
    },
    /// Registers or replaces a system tray icon (macOS/Windows). On Linux logs a warning and no-ops. `icon_path` resolves relative to the app dir.
    RegisterTrayIcon {
        /// Stable id; clicks fire `on_tray(id)`.
        id: String,
        /// Path to a PNG icon.
        icon_path: String,
        /// Optional hover tooltip.
        tooltip: Option<String>,
    },
    /// Drops a previously-registered tray icon by id.
    UnregisterTrayIcon {
        /// Matching id from `RegisterTrayIcon`.
        id: String,
    },
    /// Replace the `LumenClasses` of the entity whose `LumenId`
    /// matches `target_id` (or the root entity when `target_id` is
    /// `"<root>"`). Triggers a runtime CSS re-apply (K9) - the
    /// downstream system detects `Changed<LumenClasses>` on the root
    /// and re-spawns the tree with the new class set.
    SetClasses {
        /// Target entity id, or the sentinel `<root>` to address the
        /// markup root.
        target_id: String,
        /// Whitespace-separated class names - same shape as
        /// `class="..."` markup attr.
        classes: String,
    },
    /// Open a native file dialog (open, multi-open, save, or folder
    /// pick). The runtime serves the dialog on the main thread via
    /// `rfd`, then fires `on_file_picked(tag, path)` /
    /// `on_files_picked(tag, paths_joined_by_pipe)` /
    /// `on_folder_picked(tag, path)` (or the per-id `on()` route)
    /// once the user closes it.
    OpenFileDialog {
        /// What kind of dialog to show.
        kind: FileDialogKind,
        /// Identifier the script gets back in the event handler.
        tag: String,
        /// Filter list: `(label, extensions)` pairs. Empty = no
        /// filter (show all files).
        filters: Vec<(String, Vec<String>)>,
        /// Default filename suggestion for `Save`. Ignored for the
        /// other kinds.
        default_name: Option<String>,
    },
    /// Register an OS-level global hotkey. Accelerator strings follow
    /// the `keyboard-types`/Electron convention (`"CommandOrControl+S"`,
    /// `"Alt+Space"`, `"F11"`). `on_hotkey(name)` fires every time the
    /// OS dispatches the chord regardless of window focus.
    RegisterHotkey {
        /// Identifier the script gets back in `on_hotkey(name)`.
        name: String,
        /// Accelerator string. See `global-hotkey::HotKey::from_str`.
        accelerator: String,
    },
    /// Unregister a previously-registered hotkey by name. No-op if
    /// the name is unknown.
    UnregisterHotkey {
        /// Identifier matching the previous `RegisterHotkey` call.
        name: String,
    },
    /// Load and start playing an audio track. `path` is app-relative and
    /// resolved against the app directory by the embedder's applier.
    /// Replaces any current track and resets position to 0. The
    /// web-`<audio src>` + `play()` analog; Qt's `setSource` + `play`.
    AudioPlay {
        /// App-relative path to a decodable audio file (wav / ogg).
        path: String,
    },
    /// Pause the audio transport, holding its position. Qt: `pause`.
    AudioPause,
    /// Resume a paused transport. Qt: `play` from `PausedState`.
    AudioResume,
    /// Stop the transport and rewind to 0. Qt: `stop`.
    AudioStop,
    /// Seek the transport to `secs` seconds (clamped to the track
    /// duration). Qt: `setPosition`.
    AudioSeek {
        /// Target position in seconds.
        secs: f64,
    },
    /// Set output volume in `0.0..=1.0`. Qt: `QAudioOutput::setVolume`.
    AudioVolume {
        /// Linear gain, clamped to `0.0..=1.0` by the applier.
        level: f32,
    },

    // -- dynamic DOM: change things (phase 2) --------------------------
    /// Set an attribute on a node addressed by packed handle. KNOWN attrs
    /// (`id`, `class`, `src`, `text`, `disabled`, ...) route to their typed
    /// component; everything else lands in the generic
    /// [`lumen_core::components::LumenAttributes`] map.
    SetAttr {
        /// Target node (packed handle or reserved spawn token).
        node: u64,
        /// Attribute name.
        name: String,
        /// New value.
        value: String,
    },
    /// Remove an attribute from a node addressed by packed handle.
    RemoveAttr {
        /// Target node.
        node: u64,
        /// Attribute name to clear.
        name: String,
    },
    /// Replace a node's text content (`node.set_text`).
    SetNodeText {
        /// Target node.
        node: u64,
        /// New text.
        text: String,
    },
    /// Add one class to a node's class list (incremental, idempotent).
    ClassAdd {
        /// Target node.
        node: u64,
        /// Class name to add.
        class: String,
    },
    /// Remove one class from a node's class list.
    ClassRemove {
        /// Target node.
        node: u64,
        /// Class name to remove.
        class: String,
    },
    /// Toggle one class on a node's class list.
    ClassToggle {
        /// Target node.
        node: u64,
        /// Class name to toggle.
        class: String,
    },
    /// Set an inline style property (`element.style`), the highest cascade
    /// tier the runtime re-apply reads.
    SetStyleProp {
        /// Target node.
        node: u64,
        /// CSS property name.
        name: String,
        /// CSS value.
        value: String,
    },
    /// Remove an inline style property.
    RemoveStyleProp {
        /// Target node.
        node: u64,
        /// CSS property name to clear.
        name: String,
    },

    // -- dynamic DOM: build things (phase 3) ---------------------------
    /// Create a fresh detached element with markup tag `tag`. `reserved`
    /// is the token the host minted synchronously so the same tick's
    /// chained mutations address the node; the applier maps it onto the
    /// entity it spawns.
    Spawn {
        /// Markup tag (`div`, `button`, ...).
        tag: String,
        /// Reserved spawn token (see
        /// [`lumen_core::node::reserve_node_token`]).
        reserved: u64,
    },
    /// Attach `node` under `parent`. `before` (when non-zero) is a
    /// reference child to insert ahead of (`insert_before`); zero appends
    /// at the end (`append` / `set_parent` / `move_to` / `reparent` all
    /// route here).
    Insert {
        /// Parent node.
        parent: u64,
        /// Node to attach (detaching it from any current parent).
        node: u64,
        /// Reference child to insert before, or `0` to append.
        before: u64,
    },
    /// Replace `old` with `new` in `old`'s parent, then despawn `old`'s
    /// subtree (`replaceWith`).
    ReplaceWith {
        /// Node being replaced (removed after).
        old: u64,
        /// Replacement node, moved into `old`'s slot.
        new: u64,
    },
    /// Detach and despawn a node and its whole subtree (`node.remove`).
    RemoveNode {
        /// Node to remove.
        node: u64,
    },
    /// Deep-clone `source`'s subtree into a fresh detached node. `reserved`
    /// is the token the host minted for the clone root.
    CloneNode {
        /// Node to clone (with descendants).
        source: u64,
        /// Reserved spawn token for the clone root.
        reserved: u64,
    },
    /// Replace `node`'s children with the subtree parsed from `markup`
    /// (`element.innerHTML = ...`). The markup is parsed by the injected
    /// front-end and spawned through the same path the `<for>` reconciler
    /// uses, so layout / style / paint stay consistent.
    ///
    /// Guarded: parsing needs the injected markup front-end
    /// ([`SourceParser`](crate) is a runtime concept), which is present on
    /// the dev / from-source run path but absent in the precompiled-artifact
    /// path; there it is a no-op. Do not feed untrusted content; this
    /// injects live markup (XSS-adjacent).
    SetInnerMarkup {
        /// Target node whose children are replaced.
        node: u64,
        /// Markup fragment to parse and spawn as the new children.
        markup: String,
    },

    // -- dynamic DOM: events (phase 4) ---------------------------------
    /// Bind an event handler to a node. `token` is the off token the host
    /// minted synchronously; the host holds the handler closure keyed by
    /// the same token, and the applier records `(node, event_type, capture)`
    /// in the host-neutral binding registry the dispatcher consults.
    /// `handler_ref` is the token (a host-neutral reference), consistent
    /// with the reserved-token model the structural commands use.
    BindEvent {
        /// Target node (packed handle or reserved spawn token).
        node: u64,
        /// Event type (`"click"`, `"keydown"`, ...).
        event_type: String,
        /// `true` for a capture-phase listener.
        capture: bool,
        /// Off / handler token (`handler_ref`).
        token: u64,
    },
    /// Unbind a previously-bound event handler by its off token
    /// (`removeEventListener`). No-op if the token is unknown.
    UnbindEvent {
        /// Off token returned by the matching `on(...)`.
        token: u64,
    },

    // -- window / document (section 4.8) -------------------------------
    /// Set the OS window title (`window.set_title`).
    WindowSetTitle {
        /// New title string.
        title: String,
    },
    /// Resize the OS window to `width` x `height` logical pixels
    /// (`window.set_size`).
    WindowSetSize {
        /// Logical width.
        width: f32,
        /// Logical height.
        height: f32,
    },
}

/// File dialog flavour for [`ScriptCommand::OpenFileDialog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialogKind {
    /// Pick one existing file. Fires `on_file_picked(tag, path)`.
    Open,
    /// Pick multiple existing files. Fires
    /// `on_files_picked(tag, "p1|p2|p3")` (paths joined with `|`).
    OpenMulti,
    /// Pick a save destination (may not exist yet). Fires
    /// `on_file_picked(tag, path)` (same handler as `Open` since the
    /// shape is identical).
    Save,
    /// Pick a folder. Fires `on_folder_picked(tag, path)`.
    PickFolder,
}

/// Typed sum the script-host trait understands across the boundary.
///
/// Mirrors what `rhai::Dynamic` would carry - scalars, arrays, maps -
/// without leaking the Rhai type into the trait. Backends translate
/// to / from their native value type.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    /// Absence of a value (Rhai `()`).
    Unit,
    /// Boolean payload.
    Bool(bool),
    /// Signed 64-bit integer payload.
    I64(i64),
    /// 64-bit float payload.
    F64(f64),
    /// Owned string payload.
    Str(String),
    /// Heterogeneous list.
    Array(Vec<ScriptValue>),
    /// Field map (insertion order is not preserved across the boundary).
    Map(std::collections::HashMap<String, ScriptValue>),
}

impl From<bool> for ScriptValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<i64> for ScriptValue {
    fn from(v: i64) -> Self {
        Self::I64(v)
    }
}

impl From<f64> for ScriptValue {
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<String> for ScriptValue {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}

impl<'a> From<&'a str> for ScriptValue {
    fn from(v: &'a str) -> Self {
        Self::Str(v.to_string())
    }
}

impl ScriptValue {
    /// Stringify any variant. Strings stay verbatim; everything else
    /// uses the canonical `Display` form. UNIT becomes the empty string.
    pub fn stringify(&self) -> String {
        match self {
            Self::Unit => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::I64(i) => i.to_string(),
            Self::F64(f) => f.to_string(),
            Self::Str(s) => s.clone(),
            Self::Array(arr) => {
                let parts: Vec<String> = arr.iter().map(Self::stringify).collect();
                format!("[{}]", parts.join(", "))
            }
            Self::Map(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let parts: Vec<String> = keys
                    .iter()
                    .map(|k| format!("{}: {}", k, m[*k].stringify()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }
}

/// Backend-agnostic facade exposing the reactive property store to
/// script hosts. Mirrors QML's `QQmlContext::setContextProperty` /
/// `contextProperty()` shape: scripts read + write through a thin layer
/// the host keeps internally so callers don't have to thread an
/// `Engine` or a per-backend `Dynamic` type through.
///
/// Concrete impls (e.g. `RhaiScriptContext` in `lumen-script-rhai`)
/// own the underlying signal store. The trait is intentionally small:
/// every method maps to one host-side write so backends can rebuild
/// their internal mirror without re-implementing scalar/array policy
/// on top of `Dynamic`.
pub trait ScriptContext {
    /// Read a named scalar / structured property. `None` => the property
    /// has never been written.
    fn get(&self, name: &str) -> Option<ScriptValue>;

    /// Write a named scalar / structured property. The backend mirrors
    /// the write into its native value type and queues a stringified
    /// `SetSignal` for the ECS-side `Signals` resource.
    fn set(&mut self, name: &str, value: ScriptValue);

    /// Append `value` onto the named array property. Lazy-initialises
    /// the array if absent. Drives `<for each="<name>">` reconciliation.
    fn array_push(&mut self, name: &str, value: ScriptValue);

    /// Clear the named array property (set to empty).
    fn array_clear(&mut self, name: &str);

    /// Entity-scoped read for properties keyed by `(entity_id, name)`.
    /// Returns `None` when the backend doesn't model entity-scoped
    /// properties (most backends do not).
    fn entity(&self, entity_id: &str, name: &str) -> Option<ScriptValue> {
        let _ = (entity_id, name);
        None
    }
}

/// Outcome of one [`ScriptHost::call`] invocation.
///
/// `commands` is the full drain of the host's command sink at return -
/// note it is drained **even when `found == false`** (builtins invoked
/// outside handlers may have queued commands; the v1 host behaved this
/// way and the generic runtime relies on it).
#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// Commands the host's builtins pushed into the sink during (and
    /// before) the call, drained at return.
    pub commands: Vec<ScriptCommand>,
    /// The script function's return value. `None` when the function was
    /// not found (or no program is loaded). Carries meaning for hooks
    /// like `on_close()` -> `Bool(false)` vetoes the close.
    pub ret: Option<ScriptValue>,
    /// `false` when the function does not exist - the runtime probes
    /// optional handlers and treats a miss as silent success.
    pub found: bool,
}

/// Portable native-command extension: a Rust closure the embedder
/// registers under a script-callable name via
/// [`ScriptHost::register_command_fn`]. Receives the call args converted
/// to [`ScriptValue`]s; returns commands to push into the host sink.
pub type CommandFn = std::sync::Arc<dyn Fn(&[ScriptValue]) -> Vec<ScriptCommand> + Send + Sync>;

/// Backend that compiles + executes scripts and exposes the host-neutral
/// registries the generic runtime (`lumen-script`) drives: the
/// command sink, the rich-typed signal mirror, the per-id handler
/// registry, and the derivation registry.
///
/// `Send + Sync` bound: hosts sit in a plain bevy `Resource` and
/// participate in parallel scheduling. `RhaiHost` qualifies because the
/// workspace pins rhai's `sync` feature. A future host that cannot be
/// `Send` will need a `NonSend` plugin variant in the runtime crate.
///
/// One script-side callable the generic runtime can re-invoke is modeled
/// by [`Self::Closure`] - Rhai: `FnPtr`; candela: a function handle once its
/// embedding API lands.
pub trait ScriptHost: Send + Sync + 'static {
    /// Host-native handle to a script closure (a `derive(...)` body).
    type Closure: Clone + Send + Sync + 'static;

    // -- lifecycle -----------------------------------------------------

    /// Compile-only, side-effect free, with the exact engine settings
    /// [`Self::load`] uses - `lumenc check` and `run` must agree on what
    /// parses. `uri` seeds [`ScriptError::Compile::uri`].
    fn compile_check(&self, source: &str, uri: &str) -> Result<(), ScriptError>;

    /// Compile + evaluate the top level into persistent state.
    /// Fresh-start load; replaces any previously loaded program.
    fn load(&mut self, source: &str, uri: &str) -> Result<(), ScriptError>;

    /// Hot reload: compile FIRST (no state touched on parse error), then
    /// atomically swap - snapshot registries, clear, re-evaluate the top
    /// level into the EXISTING persistent scope, FULL rollback on eval
    /// failure. Persistent across the swap: engine registrations, scope
    /// bindings, the signal mirror, and the in-flight command sink.
    fn replace(&mut self, source: &str, uri: &str) -> Result<(), ScriptError>;

    /// Drop the loaded program and all persistent state. Genuine
    /// restart; hot reload should use [`Self::replace`].
    fn reset(&mut self);

    // -- invocation (reactive-only: no per-frame hook in this trait) ---

    /// Call a script function by name. A missing function is silent
    /// success (`found: false`); see [`CallOutcome`].
    fn call(&mut self, fn_name: &str, args: &[ScriptValue]) -> Result<CallOutcome, ScriptError>;

    /// Re-entrant closure invocation. Must be callable while the generic
    /// runtime holds NO host locks (snapshot-then-call pattern): the
    /// closure body may re-enter builtins that touch the sink / mirror.
    fn call_closure(
        &mut self,
        closure: &Self::Closure,
        args: &[ScriptValue],
    ) -> Result<ScriptValue, ScriptError>;

    /// Evaluate one derivation: read the dep values, invoke `closure`,
    /// store the rich result into the mirror under `name`, and return
    /// the host-canonical stringification for the ECS property store.
    ///
    /// Deviation from the original v2 sketch (documented in the design
    /// doc section 1.9): the default impl composes `mirror_get` -> `call_closure`
    /// -> `mirror_set` -> [`ScriptValue::stringify`], but hosts should
    /// override with a native path - round-tripping dep values through
    /// [`ScriptValue`] loses fidelity for host-native structured values,
    /// and stringification is host-canonical (Rhai renders `1.0` where
    /// Rust's `f64` Display renders `1`; store strings feed `bind-text`
    /// and the parse-back policy, so the host must own the format).
    fn eval_derivation(
        &mut self,
        closure: &Self::Closure,
        deps: &[String],
        name: &str,
    ) -> Result<String, ScriptError> {
        let args: Vec<ScriptValue> = deps
            .iter()
            .map(|d| self.mirror_get(d).unwrap_or(ScriptValue::Unit))
            .collect();
        let value = self.call_closure(closure, &args)?;
        let text = value.stringify();
        self.mirror_set(name, value);
        Ok(text)
    }

    // -- command sink --------------------------------------------------

    /// Drain commands queued by builtins since the last drain.
    fn drain_commands(&mut self) -> Vec<ScriptCommand>;

    /// Put commands back into the sink so they flow through the next
    /// tick's normal drain (the `on_start` re-stash).
    fn push_commands(&mut self, cmds: Vec<ScriptCommand>);

    // -- signal mirror (host-local, rich-typed) ------------------------

    /// Read a mirror entry. `None` => never written.
    fn mirror_get(&self, name: &str) -> Option<ScriptValue>;

    /// Write a mirror entry (host-native conversion of `value`).
    fn mirror_set(&mut self, name: &str, value: ScriptValue);

    /// section 1.3 type-preserving parse-back of a store string. The trait pins
    /// the POLICY: a mirror entry currently holding a scalar (bool / int
    /// / float) parses the string back into that SAME type; structured
    /// mirror values (arrays, maps) stay authoritative and ignore the
    /// string; unparseable strings leave the mirror untouched; absent /
    /// string entries take the store string verbatim.
    fn mirror_sync_str(&mut self, name: &str, value: &str);

    // -- registries populated by host builtins, read by the runtime ----

    /// Per-id handler lookup (`on(event, id, fn)`), including the
    /// template-suffix fallback: a handler registered for `save` also
    /// matches `user-card:save` via the last-`:` suffix.
    fn handler_for(&self, event: &str, key: &str) -> Option<String>;

    /// Snapshot of derivations matching `dirty` plus `pending`
    /// `(name, deps, closure)` - taken OUTSIDE any host lock so the
    /// driver can invoke closures re-entrantly.
    fn derivations_matching(
        &self,
        dirty: &HashSet<&str>,
        pending: &HashSet<String>,
    ) -> Vec<(String, Vec<String>, Self::Closure)>;

    /// Names of derivations registered but never successfully evaluated;
    /// they all run on the next derivation pass regardless of dirt.
    fn pending_initial(&self) -> HashSet<String>;

    /// Remove successfully-evaluated names from the pending-initial set.
    /// Erroring derivations stay pending and retry next tick.
    fn clear_pending(&mut self, evaluated: &[String]);

    // -- event handlers (phase 4) --------------------------------------

    /// Invoke the event-handler closure the host registered under `token`
    /// (via its `on(node, type, handler)` builtin). The host builds its
    /// native event object, which reads the process-global current-event
    /// cell ([`crate::event`]); commands the handler queues are drained via
    /// [`Self::drain_commands`] afterwards by the caller. Returns whether a
    /// closure was found and run.
    ///
    /// Default: not supported (no closure registry) -> `Ok(false)`. Hosts
    /// that expose `on(...)` with real closures override this.
    fn dispatch_event_handler(&mut self, token: u64) -> Result<bool, ScriptError> {
        let _ = token;
        Ok(false)
    }

    /// Drop the event-handler closure registered under `token` (the host
    /// side of `off()` / `UnbindEvent`). Default: no-op.
    fn drop_event_handler(&mut self, token: u64) {
        let _ = token;
    }

    // -- extension -----------------------------------------------------

    /// Register a portable native command function callable from script
    /// as `name(...)` with `arity` positional args. Host-specific escape
    /// hatches (e.g. `RhaiHost::engine_mut`) remain available for
    /// anything this shape cannot express.
    fn register_command_fn(
        &mut self,
        name: &str,
        arity: usize,
        f: CommandFn,
    ) -> Result<(), ScriptError>;

    // -- metadata ------------------------------------------------------

    /// Language tag (`"rhai"`, `"candela"`). Used in diagnostics prefixes
    /// (`lumen-script-<lang>: ...`).
    fn lang(&self) -> &'static str;

    /// The builtin-function metadata table feeding LSP completion /
    /// hover and the parity test.
    fn builtins(&self) -> &'static [BuiltinFn];
}

/// Marker trait reserving the script-side state-proxy contract. Currently empty.
pub trait StateProxy {}
