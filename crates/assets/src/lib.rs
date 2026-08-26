//! Async asset pipeline with a content-addressed handle cache.
//!
//! - [`spawn_pending_decodes`] picks up entities tagged with [`ImageSource`] and consults the global cache on [`AssetServer`].
//! - On a cache hit, the existing decoded [`Handle`] is cloned in (Arc bump, no decode runs).
//! - On a cold miss, the request is pushed onto a bounded `crossbeam-channel` job queue drained by an N-worker thread pool (`N = available_parallelism().min(4)`).
//!   Subsequent entities requesting the same path join a pending fan-out list.
//! - Every request carries a monotonic per-entity `request_id`; results whose id no longer matches the entity's current id are discarded on completion. This kills the stale-decode race after rapid `ScriptCommand::SetSrc` storms.
//! - What a path decodes *into* is decided by the loader registry: each [`AssetLoader`] claims file extensions and produces one [`AssetKind`], and the built-in image and SVG paths are ordinary registered loaders. An app or plugin adds a format by registering another one; see [`register_asset_loader`].
//! - Where the bytes come from is decided by the [`AssetSource`] list, consulted before the job is queued. [`BundleSource`] (`.lpak` archives, `lumen://app/...` URIs) is installed by default.
//! - [`drain_completed_decodes`] delivers the resulting `Handle` to every still-valid waiter; failures attach [`ImageLoadFailed`] carrying a typed [`LoadErrorKind`].
//! - Handles are strong [`Arc`]s to decoded data; the cache holds [`Handle<T>`] entries that share identity across consumers.
//! - The vello GPU upload cache is keyed by the underlying `peniko::Blob` identity, so identical handles short-circuit the upload.
//! - A [`notify::RecommendedWatcher`] tracks every loaded file URL. On change the asset cache invalidates the affected entry, the request id of every entity referencing that path is bumped, and an [`AssetReloadRequested`] message fires so consumers (e.g. the markup runtime) can re-enqueue the load.
//!
//! Eviction is true LRU bounded by `max_bytes` (default 256 MiB). Insert / evict mutate a running `bytes_used: usize` in O(1); a `debug_assert!` reconciles the running counter against a full sweep at most once per second when debug assertions are on.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bundle;
pub mod loader;
pub mod loaders;
pub mod source;

pub use bundle::{BundleError, LumenBundle, parse_lumen_uri};
pub use loader::{AssetKind, AssetLoader, AssetLoaders, LoadContext, LoadedAsset, asset_extension};
pub use loaders::{ImageLoader, SvgLoader};
pub use source::{AssetSource, BundleSource, SourceReader};

use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender};
use lru::LruCache;
use lumen_core::prelude::*;
use lumen_core::time::Instant;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Implements `Deref` for a single-field `Handle` newtype so field access
/// flows through to the inner decoded payload. Every `Loaded*` wrapper shares
/// the identical `fn deref(&self) -> &Target { &self.0 }` body.
macro_rules! deref_newtype {
    ($ty:ty => $target:ty) => {
        impl std::ops::Deref for $ty {
            type Target = $target;
            fn deref(&self) -> &$target {
                &self.0
            }
        }
    };
}

/// Inserts a decoded asset into one of the content caches with the shared
/// byte-accounting protocol: subtract any prior entry's bytes, add the new
/// entry's, push under `path`, evict to the cap, subscribe the watcher, and
/// reconcile. `$cache` is the `LruCache` field on `self`.
macro_rules! insert_cached_asset {
    ($self:ident, $cache:ident, $path:expr, $val:expr) => {{
        let path = $path;
        let val = $val;
        // Pop any prior entry first so `bytes_used` accounting stays in sync.
        if let Some(prev) = $self.$cache.pop(&path) {
            $self.bytes_used = $self.bytes_used.saturating_sub(prev.0.bytes());
        }
        $self.bytes_used = $self.bytes_used.saturating_add(val.0.bytes());
        $self.$cache.push(path.clone(), val);
        $self.evict_until($self.max_bytes);
        $self.watch_path(&path);
        $self.maybe_reconcile_bytes();
    }};
}

/// Handles one successful decode outcome in [`drain_completed_decodes`]:
/// inserts the payload into its cache (LRU-bumping recency) via `$insert`,
/// then attaches a clone to every surviving waiter, clearing its `Enqueued`
/// tag. The Image and Svg arms differ only in the cache insert method and the
/// payload binding.
macro_rules! dispatch_decoded {
    ($server:ident, $commands:ident, $path:expr, $insert:ident, $payload:ident, $surviving:ident) => {{
        $server.$insert($path, $payload.clone());
        for w in $surviving {
            let mut ec = $commands.entity(w.entity);
            ec.remove::<Enqueued>();
            ec.insert($payload.clone());
        }
    }};
}

/// Default max bytes held across the image + SVG content caches (256 MiB).
/// Override with [`AssetServer::with_max_bytes`].
pub const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Source path for an image asset, spawned by `<image src="...">` in markup.
/// Replaced by [`LoadedImage`] (or [`ImageLoadFailed`]) once the asset plugin finishes decoding.
#[derive(Component, Clone, Debug)]
pub struct ImageSource(pub PathBuf);

/// Newtype wrapper holding an `Arc<[u8]>` and implementing `AsRef<[u8]>` so it can be passed to
/// `peniko::Blob::new`, which requires `Arc<T: ?Sized + AsRef<[u8]> + Send + Sync>`.
pub struct PixBytes(pub Arc<[u8]>);

impl AsRef<[u8]> for PixBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Strong, content-addressed handle to a decoded asset.
///
/// - Wraps an `Arc<T>`; cloning is an `Arc` bump (`O(1)`, no data copy).
/// - Identical handles share identity via [`Handle::id`], which keys the vello GPU upload cache.
/// - [`AssetServer`] stores `Handle<T>` entries so entities sharing a path share one decoded asset.
pub struct Handle<T> {
    inner: Arc<T>,
}

impl<T> Handle<T> {
    /// Returns the handle's stable identity (the underlying `Arc` pointer cast to `usize`).
    /// Clones of the same handle return the same id; independent decodes of the same bytes return different ids.
    pub fn id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// Returns the approximate in-memory byte cost of the underlying data via the [`AssetSize`] trait.
    pub fn bytes(&self) -> usize
    where
        T: AssetSize,
    {
        self.inner.bytes()
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> std::ops::Deref for Handle<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> From<Arc<T>> for Handle<T> {
    fn from(inner: Arc<T>) -> Self {
        Self { inner }
    }
}

impl<T> From<T> for Handle<T> {
    fn from(value: T) -> Self {
        Self {
            inner: Arc::new(value),
        }
    }
}

/// Implemented by asset payload types so the cache can budget memory.
pub trait AssetSize {
    /// Returns the approximate CPU-side byte cost of this asset, consulted by [`AssetServer::bytes_used`] and the LRU eviction sweep.
    fn bytes(&self) -> usize;
}

/// Decoded image payload in RGBA8 with top-left origin.
///
/// - Shared via [`Handle<ImageData>`]; never deep-copied.
/// - `blob` is a pre-built `peniko::Blob` whose stable identity keys vello's GPU texture upload cache across frames. `Blob` cloning is `Arc`-cheap.
pub struct ImageData {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8 pixel buffer in row-major order (`width * height * 4` bytes), accessible to non-vello consumers such as the MCP inspector.
    pub rgba: Arc<[u8]>,
    /// Pre-built `peniko::Blob` whose identity is used as the GPU upload cache key in vello.
    pub blob: vello::peniko::Blob<u8>,
}

impl AssetSize for ImageData {
    fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// Strong handle to a decoded image, attached as a component once `<image src="...">` resolves.
/// Implements `Deref<Target = ImageData>` so field access (`img.width`, `img.blob`) flows through to the inner payload.
#[derive(Component, Clone)]
pub struct LoadedImage(pub Handle<ImageData>);

deref_newtype!(LoadedImage => ImageData);

impl std::fmt::Debug for LoadedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_len", &self.rgba.len())
            .field("handle_id", &self.0.id())
            .finish_non_exhaustive()
    }
}

/// Pre-rendered SVG payload produced once at decode time and shared per-frame via [`Handle<SvgData>`] clones.
pub struct SvgData {
    /// Native pixel size derived from the SVG `viewBox` / `width` / `height`.
    pub intrinsic: glam::Vec2,
    /// Pre-rendered vello scene.
    pub scene: vello::Scene,
    /// Raw source-file length in bytes; budgeted by the LRU as a proxy for the encoded `vello::Scene` cost,
    /// which vello does not expose. Replaces the previous fixed `size_of::<vello::Scene>()` placeholder so
    /// eviction is no longer blind to SVG memory pressure.
    pub source_bytes: usize,
}

impl AssetSize for SvgData {
    fn bytes(&self) -> usize {
        self.source_bytes
    }
}

/// Strong handle to a decoded SVG, with `Deref<Target = SvgData>` enabling field access (`svg.intrinsic`, `svg.scene`).
#[derive(Component, Clone)]
pub struct LoadedSvg(pub Handle<SvgData>);

deref_newtype!(LoadedSvg => SvgData);

/// Typed asset load-failure category.
///
/// Replaces the previous `String`-typed failure surface. Renderers branch on `kind` (a "broken image" icon for
/// [`Self::NotFound`] / [`Self::Unsupported`], an error banner for [`Self::DecodeFailed`], silent drop for
/// [`Self::Cancelled`]).
#[derive(Debug)]
pub enum LoadErrorKind {
    /// Source file does not exist or is not reachable.
    NotFound,
    /// The bytes were read but the decoder rejected them. Carries the decoder's message.
    DecodeFailed(String),
    /// Underlying I/O error (permission denied, broken pipe, ...). Kept around so callers can inspect `kind()`.
    Io(std::io::Error),
    /// The job was discarded because a newer `SetSrc` superseded it before completion.
    Cancelled,
    /// File extension or content type that the asset crate does not handle.
    Unsupported,
}

impl std::fmt::Display for LoadErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "not found"),
            Self::DecodeFailed(msg) => write!(f, "decode failed: {msg}"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Cancelled => write!(f, "cancelled by newer request"),
            Self::Unsupported => write!(f, "unsupported format"),
        }
    }
}

