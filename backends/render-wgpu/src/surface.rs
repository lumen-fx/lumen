//! The window front end: a wgpu swap chain the frame texture is blitted
//! onto.
//!
//! Most swap chains expose `Bgra8Unorm` / `Bgra8UnormSrgb`, never the
//! `Rgba8Unorm` vello renders into, so the frame is copied onto the surface
//! with `wgpu::util::TextureBlitter`, which handles the channel order and the
//! sRGB conversion. Acquiring the swap-chain texture, the blit, and the
//! present are the only parts of a frame that exist for a window; everything
//! before them is the shared [`GpuCore`].

use crate::gpu::GpuCore;
use lumen_core::traits::RenderError;
use std::sync::Arc;
use vello::wgpu;
use vello::wgpu::util::TextureBlitter;

/// Everything bound to one live window on top of the GPU core.
///
/// Field order matters on teardown: the surface drops before the window
/// handle it reads.
pub(crate) struct SurfaceFront {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blitter: TextureBlitter,
    /// The window this surface belongs to. Held so the window outlives
    /// every GPU object bound to it.
    #[allow(dead_code, reason = "held to pin the window's lifetime, not read")]
    window: Arc<dyn lumen_core::traits::RenderTarget>,
}

impl SurfaceFront {
    /// Configure `surface` for `gpu`'s device at the frame texture's size.
    pub(crate) fn new(
        gpu: &GpuCore,
        surface: wgpu::Surface<'static>,
        window: Arc<dyn lumen_core::traits::RenderTarget>,
    ) -> Result<Self, RenderError> {
        let caps = surface.get_capabilities(&gpu.adapter);
        // The frame is blitted through an `Rgba8UnormSrgb` view, so an sRGB
        // surface format matches the gamma assumption exactly. Prefer the
        // two sRGB 8-bit variants; fall back to whatever the platform
        // offered first otherwise.
        let format = [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| RenderError::Init("no surface formats".to_string()))?;
        let (width, height) = gpu.size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            // Input latency: cap the swap chain to a single in-flight frame
            // so a freshly encoded frame reaches the screen at the next
            // vblank instead of queueing behind a second buffered frame.
            // Trades a little GPU/CPU overlap headroom for lower
            // click-to-pixel latency, which is the right call for a UI
            // toolkit (Qt/GTK compositors present at depth 1).
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
        let blitter = TextureBlitter::new(&gpu.device, format);
        Ok(Self {
            surface,
            config,
            blitter,
            window,
        })
    }

    /// Reconfigure the swap chain to the frame texture's size.
    pub(crate) fn resize(&mut self, gpu: &GpuCore) {
        (self.config.width, self.config.height) = gpu.size();
        self.surface.configure(&gpu.device, &self.config);
    }

    /// Put the frame texture on screen: acquire the next swap-chain
    /// texture, blit, present.
    pub(crate) fn present(&mut self, gpu: &GpuCore) -> Result<(), RenderError> {
        // wgpu 29 reports each non-success acquire outcome with what to do:
        //   - Suboptimal: a usable texture, but the swap chain no longer
        //     matches the surface (resize race, Wayland scale change).
        //     Reconfigure and skip; the next redraw retries against the
        //     fresh configuration.
        //   - Outdated: same, minus the usable texture.
        //   - Lost: device reset (suspend, GPU driver crash). Reconfigure
        //     and skip; if the device itself is gone, the next attempt
        //     surfaces it again.
        //   - Timeout: compositor stall. Skip and let vsync deliver another
        //     redraw.
        //   - Occluded: minimized or fully covered. Nothing is wrong, so
        //     skip without reconfiguring and wait for the window to come
        //     back.
        //   - Validation: a validation error was raised and captured.
        //     Surface it to the caller rather than looping on it.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_)
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
                tracing::debug!(
                    target: "lumen::render",
                    "surface texture suboptimal, outdated or lost; reconfiguring + skipping frame",
                );
                self.surface.configure(&gpu.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                tracing::debug!(
                    target: "lumen::render",
                    "surface acquire timed out; skipping frame",
                );
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                tracing::debug!(target: "lumen::render", "window occluded; skipping frame");
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::Present(
                    "get_current_texture: validation error".to_string(),
                ));
            }
        };
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen blit encoder"),
            });
        self.blitter
            .copy(&gpu.device, &mut encoder, gpu.srgb_view(), &surface_view);
        gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}
