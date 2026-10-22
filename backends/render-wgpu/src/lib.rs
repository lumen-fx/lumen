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

pub(crate) mod atlas;
pub mod capability;
mod gpu;
mod rasterize;
pub mod sink;
mod surface;
pub use gpu::{GPU_INIT_DEADLINE_DEFAULT_MS, GPU_INIT_DEADLINE_ENV};
pub use rasterize::GpuRasterizer;
pub use sink::{BACKEND_ID, VelloPainter};
/// The vello version this backend draws with. A painter that needs vello
/// itself downcasts [`lumen_paint::Painter::native`] to [`VelloPainter`] and
/// encodes into its scene; it reaches vello through this re-export rather
/// than declaring its own dependency, so both sides mean the same vello.
pub use vello;

use bevy_ecs::world::World;
use gpu::{FrameTexture, GpuCore};
use lumen_core::prelude::*;
use lumen_core::render_backend::{DeviceKind, DeviceMemory, RendererInfo, install_offscreen};
use lumen_core::traits::{FrameRequest, FrameTarget, PresentMode, RenderError};
use lumen_paint::LayerCache;
use lumen_text::{ShaperService, TextShaper};
use std::collections::HashMap;
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
    match WgpuRenderer::new_offscreen(Entity::PLACEHOLDER, 4, 4) {
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

/// One window's share of the renderer: the texture its frames land in, the
/// layers its faded groups keep, and its swap chain when it is on screen.
///
/// Field order matters on teardown: the surface drops ahead of the device it
/// was configured for, which the renderer drains first.
struct Target {
    window: Option<SurfaceFront>,
    frame: FrameTexture,
    /// Faded groups' layers, kept from frame to frame. Per window: a layer
    /// is painted at its window's scale.
    layers: LayerCache,
    /// Whether the frame texture holds a painted frame at its current
    /// size. A fresh or resized texture does not, and paints regardless of
    /// the scene diff.
    holds_frame: bool,
}

/// WGPU + vello renderer, for any number of windows and offscreen images on
/// one device.
///
/// Construction is free and does no GPU work: a window backend builds one
/// before the windows exist and attaches each one once it does, and a
/// headless launch attaches an offscreen image per window (see
/// [`Self::new_offscreen`]). The first target brings the device up; later
/// ones only add a frame texture and, for a window, a swap chain.
#[derive(Default)]
pub struct WgpuRenderer {
    /// Every target, by its main-world window entity. Dropped before the
    /// core (see [`Drop`]).
    targets: HashMap<Entity, Target>,
    core: Option<GpuCore>,
    /// Frames rendered since construction. Frames the present gate skipped
    /// do not count, so a static UI redrawn on a false-positive dirty flag
    /// leaves it flat.
    render_count: u64,
    /// The last memory report and when it was taken. A report walks every
    /// allocation, so [`Renderer::info`] takes one at most every
    /// [`MEMORY_REPORT_INTERVAL`].
    memory: Option<(std::time::Instant, Option<DeviceMemory>)>,
}

impl Drop for WgpuRenderer {
    fn drop(&mut self) {
        self.detach_all();
    }
}

/// How long a memory report stands before [`Renderer::info`] takes another.
const MEMORY_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// The core's name for a wgpu device type.
fn device_kind(kind: wgpu::DeviceType) -> DeviceKind {
    match kind {
        wgpu::DeviceType::DiscreteGpu => DeviceKind::DiscreteGpu,
        wgpu::DeviceType::IntegratedGpu => DeviceKind::IntegratedGpu,
        wgpu::DeviceType::VirtualGpu => DeviceKind::VirtualGpu,
        wgpu::DeviceType::Cpu => DeviceKind::Cpu,
        wgpu::DeviceType::Other => DeviceKind::Unknown,
    }
}

impl WgpuRenderer {
    /// A renderer bound to nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A renderer with `target` attached to a `width` x `height` offscreen
    /// image.
    pub fn new_offscreen(target: Entity, width: u32, height: u32) -> Result<Self, RenderError> {
        let mut renderer = Self::new();
        renderer.attach(target, FrameTarget::Offscreen { width, height })?;
        Ok(renderer)
    }

    /// Whether any target is bound.
    pub fn is_attached(&self) -> bool {
        !self.targets.is_empty()
    }

    /// Name, backend, and device type of the adapter this renderer bound
    /// to, or `None` before the first target.
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.core.as_ref().map(|c| &c.adapter_info)
    }

    /// Whether rendering runs on a software rasterizer (lavapipe, WARP,
    /// SwiftShader) instead of a GPU. Pixel output from one is close to but not
    /// interchangeable with a hardware render, so image comparisons need to know
    /// which they got.
    pub fn is_software_adapter(&self) -> bool {
        self.adapter_info()
            .is_some_and(|info| info.device_type == wgpu::DeviceType::Cpu)
    }

    /// Pixel size of `target`'s frame, or `None` when it is not attached.
    pub fn size(&self, target: Entity) -> Option<(u32, u32)> {
        self.targets.get(&target).map(|t| t.frame.size())
    }

    /// Frames rendered since construction, across every target. Unchanged
    /// across frames the present gate skipped.
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// What `target`'s kept layers of faded groups hold and how often they
    /// were reused, or `None` when it is not attached.
    pub fn layer_stats(&self, target: Entity) -> Option<lumen_paint::LayerStats> {
        self.targets.get(&target).map(|t| t.layers.stats())
    }

    /// `target`'s last rendered frame as tightly packed RGBA8.
    pub fn read_rgba8(&self, target: Entity) -> Result<Vec<u8>, RenderError> {
        let (Some(core), Some(t)) = (self.core.as_ref(), self.targets.get(&target)) else {
            return Err(RenderError::Detached);
        };
        core.read_rgba8(&t.frame).map_err(RenderError::Present)
    }
}

