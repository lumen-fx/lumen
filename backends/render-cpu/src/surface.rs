//! On-screen presentation: the CPU-rasterized frame copied into the window
//! through softbuffer, the platform's own path for showing a CPU buffer.
//!
//! The window backend drives this through
//! [`lumen_core::traits::SurfaceRenderer`], exactly as it drives the GPU
//! renderer, so it never names either.

use crate::{CpuPainter, clamp_size, cpu_painter, straight_rgba8};
use bevy_ecs::world::World;
use lumen_core::node_ir::{PreviousScene, RetainedScene};
use lumen_core::render_world::{SurfaceCapture, SurfaceFrame, Viewport};
use lumen_core::traits::{FrameRequest, RenderTarget, Renderer, SurfaceError, SurfaceRenderer};
use lumen_paint::{PaintTarget, WalkContext, scene_has_damage, walk_node};
use lumen_text::{ShaperService, TextShaper};
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
struct Presenter {
    surface: softbuffer::Surface<Window, Window>,
    #[allow(
        dead_code,
        reason = "held so the display connection outlives the surface"
    )]
    context: softbuffer::Context<Window>,
    size: (u32, u32),
}

/// CPU renderer that presents into an OS window.
///
/// Construction is free: a window backend builds one before the window
/// exists and calls [`SurfaceRenderer::attach`] once it does.
pub struct CpuSurfaceRenderer {
    presenter: Option<Presenter>,
    painter: PaintTarget,
    pixmap: Pixmap,
}

impl Default for CpuSurfaceRenderer {
    fn default() -> Self {
        Self {
            presenter: None,
            painter: Box::new(CpuPainter::new(1, 1)),
            pixmap: Pixmap::new(1, 1),
        }
    }
}

impl CpuSurfaceRenderer {
    /// A renderer with no window bound yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a window is currently bound.
    pub fn is_attached(&self) -> bool {
        self.presenter.is_some()
    }
}

impl Renderer for CpuSurfaceRenderer {}

impl SurfaceRenderer for CpuSurfaceRenderer {
    fn attach(&mut self, target: Arc<dyn RenderTarget>) -> Result<(), SurfaceError> {
        self.presenter = None;
        let size = target.physical_size();
        let window = Window(target);
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| SurfaceError::Init(format!("softbuffer context: {e}")))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|e| SurfaceError::Init(format!("softbuffer surface: {e}")))?;
        let mut presenter = Presenter {
            surface,
            context,
            size: (0, 0),
        };
        resize_presenter(&mut presenter, size.0, size.1)?;
        self.presenter = Some(presenter);
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        match self.presenter.as_mut() {
            Some(p) if p.size != (width, height) => {
                if let Err(e) = resize_presenter(p, width, height) {
                    tracing::warn!(target: "lumen::render", "softbuffer resize failed: {e}");
                }
                true
            }
            _ => false,
        }
    }

    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        // A screenshot is answered from the frame `present` rasterizes, so a
        // request repaints even an unchanged tree.
        let capture = render_world
            .get_resource::<SurfaceCapture>()
            .is_some_and(|c| c.is_requested());
        capture || (request.dirty && (request.force_full || scene_has_damage(render_world)))
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), SurfaceError> {
        let presenter = self.presenter.as_mut().ok_or(SurfaceError::Detached)?;
        let (width, height) = presenter.size;
        let (w, h) = clamp_size(width, height);
        let retained_root = render_world.resource::<RetainedScene>().root.clone();
        let (dpr, clear) = {
            let viewport = render_world.resource::<Viewport>();
            (viewport.scale_factor.max(0.01), viewport.clear)
        };
        let natives = render_world
            .get_resource::<lumen_core::native::NativePainters>()
            .cloned();

        cpu_painter(&mut self.painter).begin_frame(w, h, lumen_paint::peniko_color(clear));
        {
            let mut shaper = render_world.get_non_send_mut::<ShaperService>();
            let shaper_ref: Option<&mut dyn TextShaper> = shaper
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn TextShaper);
            if let Some(root) = retained_root.as_ref() {
                let mut ctx = WalkContext::new_with_dpr(&mut self.painter, None, shaper_ref, dpr);
                if let Some(painters) = natives.as_ref() {
                    ctx = ctx.with_native_painters(painters);
                }
                walk_node(&mut ctx, root);
            }
        }
        render_world.resource_mut::<PreviousScene>().root = retained_root;

        if (self.pixmap.width(), self.pixmap.height()) != (w, h) {
            self.pixmap = Pixmap::new(w, h);
        }
        cpu_painter(&mut self.painter).render_into(&mut self.pixmap);

        if let Some(capture) = render_world.get_resource::<SurfaceCapture>().cloned()
            && capture.is_requested()
        {
            capture.write(SurfaceFrame {
                width: u32::from(w),
                height: u32::from(h),
                rgba8: straight_rgba8(&self.pixmap),
            });
            capture.clear_request();
        }

        let mut buffer = presenter
            .surface
            .buffer_mut()
            .map_err(|e| SurfaceError::Present(format!("softbuffer buffer: {e}")))?;
        let stride = buffer.width().get() as usize;
        let rows = buffer.height().get() as usize;
        let src_w = usize::from(w);
        for (y, row) in buffer.chunks_mut(stride).take(rows).enumerate() {
            for (x, out) in row.iter_mut().enumerate() {
                *out = if x < src_w && y < usize::from(h) {
                    // The frame is opaque wherever the clear colour is, which is
                    // everywhere a window shows; the premultiplied channels are
                    // the colour composited over black elsewhere.
                    let p = self.pixmap.data()[y * src_w + x];
                    (u32::from(p.r) << 16) | (u32::from(p.g) << 8) | u32::from(p.b)
                } else {
                    0
                };
            }
        }
        buffer
            .present()
            .map_err(|e| SurfaceError::Present(format!("softbuffer present: {e}")))
    }

    fn detach(&mut self) {
        self.presenter = None;
    }
}

