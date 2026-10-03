//! WGPU + vello renderer backend.
//!
//! [`WgpuRenderer`] paints through the walker in `lumen-paint` into a
//! [`VelloPainter`] and renders the scene with vello into a texture. One GPU
//! core (device bring-up, the vello renderer, the fragment cache, the frame
//! texture, readback) serves both kinds of target the renderer attaches to
//! through [`lumen_core::traits::Renderer`]:
//!
//! - an offscreen image, read back to the CPU for headless runs,
//!   screenshots, and tests;
//! - a window, where the frame texture is blitted onto the swap chain and
//!   presented. A window backend drives it without naming wgpu or vello.
//!
//! Headless runs therefore exercise the same bring-up, gate, walk, and
//! encode a windowed app ships.

#![warn(missing_docs)]

pub mod capability;
mod gpu;
pub mod sink;
mod surface;
pub use gpu::{GPU_INIT_DEADLINE_DEFAULT_MS, GPU_INIT_DEADLINE_ENV};
pub use sink::{BACKEND_ID, VelloPainter};
/// The vello version this backend draws with. A painter that needs vello
/// itself downcasts [`lumen_paint::Painter::native`] to [`VelloPainter`] and
/// encodes into its scene; it reaches vello through this re-export rather
/// than declaring its own dependency, so both sides mean the same vello.
pub use vello;

use bevy_ecs::world::World;
use gpu::GpuCore;
use lumen_core::prelude::*;
use lumen_core::render_backend::install_offscreen;
use lumen_core::traits::{FrameRequest, FrameTarget, RenderError};
use lumen_text::{ShaperService, TextShaper};
use surface::SurfaceFront;
use vello::RendererOptions;
use vello::wgpu;

/// The single GPU backend this per-OS build compiles and probes (Part A of
/// runtime-tree-shaking). The Cargo manifest already trims wgpu/naga to one
/// backend per OS; pinning the instance's `Backends` to the same bit is
/// defense in depth; it keeps the requested set honest and avoids a surprise
/// probe of a backend whose code was compiled out. Unknown OSes fall back to
/// Vulkan (the widest cross-platform native backend).
const NATIVE_BACKENDS: wgpu::Backends = {
    #[cfg(target_os = "linux")]
    {
        wgpu::Backends::VULKAN
    }
    #[cfg(target_os = "macos")]
    {
        wgpu::Backends::METAL
    }
    #[cfg(target_os = "windows")]
    {
        wgpu::Backends::DX12
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        wgpu::Backends::VULKAN
    }
};

/// Why this machine cannot do GPU pixel work, or `None` when it can.
///
/// Probes for an adapter and reports back: no adapter at all, or one that is a
/// software rasterizer. Callers that render and inspect pixels use it to bail
/// out with a reason instead of running. A software rasterizer's output (WARP,
/// lavapipe) is close to but not interchangeable with a GPU's, so it is not a
/// substrate for pixel-level checks.
pub fn gpu_unavailable_reason() -> Option<String> {
    match WgpuRenderer::new_offscreen(4, 4) {
        Ok(r) if r.is_software_adapter() => Some(format!(
            "adapter '{}' is a software rasterizer",
            r.adapter_info()
                .map_or("unknown", |info| info.name.as_str())
        )),
        Ok(_) => None,
        Err(e) => Some(format!("no wgpu adapter available ({e})")),
    }
}

/// The vello options for a renderer bound to `adapter`.
///
/// Direct3D's WARP rasterizer, the adapter Windows offers when there is no
/// GPU, faults the process the first time it executes vello's GPU coarse
/// stages, even for an empty scene. vello can run those stages on the CPU and
/// hand the device only fine rasterization, which WARP runs correctly, so a
/// WARP adapter gets that split. Every other adapter keeps vello's defaults.
pub(crate) fn vello_options(adapter: &wgpu::AdapterInfo) -> RendererOptions {
    RendererOptions {
        use_cpu: is_warp(adapter),
        ..RendererOptions::default()
    }
}

/// Whether `adapter` is Direct3D's WARP software rasterizer.
fn is_warp(adapter: &wgpu::AdapterInfo) -> bool {
    adapter.backend == wgpu::Backend::Dx12 && adapter.device_type == wgpu::DeviceType::Cpu
}

/// What a renderer is bound to: the GPU core, plus the swap chain when the
/// target is a window.
///
/// Field order matters on teardown: the device is drained first (see
/// [`Drop`]), then the surface drops ahead of the device it was configured
/// for.
struct Bound {
    window: Option<SurfaceFront>,
    gpu: GpuCore,
    /// Whether the frame texture holds a painted frame at its current
    /// size. A fresh or resized texture does not, and paints regardless of
    /// the scene diff.
    holds_frame: bool,
}

impl Drop for Bound {
    fn drop(&mut self) {
        self.gpu.drain();
    }
}