impl Renderer for WgpuRenderer {
    fn info(&mut self) -> RendererInfo {
        let Some(core) = self.core.as_ref() else {
            return RendererInfo {
                backend: capability::NAME,
                ..RendererInfo::default()
            };
        };
        let adapter = &core.adapter_info;
        let now = std::time::Instant::now();
        let memory = match self.memory {
            Some((at, memory)) if now.duration_since(at) < MEMORY_REPORT_INTERVAL => memory,
            _ => {
                let memory = core
                    .device
                    .generate_allocator_report()
                    .map(|report| DeviceMemory {
                        allocated: report.total_allocated_bytes,
                        reserved: report.total_reserved_bytes,
                    });
                self.memory = Some((now, memory));
                memory
            }
        };
        let driver = match (adapter.driver.is_empty(), adapter.driver_info.is_empty()) {
            (false, false) => format!("{} {}", adapter.driver, adapter.driver_info),
            (false, true) => adapter.driver.clone(),
            _ => adapter.driver_info.clone(),
        };
        RendererInfo {
            backend: capability::NAME,
            device: adapter.name.clone(),
            device_kind: device_kind(adapter.device_type),
            vendor_id: adapter.vendor,
            device_id: adapter.device,
            api: format!("{:?}", adapter.backend),
            driver,
            max_texture_side: core.device.limits().max_texture_dimension_2d,
            memory,
        }
    }

    fn attach(&mut self, target: Entity, frame: FrameTarget) -> Result<(), RenderError> {
        // Drop any previous binding of this target first so an old surface
        // releases its window before a new one claims it.
        self.targets.remove(&target);
        let (window, size) = match &frame {
            FrameTarget::Window(window) => (Some(window.clone()), window.physical_size()),
            FrameTarget::Offscreen { width, height } => (None, (*width, *height)),
        };
        let surface = match self.core.as_ref() {
            // A later target: a surface on the instance the device came from.
            Some(core) => window
                .as_ref()
                .map(|w| core.instance.create_surface(w.clone()))
                .transpose()
                .map_err(|e| RenderError::Init(format!("create_surface: {e:?}")))?,
            // The first target brings the device up.
            None => {
                let (core, surface) =
                    GpuCore::bring_up(window.as_ref()).map_err(RenderError::Init)?;
                self.core = Some(core);
                surface
            }
        };
        let core = self.core.as_ref().expect("brought up above");
        let frame_texture = FrameTexture::new(&core.device, size.0, size.1);
        let size = frame_texture.size();
        let front = match (window, surface) {
            (Some(window), Some(surface)) => Some(SurfaceFront::new(core, surface, window, size)?),
            _ => None,
        };
        self.targets.insert(
            target,
            Target {
                window: front,
                frame: frame_texture,
                layers: LayerCache::default(),
                holds_frame: false,
            },
        );
        Ok(())
    }