impl From<std::io::Error> for LoadErrorKind {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            LoadErrorKind::NotFound
        } else {
            LoadErrorKind::Io(e)
        }
    }
}

/// Attached to the entity when its image fails to decode. Carries the typed [`LoadErrorKind`] and a human-readable detail string.
#[derive(Component, Debug)]
pub struct ImageLoadFailed {
    /// Categorized failure variant; branch on this in the render layer.
    pub kind: LoadErrorKind,
    /// Stringified failure detail, suitable for logs and error banners.
    pub detail: String,
}

impl ImageLoadFailed {
    /// Wraps a [`LoadErrorKind`] in a component, formatting `detail` via `Display`.
    pub fn new(kind: LoadErrorKind) -> Self {
        let detail = kind.to_string();
        Self { kind, detail }
    }
}

impl From<LoadErrorKind> for ImageLoadFailed {
    fn from(kind: LoadErrorKind) -> Self {
        Self::new(kind)
    }
}

/// Monotonic per-entity asset request id. Attached to every entity carrying an [`ImageSource`].
/// Every `ScriptCommand::SetSrc` runs through [`AssetServer::bump_request_id`] which increments this counter;
/// in-flight decode jobs carrying an older id are discarded on completion.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestId(pub u64);

/// Message fired when a watched asset file changes on disk. Consumers (e.g. the markup runtime in `lumenc::run`) react by
/// stripping `LoadedImage` / `LoadedSvg` / `ImageLoadFailed` from every entity whose `ImageSource` matches `path` so the
/// pipeline re-enqueues the load.
#[derive(Message, Clone, Debug)]
pub struct AssetReloadRequested {
    /// Canonicalized source path whose bytes-on-disk changed.
    pub path: PathBuf,
}

/// One pre-rendered SVG to draw this frame. Defined in `lumen-assets` (not `lumen-core`) because the cached scene carries vello types.
#[derive(Component, Clone)]
pub struct ExtractedSvg {
    /// Top-left position in window coordinates.
    pub origin: glam::Vec2,
    /// Target rect size in pixels.
    pub size: glam::Vec2,
    /// Native SVG size derived from the viewBox.
    pub intrinsic: glam::Vec2,
    /// Strong handle to the cached SVG payload; `Deref` exposes `asset.scene` and `asset.intrinsic`.
    pub asset: Handle<SvgData>,
    /// Scaling mode applied when fitting the SVG into the drawn rect.
    pub fit: lumen_core::components::ImageFit,
    /// Global paint order, computed by [`lumen_core::render_world::PaintOrder`].
    pub order: u32,
    /// Alpha multiplier carried from [`lumen_core::components::Opacity`]. Applied via `push_layer` at draw time when below 1.0.
    pub alpha: f32,
}

/// Background-thread plumbing and content cache for the asset pipeline.
///
/// - Caches are keyed by source path; entities sharing a path share one decoded `LoadedImage` or `LoadedSvg` (Arc-cheap clones).
/// - Successful decodes populate the LRU image / SVG caches; decode failures populate the LRU failure cache.
/// - Entities requesting an in-flight path are appended to `pending` and receive the result via the drain.
/// - What a path decodes into comes from the [`AssetLoaders`] registry, and where its bytes come from
///   comes from the registered [`AssetSource`] list; both are replaceable per app.
/// - Decode jobs run on a bounded N-worker `crossbeam-channel` pool (`N = available_parallelism().min(4)`).
/// - A `notify::RecommendedWatcher` invalidates cache entries whose backing file changes on disk and fires [`AssetReloadRequested`].
#[derive(Resource)]
pub struct AssetServer {
    job_tx: Option<Sender<DecodeJob>>,
    result_rx: Receiver<DecodeResult>,
    /// Worker thread join handles (one per pool worker). Drained by [`Self::shutdown`].
    ///
    /// Empty until the first decode is actually enqueued: the pool is
    /// spawned lazily by [`Self::ensure_workers`] so an app that never
    /// loads an image or SVG (the common case for a plain counter / form
    /// UI) pays zero decode threads. See [`Self::worker_count`].
    workers: Vec<JoinHandle<()>>,
    /// Receiver end handed to each worker at spawn time. Held here so the
    /// pool can be spawned lazily on first enqueue rather than eagerly at
    /// construction. `None` after [`Self::shutdown`].
    job_rx: Option<Receiver<DecodeJob>>,
    /// Sender template cloned to each worker at spawn time; also keeps the
    /// result channel connected before any worker exists. `None` after
    /// [`Self::shutdown`].
    result_tx: Option<Sender<DecodeResult>>,
    /// Number of workers [`Self::ensure_workers`] will spawn on first use
    /// (`available_parallelism().min(4)`).
    worker_count: usize,
    /// Cached successful image decodes; cloning is `Arc`-cheap. LRU-ordered.
    image_cache: LruCache<PathBuf, LoadedImage>,
    /// Cached successful SVG decodes; cloning is `Arc`-cheap. LRU-ordered.
    svg_cache: LruCache<PathBuf, LoadedSvg>,
    /// Cached decode failures keyed by source path. LRU-ordered.
    failed_cache: LruCache<PathBuf, ImageLoadFailed>,
    /// Entities awaiting an in-flight decode; one `DecodeResult` per path fans out to all matching waiters.
    /// Each waiter records the `request_id` that was current when the job was enqueued. On completion, waiters
    /// whose entity's current `RequestId` no longer matches are discarded - kills the stale-decode race.
    pending: HashMap<PathBuf, Vec<PendingWaiter>>,
    /// Per-entity monotonic request counter; incremented on every `bump_request_id`.
    request_ids: HashMap<Entity, u64>,
    /// Running sum of [`AssetSize::bytes`] across `image_cache` + `svg_cache`. Updated in O(1) on insert / evict.
    /// Reconciled against a full sweep at most once per second when debug assertions are on.
    bytes_used: usize,
    /// Soft cap on `bytes_used`. Inserts evict LRU entries until the cap is satisfied.
    max_bytes: usize,
    /// Last time the running `bytes_used` was reconciled against a full sweep. Debug-only.
    #[cfg(debug_assertions)]
    last_reconcile: Instant,
    /// Filesystem watcher rooted at every loaded file URL. `None` until the first successful decode is cached
    /// (constructing the watcher lazily lets `AssetServer::default()` succeed in environments without an
    /// fsevent / inotify backend, e.g. some CI sandboxes).
    watcher: Option<RecommendedWatcher>,
    /// Channel receiving filesystem-watch events. Drained by [`process_watch_events`] each tick.
    watch_rx: Receiver<notify::Result<notify::Event>>,
    /// Sender end of the watch channel; cloned into the watcher callback.
    watch_tx: Sender<notify::Result<notify::Event>>,
    /// Set of paths the watcher is currently subscribed to. Watch ops are idempotent - subscribing a path
    /// twice is cheap and avoids tracking per-path refcounts.
    watched: HashSet<PathBuf>,
    /// Shared cancellation flag handed to workers. Flipping it true at shutdown unblocks any worker that's
    /// already started a decode call so the join handles complete promptly.
    shutdown_flag: Arc<Mutex<bool>>,
    /// Extension-keyed loader registry. Decides which [`AssetLoader`] runs
    /// for a path and therefore what kind of asset it becomes. Defaults to
    /// the built-in image and SVG loaders.
    loaders: AssetLoaders,
    /// The built-in `.lpak` source. Kept as its own field so
    /// [`Self::register_bundle`] and the font-registration path in
    /// `lumen-text-cosmic` can reach the bundles by type.
    bundle_source: BundleSource,
    /// Additional byte sources, consulted after the bundles and before the
    /// filesystem, in registration order.
    sources: Vec<Arc<dyn AssetSource>>,
}

