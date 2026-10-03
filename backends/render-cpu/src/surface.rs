//! The window front end: the CPU-rasterized frame copied into the window
//! through softbuffer, the platform's own path for showing a CPU buffer.

use lumen_core::traits::{RenderError, RenderTarget};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::num::NonZeroU32;
use std::sync::Arc;
use vello_cpu::Pixmap;

/// The window, in the two shapes softbuffer asks for it.
#[derive(Clone)]
struct Window(Arc<dyn RenderTarget>);

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

/// Everything bound to one live window. Field order matters on teardown:
/// the surface drops ahead of the context it was made from.
pub(crate) struct Presenter {
    surface: softbuffer::Surface<Window, Window>,
    #[allow(
        dead_code,
        reason = "held so the display connection outlives the surface"
    )]
    context: softbuffer::Context<Window>,
}

impl Presenter {
    /// Bind softbuffer to `window`.
    pub(crate) fn new(window: Arc<dyn RenderTarget>) -> Result<Self, RenderError> {
        let window = Window(window);
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| RenderError::Init(format!("softbuffer context: {e}")))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|e| RenderError::Init(format!("softbuffer surface: {e}")))?;
        Ok(Self { surface, context })
    }

    /// Size the window's buffer to `width` x `height`, at least one pixel
    /// each way.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        let w = NonZeroU32::new(width.max(1)).expect("non-zero");
        let h = NonZeroU32::new(height.max(1)).expect("non-zero");
        self.surface
            .resize(w, h)
            .map_err(|e| RenderError::Init(format!("softbuffer resize: {e}")))
    }

    /// Copy `pixmap` into the window's buffer and present it.
    pub(crate) fn present(&mut self, pixmap: &Pixmap) -> Result<(), RenderError> {
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| RenderError::Present(format!("softbuffer buffer: {e}")))?;
        let stride = buffer.width().get() as usize;
        let rows = buffer.height().get() as usize;
        let src_w = usize::from(pixmap.width());
        let src_h = usize::from(pixmap.height());
        for (y, row) in buffer.chunks_mut(stride).take(rows).enumerate() {
            for (x, out) in row.iter_mut().enumerate() {
                *out = if x < src_w && y < src_h {
                    // The frame is opaque wherever the clear colour is, which is
                    // everywhere a window shows; the premultiplied channels are
                    // the colour composited over black elsewhere.
                    let p = pixmap.data()[y * src_w + x];
                    (u32::from(p.r) << 16) | (u32::from(p.g) << 8) | u32::from(p.b)
                } else {
                    0
                };
            }
        }
        buffer
            .present()
            .map_err(|e| RenderError::Present(format!("softbuffer present: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CpuRenderer;
    use lumen_core::traits::{FrameTarget, Renderer};

    /// A window that cannot produce a handle fails the bind with an init
    /// error, which is what lets an `auto` launch fall through to another
    /// renderer instead of exiting.
    #[test]
    fn a_window_without_handles_fails_to_attach() {
        struct NoHandles;
        impl HasWindowHandle for NoHandles {
            fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
                Err(HandleError::Unavailable)
            }
        }
        impl HasDisplayHandle for NoHandles {
            fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
                Err(HandleError::Unavailable)
            }
        }
        impl RenderTarget for NoHandles {
            fn physical_size(&self) -> (u32, u32) {
                (8, 8)
            }
        }
        let mut renderer = CpuRenderer::new();
        assert!(matches!(
            renderer.attach(FrameTarget::Window(Arc::new(NoHandles))),
            Err(RenderError::Init(_))
        ));
        assert!(!renderer.is_attached());
    }
}
