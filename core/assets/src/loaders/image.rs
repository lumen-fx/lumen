//! Raster image loader, decoding to RGBA8 plus a `peniko::Blob`.

use std::sync::Arc;

use crate::{
    AssetKind, AssetLoader, ImageData, LoadContext, LoadErrorKind, LoadedAsset, LoadedImage,
    PixBytes,
};

/// Extensions the image loader claims.
///
/// The workspace builds the `image` crate with PNG support only, so PNG is
/// the format that decodes. Other raster extensions still reach this loader
/// through [`crate::AssetLoaders`]' fallback and fail with a decoder message
/// naming the format.
pub const IMAGE_EXTENSIONS: &[&str] = &["png"];

/// Decodes a raster image into [`ImageData`].
///
/// The decoded pixels are published twice over one allocation: as `rgba` for
/// consumers that read pixels directly, and as a `peniko::Blob` whose
/// identity keys vello's GPU upload cache across frames.
pub struct ImageLoader;

impl AssetLoader for ImageLoader {
    fn extensions(&self) -> &[&str] {
        IMAGE_EXTENSIONS
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Image
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let path = ctx.path();
        // When a source already produced the bytes the decode runs from
        // memory; otherwise `image::open` reads the file itself, which is
        // also what distinguishes a missing file from undecodable contents.
        let img = match ctx.bytes() {
            Some(bytes) => image::load_from_memory(bytes)
                .map_err(|e| LoadErrorKind::DecodeFailed(format!("{path:?}: {e}")))?,
            None => match image::open(path) {
                Ok(img) => img,
                Err(image::ImageError::IoError(io)) => return Err(LoadErrorKind::from(io)),
                Err(image::ImageError::Unsupported(e)) => {
                    return Err(LoadErrorKind::DecodeFailed(format!("{path:?}: {e}")));
                }
                Err(e) => return Err(LoadErrorKind::DecodeFailed(format!("{path:?}: {e}"))),
            },
        };
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let pixels: Arc<[u8]> = Arc::from(rgba.into_raw());
        let blob = vello::peniko::Blob::new(Arc::new(PixBytes(pixels.clone())));
        let data = ImageData {
            width,
            height,
            rgba: pixels,
            blob,
        };
        Ok(LoadedAsset::Image(LoadedImage(data.into())))
    }
}
