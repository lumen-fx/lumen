//! WGPU + vello renderer backend.
//!
//! Both entry points paint through the walker in `lumen-paint` into a [`VelloPainter`], then hand
//! the scene to vello:
//!
//! - [`WgpuRenderer`], an offscreen renderer that draws into an `Rgba8Unorm` texture. Tests read the framebuffer back to CPU for cross-platform parity.
//! - [`WgpuSurfaceRenderer`], the on-screen path. It presents into whatever window a window backend attaches, through [`lumen_core::traits::SurfaceRenderer`], so the window backend never names wgpu or vello.

#![warn(missing_docs)]

pub mod capability;
pub mod sink;
pub mod surface;
pub use sink::{BACKEND_ID, VelloPainter};
pub use surface::{GPU_INIT_DEADLINE_DEFAULT_MS, GPU_INIT_DEADLINE_ENV, WgpuSurfaceRenderer};
/// The vello version this backend draws with. A painter that needs vello
/// itself downcasts [`lumen_paint::Painter::native`] to [`VelloPainter`] and
/// encodes into its scene; it reaches vello through this re-export rather
/// than declaring its own dependency, so both sides mean the same vello.
pub use vello;

use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use lumen_core::components::Color as LumenColor;
use lumen_core::prelude::*;
use lumen_core::render_world::{SurfaceCapture, SurfaceFrame};
use lumen_paint::{FragmentCache, PaintTarget, WalkContext};
use lumen_text::{ShaperService, TextShaper};
use thiserror::Error;
use vello::wgpu;
use vello::{AaConfig, RenderParams, RendererOptions};

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

/// Errors from constructing or operating a [`WgpuRenderer`].
#[derive(Debug, Error)]
pub enum WgpuRendererError {
    /// No suitable wgpu adapter is available on this machine.
    #[error("no suitable wgpu adapter: {0}")]
    NoAdapter(String),
    /// wgpu device request failed.
    #[error("request_device failed: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    /// vello renderer construction failed.
    #[error("vello renderer init failed: {0}")]
    Vello(String),
    /// vello render call failed.
    #[error("vello render failed: {0}")]
    Render(String),
}

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
            r.adapter_info().name
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

/// Offscreen WGPU + vello renderer.
///
/// Holds the device, queue, vello renderer, the [`VelloPainter`] a frame is
/// painted into, and the offscreen texture target. The render-world system
/// paints the frame and calls [`Self::render_current`].
pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    vello: vello::Renderer,
    /// The sink a frame is painted into; a [`VelloPainter`], reset by the
    /// render system each frame.
    painter: PaintTarget,
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    /// Number of actual GPU encode+submit passes ([`render_current`]) since
    /// construction. Frames skipped by the empty-damage partial-repaint gate do
    /// not increment it, so `render_count` measures real present work - a static
    /// UI redrawn on a false-positive dirty flag leaves it flat.
    render_count: u64,
    /// Adapter this renderer bound to. Kept so callers can tell a GPU from a
    /// software rasterizer without re-enumerating adapters.
    adapter_info: wgpu::AdapterInfo,
}

impl WgpuRenderer {
    /// Construct an offscreen renderer of the given pixel size.
    pub fn new_offscreen(width: u32, height: u32) -> Result<Self, WgpuRendererError> {
        pollster::block_on(Self::new_offscreen_async(width, height))
    }

