//! How a source path becomes a decoded asset.
//!
//! [`AssetLoader`] is the extension point: a loader claims one or more file
//! extensions, declares which [`AssetKind`] it produces, and turns one
//! [`LoadContext`] into a [`LoadedAsset`]. [`AssetLoaders`] is the registry
//! the [`crate::AssetServer`] resolves through, and the built-in image, SVG,
//! and audio paths are ordinary loaders registered into it by
//! [`AssetLoaders::default`].
//!
//! The shape follows Bevy's `AssetLoader`, adapted to how this crate loads:
//!
//! - Loading is synchronous. A load runs on the decode worker pool, so a
//!   loader blocks its worker thread instead of awaiting.
//! - A load is described by a path plus, optionally, bytes an
//!   [`crate::AssetSource`] already resolved. It is not a byte stream,
//!   because the built-in image path hands the path to `image::open` rather
//!   than reading the file itself.
//! - The loader is resolved on the main thread at enqueue time and travels
//!   with the job, so registering a loader never has to be synchronised with
//!   a running worker.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::loaders::{AudioLoader, ImageLoader, SvgLoader};
use crate::{LoadErrorKind, LoadedAudio, LoadedImage, LoadedSvg};

/// Which family of asset a loader produces.
///
/// The kind selects the content cache the decoded payload lands in and, when
/// a load fails, which failure component the waiting entities get.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// Raster image, cached as [`LoadedImage`].
    Image,
    /// Vector image, cached as [`LoadedSvg`].
    Svg,
    /// Audio track, cached as [`LoadedAudio`].
    Audio,
}

/// One successfully loaded asset, tagged by family.
///
/// This is what a loader returns and what the drain dispatches on.
pub enum LoadedAsset {
    /// A decoded raster image.
    Image(LoadedImage),
    /// A pre-rendered SVG.
    Svg(LoadedSvg),
    /// An audio track's encoded bytes.
    Audio(LoadedAudio),
}

impl LoadedAsset {
    /// Returns the family this payload belongs to.
    pub fn kind(&self) -> AssetKind {
        match self {
            Self::Image(_) => AssetKind::Image,
            Self::Svg(_) => AssetKind::Svg,
            Self::Audio(_) => AssetKind::Audio,
        }
    }
}

/// Everything a loader is given for one load.
///
/// `bytes` is `Some` when an asset source (a registered `.lpak` bundle, say)
/// already produced the file contents; in that case `path` is the requested
/// path and may be a `lumen://app/...` URI rather than a filesystem location.
/// `bytes` is `None` when the path is expected to be read from disk.
pub struct LoadContext<'a> {
    path: &'a Path,
    bytes: Option<&'a [u8]>,
}

impl<'a> LoadContext<'a> {
    /// Builds a context for `path`, optionally carrying bytes an asset source
    /// already resolved.
    pub fn new(path: &'a Path, bytes: Option<&'a [u8]>) -> Self {
        Self { path, bytes }
    }

    /// The requested path. Use it for error messages and for decoders that
    /// take a path directly.
    pub fn path(&self) -> &'a Path {
        self.path
    }

    /// Bytes an asset source already resolved, or `None` when the loader is
    /// expected to read [`Self::path`] itself.
    pub fn bytes(&self) -> Option<&'a [u8]> {
        self.bytes
    }

    /// The asset's bytes: the pre-resolved ones when a source supplied them,
    /// otherwise the file read from disk.
    pub fn read_bytes(&self) -> Result<Cow<'a, [u8]>, LoadErrorKind> {
        match self.bytes {
            Some(b) => Ok(Cow::Borrowed(b)),
            None => std::fs::read(self.path)
                .map(Cow::Owned)
                .map_err(LoadErrorKind::from),
        }
    }
}

