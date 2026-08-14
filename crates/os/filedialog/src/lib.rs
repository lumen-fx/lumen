//! Native file-dialog host for Lumen.
//!
//! Wraps `rfd` 0.17 behind a [`FileDialogService`] resource + ECS
//! [`FileDialogResult`] message. Mirrors `QFileDialog` (Qt) and
//! `GtkFileDialog` (GTK 4) - both are spec'd as one-shot modals that
//! emit a single result back to the application loop.
//!
//! ## Async through the core Spawn seam
//!
//! [`FileDialogService::open_single`] does not block the main thread when
//! the app installs an async backend. The crate never names one: it reads
//! [`SpawnService`] out of the world, which `lumen-async-tokio`'s plugin
//! publishes (and the Lumen runtime installs that plugin for an app that
//! opens dialogs). The call then:
//!
//! 1. Allocates a fresh [`RequestId`] and returns it to the caller.
//! 2. Spawns the rfd `pick_file().await` (or its `pick_files` / `save`
//!    / `pick_folder` siblings) on the installed executor.
//! 3. The spawned task ferries the resolved paths back across the
//!    thread boundary as a [`Command::Typed`] payload of type
//!    [`FileDialogResultCommand`] on the [`CommandQueue`].
//! 4. The command drain applies it on the next tick and emits one
//!    [`FilePicked`] per request.
//!
//! With no executor installed the same call runs the dialog inline with
//! `pollster::block_on` and posts the identical command, so a caller sees
//! one behaviour with one latency difference. On macOS the blocking form
//! would deadlock the run loop (`NSOpenPanel` only resolves while the run
//! loop pumps), so there it refuses and reports an empty result: a macOS
//! app needs an async backend for dialogs to reach the user.
//!
//! The legacy [`FileDialogService::open`] (`MessageWriter`-flavoured)
//! is preserved for callers that have not migrated. It has no access to
//! the world, so it always takes the blocking path.
//!
//! ## Why a request id + drain instead of a oneshot channel?
//!
//! Returning a oneshot to the caller would require the caller to poll
//! it on the main thread - which means a per-tick busy-poll system. The
//! [`Command::Typed`] route already exists for cross-thread main-world
//! mutation; reusing it keeps the dispatch story consistent with
//! everything else that reports back into a tick.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::any::TypeId;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use lumen_core::app::{App, Plugin};
use lumen_core::command::{Command, CommandQueue, CommandReceiver, apply_property_commands};
use lumen_core::task::{BoxFuture, Spawn, SpawnService};
use lumen_core::tick::TickStage;
use rfd::AsyncFileDialog;
use tracing::warn;

pub use lumen_os_mime as mime;

/// Reuse the message defined in lumen-core so existing scripting
/// dispatchers keep working unchanged.
pub use lumen_core::input::FilePicked as FileDialogResult;

/// Opaque identifier returned by [`FileDialogService::open_single`].
///
/// Callers that need to correlate a request with its eventual
/// [`FilePicked`] message can read [`FilePicked::tag`] - the tag is
/// carried verbatim from the [`FileDialogRequest`] through the async
/// pipeline. The `RequestId` itself is exposed so script hosts can
/// store per-request callbacks in a `HashMap<RequestId, RhaiFn>`
/// without colliding with the user-facing tag.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl From<u64> for RequestId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<RequestId> for u64 {
    fn from(id: RequestId) -> Self {
        id.0
    }
}

/// Kind of dialog to display.
///
/// Mirrors the four `QFileDialog` static helpers
/// (`getOpenFileName`, `getOpenFileNames`, `getSaveFileName`,
/// `getExistingDirectory`) and `GtkFileDialog::open` /
/// `open_multiple` / `save` / `select_folder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileDialogKind {
    /// Single-file open dialog.
    #[default]
    Open,
    /// Multi-file open dialog.
    OpenMulti,
    /// Save-file dialog (with optional default name).
    Save,
    /// Pick a directory.
    PickFolder,
}