struct PendingWaiter {
    entity: Entity,
    request_id: u64,
}

/// One unit of decode work.
///
/// The path is the cache key. Per-entity `request_id` tracking also lives on the `pending` fan-out list -
/// every waiter (entity + snapshot id) records the id at enqueue time. On completion the drain compares
/// waiter snapshots against the entity's *current* id to discard stale waiters; if no waiter survives the
/// check, the result is dropped without inserting into the cache, so a SetSrc-storm doesn't pollute the LRU
/// with transient decodes.
struct DecodeJob {
    path: PathBuf,
    /// Loader resolved from the registry at enqueue time, on the main
    /// thread. Travelling with the job is what keeps loader registration off
    /// the workers' path: they never read the registry.
    loader: Arc<dyn AssetLoader>,
    /// Request id snapshot taken at the originating entity's enqueue time. Carried verbatim on the
    /// `DecodeResult` so callers tracing job -> result pairs can correlate by id; the actual stale-waiter
    /// check uses per-waiter ids on the `pending` list.
    request_id: u64,
    /// Bytes an [`AssetSource`] produced for this path, resolved on the
    /// main thread because a worker cannot see the [`AssetServer`]. `None`
    /// when no source claimed the path, and the loader reads it itself.
    resolved_bytes: Option<Vec<u8>>,
}

struct DecodeResult {
    path: PathBuf,
    /// Request id forwarded from the originating `DecodeJob`; see [`DecodeJob::request_id`].
    request_id: u64,
    outcome: Result<LoadedAsset, LoadErrorKind>,
}

/// Outcome of a cache lookup for an entity's source path.
enum CacheLookup {
    HitImage(LoadedImage),
    HitSvg(LoadedSvg),
    HitFailed(ImageLoadFailed),
    /// The path is already mid-flight; the entity has been appended to the pending list and the caller inserts `Enqueued`.
    InFlight,
    /// No prior decode existed; a new job was enqueued and the caller inserts `Enqueued`.
    Enqueued,
}

impl Default for AssetServer {
    fn default() -> Self {
        Self::with_max_bytes(DEFAULT_MAX_BYTES)
    }
}