/// Size the window's buffer to `width` x `height`, at least one pixel each way.
fn resize_presenter(p: &mut Presenter, width: u32, height: u32) -> Result<(), SurfaceError> {
    let w = NonZeroU32::new(width.max(1)).expect("non-zero");
    let h = NonZeroU32::new(height.max(1)).expect("non-zero");
    p.surface
        .resize(w, h)
        .map_err(|e| SurfaceError::Init(format!("softbuffer resize: {e}")))?;
    p.size = (w.get(), h.get());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_world() -> World {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        world.insert_resource(RetainedScene::default());
        world.insert_resource(PreviousScene::default());
        world
    }

    /// With no window bound the renderer answers every call without
    /// touching a display.
    #[test]
    fn a_detached_renderer_has_nothing_to_present() {
        let mut renderer = CpuSurfaceRenderer::new();
        assert!(!renderer.is_attached());
        assert!(!renderer.resize(800, 600));
        let mut world = render_world();
        assert!(matches!(
            renderer.present(&mut world),
            Err(SurfaceError::Detached)
        ));
        renderer.detach();
    }

    /// The present gate matches the GPU renderer's: a clean tick paints
    /// nothing, an unchanged dirty tick paints nothing, a recreated surface
    /// and a screenshot request always paint.
    #[test]
    fn the_present_gate_follows_dirty_damage_and_capture() {
        let mut renderer = CpuSurfaceRenderer::new();
        let mut world = render_world();
        let req = |dirty, force_full| FrameRequest { dirty, force_full };
        assert!(!renderer.wants_present(&mut world, req(false, false)));
        assert!(!renderer.wants_present(&mut world, req(true, false)));
        assert!(renderer.wants_present(&mut world, req(true, true)));
        let capture = SurfaceCapture::default();
        capture.request();
        world.insert_resource(capture);
        assert!(renderer.wants_present(&mut world, req(false, false)));
    }

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
        let mut renderer = CpuSurfaceRenderer::new();
        assert!(matches!(
            renderer.attach(Arc::new(NoHandles)),
            Err(SurfaceError::Init(_))
        ));
        assert!(!renderer.is_attached());
    }
}
