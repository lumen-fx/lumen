//! Where an asset's bytes come from before a loader sees them.
//!
//! A source answers "do you have this path?" with bytes or nothing. The
//! server asks each source in turn on the main thread, before the load is
//! handed to a worker; a path no source claims is read from disk by the
//! loader itself. Bundled apps work because [`BundleSource`] is installed by
//! default, and an app that keeps its assets somewhere else (embedded in the
//! binary, a download cache) registers its own with
//! [`crate::AssetServer::register_source`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bundle::{LumenBundle, parse_lumen_uri};

/// Resolves a requested path to bytes, or declines it.
///
/// Sources are consulted on the main thread while the load is being queued,
/// so `read` should be cheap: an index lookup and a copy, not a network
/// round trip.
pub trait AssetSource: Send + Sync + 'static {
    /// Returns the bytes for `path`, or `None` to let the next source (and
    /// ultimately the filesystem) answer.
    fn read(&self, path: &Path) -> Option<Vec<u8>>;
}

/// The built-in source: bytes served out of registered `.lpak` bundles.
///
/// A bundle is reachable two ways. A `lumen://app/<key>` URI addresses it
/// directly, and once [`Self::set_root`] names the directory the archive was
/// packed from, an ordinary path under that directory resolves to the
/// matching key as well, so markup that names `icons/sun.png` works whether
/// the app ships loose files or an archive. Bundled entries win over files on
/// disk, which is what lets an app ship the archive alone.
#[derive(Default, Clone)]
pub struct BundleSource {
    bundles: Vec<LumenBundle>,
    root: Option<PathBuf>,
}

impl BundleSource {
    /// Adds a bundle. Bundles are consulted in registration order and the
    /// first hit wins.
    pub fn register(&mut self, bundle: LumenBundle) {
        self.bundles.push(bundle);
    }

    /// Declares the directory the registered bundles were packed from.
    pub fn set_root(&mut self, root: impl Into<PathBuf>) {
        self.root = Some(root.into());
    }

    /// The registered bundles, in registration order.
    pub fn bundles(&self) -> &[LumenBundle] {
        &self.bundles
    }

    /// Resolves a `lumen://app/<key>` URI. `None` when no bundle holds the
    /// key or the URI uses another scheme.
    pub fn read_uri(&self, uri: &str) -> Option<Vec<u8>> {
        self.read_key(parse_lumen_uri(uri)?)
    }

    /// First registered bundle holding `key`, if any.
    fn read_key(&self, key: &str) -> Option<Vec<u8>> {
        self.bundles.iter().find_map(|b| b.read(key))
    }
}

/// The first source chain hit for `path`: the bundles, then every
/// registered source, in registration order. `None` when nothing claims it
/// and the filesystem answers. The one lookup behind both the server's
/// queue-time resolution and [`SourceReader::read`].
pub(crate) fn read_chain(
    bundle: &BundleSource,
    sources: &[Arc<dyn AssetSource>],
    path: &Path,
) -> Option<Vec<u8>> {
    bundle
        .read(path)
        .or_else(|| sources.iter().find_map(|s| s.read(path)))
}

/// A detached reader over the server's byte sources, for code that needs raw
/// bytes away from the ECS: the bundles, then every registered
/// [`AssetSource`], then the filesystem. Kind-agnostic - no decoder runs and
/// no cache is touched - so any plugin or runtime module that loads its own
/// data (media files, archives, anything it ships with the app) reads
/// through the same chain the asset pipeline uses instead of the raw
/// filesystem.
///
/// Get one from [`crate::AssetServer::source_reader`]. The reader is a cheap
/// snapshot: `Send + Sync`, safe to move to another thread, and free to
/// block there (an archive read is a copy, a filesystem read is I/O). It
/// does not follow later registrations - a source or bundle registered after
/// the snapshot is invisible to it - so take a fresh reader per request
/// rather than holding one for the app's lifetime.
///
/// The reader resolves the path it is given. Callers keep the same
/// conventions the rest of the pipeline uses: resolve app-relative paths
/// first (`lumen_core::app_paths::resolve`), and pass `lumen://app/...`
/// URIs through untouched - the bundle source claims those itself.
#[derive(Clone)]
pub struct SourceReader {
    bundle: BundleSource,
    sources: Vec<Arc<dyn AssetSource>>,
}

impl SourceReader {
    pub(crate) fn new(bundle: BundleSource, sources: Vec<Arc<dyn AssetSource>>) -> Self {
        Self { bundle, sources }
    }

    /// The bytes for `path`: the first source that claims it, else the
    /// filesystem. A path nothing holds is the filesystem's `NotFound`
    /// error, so a missing bundled entry and a missing file report the same
    /// way.
    pub fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        match read_chain(&self.bundle, &self.sources, path) {
            Some(bytes) => Ok(bytes),
            None => std::fs::read(path),
        }
    }
}

impl AssetSource for BundleSource {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        if self.bundles.is_empty() {
            return None;
        }
        let s = path.to_str()?;
        if s.starts_with("lumen://") {
            return self.read_uri(s);
        }
        let rel = path.strip_prefix(self.root.as_ref()?).ok()?;
        // Bundle keys are forward-slash joined at pack time, so rebuild
        // the key the same way rather than trusting the host separator.
        let key = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        self.read_key(&key)
    }
}