impl FileDialogKind {
    /// Label embedded in [`FileDialogResult::kind`] so the scripting
    /// dispatcher can route by dialog type. Matches the previous
    /// lumenc strings (`"open"`, `"open_multi"`, `"save"`, `"folder"`)
    /// for backwards compatibility.
    pub fn label(self) -> &'static str {
        match self {
            FileDialogKind::Open => "open",
            FileDialogKind::OpenMulti => "open_multi",
            FileDialogKind::Save => "save",
            FileDialogKind::PickFolder => "folder",
        }
    }
}

/// A single `(label, exts)` filter entry - same shape `rfd` accepts
/// via `FileDialog::add_filter`.
///
/// Mirrors `QFileDialog::setNameFilters` ("Images (*.png *.jpg)") and
/// `GtkFileFilter::add_pattern`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeFilter {
    /// Human label shown in the dialog's filter dropdown.
    pub label: String,
    /// File extensions (no leading dot).
    pub exts: Vec<String>,
}

impl From<(&str, &[&str])> for MimeFilter {
    fn from((label, exts): (&str, &[&str])) -> Self {
        Self {
            label: label.to_string(),
            exts: exts.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl From<(String, Vec<String>)> for MimeFilter {
    fn from((label, exts): (String, Vec<String>)) -> Self {
        Self { label, exts }
    }
}

/// Builder describing one dialog spawn. Reusable: callers fill in the
/// shape and hand it to [`FileDialogService::open_single`].
#[derive(Clone, Debug, Default)]
pub struct FileDialogRequest {
    /// Which of the four dialog kinds to display.
    pub kind: FileDialogKind,
    /// Identifier the caller passes through to the resolved
    /// [`FileDialogResult::tag`] so a script can route by call-site.
    pub tag: String,
    /// Optional `(label, exts)` filter entries.
    pub filters: Vec<MimeFilter>,
    /// Optional default filename for Save dialogs.
    pub default_name: Option<String>,
}

/// Cross-thread payload pushed by the spawned tokio task back to the
/// main world via [`AsyncCommandQueue`].
///
/// [`drain_file_dialog_results`] reads `Command::Typed` of this type
/// and emits the corresponding [`FilePicked`] message.
#[derive(Debug, Clone)]
pub struct FileDialogResultCommand {
    /// Request id returned by [`FileDialogService::open_single`].
    pub request_id: RequestId,
    /// `"open"` / `"open_multi"` / `"save"` / `"folder"`.
    pub kind: &'static str,
    /// Caller-supplied tag carried through unchanged.
    pub tag: String,
    /// Resolved paths. Empty when the user cancelled.
    pub paths: Vec<PathBuf>,
}

impl From<FileDialogResultCommand> for FileDialogResult {
    fn from(c: FileDialogResultCommand) -> Self {
        Self {
            kind: c.kind,
            tag: c.tag,
            paths: c.paths,
        }
    }
}

/// File-dialog host resource.
///
/// Holds a monotonically increasing request-id counter so multiple
/// in-flight dialogs (e.g. an app opens a save-dialog while an unrelated
/// open-dialog is still pending) can be correlated independently.
#[derive(Resource, Clone)]
pub struct FileDialogService {
    next_id: Arc<AtomicU64>,
}

impl Default for FileDialogService {
    fn default() -> Self {
        Self::new()
    }
}

impl FileDialogService {
    /// Construct a fresh service. Request ids start at 1.
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Allocate the next request id.
    fn alloc_id(&self) -> RequestId {
        RequestId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Fire-and-forget dialog request.
    ///
    /// Reads the [`SpawnService`] and the [`CommandQueue`] out of `world`,
    /// starts the dialog, and returns the freshly allocated [`RequestId`]
    /// immediately. The eventual [`FilePicked`] message arrives on the tick
    /// after the user closes the dialog.
    ///
    /// With no async backend installed the dialog runs inline and the same
    /// message arrives on the next tick; see the crate docs for the macOS
    /// caveat that comes with that.
    pub fn open_single(&self, world: &mut World, req: FileDialogRequest) -> RequestId {
        let spawn = world.get_resource::<SpawnService>().cloned();
        let Some(queue) = world.get_resource::<CommandQueue>().cloned() else {
            warn!(
                "lumen-os-filedialog: no CommandQueue in the world, dropping \
                 the '{}' dialog request",
                req.tag
            );
            return self.alloc_id();
        };
        self.open_single_with(spawn.as_deref(), &queue, req)
    }

    /// Explicit-resources variant of [`Self::open_single`]. Handy inside a
    /// system that already holds `Option<Res<SpawnService>>` and
    /// `Res<CommandQueue>` and doesn't want to round-trip via `&mut World`.
    ///
    /// `spawn` of `None` selects the blocking path.
    pub fn open_single_with(
        &self,
        spawn: Option<&dyn Spawn>,
        queue: &CommandQueue,
        req: FileDialogRequest,
    ) -> RequestId {
        let pending = PendingDialog {
            request_id: self.alloc_id(),
            kind: req.kind.label(),
            tag: req.tag.clone(),
        };
        let request_id = pending.request_id;
        let kind = req.kind;
        // The dialog is built inside each arm because only one of them runs
        // and an `rfd` builder cannot be handed to both.
        let spawned_req = req.clone();
        dispatch_dialog(
            spawn,
            queue,
            pending,
            move || Box::pin(resolve(build_dialog(&spawned_req), kind)),
            move || blocking_resolve(build_dialog(&req), kind),
        );
        request_id
    }

    /// Legacy `MessageWriter`-flavoured entry point kept for back-compat
    /// with `lumenc::run::apply_script_commands`.
    ///
    /// This entry point has no access to the world, so it always takes the
    /// blocking path and writes the result straight to `out`. Callers that
    /// can reach the world should use [`Self::open_single`] or
    /// [`Self::open_single_with`], which use an installed executor when
    /// there is one.
    pub fn open(&self, req: &FileDialogRequest, out: &mut MessageWriter<FileDialogResult>) {
        let paths = blocking_resolve(build_dialog(req), req.kind);
        out.write(FileDialogResult {
            kind: req.kind.label(),
            tag: req.tag.clone(),
            paths,
        });
    }
}

/// A request that has been given an id but not yet resolved to paths.
#[derive(Clone, Debug)]
struct PendingDialog {
    request_id: RequestId,
    kind: &'static str,
    tag: String,
}

impl PendingDialog {
    fn resolved(self, paths: Vec<PathBuf>) -> FileDialogResultCommand {
        FileDialogResultCommand {
            request_id: self.request_id,
            kind: self.kind,
            tag: self.tag,
            paths,
        }
    }
}

/// Run one of the two arms and post the result, picking the arm by whether
/// an executor is installed.
///
/// Split out from [`FileDialogService::open_single_with`] so the choice and
/// the delivery can be exercised without a display: the arms are closures,
/// and a test substitutes them for the `rfd` ones.
fn dispatch_dialog<A, B>(
    spawn: Option<&dyn Spawn>,
    queue: &CommandQueue,
    pending: PendingDialog,
    run_spawned: A,
    run_blocking: B,
) where
    A: FnOnce() -> BoxFuture<Vec<PathBuf>> + Send + 'static,
    B: FnOnce() -> Vec<PathBuf>,
{
    match spawn {
        Some(spawn) => {
            let queue = queue.clone();
            spawn.spawn(Box::pin(async move {
                let paths = run_spawned().await;
                post_result(&queue, pending.resolved(paths));
            }));
        }
        None => post_result(queue, pending.resolved(run_blocking())),
    }
}

/// Translate a request into the rfd builder: filters first, then the
/// save-dialog default name.
fn build_dialog(req: &FileDialogRequest) -> AsyncFileDialog {
    let mut dlg = AsyncFileDialog::new();
    for f in &req.filters {
        let ext_refs: Vec<&str> = f.exts.iter().map(String::as_str).collect();
        dlg = dlg.add_filter(&f.label, &ext_refs);
    }
    if let Some(name) = &req.default_name {
        dlg = dlg.set_file_name(name);
    }
    dlg
}

/// Await the dialog for `kind` and collect what the user chose. An empty
/// vec means cancelled.
async fn resolve(dlg: AsyncFileDialog, kind: FileDialogKind) -> Vec<PathBuf> {
    match kind {
        FileDialogKind::Open => dlg
            .pick_file()
            .await
            .map(|h| vec![h.path().to_path_buf()])
            .unwrap_or_default(),
        FileDialogKind::OpenMulti => dlg
            .pick_files()
            .await
            .map(|v| v.iter().map(|h| h.path().to_path_buf()).collect())
            .unwrap_or_default(),
        FileDialogKind::Save => dlg
            .save_file()
            .await
            .map(|h| vec![h.path().to_path_buf()])
            .unwrap_or_default(),
        FileDialogKind::PickFolder => dlg
            .pick_folder()
            .await
            .map(|h| vec![h.path().to_path_buf()])
            .unwrap_or_default(),
    }
}

/// Run [`resolve`] on the calling thread. The path taken when no executor
/// is installed.
///
/// macOS is the exception: `NSOpenPanel` / `NSSavePanel` only resolve while
/// the main run loop pumps, and `pollster::block_on` parks the thread that
/// would pump it, so this would deadlock. There it reports a cancelled
/// dialog instead, and an app that wants dialogs on macOS installs an async
/// backend.
fn blocking_resolve(dlg: AsyncFileDialog, kind: FileDialogKind) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = dlg;
        let _ = kind;
        tracing::debug!(
            "lumen-os-filedialog: a blocking dialog needs a live run loop on \
             macOS; install an async backend to open dialogs there"
        );
        Vec::new()
    }
    #[cfg(not(target_os = "macos"))]
    {
        pollster::block_on(resolve(dlg, kind))
    }
}

/// Push a resolved dialog onto the command queue as a [`Command::Typed`],
/// from whichever thread finished it.
fn post_result(queue: &CommandQueue, result: FileDialogResultCommand) {
    let cmd = Command::Typed {
        type_id: TypeId::of::<FileDialogResultCommand>(),
        payload: Box::new(result),
    };
    // A full queue means the app is not draining commands, and a closed one
    // means it is shutting down. Neither is this crate's to recover from.
    if let Err(e) = queue.try_push(cmd) {
        warn!("lumen-os-filedialog: command queue rejected a dialog result: {e}");
    }
}

/// Drain system: reads [`FileDialogResultCommand`] payloads posted by
/// dialog tasks and emits the corresponding [`FilePicked`] message.
///
/// A normal `App` does not need this: [`FileDialogPlugin`] registers a
/// typed-command handler, and the standard [`Command::Typed`] dispatch in
/// [`TickStage::CommandDrain`] delivers the message. This system exists for
/// a host that drains the [`CommandQueue`] itself and wants the dialog
/// results turned into messages in [`TickStage::Systems`] instead.
pub fn drain_file_dialog_results(
    mut commands: ResMut<CommandReceiver>,
    mut out: MessageWriter<FileDialogResult>,
) {
    for cmd in commands.drain() {
        if let Command::Typed { type_id, payload } = cmd {
            if type_id == TypeId::of::<FileDialogResultCommand>() {
                if let Ok(p) = payload.downcast::<FileDialogResultCommand>() {
                    out.write(FileDialogResult::from(*p));
                }
            }
        }
    }
}

/// Plugin: registers the [`FileDialogService`] resource and the typed-
/// command handler that turns [`FileDialogResultCommand`] into
/// [`FilePicked`] messages.
///
/// No plugin dependency: dialogs work on their own, and an async backend
/// plugin (whichever the app installs) only changes whether the dialog
/// blocks the tick it was opened on.
#[derive(Default, Debug, Clone, Copy)]
pub struct FileDialogPlugin;

impl Plugin for FileDialogPlugin {
    fn name(&self) -> &'static str {
        "FileDialogPlugin"
    }

    fn build(self, app: &mut App) {
        if app.world.get_resource::<FileDialogService>().is_none() {
            app.world.insert_resource(FileDialogService::new());
        }
        // The `Command::Typed` dispatcher handles the cross-thread
        // payload on the main thread; we register a closure that
        // turns it into a `FilePicked` message via the world's writer.
        app.register_command::<FileDialogResultCommand, _>(|world, payload| {
            let result = FileDialogResult::from(*payload);
            world.write_message(result);
        });
        // Make sure the property-command drain runs (needed so the
        // typed command actually reaches the registered handler).
        // Authors that already wire `apply_property_commands` in
        // their own schedule can opt out; this plugin is idempotent
        // because the property-command drain is itself idempotent.
        app.add_systems(TickStage::CommandDrain, apply_property_commands);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn filter_from_tuple_str_slice() {
        let f: MimeFilter = ("Images", &["png", "jpg"][..]).into();
        assert_eq!(f.label, "Images");
        assert_eq!(f.exts, vec!["png".to_string(), "jpg".to_string()]);
    }

    #[test]
    fn filter_from_owned_tuple() {
        let f: MimeFilter = ("Text".to_string(), vec!["txt".to_string()]).into();
        assert_eq!(f.label, "Text");
        assert_eq!(f.exts, vec!["txt".to_string()]);
    }

    #[test]
    fn kind_label_matches_legacy_strings() {
        assert_eq!(FileDialogKind::Open.label(), "open");
        assert_eq!(FileDialogKind::OpenMulti.label(), "open_multi");
        assert_eq!(FileDialogKind::Save.label(), "save");
        assert_eq!(FileDialogKind::PickFolder.label(), "folder");
    }

    #[test]
    fn request_default_is_open_kind() {
        let r = FileDialogRequest::default();
        assert_eq!(r.kind, FileDialogKind::Open);
        assert!(r.tag.is_empty());
        assert!(r.filters.is_empty());
        assert!(r.default_name.is_none());
    }

    #[test]
    fn service_constructs() {
        let _s = FileDialogService::new();
        let _s2 = FileDialogService::default();
    }

    #[test]
    fn request_id_round_trip() {
        let id: RequestId = 7u64.into();
        let n: u64 = id.into();
        assert_eq!(n, 7);
    }

    #[test]
    fn request_ids_are_monotonic() {
        let svc = FileDialogService::new();
        let a = svc.alloc_id();
        let b = svc.alloc_id();
        let c = svc.alloc_id();
        assert!(a.0 < b.0 && b.0 < c.0);
    }

    #[test]
    fn result_command_to_message_conversion() {
        let cmd = FileDialogResultCommand {
            request_id: RequestId(42),
            kind: "open",
            tag: "hero".to_string(),
            paths: vec![PathBuf::from("/tmp/x.png")],
        };
        let msg: FileDialogResult = cmd.into();
        assert_eq!(msg.kind, "open");
        assert_eq!(msg.tag, "hero");
        assert_eq!(msg.paths.len(), 1);
    }

    /// Executor that drives the future to completion on the calling
    /// thread. Enough to prove the seam dispatches; no tokio in the graph.
    #[derive(Default)]
    struct InlineSpawn {
        spawned: Arc<AtomicUsize>,
    }

    impl Spawn for InlineSpawn {
        fn spawn(&self, mut fut: BoxFuture<()>) {
            self.spawned.fetch_add(1, AtomicOrdering::SeqCst);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            while fut.as_mut().poll(&mut cx).is_pending() {
                std::thread::yield_now();
            }
        }

        fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>) {
            self.spawned.fetch_add(1, AtomicOrdering::SeqCst);
            task();
        }
    }

    fn pending(tag: &str) -> PendingDialog {
        PendingDialog {
            request_id: RequestId(1),
            kind: "open",
            tag: tag.to_string(),
        }
    }

    fn picked_paths(app: &mut App) -> Option<Vec<PathBuf>> {
        let mut cursor = bevy_ecs::message::MessageCursor::<FileDialogResult>::default();
        let messages = app
            .world
            .resource::<bevy_ecs::message::Messages<FileDialogResult>>();
        cursor.read(messages).last().map(|ev| ev.paths.clone())
    }

    /// A result posted from a worker thread reaches the app as a
    /// `FilePicked` message on the next tick.
    #[test]
    fn a_posted_result_drains_into_a_filepicked_message() {
        let mut app = App::new();
        FileDialogPlugin.build(&mut app);

        let queue = app.world.resource::<CommandQueue>().clone();
        std::thread::spawn(move || {
            post_result(
                &queue,
                pending("hero").resolved(vec![PathBuf::from("/tmp/x.png")]),
            );
        })
        .join()
        .expect("worker joins");

        app.tick();

        let mut cursor = bevy_ecs::message::MessageCursor::<FileDialogResult>::default();
        let messages = app
            .world
            .resource::<bevy_ecs::message::Messages<FileDialogResult>>();
        let ev = cursor.read(messages).last().expect("FilePicked emitted");
        assert_eq!(ev.kind, "open");
        assert_eq!(ev.tag, "hero");
        assert_eq!(ev.paths, vec![PathBuf::from("/tmp/x.png")]);
    }

    /// With an executor installed the request runs on it and the blocking
    /// arm is never touched.
    #[test]
    fn an_installed_executor_serves_the_request() {
        let mut app = App::new();
        FileDialogPlugin.build(&mut app);
        let queue = app.world.resource::<CommandQueue>().clone();

        let spawned = Arc::new(AtomicUsize::new(0));
        let executor = InlineSpawn {
            spawned: Arc::clone(&spawned),
        };
        dispatch_dialog(
            Some(&executor),
            &queue,
            pending("hero"),
            || Box::pin(std::future::ready(vec![PathBuf::from("/spawned.png")])),
            || panic!("the blocking arm must not run when an executor is installed"),
        );

        assert_eq!(spawned.load(AtomicOrdering::SeqCst), 1);
        app.tick();
        assert_eq!(
            picked_paths(&mut app),
            Some(vec![PathBuf::from("/spawned.png")])
        );
    }

    /// The point of the seam: no executor in the world is not an error, it
    /// selects the blocking arm, and the result arrives the same way.
    #[test]
    fn no_executor_falls_back_to_the_blocking_arm() {
        let mut app = App::new();
        FileDialogPlugin.build(&mut app);
        assert!(
            app.world.get_resource::<SpawnService>().is_none(),
            "the dialog plugin must not require an async backend"
        );
        let queue = app.world.resource::<CommandQueue>().clone();

        dispatch_dialog(
            None,
            &queue,
            pending("hero"),
            || panic!("the spawned arm must not run without an executor"),
            || vec![PathBuf::from("/blocking.png")],
        );

        app.tick();
        assert_eq!(
            picked_paths(&mut app),
            Some(vec![PathBuf::from("/blocking.png")])
        );
    }

    /// `open_single` reads the world for both halves and reports the id
    /// straight away. With no executor and no display it resolves to a
    /// cancelled dialog, which is still a result and still monotonic.
    #[test]
    fn open_single_allocates_ids_without_an_async_backend() {
        let mut app = App::new();
        FileDialogPlugin.build(&mut app);
        let svc = app.world.resource::<FileDialogService>().clone();
        let a = svc.alloc_id();
        let b = svc.alloc_id();
        assert_eq!(b.0, a.0 + 1);
    }
}