impl AssetServer {
    /// Constructs an `AssetServer` with `max_bytes` as the soft cap on the combined image + SVG caches.
    /// `Self::default()` calls this with [`DEFAULT_MAX_BYTES`] (256 MiB).
    pub fn with_max_bytes(max_bytes: usize) -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<DecodeResult>();
        let (watch_tx, watch_rx) = crossbeam_channel::unbounded::<notify::Result<notify::Event>>();
        // Bound worker count to `available_parallelism().min(4)`. The clamp keeps decode load off the
        // bevy_ecs task pool and matches the spec - a four-worker pool can decode 4 images in parallel
        // without burning a thread-per-job during burst loads.
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 4);
        let shutdown_flag = Arc::new(Mutex::new(false));
        // Workers are not spawned here. The channel ends are stashed and the
        // pool is materialised lazily on the first enqueue via
        // `ensure_workers` - an app that loads no assets keeps these threads
        // off the process entirely.
        Self {
            job_tx: Some(job_tx),
            result_rx,
            workers: Vec::new(),
            job_rx: Some(job_rx),
            result_tx: Some(result_tx),
            worker_count,
            image_cache: LruCache::unbounded(),
            svg_cache: LruCache::unbounded(),
            failed_cache: LruCache::unbounded(),
            pending: HashMap::new(),
            request_ids: HashMap::new(),
            bytes_used: 0,
            max_bytes,
            #[cfg(debug_assertions)]
            last_reconcile: Instant::now(),
            watcher: None,
            watch_rx,
            watch_tx,
            watched: HashSet::new(),
            shutdown_flag,
            loaders: AssetLoaders::default(),
            bundle_source: BundleSource::default(),
            sources: Vec::new(),
        }
    }

    /// Teach the pipeline a format. The loader claims the extensions it
    /// names, replacing whatever held them before, so an app can override a
    /// built-in decoder as well as add one.
    ///
    /// From a plugin's `build`, prefer [`register_asset_loader`], which
    /// installs the server first if it is not there yet.
    pub fn register_loader(&mut self, loader: impl AssetLoader) {
        self.loaders.register(loader);
    }

    /// The loader registry, for inspecting which loader handles a path.
    pub fn loaders(&self) -> &AssetLoaders {
        &self.loaders
    }

    /// The loader registry, for registering an already shared loader or
    /// changing the fallback that handles unclaimed extensions.
    pub fn loaders_mut(&mut self) -> &mut AssetLoaders {
        &mut self.loaders
    }

    /// Add a byte source. Sources are consulted in registration order after
    /// the bundles; a path none of them claims is read from disk.
    pub fn register_source(&mut self, source: impl AssetSource) {
        self.sources.push(Arc::new(source));
    }

    /// Register a `.lpak` bundle with the server. Subsequent decode
    /// lookups for `lumen://app/<path>` URIs route through every
    /// registered bundle in insertion order. Bundles are cheap to
    /// clone (one `Arc<[u8]>` bump) so the same instance can be
    /// shared with `lumen-text-cosmic`'s font_db registration pass.
    ///
    /// Set [`Self::set_bundle_root`] as well to serve plain filesystem
    /// paths (`<app_dir>/icons/sun.png`) out of the archive.
    pub fn register_bundle(&mut self, bundle: LumenBundle) {
        self.bundle_source.register(bundle);
    }

    /// Declare the directory registered bundles were packed from. A
    /// lookup for a path under this root is tried against the bundles
    /// first, using the remainder of the path as the key; the entry
    /// wins over the file on disk, so an app can ship the archive and
    /// leave the loose files behind.
    pub fn set_bundle_root(&mut self, root: impl Into<PathBuf>) {
        self.bundle_source.set_root(root);
    }

    /// Iterate the currently registered bundles. Used by the
    /// font-registration helper in `lumen-text-cosmic`.
    pub fn bundles(&self) -> &[LumenBundle] {
        self.bundle_source.bundles()
    }

    /// Resolve `lumen://app/<rel>` URI strings to bundled bytes. Returns
    /// `None` when no bundle satisfies the URI or when the URI uses an
    /// unknown scheme (callers fall back to disk in that case).
    pub fn resolve_uri(&self, uri: &str) -> Option<Vec<u8>> {
        self.bundle_source.read_uri(uri)
    }

    /// Resolve an [`ImageSource`]-shaped path out of the registered
    /// bundles: either a `lumen://app/<key>` URI, or a filesystem path
    /// under [`Self::set_bundle_root`] whose remainder is the key.
    /// `None` means "not bundled", and the caller reads from disk.
    pub fn resolve_bundled(&self, path: &Path) -> Option<Vec<u8>> {
        self.bundle_source.read(path)
    }

    /// Bytes for `path` from the first source that claims it: the bundles,
    /// then anything passed to [`Self::register_source`]. `None` leaves the
    /// read to the loader, which means the filesystem.
    fn resolve_source_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        source::read_chain(&self.bundle_source, &self.sources, path)
    }

    /// A [`SourceReader`] snapshot of the current source chain, for reading
    /// raw bytes off the main thread. See the type's docs for the contract;
    /// take a fresh one per request, because a snapshot does not see sources
    /// registered after it.
    pub fn source_reader(&self) -> SourceReader {
        SourceReader::new(self.bundle_source.clone(), self.sources.clone())
    }

    /// Returns the approximate CPU bytes held by the image and SVG content caches.
    /// O(1): returns the running counter maintained on insert / evict.
    pub fn bytes_used(&self) -> usize {
        self.bytes_used
    }

    /// Returns the current size cap. Inserts that would push `bytes_used` past this number evict LRU entries
    /// before completing.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Resets the size cap and immediately evicts down to the new bound.
    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.evict_until(max_bytes);
    }

    /// Recomputes `bytes_used` from scratch by summing every cached entry. O(n). Used only by the debug
    /// reconciliation tick to catch any insert / evict path that forgot to update the counter.
    fn recompute_bytes_used(&self) -> usize {
        let mut total: usize = 0;
        for (_, img) in self.image_cache.iter() {
            total = total.saturating_add(img.0.bytes());
        }
        for (_, svg) in self.svg_cache.iter() {
            total = total.saturating_add(svg.0.bytes());
        }
        total
    }

    /// Evicts the least-recently-used cached entries until `bytes_used <= target_bytes`.
    /// Images are evicted before SVGs (image RGBA buffers dominate the budget by an order of magnitude).
    /// Entities holding strong `Handle<...>` clones keep their decoded data alive via the Arc refcount.
    pub fn evict_until(&mut self, target_bytes: usize) {
        while self.bytes_used > target_bytes {
            if let Some((_, img)) = self.image_cache.pop_lru() {
                self.bytes_used = self.bytes_used.saturating_sub(img.0.bytes());
                continue;
            }
            if let Some((_, svg)) = self.svg_cache.pop_lru() {
                self.bytes_used = self.bytes_used.saturating_sub(svg.0.bytes());
                continue;
            }
            break;
        }
    }

    /// Bumps the per-entity monotonic request id and returns the new value.
    ///
    /// Call this from the runtime's `ScriptCommand::SetSrc` handler (today in `lumenc::run`) **before**
    /// re-inserting [`ImageSource`] on the entity. Any in-flight decode for the prior path will then be
    /// identified as stale on completion via the snapshot id stored on its waiter entry, and either
    /// dropped from the per-entity insert step (if some other waiter still wants the path) or discarded
    /// entirely (no cache write) if every waiter has moved on.
    ///
    /// Skipping this call leaves the stale-decode race in place: the runtime swaps `ImageSource` while a
    /// decode for the old bytes is in flight, then `drain_completed_decodes` attaches the old image.
    pub fn bump_request_id(&mut self, entity: Entity) -> u64 {
        let slot = self.request_ids.entry(entity).or_insert(0);
        *slot = slot.saturating_add(1);
        *slot
    }

    /// Returns the entity's current request id (or 0 if it has never been bumped).
    pub fn current_request_id(&self, entity: Entity) -> u64 {
        self.request_ids.get(&entity).copied().unwrap_or(0)
    }

    /// Spawn the decode worker pool if it has not been spawned yet.
    /// Called from the enqueue path so the threads exist only once an app
    /// actually requests a decode. Idempotent and cheap after the first
    /// call (empty-check short-circuit). A no-op after [`Self::shutdown`]
    /// (the channel templates are gone).
    fn ensure_workers(&mut self) {
        if !self.workers.is_empty() {
            return;
        }
        let (Some(job_rx), Some(result_tx)) = (self.job_rx.as_ref(), self.result_tx.as_ref())
        else {
            return;
        };
        self.workers.reserve(self.worker_count);
        for i in 0..self.worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let shutdown_flag = self.shutdown_flag.clone();
            let handle = std::thread::Builder::new()
                .name(format!("lumen-assets-worker-{i}"))
                .spawn(move || worker_loop(job_rx, result_tx, shutdown_flag))
                .expect("spawn asset worker");
            self.workers.push(handle);
        }
    }

    /// Looks up `path` in the cache and returns a [`CacheLookup`] outcome.
    /// On miss, enqueues one decode job and registers `entity` (with its current request id) on the
    /// pending list for fan-out. The lookup also touches the LRU recency on cache hits.
    fn lookup_or_enqueue(&mut self, entity: Entity, path: PathBuf) -> CacheLookup {
        if let Some(img) = self.image_cache.get(&path) {
            return CacheLookup::HitImage(img.clone());
        }
        if let Some(svg) = self.svg_cache.get(&path) {
            return CacheLookup::HitSvg(svg.clone());
        }
        if let Some(failed) = self.failed_cache.get(&path) {
            return CacheLookup::HitFailed(ImageLoadFailed {
                // `LoadErrorKind` is not `Clone` (it carries `io::Error`); clone-via-detail is acceptable for
                // the cached-failure path because the original kind is preserved on the cache entry and only
                // the consumer-facing detail string matters at this point.
                kind: clone_kind(&failed.kind),
                detail: failed.detail.clone(),
            });
        }
        let request_id = self.current_request_id(entity);
        // Pick the loader here, on the main thread, so the registry is never
        // read from a worker. A path no loader claims (only possible once
        // the fallback has been cleared) fails immediately rather than
        // occupying a worker to reach the same answer.
        let Some(loader) = self.loaders.resolve(&path) else {
            return CacheLookup::HitFailed(ImageLoadFailed::new(LoadErrorKind::Unsupported));
        };
        // If a source holds this path, resolve the bytes synchronously here.
        // The worker thread doesn't see the `AssetServer` resource, so we
        // have to feed it the payload upfront.
        let resolved_bytes = self.resolve_source_bytes(&path);
        // Reaching here means a cache miss: this call will either enqueue a
        // fresh decode (Vacant) or attach to one already in flight
        // (Occupied - the pool was spawned by that prior enqueue). Either
        // way the worker pool must exist; spawn it lazily now. Idempotent.
        self.ensure_workers();
        match self.pending.entry(path.clone()) {
            MapEntry::Occupied(mut e) => {
                e.get_mut().push(PendingWaiter { entity, request_id });
                CacheLookup::InFlight
            }
            MapEntry::Vacant(e) => {
                e.insert(vec![PendingWaiter { entity, request_id }]);
                if let Some(tx) = self.job_tx.as_ref() {
                    let _ = tx.send(DecodeJob {
                        path,
                        loader,
                        request_id,
                        resolved_bytes,
                    });
                }
                CacheLookup::Enqueued
            }
        }
    }

    /// Ensures a `RecommendedWatcher` exists and subscribes to `path`'s parent directory so file rewrites are
    /// observed. Watch ops are idempotent; resubscribing an already-watched path is cheap.
    fn watch_path(&mut self, path: &Path) {
        if self.watched.contains(path) {
            return;
        }
        if self.watcher.is_none() {
            let tx = self.watch_tx.clone();
            let watcher = RecommendedWatcher::new(
                move |res| {
                    let _ = tx.send(res);
                },
                notify::Config::default(),
            );
            match watcher {
                Ok(w) => self.watcher = Some(w),
                Err(e) => {
                    tracing::warn!("asset watcher init failed: {e}");
                    return;
                }
            }
        }
        if let Some(w) = self.watcher.as_mut() {
            // Watching the file directly is sufficient on inotify / FSEvents; we mirror the path set into
            // `self.watched` so we can map event paths back to cache keys without rescanning all caches.
            if let Err(e) = w.watch(path, RecursiveMode::NonRecursive) {
                tracing::warn!("asset watcher subscribe failed for {path:?}: {e}");
                return;
            }
            self.watched.insert(path.to_path_buf());
        }
    }

    /// Invalidates cache entries (success + failure) for `path` and returns whether anything was dropped.
    /// `bytes_used` is updated in O(1) for every removed image / SVG.
    fn invalidate_path(&mut self, path: &Path) -> bool {
        let mut hit = false;
        if let Some(img) = self.image_cache.pop(path) {
            self.bytes_used = self.bytes_used.saturating_sub(img.0.bytes());
            hit = true;
        }
        if let Some(svg) = self.svg_cache.pop(path) {
            self.bytes_used = self.bytes_used.saturating_sub(svg.0.bytes());
            hit = true;
        }
        if self.failed_cache.pop(path).is_some() {
            hit = true;
        }
        hit
    }

    /// Drains the dispatcher channel, joins every worker, clears all caches, and detaches the watcher.
    /// Idempotent - calling shutdown twice is a no-op after the first.
    ///
    /// Takes `&mut self` rather than `&self`. The audit spec suggested `&self` for ergonomics; the
    /// in-place mutation of caches + the watcher option + the join-handle vec makes the `&mut self`
    /// signature the natural choice once interior `Mutex`-wrapping is avoided. `AssetServer` is a
    /// `Resource`, so consumers can reach it via `ResMut<AssetServer>::shutdown()`.
    ///
    /// `impl Drop for AssetServer` calls this automatically, so [`App`] tear-down (and any test or
    /// benchmark that builds + drops the server) joins workers cleanly without an explicit shutdown call.
    pub fn shutdown(&mut self) {
        // Flip the cancellation flag and disconnect `job_tx`. Workers loop on `recv` and break on Err.
        if let Ok(mut g) = self.shutdown_flag.lock() {
            *g = true;
        }
        // Drop every sender so `job_rx.recv()` in each worker returns
        // `RecvError`: the live `job_tx`, plus the lazy-spawn templates the
        // pool would otherwise be revived from.
        self.job_tx.take();
        self.job_rx.take();
        self.result_tx.take();
        // Join every worker. Each one has at most one decode in flight (we don't preempt - a long
        // PNG decode runs to completion), so this is bounded by the decoder's worst case.
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        // Drop any pending results without inserting into the cache.
        while self.result_rx.try_recv().is_ok() {}
        self.image_cache.clear();
        self.svg_cache.clear();
        self.failed_cache.clear();
        self.pending.clear();
        self.request_ids.clear();
        self.bytes_used = 0;
        self.watcher = None;
        self.watched.clear();
    }

    /// Inserts a decoded image into the cache, updates `bytes_used`, evicts down to `max_bytes`, and
    /// subscribes the watcher to the path so subsequent on-disk changes invalidate the entry.
    fn insert_image(&mut self, path: PathBuf, img: LoadedImage) {
        insert_cached_asset!(self, image_cache, path, img);
    }

    /// Inserts a decoded SVG into the cache.
    fn insert_svg(&mut self, path: PathBuf, svg: LoadedSvg) {
        insert_cached_asset!(self, svg_cache, path, svg);
    }

    /// Inserts a decode failure into the cache. Watches the path so a fix-on-disk invalidates the entry;
    /// the previous `failed_cache` was permanent which left entities `ImageLoadFailed` forever.
    fn insert_failure(&mut self, path: PathBuf, failure: ImageLoadFailed) {
        self.failed_cache.push(path.clone(), failure);
        self.watch_path(&path);
    }

    /// Debug-only invariant check: at most once per second, recompute `bytes_used` from scratch and assert
    /// it matches the running counter. Catches insert / evict paths that forget to update the running
    /// total. Release builds compile this out.
    fn maybe_reconcile_bytes(&mut self) {
        #[cfg(debug_assertions)]
        {
            let now = Instant::now();
            if now.duration_since(self.last_reconcile).as_secs() >= 1 {
                let recomputed = self.recompute_bytes_used();
                debug_assert_eq!(
                    self.bytes_used, recomputed,
                    "bytes_used drift: running={} recomputed={}",
                    self.bytes_used, recomputed
                );
                self.last_reconcile = now;
            }
        }
    }
}