/// WGPU + vello renderer, for a window or an offscreen image.
///
/// Construction is free and does no GPU work: a window backend builds one
/// before the window exists and attaches the window once it does, and a
/// headless launch attaches an offscreen image (see
/// [`Self::new_offscreen`]).
#[derive(Default)]
pub struct WgpuRenderer {
    bound: Option<Bound>,
    /// Frames rendered since construction. Frames the present gate skipped
    /// do not count, so a static UI redrawn on a false-positive dirty flag
    /// leaves it flat.
    render_count: u64,
}

impl WgpuRenderer {
    /// A renderer bound to nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A renderer already attached to a `width` x `height` offscreen image.
    pub fn new_offscreen(width: u32, height: u32) -> Result<Self, RenderError> {
        let mut renderer = Self::new();
        renderer.attach(FrameTarget::Offscreen { width, height })?;
        Ok(renderer)
    }

    /// Whether a target is currently bound.
    pub fn is_attached(&self) -> bool {
        self.bound.is_some()
    }

    /// Name, backend, and device type of the adapter this renderer bound
    /// to, or `None` while detached.
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.bound.as_ref().map(|b| &b.gpu.adapter_info)
    }

    /// Whether rendering runs on a software rasterizer (lavapipe, WARP,
    /// SwiftShader) instead of a GPU. Pixel output from one is close to but not
    /// interchangeable with a hardware render, so image comparisons need to know
    /// which they got.
    pub fn is_software_adapter(&self) -> bool {
        self.adapter_info()
            .is_some_and(|info| info.device_type == wgpu::DeviceType::Cpu)
    }

    /// Pixel size of the target, or `None` while detached.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.bound.as_ref().map(|b| b.gpu.size())
    }

    /// Frames rendered since construction. Unchanged across frames the
    /// present gate skipped.
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// The last rendered frame as tightly packed RGBA8.
    pub fn read_rgba8(&self) -> Result<Vec<u8>, RenderError> {
        let bound = self.bound.as_ref().ok_or(RenderError::Detached)?;
        bound.gpu.read_rgba8().map_err(RenderError::Present)
    }
}

impl Renderer for WgpuRenderer {
    fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError> {
        // Drop any previous binding first so an old surface releases its
        // window before a new one claims it.
        self.bound = None;
        let (window, size) = match &target {
            FrameTarget::Window(window) => (Some(window), window.physical_size()),
            FrameTarget::Offscreen { width, height } => (None, (*width, *height)),
        };
        let (gpu, surface) =
            GpuCore::bring_up(window, size.0, size.1).map_err(RenderError::Init)?;
        let window = match (target, surface) {
            (FrameTarget::Window(window), Some(surface)) => {
                Some(SurfaceFront::new(&gpu, surface, window)?)
            }
            _ => None,
        };
        self.bound = Some(Bound {
            window,
            gpu,
            holds_frame: false,
        });
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        let Some(bound) = self.bound.as_mut() else {
            return false;
        };
        if !bound.gpu.resize(width, height) {
            return false;
        }
        if let Some(window) = bound.window.as_mut() {
            window.resize(&bound.gpu);
        }
        bound.holds_frame = false;
        true
    }

    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        self.bound
            .as_ref()
            .is_some_and(|b| lumen_paint::wants_frame(render_world, request, b.holds_frame))
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), RenderError> {
        let bound = self.bound.as_mut().ok_or(RenderError::Detached)?;
        bound
            .gpu
            .render(render_world)
            .map_err(RenderError::Present)?;
        bound.holds_frame = true;
        self.render_count += 1;
        // Read back before the blit: the frame texture holds exactly what
        // the window is about to show.
        lumen_paint::answer_capture(render_world, bound.gpu.size(), || bound.gpu.read_rgba8());
        if let Some(window) = bound.window.as_mut() {
            window.present(&bound.gpu)?;
        }
        Ok(())
    }

    fn detach(&mut self) {
        self.bound = None;
    }
}

/// Plugin: installs an offscreen [`WgpuRenderer`] into the render world,
/// driven each frame by [`install_offscreen`]'s render-world system.
///
/// Optionally accepts a [`TextShaper`] via
/// [`WgpuRendererPlugin::with_text_shaper`]; without it, text draw commands
/// are skipped. The shaper is installed as a render-world [`ShaperService`],
/// the same holder a windowed renderer reads.
pub struct WgpuRendererPlugin {
    /// Initial offscreen target width.
    pub width: u32,
    /// Initial offscreen target height.
    pub height: u32,
    /// Optional text shaper; installed as a non-send render-world resource.
    pub text_shaper: Option<Box<dyn TextShaper>>,
    /// Optional pre-initialised renderer. When set, `build` installs it
    /// instead of creating one, letting callers surface GPU-init failure
    /// as a `Result` (via [`WgpuRenderer::new_offscreen`]) rather than the
    /// panic `build` would otherwise raise. `width` / `height` are ignored
    /// in that case - the renderer keeps its own target size.
    renderer: Option<WgpuRenderer>,
}