    /// Async constructor.
    ///
    /// Surface-less adapter discovery (W6 T1): `compatible_surface` stays
    /// `None` so a host with zero display sockets (no Wayland/X) still
    /// yields a compute-capable adapter. The instance is pinned to this OS's
    /// single [`NATIVE_BACKENDS`] backend (Part A). If that turns up nothing
    /// (e.g. no Vulkan ICD), a `gl-fallback` build additionally tries a GL-only
    /// instance before surfacing a clear [`WgpuRendererError::NoAdapter`] --
    /// callers exit with the message, never a crash. Without the `gl-fallback`
    /// feature the GL backend is compiled out, so the error is returned
    /// directly.
    pub async fn new_offscreen_async(width: u32, height: u32) -> Result<Self, WgpuRendererError> {
        let opts = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        };
        // No display handle: this renderer is offscreen and never presents, so
        // there is no compositor connection to declare. wgpu 29 requires the
        // choice to be explicit.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: NATIVE_BACKENDS,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = match instance.request_adapter(&opts).await {
            Ok(a) => a,
            Err(primary_err) => {
                #[cfg(feature = "gl-fallback")]
                {
                    let gl_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                        backends: wgpu::Backends::GL,
                        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
                    });
                    gl_instance.request_adapter(&opts).await.map_err(|gl_err| {
                        WgpuRendererError::NoAdapter(format!(
                            "no adapter on the {NATIVE_BACKENDS:?} backend (primary: {primary_err}; GL fallback: {gl_err})"
                        ))
                    })?
                }
                #[cfg(not(feature = "gl-fallback"))]
                {
                    return Err(WgpuRendererError::NoAdapter(format!(
                        "no adapter on the {NATIVE_BACKENDS:?} backend (primary: {primary_err}); \
                         rebuild lumen-render-wgpu with --features gl-fallback for the GL compat path"
                    )));
                }
            }
        };
        let adapter_info = adapter.get_info();
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lumen-render-wgpu device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await?;

        let vello = vello::Renderer::new(&device, vello_options(&adapter_info))
            .map_err(|e| WgpuRendererError::Vello(format!("{e:?}")))?;

        let (texture, texture_view) = make_target(&device, width, height);

        Ok(Self {
            device,
            queue,
            vello,
            painter: Box::new(VelloPainter::new()),
            width,
            height,
            texture,
            texture_view,
            render_count: 0,
            adapter_info,
        })
    }

    /// Name, backend, and device type of the adapter this renderer bound to.
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Whether rendering runs on a software rasterizer (lavapipe, WARP,
    /// SwiftShader) instead of a GPU. Pixel output from one is close to but not
    /// interchangeable with a hardware render, so image comparisons need to know
    /// which they got.
    pub fn is_software_adapter(&self) -> bool {
        self.adapter_info.device_type == wgpu::DeviceType::Cpu
    }

    /// Pixel size of the offscreen target.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Resize the offscreen target. Allocates a fresh texture if needed.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        let (texture, view) = make_target(&self.device, width, height);
        self.texture = texture;
        self.texture_view = view;
        self.width = width;
        self.height = height;
    }

    /// Number of real GPU encode+submit passes since construction. Unchanged
    /// across frames the empty-damage gate skipped - see [`Self::render_count`].
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// The sink frames are painted into, as the walker takes it.
    pub fn painter_mut(&mut self) -> &mut PaintTarget {
        &mut self.painter
    }

    /// The sink frames are painted into, as its concrete type.
    pub fn vello_painter(&mut self) -> &mut VelloPainter {
        self.painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the offscreen renderer paints through a VelloPainter")
    }

    /// Render the painted frame into the offscreen target.
    pub fn render_current(&mut self, clear: LumenColor) {
        self.render_count += 1;
        let params = RenderParams {
            base_color: lumen_paint::peniko_color(clear),
            width: self.width,
            height: self.height,
            antialiasing_method: AaConfig::Area,
        };
        let scene = self
            .painter
            .native()
            .downcast_ref::<VelloPainter>()
            .expect("the offscreen renderer paints through a VelloPainter")
            .scene();
        if let Err(e) = self.vello.render_to_texture(
            &self.device,
            &self.queue,
            scene,
            &self.texture_view,
            &params,
        ) {
            eprintln!("lumen-render-wgpu: vello render failed: {e:?}");
        }
    }

    /// Read back the offscreen texture as RGBA8.
    pub fn read_rgba8(&self) -> Result<Vec<u8>, WgpuRendererError> {
        pollster::block_on(self.read_rgba8_async())
    }

    /// Async variant of [`read_rgba8`](Self::read_rgba8).
    pub async fn read_rgba8_async(&self) -> Result<Vec<u8>, WgpuRendererError> {
        let unpadded = self.width as usize * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded = unpadded.div_ceil(align) * align;
        let size = (padded * self.height as usize) as u64;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumen wgpu readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen wgpu readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .map_err(|_| WgpuRendererError::Render("map channel dropped".into()))?
            .map_err(|e| WgpuRendererError::Render(format!("{e:?}")))?;

        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity(unpadded * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * padded;
            out.extend_from_slice(&raw[start..start + unpadded]);
        }
        drop(raw);
        buffer.unmap();
        Ok(out)
    }
}

/// Drain the device before the queue drops. wgpu-core's `Queue::drop` waits
/// on the last submission with a fixed timeout and panics when it expires;
/// a software rasterizer on a loaded machine can hold a frame past it, and
/// a panic inside drop aborts the process. Waiting here, without a
/// deadline, turns that abort into a quiet finish.
impl Drop for WgpuRenderer {
    fn drop(&mut self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

impl lumen_core::traits::Renderer for WgpuRenderer {}

/// Plugin: installs the offscreen [`WgpuRenderer`] into the render world and
/// registers the render-world system in [`RenderStage::Render`].
///
/// Optionally accepts a [`TextShaper`] via
/// [`WgpuRendererPlugin::with_text_shaper`]; without it, text draw commands
/// are skipped. The shaper is installed as a render-world [`ShaperService`],
/// the same holder the on-screen path reads, so both render paths find the
/// shaper in one place.
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
        Self {
            width: 800,
            height: 600,
            text_shaper: None,
            renderer: None,
        }
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
    /// already holds a `Box<dyn TextShaper>` - e.g. the one built for
    /// `WindowOptions`).
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
        // The walker below reads the retained tree; this is what builds it.
        lumen_core::render_world::install_extract_pipeline(app);
        let renderer = match self.renderer {
            Some(r) => r,
            None => WgpuRenderer::new_offscreen(self.width, self.height)
                .expect("WgpuRenderer offscreen init"),
        };
        app.render_world.insert_non_send(renderer);
        // The fragment cache lets the walker encode each repeated appearance once.
        app.render_world.insert_resource(FragmentCache::default());
        if let Some(shaper) = self.text_shaper {
            app.render_world
                .insert_non_send(ShaperService::from(shaper));
        }
        app.add_render_systems(RenderStage::Render, wgpu_render_system);
    }
}