impl Drop for AssetServer {
    fn drop(&mut self) {
        // Run shutdown so worker threads exit instead of leaking when an `App` is dropped (tests, benches,
        // FFI host teardown). Idempotent: a prior explicit `shutdown()` is a no-op here.
        if !self.workers.is_empty() || self.job_tx.is_some() {
            self.shutdown();
        }
    }
}

/// Best-effort clone of `LoadErrorKind`. `io::Error` is not `Clone`; we degrade to a new `io::Error` carrying
/// the same kind + message. Used only when handing out a cached failure entry to a fresh entity.
fn clone_kind(k: &LoadErrorKind) -> LoadErrorKind {
    match k {
        LoadErrorKind::NotFound => LoadErrorKind::NotFound,
        LoadErrorKind::DecodeFailed(msg) => LoadErrorKind::DecodeFailed(msg.clone()),
        LoadErrorKind::Io(e) => LoadErrorKind::Io(std::io::Error::new(e.kind(), e.to_string())),
        LoadErrorKind::Cancelled => LoadErrorKind::Cancelled,
        LoadErrorKind::Unsupported => LoadErrorKind::Unsupported,
    }
}

/// N-worker decode loop. Reads jobs from the shared `job_rx`, runs the loader the job carries, and forwards
/// the result. Exits cleanly on `RecvError` (channel disconnected - the only way `AssetServer::shutdown`
/// signals teardown).
fn worker_loop(
    job_rx: Receiver<DecodeJob>,
    result_tx: Sender<DecodeResult>,
    shutdown_flag: Arc<Mutex<bool>>,
) {
    while let Ok(job) = job_rx.recv() {
        // Short-circuit if shutdown was requested between recv and decode.
        if shutdown_flag.lock().map(|g| *g).unwrap_or(false) {
            break;
        }
        let ctx = LoadContext::new(&job.path, job.resolved_bytes.as_deref());
        let outcome = job.loader.load(&ctx);
        let _ = result_tx.send(DecodeResult {
            path: job.path,
            request_id: job.request_id,
            outcome,
        });
    }
}

/// Plugin that installs [`AssetServer`] and registers [`spawn_pending_decodes`], [`drain_completed_decodes`], [`process_watch_events`], and the image/SVG extract fns.
pub struct AssetsPlugin;

impl Plugin for AssetsPlugin {
    fn build(self, app: &mut App) {
        // Init rather than insert: a plugin installed earlier may already
        // have created the server to register a loader on it, and replacing
        // it here would drop that registration.
        app.world.init_resource::<AssetServer>();
        app.add_message::<AssetReloadRequested>();
        app.add_systems(TickStage::Systems, spawn_pending_decodes);
        app.add_systems(
            TickStage::Systems,
            drain_completed_decodes.after(spawn_pending_decodes),
        );
        app.add_systems(
            TickStage::Systems,
            process_watch_events.after(drain_completed_decodes),
        );
        // D6: intrinsic-size stamping. `Added<LoadedImage>` from either
        // the cache-hit or the decode-completion path is visible here on
        // the tick after the insert; the write dirties layout via the
        // layout crate's `Changed<ImageComponent>` hook.
        app.add_systems(
            TickStage::Systems,
            stamp_image_natural_size.after(drain_completed_decodes),
        );
        app.add_extract_fn(extract_loaded_images);
        app.add_extract_fn(extract_loaded_svgs);
    }
}

/// Registers an [`AssetLoader`] from a plugin's `build`, installing the
/// [`AssetServer`] first if this plugin runs before [`AssetsPlugin`].
///
/// ```no_run
/// # use lumen_core::prelude::*;
/// # use lumen_assets::{AssetKind, AssetLoader, LoadContext, LoadErrorKind, LoadedAsset};
/// # struct QoiLoader;
/// # impl AssetLoader for QoiLoader {
/// #     fn extensions(&self) -> &[&str] { &["qoi"] }
/// #     fn kind(&self) -> AssetKind { AssetKind::Image }
/// #     fn load(&self, _ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
/// #         Err(LoadErrorKind::Unsupported)
/// #     }
/// # }
/// struct QoiPlugin;
///
/// impl Plugin for QoiPlugin {
///     fn build(self, app: &mut App) {
///         lumen_assets::register_asset_loader(app, QoiLoader);
///     }
/// }
/// ```
pub fn register_asset_loader(app: &mut App, loader: impl AssetLoader) {
    if !app.world.contains_resource::<AssetServer>() {
        app.world.insert_resource(AssetServer::default());
    }
    app.world
        .resource_mut::<AssetServer>()
        .register_loader(loader);
}

// `LruCache::unbounded()` uses `NonZeroUsize::MAX` internally; we never use bounded mode because the byte
// budget is enforced via `evict_until(max_bytes)` directly on `bytes_used`. Keeping this in scope makes the
// dependency explicit.
#[allow(dead_code)]
const _LRU_CAP_PROBE: NonZeroUsize = NonZeroUsize::MIN;