impl Default for WgpuRendererPlugin {
    fn default() -> Self {
        Self::new(800, 600)
    }
}

impl WgpuRendererPlugin {
    /// New plugin with given size, no text support.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            text_shaper: None,
            renderer: None,
        }
    }

    /// Attach a text shaper. Required to render [`ExtractedText`].
    pub fn with_text_shaper<S: TextShaper + 'static>(mut self, shaper: S) -> Self {
        self.text_shaper = Some(Box::new(shaper));
        self
    }

    /// Attach an already-boxed text shaper (same effect as
    /// [`Self::with_text_shaper`]; avoids double-boxing when the caller
    /// already holds a `Box<dyn TextShaper>`).
    pub fn with_boxed_text_shaper(mut self, shaper: Box<dyn TextShaper>) -> Self {
        self.text_shaper = Some(shaper);
        self
    }

    /// Install this pre-built renderer instead of constructing one inside
    /// `build`. Lets callers keep GPU-init failure on a `Result` path.
    pub fn with_renderer(mut self, renderer: WgpuRenderer) -> Self {
        self.renderer = Some(renderer);
        self
    }
}

impl Plugin for WgpuRendererPlugin {
    fn build(self, app: &mut App) {
        let renderer = match self.renderer {
            Some(r) => r,
            None => WgpuRenderer::new_offscreen(self.width, self.height)
                .expect("WgpuRenderer offscreen init"),
        };
        if let Some(shaper) = self.text_shaper {
            app.render_world
                .insert_non_send(ShaperService::from(shaper));
        }
        install_offscreen(app, renderer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_text::NullShaper;

    fn adapter(backend: wgpu::Backend, device_type: wgpu::DeviceType) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "test adapter".into(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            subgroup_min_size: 4,
            subgroup_max_size: 4,
            transient_saves_memory: false,
        }
    }

    /// WARP runs vello's coarse stages on the CPU; a GPU, and a software
    /// rasterizer on any other backend, keep vello's defaults.
    #[test]
    fn only_warp_moves_the_coarse_stages_to_the_cpu() {
        use wgpu::{Backend, DeviceType};
        assert!(vello_options(&adapter(Backend::Dx12, DeviceType::Cpu)).use_cpu);
        for (backend, device_type) in [
            (Backend::Dx12, DeviceType::DiscreteGpu),
            (Backend::Dx12, DeviceType::IntegratedGpu),
            (Backend::Vulkan, DeviceType::Cpu),
            (Backend::Metal, DeviceType::IntegratedGpu),
        ] {
            assert!(
                !vello_options(&adapter(backend, device_type)).use_cpu,
                "{backend:?} {device_type:?}"
            );
        }
    }

    /// The plugin's builders are the composition point's only say over
    /// text: a plugin built without a shaper renders none, and either
    /// builder attaches one, so a caller already holding a boxed shaper
    /// does not have to unbox it.
    #[test]
    fn the_plugin_takes_a_shaper_boxed_or_not() {
        let plain = WgpuRendererPlugin::new(320, 200);
        assert_eq!((plain.width, plain.height), (320, 200));
        assert!(
            plain.text_shaper.is_none(),
            "no shaper means text is skipped, not defaulted",
        );

        let with_value = WgpuRendererPlugin::default().with_text_shaper(NullShaper);
        assert!(with_value.text_shaper.is_some());

        let boxed: Box<dyn TextShaper> = Box::new(NullShaper);
        let with_box = WgpuRendererPlugin::new(8, 8).with_boxed_text_shaper(boxed);
        assert!(with_box.text_shaper.is_some());
    }

    /// A renderer with no target bound answers every call without touching
    /// the GPU: nothing to resize, nothing to read, no frame wanted, and no
    /// panic. A window backend builds one before the window exists, so
    /// this is the state it starts in.
    #[test]
    fn a_detached_renderer_has_nothing_to_present() {
        let mut renderer = WgpuRenderer::new();
        assert!(!renderer.is_attached());
        assert_eq!(renderer.size(), None);
        assert!(renderer.adapter_info().is_none());
        assert!(!renderer.is_software_adapter());
        assert!(!renderer.resize(800, 600));
        assert!(matches!(renderer.read_rgba8(), Err(RenderError::Detached)));
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        let request = FrameRequest {
            dirty: true,
            force_full: true,
        };
        assert!(!renderer.wants_present(&mut world, request));
        assert!(matches!(
            renderer.present(&mut world),
            Err(RenderError::Detached)
        ));
        // Detaching an unattached renderer is a no-op, not an error.
        renderer.detach();
        assert!(!renderer.is_attached());
    }
}
