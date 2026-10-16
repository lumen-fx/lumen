//! Turning a drawing into pixels outside the frame.
//!
//! A frame is painted into the window, and nothing in it can be read back
//! piecewise. Some drawings are wanted as pixels in their own right: a
//! drawing surface read back by the app, a thumbnail, an export. A render
//! backend that can rasterize on demand publishes a [`Rasterizer`] into its
//! render world, and whoever needs pixels hands it a [`Recording`].
//!
//! The rasterizer is the backend's own, so what comes back is what that
//! backend would have drawn: the CPU renderer rasterizes with vello_cpu, the
//! GPU renderer renders on its device and copies the result back.

use std::sync::Arc;

use bevy_ecs::resource::Resource;
use peniko::kurbo::Affine;

use crate::Recording;

/// Rasterizes a [`Recording`] into straight-alpha RGBA8 on demand.
pub trait Rasterize: Send + Sync + 'static {
    /// Paint `drawing` under `transform` into a transparent `width` x
    /// `height` image and hand back its pixels: rows top to bottom, four
    /// straight (not premultiplied) bytes per pixel.
    ///
    /// Blocks until the pixels are on the CPU. The cost is a full rasterize
    /// of the drawing at that size, plus, on a GPU, the copy back.
    fn rasterize(
        &self,
        drawing: &Recording,
        transform: Affine,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String>;
}

/// The render world's rasterizer, published by the render backend once it
/// draws.
///
/// Cheap to clone, and safe to call from any thread.
#[derive(Resource, Clone)]
pub struct Rasterizer(Arc<dyn Rasterize>);

impl Rasterizer {
    /// Wrap a backend's rasterizer.
    pub fn new(rasterizer: impl Rasterize) -> Self {
        Self(Arc::new(rasterizer))
    }

    /// Rasterize `drawing`; see [`Rasterize::rasterize`].
    pub fn rasterize(
        &self,
        drawing: &Recording,
        transform: Affine,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        self.0.rasterize(drawing, transform, width, height)
    }
}

impl std::fmt::Debug for Rasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Rasterizer")
    }
}