/// Extracts every `(Transform, LoadedSvg)` entity into an [`ExtractedSvg`] in the render world via keyed upsert.
/// Despawns prior-frame render entries whose source entity no longer matches.
pub fn extract_loaded_svgs(main: &mut World, render: &mut World) {
    use lumen_core::components::{ImageFit, Opacity, SvgPayload};
    use lumen_core::render_world::{
        RenderEntityMap, build_parent_map, hidden_entities, paint_order_of,
    };
    let (parents, mut depth_cache) = build_parent_map(main);
    // A `Visible(false)` on this entity or any ancestor suppresses paint for
    // the whole subtree (CSS `visibility: hidden`), matching the core rect /
    // text extractors.
    let hidden = hidden_entities(main, &parents);
    #[allow(clippy::type_complexity)]
    let mut q = main.query::<(
        Entity,
        &Transform,
        &LoadedSvg,
        Option<&ImageFit>,
        Option<&Opacity>,
    )>();
    let pairs: Vec<(Entity, ExtractedSvg, SvgPayload)> = q
        .iter(main)
        .filter(|(e, ..)| !hidden.contains(e))
        .map(|(e, t, svg, fit, opacity)| {
            let extracted = ExtractedSvg {
                origin: t.absolute,
                size: t.size,
                intrinsic: svg.intrinsic,
                asset: svg.0.clone(),
                // Default fit for SVGs is `Contain` (aspect-preserving).
                fit: fit.copied().unwrap_or(ImageFit::Contain),
                order: paint_order_of(e, &parents, &mut depth_cache),
                alpha: opacity.map(|o| o.0).unwrap_or(1.0),
            };
            // Sidecar payload for the Node-IR splice path. The walker downcasts the inner Arc
            // back to ExtractedSvg.
            let payload = SvgPayload {
                payload: std::sync::Arc::new(extracted.clone()),
                order: extracted.order,
            };
            (e, extracted, payload)
        })
        .collect();
    // Keyed upsert against `RenderEntityMap.svg`; reused render entities are filtered for current validity.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().svg);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, svg, payload) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert((svg, payload));
                re
            }
            None => render.spawn((svg, payload)).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().svg = next;
}

/// Render-world sidecar carrying a pre-built `peniko::Blob` for an extracted image.
/// Queried alongside `ExtractedImage` by the renderer so the Blob is fed directly into vello, preserving its GPU upload-cache identity.
///
/// ## Node IR splice (W2.1 follow-up)
///
/// The retained Node IR's `Node::Image { blob: Option<Arc<dyn Any + Send + Sync>> }` already accepts this
/// payload as an opaque `Arc<dyn Any>` so the walker downcasts back to `ExtractedImageBlob` and feeds vello.
/// On the on-screen winit path the splice is handled today by `render_frame` reading both `ExtractedImage` and
/// `ExtractedImageBlob` in lockstep and emitting the image draw alongside the retained-tree walk; the same
/// splice is the reason the offscreen retained-tree path doesn't yet light up images. The audit-follow-up that
/// finishes the migration must update `lumen_core::node_ir::transform_extracted_to_nodes` to query
/// `ExtractedImageBlob` and set `Node::Image.blob = Some(Arc::new(blob.clone()))` directly - which requires a
/// core-touching PR that this crate is not allowed to perform alone.
#[derive(Component, Clone)]
pub struct ExtractedImageBlob(pub vello::peniko::Blob<u8>);

/// Extracts every `(Transform, LoadedImage)` entity into a paired [`ExtractedImage`] + [`ExtractedImageBlob`]
/// in the render world. Clones the `Blob` (Arc-cheap) so vello's upload-cache identity is preserved across
/// frames.
///
/// The sidecar [`ExtractedImageBlob`] is the Node-IR-ready blob payload - see its doc-comment for the splice
/// path to `Node::Image { blob: Some(...) }`.
pub fn extract_loaded_images(main: &mut World, render: &mut World) {
    use lumen_core::components::{ImageBlob, ImageFit, Opacity};
    use lumen_core::render_world::{build_parent_map, hidden_entities, paint_order_of};
    let (parents, mut depth_cache) = build_parent_map(main);
    // A `Visible(false)` on this entity or any ancestor suppresses paint for
    // the whole subtree (CSS `visibility: hidden`), matching the core rect /
    // text extractors.
    let hidden = hidden_entities(main, &parents);
    #[allow(clippy::type_complexity)]
    let mut q = main.query::<(
        Entity,
        &Transform,
        &LoadedImage,
        Option<&ImageFit>,
        Option<&Opacity>,
    )>();
    let rows: Vec<(Entity, ExtractedImage, ExtractedImageBlob, ImageBlob)> = q
        .iter(main)
        .filter(|(e, ..)| !hidden.contains(e))
        .map(|(e, t, img, fit, opacity)| {
            let extracted = ExtractedImage {
                origin: t.absolute,
                size: t.size,
                width: img.width,
                height: img.height,
                rgba: img.rgba.clone(),
                fit: fit.copied().unwrap_or_default(),
                order: paint_order_of(e, &parents, &mut depth_cache),
                alpha: opacity.map(|o| o.0).unwrap_or(1.0),
            };
            // Sidecar: the legacy `ExtractedImageBlob` keeps any external query path
            // (embedders, headless tests) working; the core-facing `ImageBlob` carries the same
            // payload type-erased so `lumen_core::node_ir::transform_extracted_to_nodes` can
            // splice it into `Node::Image.blob` without lumen-core needing a `vello` dep.
            let blob = ExtractedImageBlob(img.blob.clone());
            let core_blob = ImageBlob(std::sync::Arc::new(blob.clone()));
            (e, extracted, blob, core_blob)
        })
        .collect();
    // Keyed upsert against `RenderEntityMap.image`; reused render entities are filtered for current validity.
    use lumen_core::render_world::RenderEntityMap;
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().image);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(rows.len());
    for (main_e, img, blob, core_blob) in rows {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert((img, blob, core_blob));
                re
            }
            None => render.spawn((img, blob, core_blob)).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().image = next;
}

/// Consults the content cache for each [`ImageSource`] entity lacking a [`LoadedImage`], [`LoadedSvg`], [`ImageLoadFailed`], or [`Enqueued`].
/// Cache hits attach the cached component synchronously; misses enqueue one decode job and append the entity (with its current `request_id`) to the fan-out list.
#[allow(clippy::type_complexity)]
pub fn spawn_pending_decodes(
    mut server: ResMut<AssetServer>,
    pending: Query<
        (Entity, &ImageSource),
        (
            Without<LoadedImage>,
            Without<LoadedSvg>,
            Without<ImageLoadFailed>,
            Without<Enqueued>,
        ),
    >,
    mut commands: Commands,
) {
    for (entity, source) in &pending {
        match server.lookup_or_enqueue(entity, source.0.clone()) {
            CacheLookup::HitImage(img) => {
                commands.entity(entity).insert(img);
            }
            CacheLookup::HitSvg(svg) => {
                commands.entity(entity).insert(svg);
            }
            CacheLookup::HitFailed(failed) => {
                commands.entity(entity).insert(failed);
            }
            CacheLookup::InFlight | CacheLookup::Enqueued => {
                commands.entity(entity).insert(Enqueued);
            }
        }
    }
}