/// Turns one source path into a decoded asset.
///
/// Implement it to teach the asset pipeline a format it does not know, then
/// register it with [`crate::AssetServer::register_loader`] (or
/// [`crate::register_asset_loader`] from a plugin's `build`). Loaders run on
/// the decode worker pool, so an implementation must be `Send + Sync` and
/// should assume it is blocking a thread.
pub trait AssetLoader: Send + Sync + 'static {
    /// File extensions this loader claims, lowercase and without the dot.
    /// Registering a loader claims every extension it names; a later
    /// registration wins over an earlier one for the same extension.
    fn extensions(&self) -> &[&str];

    /// The family this loader produces. The object-safe stand-in for Bevy's
    /// associated `type Asset`.
    fn kind(&self) -> AssetKind;

    /// Loads one asset. Returning `Err` caches the failure against the path
    /// and attaches a failure component to every entity waiting on it.
    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind>;
}

/// Extension-keyed loader registry.
///
/// [`Self::default`] registers the built-in image, SVG, and audio loaders and
/// installs the image loader as the fallback, which is what makes a path with
/// an unrecognised extension attempt an image decode.
pub struct AssetLoaders {
    by_extension: HashMap<String, Arc<dyn AssetLoader>>,
    fallback: Option<Arc<dyn AssetLoader>>,
}

impl Default for AssetLoaders {
    fn default() -> Self {
        let mut loaders = Self::empty();
        let image: Arc<dyn AssetLoader> = Arc::new(ImageLoader);
        loaders.register_arc(image.clone());
        loaders.register(SvgLoader);
        loaders.register(AudioLoader);
        loaders.set_fallback(Some(image));
        loaders
    }
}

impl AssetLoaders {
    /// A registry with no loaders and no fallback. Every load fails with
    /// [`LoadErrorKind::Unsupported`] until something is registered.
    pub fn empty() -> Self {
        Self {
            by_extension: HashMap::new(),
            fallback: None,
        }
    }

    /// Registers `loader` for each extension it names.
    pub fn register(&mut self, loader: impl AssetLoader) {
        self.register_arc(Arc::new(loader));
    }

    /// Registers an already shared loader. Useful when the same instance
    /// should also serve as the fallback.
    pub fn register_arc(&mut self, loader: Arc<dyn AssetLoader>) {
        for ext in loader.extensions() {
            self.by_extension
                .insert(ext.to_ascii_lowercase(), loader.clone());
        }
    }

    /// Sets the loader used for paths whose extension nothing claims, or
    /// clears it so those paths fail with [`LoadErrorKind::Unsupported`].
    pub fn set_fallback(&mut self, loader: Option<Arc<dyn AssetLoader>>) {
        self.fallback = loader;
    }

    /// The loader that handles unclaimed extensions, if any.
    pub fn fallback(&self) -> Option<&Arc<dyn AssetLoader>> {
        self.fallback.as_ref()
    }

    /// The loader that will handle `path`: the one registered for its
    /// extension, else the fallback.
    pub fn resolve(&self, path: &Path) -> Option<Arc<dyn AssetLoader>> {
        asset_extension(path)
            .and_then(|ext| self.by_extension.get(&ext))
            .or(self.fallback.as_ref())
            .cloned()
    }

    /// The family `path` will load as, if any loader handles it.
    pub fn kind_for(&self, path: &Path) -> Option<AssetKind> {
        self.resolve(path).map(|l| l.kind())
    }

    /// Every claimed extension, unordered.
    pub fn extensions(&self) -> impl Iterator<Item = &str> {
        self.by_extension.keys().map(String::as_str)
    }
}

/// The lowercase extension used for loader lookup.
///
/// Falls back to the last dot-separated segment of the final path component
/// when `Path::extension` yields nothing, so `lumen://app/icons/sun.svg`
/// resolves the same way a filesystem path does.
pub fn asset_extension(path: &Path) -> Option<String> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        return Some(ext.to_ascii_lowercase());
    }
    let name = path.to_str()?.rsplit(['/', '\\']).next()?;
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}
