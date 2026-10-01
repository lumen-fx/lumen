//! SVG loader: parses with usvg and keeps the tree for the renderer to paint.

use crate::{AssetKind, AssetLoader, LoadContext, LoadErrorKind, LoadedAsset, LoadedSvg, SvgData};

/// Extensions the SVG loader claims.
pub const SVG_EXTENSIONS: &[&str] = &["svg"];

/// Parses an SVG into a [`SvgData`].
///
/// Parsing happens once, at load time, on the asset worker; the renderer
/// paints the parsed tree and caches the drawing where its backend can. The
/// source file length is recorded as the payload's byte cost, because usvg
/// does not expose the size of a parsed tree.
pub struct SvgLoader;

impl AssetLoader for SvgLoader {
    fn extensions(&self) -> &[&str] {
        SVG_EXTENSIONS
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Svg
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let path = ctx.path();
        let bytes = ctx.read_bytes()?;
        let source_bytes = bytes.len();
        let opt = usvg::Options::default();
        let tree = usvg::Tree::from_data(&bytes, &opt)
            .map_err(|e| LoadErrorKind::DecodeFailed(format!("{path:?}: {e}")))?;
        let size = tree.size();
        let data = SvgData {
            intrinsic: glam::Vec2::new(size.width(), size.height()),
            tree,
            id: SvgData::next_id(),
            source_bytes,
        };
        Ok(LoadedAsset::Svg(LoadedSvg(data.into())))
    }
}