    fn resize(&mut self, target: Entity, width: u32, height: u32) -> bool {
        let (Some(core), Some(t)) = (self.core.as_ref(), self.targets.get_mut(&target)) else {
            return false;
        };
        if (width.max(1), height.max(1)) == t.frame.size() {
            return false;
        }
        t.frame = FrameTexture::new(&core.device, width, height);
        let size = t.frame.size();
        if let Some(window) = t.window.as_mut() {
            window.resize(core, size);
        }
        t.holds_frame = false;
        true
    }

    fn wants_present(
        &mut self,
        target: Entity,
        render_world: &mut World,
        request: FrameRequest,
    ) -> bool {
        self.targets
            .get(&target)
            .is_some_and(|t| lumen_paint::wants_frame(render_world, target, request, t.holds_frame))
    }

    fn present(&mut self, target: Entity, render_world: &mut World) -> Result<(), RenderError> {
        let (Some(core), Some(t)) = (self.core.as_mut(), self.targets.get_mut(&target)) else {
            return Err(RenderError::Detached);
        };
        // What a drawing read back as pixels is rasterized with: this
        // device, published once it draws.
        if !render_world.contains_resource::<lumen_paint::Rasterizer>() {
            render_world.insert_resource(lumen_paint::Rasterizer::new(core.rasterizer()));
        }
        core.render(target, &t.frame, &mut t.layers, render_world)
            .map_err(RenderError::Present)?;
        t.holds_frame = true;
        self.render_count += 1;
        // Read back before the blit: the frame texture holds exactly what
        // the window is about to show.
        lumen_paint::answer_capture(render_world, target, t.frame.size(), || {
            core.read_rgba8(&t.frame)
        });
        if let Some(window) = t.window.as_mut()
            && window.present(core, &t.frame)?
        {
            gpu::stamp(
                render_world,
                lumen_core::tick::RenderTimestamps::mark_presented,
            );
        }
        Ok(())
    }

    fn set_present_mode(&mut self, target: Entity, mode: PresentMode) {
        if let (Some(core), Some(window)) = (
            self.core.as_ref(),
            self.targets
                .get_mut(&target)
                .and_then(|t| t.window.as_mut()),
        ) {
            window.set_present_mode(core, mode);
        }
    }

    fn targets(&self) -> Vec<Entity> {
        let mut targets: Vec<Entity> = self.targets.keys().copied().collect();
        targets.sort();
        targets
    }

    fn detach(&mut self, target: Entity) {
        if let Some(core) = self.core.as_ref() {
            // The last frame may still be in flight on this target.
            core.drain();
        }
        self.targets.remove(&target);
    }

    fn detach_all(&mut self) {
        // Drain first, so the queue drops idle: wgpu-core's `Queue::drop`
        // waits on the last submission with a fixed timeout and panics when
        // it expires, and a panic inside drop aborts the process.
        if let Some(core) = self.core.as_ref() {
            core.drain();
        }
        self.targets.clear();
        self.core = None;
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
            None => {
                let primary =
                    lumen_core::window::primary_window(&mut app.world).expect("a primary window");
                WgpuRenderer::new_offscreen(primary, self.width, self.height)
                    .expect("WgpuRenderer offscreen init")
            }
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
        let target = Entity::PLACEHOLDER;
        assert!(!renderer.is_attached());
        assert_eq!(renderer.size(target), None);
        assert!(renderer.adapter_info().is_none());
        assert!(!renderer.is_software_adapter());
        assert!(!renderer.resize(target, 800, 600));
        assert!(renderer.layer_stats(target).is_none());
        assert!(renderer.targets().is_empty());
        assert!(matches!(
            renderer.read_rgba8(target),
            Err(RenderError::Detached)
        ));
        let mut world = World::new();
        let request = FrameRequest {
            dirty: true,
            force_full: true,
        };
        assert!(!renderer.wants_present(target, &mut world, request));
        assert!(matches!(
            renderer.present(target, &mut world),
            Err(RenderError::Detached)
        ));
        renderer.set_present_mode(target, PresentMode::NoWait);
        // Detaching an unattached renderer is a no-op, not an error.
        renderer.detach(target);
        renderer.detach_all();
        assert!(!renderer.is_attached());
    }
}