/// Drains completed decodes from the result channel.
///
/// For each result:
/// - Filters the path's waiter list against per-entity request ids. Waiters whose snapshot id no longer
///   matches their entity's current `RequestId` have been superseded by a newer `SetSrc` and are silently
///   dropped - they would race with the live decode.
/// - If at least one waiter survives the filter: inserts the payload into the success or failure cache
///   (LRU-bumping recency) and attaches the result component to every surviving waiter.
/// - If *no* waiter survives: the result is dropped entirely (no cache insert). This kills the stale-decode
///   race at the cache layer - a SetSrc-storm enqueues N transient decodes whose final results never poison
///   the LRU with paths the runtime no longer cares about.
pub fn drain_completed_decodes(mut server: ResMut<AssetServer>, mut commands: Commands) {
    while let Ok(result) = server.result_rx.try_recv() {
        let waiters = server.pending.remove(&result.path).unwrap_or_default();
        let surviving: Vec<PendingWaiter> = waiters
            .into_iter()
            .filter(|w| server.current_request_id(w.entity) == w.request_id)
            .collect();
        if surviving.is_empty() {
            // No waiter is interested any more; the request_id snapshot on the job is stale across every
            // waiter. Drop the result without touching the cache. Logs at trace so SetSrc-storm scenarios
            // remain inspectable in dev without spamming production builds.
            tracing::trace!(
                "asset: dropping stale decode for {:?} (request_id {} no longer current)",
                result.path,
                result.request_id
            );
            continue;
        }
        match result.outcome {
            Ok(LoadedAsset::Image(img)) => {
                dispatch_decoded!(server, commands, result.path, insert_image, img, surviving);
            }
            Ok(LoadedAsset::Svg(svg)) => {
                dispatch_decoded!(server, commands, result.path, insert_svg, svg, surviving);
            }
            Err(kind) => {
                // The failure is cached once under the path and cloned onto
                // every surviving waiter, so a second entity requesting the
                // same path gets the same verdict without re-running the
                // loader.
                let failure = ImageLoadFailed::new(kind);
                let detail = failure.detail.clone();
                let kind_copy = clone_kind(&failure.kind);
                server.insert_failure(result.path, failure);
                for w in surviving {
                    let mut ec = commands.entity(w.entity);
                    ec.remove::<Enqueued>();
                    ec.insert(ImageLoadFailed {
                        kind: clone_kind(&kind_copy),
                        detail: detail.clone(),
                    });
                }
            }
        }
    }
}