/// Render-world system that drives the offscreen [`WgpuRenderer`] via the shared Node IR walker.
///
/// Walks the [`lumen_core::node_ir::RetainedScene`]; leaves route through the cached emitters when the
/// [`FragmentCache`] resource is present.
///
/// Damage-driven partial repaint: the system calls [`lumen_paint::diff_retained_scenes`] against the
/// previous frame's root and skips the entire encode + submit when the diff is empty (the visual tree is
/// unchanged), keeping the last-rendered target on screen. A non-empty diff re-encodes the whole scene -
/// bounding the encode to the damage rect is not pixel-safe while vello clears the whole target per call.
///
/// A pending [`SurfaceCapture`] request is answered from the target after the frame.
#[allow(clippy::too_many_arguments)]
fn wgpu_render_system(
    mut renderer: NonSendMut<WgpuRenderer>,
    mut cache: Option<ResMut<FragmentCache>>,
    shaper: Option<NonSendMut<ShaperService>>,
    viewport: Res<Viewport>,
    retained: Res<lumen_core::node_ir::RetainedScene>,
    mut previous: ResMut<lumen_core::node_ir::PreviousScene>,
    mut damage: ResMut<FrameDamage>,
    natives: Option<Res<lumen_core::native::NativePainters>>,
    capture: Option<Res<SurfaceCapture>>,
) {
    // Device pixel ratio: the walker scales every leaf (and clip) from logical to physical pixels
    // at emit time, so the target texture and the damage scissor must be sized in the same physical
    // space. Offscreen viewports default to `scale_factor == 1.0`, making this a no-op there.
    let dpr = viewport.scale_factor.max(0.01);
    let w = (viewport.size.x * dpr).max(1.0) as u32;
    let h = (viewport.size.y * dpr).max(1.0) as u32;
    // A reallocated target holds no frame, so a resize repaints even an unchanged tree.
    let resized = renderer.size() != (w, h);
    renderer.resize(w, h);

    let viewport_rect = lumen_core::render_world::Rect {
        origin: glam::Vec2::ZERO,
        size: viewport.size,
    };
    damage.clear();
    lumen_paint::diff_retained_scenes(
        previous.root.as_ref(),
        retained.root.as_ref(),
        viewport_rect,
        &mut damage,
    );

    // Partial-repaint gate. The retained Node-IR diff tells us whether the
    // visual tree actually changed this frame. When it did not (empty damage)
    // and a previous frame already rendered into the target, skip the whole
    // encode + submit - the offscreen texture still holds the pixel-identical
    // last frame. Mirrors Qt `QWidget::update()` collapsing to no backing-store
    // flush when the computed dirty region is empty, and GTK's damage-region
    // coalescing.
    //
    // When the tree did change we re-encode the entire scene. A damage-bounded
    // scissor is deliberately not applied: `render_to_texture` clears the whole
    // target to `base_color` on every call, so clipping the encode to the
    // damage rect would blank every untouched pixel - not pixel-identical.
    // Pixel-safe partial *encode* needs a preserved backing store (deferred
    // slice). `FrameDamage` is still populated
    // for consumers that only need the dirty-region *size*.
    let first_frame = previous.root.is_none();
    if first_frame || resized || !damage.is_empty() {
        renderer.vello_painter().reset();
        {
            let mut shaper_opt = shaper;
            let shaper_ref: Option<&mut dyn TextShaper> = shaper_opt
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn TextShaper);
            let mut ctx = WalkContext::new_with_dpr(
                renderer.painter_mut(),
                cache.as_deref_mut(),
                shaper_ref,
                dpr,
            );
            if let Some(painters) = natives.as_deref() {
                ctx = ctx.with_native_painters(painters);
            }
            lumen_paint::walk_retained_scene(&mut ctx, &retained);
        }

        let clear = viewport.clear;
        renderer.render_current(clear);
    }

    // Park the just-walked tree so the next frame's diff has something to compare against.
    previous.root = retained.root.clone();

    // A screenshot request is answered from the target, which holds this
    // frame whether it was painted now or kept from an unchanged tree.
    if let Some(capture) = capture
        && capture.is_requested()
    {
        let (width, height) = renderer.size();
        match renderer.read_rgba8() {
            Ok(rgba8) => capture.write(SurfaceFrame {
                width,
                height,
                rgba8,
            }),
            Err(e) => eprintln!("lumen-render-wgpu: offscreen readback failed: {e}"),
        }
        // Cleared either way so a persistent GPU error cannot wedge the requester.
        capture.clear_request();
    }
}

fn make_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lumen wgpu offscreen target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
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
}