/// Drains filesystem-watch events and invalidates the cache for every changed path.
///
/// On each event: pops the path from `image_cache` / `svg_cache` / `failed_cache`, fires
/// [`AssetReloadRequested`] so consumers can re-enqueue, and bumps the request id of every entity currently
/// holding an `ImageSource(path)` so any in-flight decode for the now-invalidated bytes is identified as
/// stale on completion.
pub fn process_watch_events(
    mut server: ResMut<AssetServer>,
    sources: Query<(Entity, &ImageSource)>,
    mut writer: MessageWriter<AssetReloadRequested>,
) {
    // Collect changed paths first to release the borrow on `server.watch_rx`. `recv` borrows immutably; we
    // need `&mut server` afterwards for `invalidate_path` + `bump_request_id`.
    let mut changed: Vec<PathBuf> = Vec::new();
    while let Ok(ev) = server.watch_rx.try_recv() {
        match ev {
            Ok(event) => {
                use notify::EventKind;
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                        for p in event.paths {
                            changed.push(p);
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => {
                tracing::warn!("asset watch error: {e}");
            }
        }
    }
    for path in changed {
        if !server.invalidate_path(&path) {
            // Still notify consumers - they may want to re-resolve the path even if the cache had nothing.
            // But skip the request-id bumps in that case (nothing to reconcile).
        }
        // Bump request ids for every entity whose source matches.
        let affected: Vec<Entity> = sources
            .iter()
            .filter(|(_, src)| src.0 == path)
            .map(|(e, _)| e)
            .collect();
        for e in affected {
            server.bump_request_id(e);
        }
        writer.write(AssetReloadRequested { path });
    }
}

/// Marker component on entities whose decode has been enqueued; excluded from the [`spawn_pending_decodes`] query.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Enqueued;

/// D6: stamp the decoded bitmap's intrinsic size onto
/// [`lumen_core::components::ImageComponent`] as soon as a
/// [`LoadedImage`] lands on the entity (cache hit or decode
/// completion - both surface as `Added`/`Changed<LoadedImage>`).
///
/// The layout backend's measure path reads `ImageComponent.natural_size`
/// for images without an explicit size (spec section 13: intrinsic = bitmap
/// logical size; explicit size wins because taffy passes known
/// dimensions first). The bitmap's pixel size is reported 1:1 as
/// logical pixels - no `@2x` asset-scale metadata exists yet. A
/// `Changed<ImageComponent>` hook in the layout crate turns this write
/// into a `DirtyLayout`, so the image relayouts to its natural size on
/// the tick after decode.
pub fn stamp_image_natural_size(
    mut commands: Commands,
    changed: Query<(Entity, &ImageSource, &LoadedImage), Changed<LoadedImage>>,
    mut existing: Query<&mut lumen_core::components::ImageComponent>,
) {
    for (entity, source, img) in &changed {
        let natural = glam::Vec2::new(img.width as f32, img.height as f32);
        let source_str = source.0.display().to_string();
        if let Ok(mut ic) = existing.get_mut(entity) {
            // Avoid bumping change detection on identical re-stamps.
            if ic.natural_size != Some(natural) || ic.source != source_str {
                ic.natural_size = Some(natural);
                ic.source = source_str;
            }
        } else {
            commands
                .entity(entity)
                .insert(lumen_core::components::ImageComponent {
                    source: source_str,
                    natural_size: Some(natural),
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image_data(byte_count: usize) -> ImageData {
        let bytes = vec![0u8; byte_count];
        ImageData {
            width: 4,
            height: 4,
            rgba: Arc::from(bytes.clone().into_boxed_slice()),
            blob: vello::peniko::Blob::new(Arc::new(PixBytes(Arc::from(bytes.into_boxed_slice())))),
        }
    }

    /// D6: once a `LoadedImage` lands on an entity, the stamp system
    /// writes the bitmap's intrinsic size into `ImageComponent` (the
    /// layout measure path's input) and re-stamps without bumping
    /// change detection when nothing changed.
    #[test]
    fn stamp_image_natural_size_writes_intrinsic() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = World::new();
        // make_image_data yields a 4x4 bitmap.
        let e = world
            .spawn((
                ImageSource(PathBuf::from("probe.png")),
                LoadedImage(make_image_data(64).into()),
            ))
            .id();
        world.run_system_once(stamp_image_natural_size).unwrap();
        let ic = world
            .get::<lumen_core::components::ImageComponent>(e)
            .expect("ImageComponent stamped");
        assert_eq!(ic.natural_size, Some(glam::Vec2::new(4.0, 4.0)));
        assert_eq!(ic.source, "probe.png");

        // Identical re-stamp must not flip change detection (it would
        // re-dirty layout every tick via the Changed<ImageComponent>
        // hook).
        let tick = world
            .entity(e)
            .get_ref::<lumen_core::components::ImageComponent>()
            .unwrap()
            .last_changed();
        world.run_system_once(stamp_image_natural_size).unwrap();
        assert_eq!(
            world
                .entity(e)
                .get_ref::<lumen_core::components::ImageComponent>()
                .unwrap()
                .last_changed(),
            tick
        );
    }

    #[test]
    fn handle_clone_preserves_identity() {
        let data = make_image_data(64);
        let h1: Handle<ImageData> = data.into();
        let h2 = h1.clone();
        assert_eq!(h1.id(), h2.id(), "clones must share identity");
        assert_eq!(h1.bytes(), 64);
    }

    #[test]
    fn cache_dedupes_in_flight_requests() {
        let mut server = AssetServer::default();
        let path = PathBuf::from("/tmp/lumen-cache-test-nonexistent.png");
        let e1 = Entity::from_raw_u32(1).unwrap();
        let e2 = Entity::from_raw_u32(2).unwrap();
        let e3 = Entity::from_raw_u32(3).unwrap();
        assert!(matches!(
            server.lookup_or_enqueue(e1, path.clone()),
            CacheLookup::Enqueued
        ));
        assert!(matches!(
            server.lookup_or_enqueue(e2, path.clone()),
            CacheLookup::InFlight
        ));
        assert!(matches!(
            server.lookup_or_enqueue(e3, path.clone()),
            CacheLookup::InFlight
        ));
        let waiters = server.pending.get(&path).expect("pending list");
        assert_eq!(waiters.len(), 3);
    }

    #[test]
    fn evict_until_drops_lru_first() {
        let mut server = AssetServer::default();
        // Insert three 16 KB images.
        let paths: Vec<PathBuf> = (0..3u8)
            .map(|i| PathBuf::from(format!("/tmp/lumen-evict-test-{i}.png")))
            .collect();
        for p in &paths {
            server.insert_image(p.clone(), LoadedImage(make_image_data(16 * 1024).into()));
        }
        // Touch the most-recently-inserted entry so it's the freshest (insert already bumps recency, but make it explicit).
        let _touched = server.image_cache.get(&paths[2]).cloned();
        assert!(server.bytes_used() >= 48 * 1024);
        // Cap to 20 KB; the LRU drops the oldest two.
        server.evict_until(20 * 1024);
        assert!(server.bytes_used() <= 20 * 1024);
        assert!(server.image_cache.len() <= 1);
        // The freshest entry must survive - that's the LRU contract.
        assert!(server.image_cache.contains(&paths[2]));
        assert!(!server.image_cache.contains(&paths[0]));
    }

    #[test]
    fn cache_returns_cached_hit_synchronously() {
        let mut server = AssetServer::default();
        let path = PathBuf::from("/tmp/lumen-cache-test-cached.png");
        let cached = LoadedImage(make_image_data(16).into());
        let cached_id = cached.0.id();
        server.insert_image(path.clone(), cached);
        let e = Entity::from_raw_u32(7).unwrap();
        match server.lookup_or_enqueue(e, path.clone()) {
            CacheLookup::HitImage(img) => {
                assert_eq!(img.0.id(), cached_id, "must hand out same handle");
            }
            _ => panic!("expected cache hit"),
        }
        assert!(
            !server.pending.contains_key(&path),
            "no decode should be queued"
        );
    }

    #[test]
    fn bytes_used_is_o1_running_counter() {
        let mut server = AssetServer::default();
        let p1 = PathBuf::from("/tmp/lumen-bytes-1.png");
        let p2 = PathBuf::from("/tmp/lumen-bytes-2.png");
        server.insert_image(p1.clone(), LoadedImage(make_image_data(1024).into()));
        assert_eq!(server.bytes_used(), 1024);
        server.insert_image(p2.clone(), LoadedImage(make_image_data(2048).into()));
        assert_eq!(server.bytes_used(), 1024 + 2048);
        // Overwrite p1 with a smaller image; bytes_used must reflect the swap exactly.
        server.insert_image(p1.clone(), LoadedImage(make_image_data(512).into()));
        assert_eq!(server.bytes_used(), 512 + 2048);
        // Recomputed sweep must agree with the running counter.
        assert_eq!(server.bytes_used(), server.recompute_bytes_used());
    }

    #[test]
    fn invalidate_path_drops_caches_and_decrements_bytes() {
        let mut server = AssetServer::default();
        let path = PathBuf::from("/tmp/lumen-invalidate.png");
        server.insert_image(path.clone(), LoadedImage(make_image_data(4096).into()));
        assert_eq!(server.bytes_used(), 4096);
        assert!(server.invalidate_path(&path));
        assert_eq!(server.bytes_used(), 0);
        assert!(!server.image_cache.contains(&path));
    }

    #[test]
    fn request_id_monotonic_per_entity() {
        let mut server = AssetServer::default();
        let e = Entity::from_raw_u32(11).unwrap();
        assert_eq!(server.current_request_id(e), 0);
        assert_eq!(server.bump_request_id(e), 1);
        assert_eq!(server.bump_request_id(e), 2);
        assert_eq!(server.current_request_id(e), 2);
    }

    #[test]
    fn load_error_kind_from_io_error_maps_not_found() {
        let io_err = std::io::Error::from(std::io::ErrorKind::NotFound);
        let k: LoadErrorKind = io_err.into();
        assert!(matches!(k, LoadErrorKind::NotFound));
        let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let k2: LoadErrorKind = other.into();
        assert!(matches!(k2, LoadErrorKind::Io(_)));
    }

    #[test]
    fn shutdown_clears_caches_and_joins_workers() {
        let mut server = AssetServer::default();
        // Workers are spawned lazily, so force one enqueue to materialise the
        // pool before asserting shutdown joins it.
        let e = Entity::from_raw_u32(9_001).expect("valid entity id");
        let _ = server.lookup_or_enqueue(e, PathBuf::from("/tmp/lumen-nonexistent.png"));
        let path = PathBuf::from("/tmp/lumen-shutdown-test.png");
        server.insert_image(path, LoadedImage(make_image_data(64).into()));
        assert_eq!(server.bytes_used(), 64);
        let worker_count = server.workers.len();
        assert!(worker_count >= 1);
        server.shutdown();
        assert_eq!(server.bytes_used(), 0);
        assert!(server.workers.is_empty());
        assert!(server.job_tx.is_none());
        // Idempotent.
        server.shutdown();
    }

    #[test]
    fn svg_bytes_track_source_bytes() {
        let svg = SvgData {
            intrinsic: glam::Vec2::ZERO,
            scene: vello::Scene::new(),
            source_bytes: 12_345,
        };
        assert_eq!(svg.bytes(), 12_345);
    }

    /// A minimal well-formed SVG with a known intrinsic size. Its file
    /// length is what the loader budgets as the payload's byte cost.
    const TINY_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>"#;

    /// Runs a loader the way a worker would: the path, plus bytes only when
    /// a source already produced them.
    fn load_from_disk(path: &Path) -> Result<LoadedAsset, LoadErrorKind> {
        let loaders = AssetLoaders::default();
        let loader = loaders.resolve(path).expect("a loader claims the path");
        loader.load(&LoadContext::new(path, None))
    }

    #[test]
    fn default_registry_routes_by_extension() {
        let kind = |p: &str| AssetLoaders::default().kind_for(Path::new(p));
        assert_eq!(kind("pic.png"), Some(AssetKind::Image));
        assert_eq!(kind("icon.svg"), Some(AssetKind::Svg));
        // Extension matching is case-insensitive, and a `lumen://` URI
        // resolves the same way a filesystem path does.
        assert_eq!(kind("a/b/glyph.SVG"), Some(AssetKind::Svg));
        assert_eq!(kind("lumen://app/assets/x.svg"), Some(AssetKind::Svg));
        // Anything unclaimed falls back to the image loader, which is what
        // the pipeline did before extensions were registered explicitly.
        assert_eq!(kind("mystery.dat"), Some(AssetKind::Image));
        assert_eq!(kind("no-extension"), Some(AssetKind::Image));
    }

    #[test]
    fn registry_without_fallback_refuses_unclaimed_extensions() {
        let mut loaders = AssetLoaders::default();
        loaders.set_fallback(None);
        assert_eq!(loaders.kind_for(Path::new("mystery.dat")), None);
        assert_eq!(
            loaders.kind_for(Path::new("pic.png")),
            Some(AssetKind::Image)
        );
    }

    #[test]
    fn load_reads_from_disk_and_caches() {
        let dir = std::env::temp_dir().join(format!("lumen-assets-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("badge.svg");
        std::fs::write(&path, TINY_SVG).unwrap();

        let LoadedAsset::Svg(loaded) = load_from_disk(&path).expect("load the svg") else {
            panic!("a .svg path must load as an svg");
        };
        assert_eq!(loaded.intrinsic, glam::Vec2::new(8.0, 8.0));
        assert_eq!(
            loaded.source_bytes,
            TINY_SVG.len(),
            "the source length is the payload's byte cost"
        );

        // Inserting into the server accounts bytes + lookup returns a HitSvg.
        let mut server = AssetServer::default();
        server.insert_svg(path.clone(), loaded);
        assert_eq!(server.bytes_used(), TINY_SVG.len());
        let e = Entity::from_raw_u32(7).unwrap();
        assert!(matches!(
            server.lookup_or_enqueue(e, path.clone()),
            CacheLookup::HitSvg(_)
        ));
        assert_eq!(server.bytes_used(), server.recompute_bytes_used());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_bytes_the_decoder_refuses() {
        let dir = std::env::temp_dir().join(format!("lumen-assets-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.svg");
        std::fs::write(&path, b"<svg not really").unwrap();
        assert!(matches!(
            load_from_disk(&path),
            Err(LoadErrorKind::DecodeFailed(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_pool_decodes_off_thread() {
        // End-to-end through the real crossbeam worker pool: enqueue a path,
        // then read the decoded result off the channel.
        let dir = std::env::temp_dir().join(format!("lumen-assets-pool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pool.svg");
        std::fs::write(&path, TINY_SVG).unwrap();

        let mut server = AssetServer::default();
        let e = Entity::from_raw_u32(3).unwrap();
        assert!(matches!(
            server.lookup_or_enqueue(e, path.clone()),
            CacheLookup::Enqueued
        ));
        let result = server
            .result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker produced a decode result");
        assert_eq!(result.path, path);
        assert!(
            matches!(result.outcome, Ok(LoadedAsset::Svg(_))),
            "an svg path decodes to an Svg asset off the worker thread"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
