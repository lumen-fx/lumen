//! On-screen presentation: a wgpu swap chain fed by the same vello scene
//! walker the offscreen renderer uses.
//!
//! Vello's compute pipeline binds the render target as a storage texture
//! with the format hard-pinned to `Rgba8Unorm`, while most swap chains
//! expose `Bgra8Unorm` / `Bgra8UnormSrgb`. To stay portable the frame is
//! rendered into an `Rgba8Unorm` intermediate texture and blitted onto the
//! surface with `wgpu::util::TextureBlitter`, which handles the channel and
//! sRGB conversion.
//!
//! The window backend drives this through
//! [`lumen_core::traits::SurfaceRenderer`], so it never sees a vello or
//! wgpu type: it attaches a window, reports resizes, and asks for frames.

use crate::walker::{WalkContext, diff_retained_scenes, walk_node};
use crate::{NATIVE_BACKENDS, SceneFragmentCache};
use bevy_ecs::world::World;
use lumen_core::node_ir::{PreviousScene, RetainedScene};
use lumen_core::render_world::{
    FrameDamage, Rect as LumenRect, SurfaceCapture, SurfaceFrame, Viewport,
};
use lumen_core::traits::{FrameRequest, RenderTarget, Renderer, SurfaceError, SurfaceRenderer};
use lumen_text::{ShaperService, TextShaper};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vello::peniko::color::{AlphaColor, Srgb};
use vello::wgpu;
// wgpu re-exports the handle crate it was built against, so the seam
// cannot drift from it.
use vello::wgpu::rwh::{DisplayHandle, HandleError, HasDisplayHandle};
use vello::wgpu::util::TextureBlitter;
use vello::{AaConfig, RenderParams, RendererOptions};

/// Environment variable controlling the GPU adapter / device init
/// deadline, in milliseconds. Defaults to
/// [`GPU_INIT_DEADLINE_DEFAULT_MS`] when unset or unparsable. When the
/// deadline is exceeded, [`WgpuSurfaceRenderer::attach`] panics with a
/// diagnostic instead of leaving the process frozen.
pub const GPU_INIT_DEADLINE_ENV: &str = "LUMEN_GPU_INIT_DEADLINE_MS";

/// Default GPU init deadline, in milliseconds. Surfaces driver hangs (a
/// broken Vulkan loader, a blocked Wayland compositor) within a bounded
/// wall-clock budget.
pub const GPU_INIT_DEADLINE_DEFAULT_MS: u64 = 5000;

fn gpu_init_deadline_ms() -> u64 {
    parse_deadline_ms(std::env::var(GPU_INIT_DEADLINE_ENV).ok().as_deref())
}

/// The deadline an environment value asks for. Anything unset or
/// unparsable falls back to the default rather than failing the launch,
/// since a mistyped tuning knob should not stop an app from starting.
fn parse_deadline_ms(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(GPU_INIT_DEADLINE_DEFAULT_MS)
}

/// Spawn a watchdog thread that panics if `flag` is not lit within
/// `deadline_ms` milliseconds.
fn spawn_gpu_init_watchdog(deadline_ms: u64, flag: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("lumen-gpu-init-watchdog".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let deadline = std::time::Duration::from_millis(deadline_ms);
            // Sleep in 50 ms slices so we wake promptly on success.
            while start.elapsed() < deadline {
                if flag.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if !flag.load(Ordering::Acquire) {
                // We panic from the watchdog thread; this aborts the
                // process via the default panic hook, which is the useful
                // failure mode here - the main thread is wedged inside
                // `pollster::block_on` waiting for a driver callback that
                // will never come.
                panic!(
                    "lumen-render-wgpu: GPU init exceeded {deadline_ms} ms deadline (\
                     set {GPU_INIT_DEADLINE_ENV}=<ms> to tune). This usually means the GPU \
                     adapter / device request is blocked at the driver \
                     level (Vulkan loader, Wayland compositor handshake, \
                     or device reset). Re-run with `LUMEN_GPU_INIT_TRACE=1` \
                     and a tracing subscriber to narrow the stage.",
                );
            }
        })
        .expect("spawn lumen-gpu-init-watchdog")
}

/// The window's display connection, in the shape wgpu's instance
/// descriptor wants: it takes a boxed display handle and requires `Debug`
/// on it, which is a wgpu detail rather than something to push onto every
/// [`RenderTarget`] implementor.
struct DisplayTarget(Arc<dyn RenderTarget>);

impl std::fmt::Debug for DisplayTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.0.physical_size();
        f.debug_struct("DisplayTarget")
            .field("width", &w)
            .field("height", &h)
            .finish()
    }
}

impl HasDisplayHandle for DisplayTarget {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

/// Everything bound to one live window: the swap chain, the device, the
/// vello renderer, and the intermediate texture the frame is composed in.
///
/// Field order matters on teardown: the surface must drop ahead of the
/// device and instance.
struct GpuState {
    /// The window this surface belongs to. Held so the window outlives
    /// every GPU object bound to it: the swap chain reads the handle for
    /// as long as it exists, and `detach` drops this last reference in the
    /// same breath as the surface.
    #[allow(dead_code, reason = "held to pin the window's lifetime, not read")]
    target: Arc<dyn RenderTarget>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    vello: vello::Renderer,
    vello_scene: vello::Scene,
    /// Vello sub-scene fragment cache: position-independent encoded paths
    /// for rects, shadows, and outlines keyed by appearance hash.
    /// `Scene::append(&frag, Some(translate))` reuses each encoded
    /// fragment across every position it appears at.
    fragment_cache: SceneFragmentCache,
    /// Rgba8Unorm intermediate (storage-binding compatible). Vello writes
    /// here; the blitter samples it via [`GpuState::intermediate_view_srgb`],
    /// which re-interprets the bytes as sRGB-encoded so a final blit into
    /// an sRGB surface format does not double-encode.
    intermediate: wgpu::Texture,
    /// Linear view used by vello as a storage write target.
    intermediate_view_linear: wgpu::TextureView,
    /// sRGB view of the same texture, used by the blitter as the sample
    /// source. The bytes are identical to what vello wrote; the GPU
    /// re-reads them through the sRGB-to-linear lookup, which exactly
    /// cancels the final linear-to-sRGB encode the surface applies.
    intermediate_view_srgb: wgpu::TextureView,
    blitter: TextureBlitter,
}

/// Drain the device before the queue drops, for the same reason as
/// `WgpuRenderer`: wgpu-core's `Queue::drop` panics if its fixed-timeout
/// wait on the last submission expires, and a panic inside drop aborts.
impl Drop for GpuState {
    fn drop(&mut self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

/// WGPU + vello renderer that presents into an OS window.
///
/// Construction is free and does no GPU work: a window backend builds one
/// before the window exists and calls
/// [`SurfaceRenderer::attach`] once it does.
#[derive(Default)]
pub struct WgpuSurfaceRenderer {
    gpu: Option<GpuState>,
}

impl WgpuSurfaceRenderer {
    /// A renderer with no window bound yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a window is currently bound.
    pub fn is_attached(&self) -> bool {
        self.gpu.is_some()
    }

    /// Physical size of the swap chain, or `None` while detached.
    pub fn surface_size(&self) -> Option<(u32, u32)> {
        self.gpu
            .as_ref()
            .map(|g| (g.surface_config.width, g.surface_config.height))
    }
}

impl Renderer for WgpuSurfaceRenderer {}

impl SurfaceRenderer for WgpuSurfaceRenderer {
    fn attach(&mut self, target: Arc<dyn RenderTarget>) -> Result<(), SurfaceError> {
        // Drop any previous binding first so the old surface releases the
        // window before a new one claims it.
        self.gpu = None;
        self.gpu = Some(GpuState::new(target)?);
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        match self.gpu.as_mut() {
            Some(gpu) => gpu.resize(width, height),
            None => false,
        }
    }

    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        // A screenshot request bypasses the dirty gate entirely: the
        // readback happens inside `present`, so an unchanged frame still
        // has to be re-encoded to answer it.
        if capture_requested(render_world) {
            return true;
        }
        // Two-level partial-repaint gate.
        //
        // Level 1 - `FrameRequest::dirty` (coarse): the tick folded every
        // render-relevant change into a single bool. A clear flag means the
        // tick left the render world untouched, so there is nothing new to
        // paint.
        //
        // Level 2 - the retained-scene diff (precise): the dirty flag
        // over-approximates. It is raised by writes that leave the painted
        // tree byte-for-byte identical - a signal re-set to the same value,
        // a hover class whose style resolves to the same visuals, a caret
        // tick landing on the same pixel. An empty damage region means the
        // last presented frame is still correct, so encode, submit, and
        // present are all skipped and the window keeps showing it. This
        // mirrors Qt `QWidget::update()` collapsing to no backing-store
        // flush on an empty region, and GTK's damage-region coalescing.
        //
        // `force_full` overrides level 2: after a resize or DPI change the
        // intermediate texture was just recreated and holds no valid
        // pixels, so the frame must be repainted even when the logical tree
        // is identical.
        request.dirty && (request.force_full || scene_has_damage(render_world))
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), SurfaceError> {
        let gpu = self.gpu.as_mut().ok_or(SurfaceError::Detached)?;
        gpu.present(render_world)
    }

    fn detach(&mut self) {
        self.gpu = None;
    }
}

/// Whether an off-thread screenshot request is waiting on this frame.
fn capture_requested(render_world: &World) -> bool {
    render_world
        .get_resource::<SurfaceCapture>()
        .is_some_and(|c| c.is_requested())
}

/// Whether the retained Node IR differs visually from the last painted
/// frame.
///
/// Diffs `PreviousScene` (the last painted tree) against `RetainedScene`
/// (this tick's freshly built tree). The first frame - `PreviousScene.root
/// == None` - reports damage so the initial paint always runs.
///
/// Conservative: the diff assumes-changed for any leaf it cannot compare
/// (images, SVGs, native), so it never under-reports the dirty region.
fn scene_has_damage(render_world: &World) -> bool {
    let previous = render_world.get_resource::<PreviousScene>();
    let retained = render_world.get_resource::<RetainedScene>();
    let (Some(previous), Some(retained)) = (previous, retained) else {
        // Resources missing (non-standard embed) - never skip.
        return true;
    };
    let size = render_world.resource::<Viewport>().size;
    let viewport_rect = LumenRect {
        origin: glam::Vec2::ZERO,
        size,
    };
    let mut damage = FrameDamage::default();
    diff_retained_scenes(
        previous.root.as_ref(),
        retained.root.as_ref(),
        viewport_rect,
        &mut damage,
    );
    !damage.is_empty()
}

impl GpuState {
    fn new(target: Arc<dyn RenderTarget>) -> Result<Self, SurfaceError> {
        // Wrap the adapter + device requests in a tracing span so users
        // running with `LUMEN_GPU_INIT_TRACE=1` (or any
        // `tracing_subscriber` consumer of the `lumen::render::gpu_init`
        // target) can see where the few-hundred-ms launch freeze comes
        // from.
        let _init_span = tracing::info_span!(
            target: "lumen::render::gpu_init",
            "lumen_gpu_init",
        )
        .entered();
        // Watchdog: lit by `init_done.store(true, ...)` once both adapter
        // and device requests resolve. If the deadline elapses with the
        // flag still unset, the watchdog thread panics with a diagnostic
        // so a wedged driver is visible instead of a silently frozen
        // window.
        let init_done = Arc::new(AtomicBool::new(false));
        let watchdog = spawn_gpu_init_watchdog(gpu_init_deadline_ms(), init_done.clone());
        let (init_w, init_h) = target.physical_size();
        // wgpu 29 moved the platform display connection into the instance
        // descriptor. This path presents to a real surface, so hand it the
        // window's own display: it is what GLES needs to present on
        // Wayland, and it must be the same handle `create_surface` below
        // receives. Vulkan / Metal / DX12 ignore it.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: NATIVE_BACKENDS,
            ..wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(DisplayTarget(
                target.clone(),
            )))
        });
        let surface = instance
            .create_surface(target.clone())
            .map_err(|e| SurfaceError::Init(format!("create_surface: {e:?}")))?;
        let adapter = {
            let _span = tracing::info_span!(
                target: "lumen::render::gpu_init",
                "request_adapter",
            )
            .entered();
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .map_err(|e| SurfaceError::Init(format!("request_adapter: {e:?}")))?
        };
        let limits = adapter.limits();
        let (device, queue) = {
            let _span = tracing::info_span!(
                target: "lumen::render::gpu_init",
                "request_device",
            )
            .entered();
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("lumen-render-wgpu surface device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            }))
            .map_err(|e| SurfaceError::Init(format!("request_device: {e:?}")))?
        };
        // Release: tell the watchdog we crossed the line cleanly so it
        // exits without panicking. `join` waits for the thread to wake
        // from its 50 ms sleep slice and observe the flag.
        init_done.store(true, Ordering::Release);
        let _ = watchdog.join();

        let caps = surface.get_capabilities(&adapter);
        // Surface-format negotiation. The frame is rendered into an
        // `Rgba8Unorm` intermediate and blitted through an
        // `Rgba8UnormSrgb` view, so an sRGB surface format matches the
        // gamma assumption exactly. Prefer the two sRGB 8-bit variants;
        // fall back to whatever the platform offered first otherwise.
        let surface_format = [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| SurfaceError::Init("no surface formats".to_string()))?;

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: init_w.max(1),
            height: init_h.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            // Input latency: cap the swap chain to a single in-flight frame
            // so a freshly encoded frame reaches the screen at the next
            // vblank instead of queueing behind a second buffered frame.
            // Trades a little GPU/CPU overlap headroom for lower
            // click-to-pixel latency, which is the right call for a UI
            // toolkit (Qt/GTK compositors present at depth 1). Present mode
            // stays AutoVsync.
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let vello = vello::Renderer::new(&device, RendererOptions::default())
            .map_err(|e| SurfaceError::Init(format!("vello renderer init: {e:?}")))?;

        let (intermediate, intermediate_view_linear, intermediate_view_srgb) =
            make_intermediate(&device, surface_config.width, surface_config.height);

        let blitter = TextureBlitter::new(&device, surface_format);

        Ok(Self {
            target,
            surface,
            surface_config,
            device,
            queue,
            vello,
            vello_scene: vello::Scene::new(),
            fragment_cache: SceneFragmentCache::default(),
            intermediate,
            intermediate_view_linear,
            intermediate_view_srgb,
            blitter,
        })
    }

    /// Reconfigure the surface + intermediate for a new physical size.
    /// Returns `true` when the size actually changed.
    fn resize(&mut self, width: u32, height: u32) -> bool {
        if width == self.surface_config.width && height == self.surface_config.height {
            return false;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        let (intermediate, linear, srgb) = make_intermediate(&self.device, width, height);
        self.intermediate = intermediate;
        self.intermediate_view_linear = linear;
        self.intermediate_view_srgb = srgb;
        true
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), SurfaceError> {
        // The retained Node IR replaces a flat sort-and-emit dance: the
        // walker takes the `RetainedScene` root and dispatches each variant
        // onto the right cached or uncached emitter, honouring
        // opacity/transform/clip composition.
        //
        // Image / SVG leaves: lumen-assets emits `ImageBlob` / `SvgPayload`
        // sidecars on the extracted entities, and
        // `transform_extracted_to_nodes` splices the type-erased payloads
        // into `Node::Image.blob` / `Node::Svg.payload`. The walker
        // downcasts those payloads back to the concrete vello types inline.
        //
        // Carve a local Arc of the tree so the borrow on the render world
        // releases before the walker takes mutable refs.
        let retained_root = render_world.resource::<RetainedScene>().root.clone();
        // Walker coords are logical pixels (matching layout-taffy /
        // Transform.absolute), while the vello surface is physical pixels.
        // Seed the root transform with `scale(scale_factor)` so logical
        // coords land at the right physical pixels - without this, on
        // hi-DPI the layout draws into the top-left `1/dpr x 1/dpr` of the
        // surface only.
        let (dpr, clear) = {
            let viewport = render_world.resource::<Viewport>();
            (viewport.scale_factor.max(0.01), viewport.clear)
        };
        // Snapshot the painter registry before the shaper borrow below takes the world mutably.
        // Cloning shares the table, so this costs one refcount.
        let natives = render_world
            .get_resource::<lumen_core::native::NativePainters>()
            .cloned();

        self.vello_scene.reset();
        {
            // The Node IR stays in LOGICAL pixels end to end
            // (`transform_extracted_to_nodes` does not pre-scale). The
            // walker scales every leaf and clip shape by `ctx.dpr` at emit
            // time, producing physical-pixel vello geometry that matches
            // the surface texture.
            //
            // The shaper is a render-world service, so a build with no text
            // backend simply has none installed and text is skipped.
            let mut shaper = render_world.get_non_send_mut::<ShaperService>();
            let shaper_ref: Option<&mut dyn TextShaper> = shaper
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn TextShaper);
            if let Some(root) = retained_root.as_ref() {
                let mut ctx = WalkContext::new_with_dpr(
                    &mut self.vello_scene,
                    Some(&mut self.fragment_cache),
                    shaper_ref,
                    dpr,
                );
                if let Some(painters) = natives.as_ref() {
                    ctx = ctx.with_native_painters(painters);
                }
                walk_node(&mut ctx, root);
            }
        }

        // Park the just-walked tree as PreviousScene for the next frame's
        // diff.
        render_world.resource_mut::<PreviousScene>().root = retained_root;

        let [r, g, b, a] = clear.to_rgba8();
        let params = RenderParams {
            base_color: AlphaColor::<Srgb>::from_rgba8(r, g, b, a),
            width: self.surface_config.width,
            height: self.surface_config.height,
            antialiasing_method: AaConfig::Area,
        };
        self.vello
            .render_to_texture(
                &self.device,
                &self.queue,
                &self.vello_scene,
                &self.intermediate_view_linear,
                &params,
            )
            .map_err(|e| SurfaceError::Present(format!("vello render: {e:?}")))?;

        // Cheap one-load check first; the readback path (allocate buffer +
        // GPU copy + map_async + memcpy) only fires when a client has set
        // the request flag. The no-screenshot path is just an atomic load.
        if let Some(capture) = render_world.get_resource::<SurfaceCapture>().cloned()
            && capture.is_requested()
        {
            let width = self.surface_config.width;
            let height = self.surface_config.height;
            match self.readback_intermediate(width, height) {
                Ok(rgba8) => {
                    capture.write(SurfaceFrame {
                        width,
                        height,
                        rgba8,
                    });
                }
                Err(e) => {
                    eprintln!("lumen-render-wgpu: surface readback failed: {e}");
                }
            }
            // Clear the flag either way so a persistent GPU error does not
            // spin the request forever.
            capture.clear_request();
        }

        // Acquire the next swap-chain texture. wgpu 29 replaced the
        // `Result<SurfaceTexture, SurfaceError>` with an enum whose
        // non-success arms each say what to do:
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
        // OutOfMemory is no longer an acquire outcome in wgpu 29 -
        // allocation failure now arrives through the device error scope -
        // so the old panic arm has no equivalent here.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_)
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
                tracing::debug!(
                    target: "lumen::render",
                    "surface texture suboptimal, outdated or lost; reconfiguring + skipping frame",
                );
                self.surface.configure(&self.device, &self.surface_config);
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
                tracing::debug!(
                    target: "lumen::render",
                    "window occluded; skipping frame",
                );
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(SurfaceError::Present(
                    "get_current_texture: validation error".to_string(),
                ));
            }
        };
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen blit encoder"),
            });
        self.blitter.copy(
            &self.device,
            &mut encoder,
            &self.intermediate_view_srgb,
            &surface_view,
        );
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Copy the post-vello intermediate texture to CPU as tightly packed
    /// RGBA8.
    ///
    /// Mirrors [`crate::WgpuRenderer::read_rgba8_async`]: padded rows
    /// (256-aligned `bytes_per_row`) on the GPU side, unpadded on the CPU
    /// side. No color conversion: the bytes are exactly what vello wrote
    /// into the `Rgba8Unorm` storage texture, which is the same payload the
    /// surface blitter samples through its sRGB view.
    fn readback_intermediate(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let unpadded = width as usize * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded = unpadded.div_ceil(align) * align;
        let size = (padded * height as usize) as u64;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumen surface readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen surface readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.intermediate,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
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
            .map_err(|_| "map channel dropped".to_string())?
            .map_err(|e| format!("{e:?}"))?;

        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity(unpadded * height as usize);
        for row in 0..height as usize {
            let start = row * padded;
            out.extend_from_slice(&raw[start..start + unpadded]);
        }
        drop(raw);
        buffer.unmap();
        Ok(out)
    }
}

fn make_intermediate(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lumen vello intermediate"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        // COPY_SRC enables the on-demand readback that screenshot clients
        // use; the no-screenshot path pays no runtime cost.
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        // Allow a second view in the sRGB variant so the blit input can
        // re-decode the bytes vello wrote.
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let linear = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("lumen intermediate (linear write)"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        // Storage-bindable view used by vello.
        usage: Some(wgpu::TextureUsages::STORAGE_BINDING),
        ..Default::default()
    });
    let srgb = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("lumen intermediate (sRGB sample)"),
        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
        // sRGB does not support STORAGE; this view is sample-only.
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        ..Default::default()
    });
    (texture, linear, srgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::node_ir::{PreviousScene, RetainedScene};
    use vello::wgpu::rwh::HasWindowHandle;

    fn render_world() -> World {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        world.insert_resource(RetainedScene::default());
        world.insert_resource(PreviousScene::default());
        world
    }

    /// A renderer with no window bound answers every surface call without
    /// touching the GPU: nothing to resize, nothing to present, and no
    /// panic. The window backend builds one before the window exists, so
    /// this is the state it starts in.
    #[test]
    fn detached_renderer_reports_no_surface() {
        let mut renderer = WgpuSurfaceRenderer::new();
        assert!(!renderer.is_attached());
        assert_eq!(renderer.surface_size(), None);
        assert!(!renderer.resize(800, 600));
        let mut world = render_world();
        assert!(matches!(
            renderer.present(&mut world),
            Err(SurfaceError::Detached)
        ));
        // Detaching an unattached renderer is a no-op, not an error.
        renderer.detach();
        assert!(!renderer.is_attached());
    }

    /// The present gate: a clean tick paints nothing, a dirty tick whose
    /// tree is unchanged still paints nothing, and a forced full frame
    /// paints regardless. Screenshot requests are checked separately
    /// because they bypass the dirty flag.
    #[test]
    fn present_gate_follows_dirty_and_damage() {
        let mut renderer = WgpuSurfaceRenderer::new();
        let mut world = render_world();

        // Nothing changed this tick.
        assert!(!renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: false,
                force_full: false,
            }
        ));
        // Dirty, but both trees are empty and identical, so there is no
        // visible difference to paint.
        assert!(!renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: true,
                force_full: false,
            }
        ));
        // A recreated surface holds no pixels: repaint even so.
        assert!(renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: true,
                force_full: true,
            }
        ));
    }

    /// The first frame always paints: `PreviousScene` is empty while
    /// `RetainedScene` holds a tree, so the diff reports damage. Once the
    /// same tree has been parked as the previous frame, it does not.
    #[test]
    fn first_frame_reports_damage() {
        let mut world = render_world();
        let root = one_rect();
        world.resource_mut::<RetainedScene>().root = Some(root.clone());
        assert!(scene_has_damage(&world));

        world.resource_mut::<PreviousScene>().root = Some(root);
        assert!(!scene_has_damage(&world));
    }

    /// A dirty tick whose tree really did change gets a frame.
    #[test]
    fn changed_tree_wants_present() {
        let mut renderer = WgpuSurfaceRenderer::new();
        let mut world = render_world();
        world.resource_mut::<RetainedScene>().root = Some(one_rect());
        assert!(renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: true,
                force_full: false,
            }
        ));
    }

    /// A render world missing the scene resources belongs to a non-standard
    /// embed. The diff cannot say anything about it, so it must claim
    /// damage rather than let the window keep a frame that may be stale.
    #[test]
    fn a_world_without_scene_resources_always_reports_damage() {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        assert!(scene_has_damage(&world));
        // Nor is a missing screenshot channel an error.
        assert!(!capture_requested(&world));
    }

    /// The init deadline is a tuning knob, not a validated setting: a
    /// missing or malformed value falls back to the default so a typo in
    /// the environment cannot stop an app from starting.
    #[test]
    fn a_malformed_deadline_falls_back_to_the_default() {
        assert_eq!(parse_deadline_ms(Some("250")), 250);
        assert_eq!(parse_deadline_ms(None), GPU_INIT_DEADLINE_DEFAULT_MS);
        assert_eq!(parse_deadline_ms(Some("")), GPU_INIT_DEADLINE_DEFAULT_MS);
        assert_eq!(
            parse_deadline_ms(Some("soon please")),
            GPU_INIT_DEADLINE_DEFAULT_MS
        );
        assert_eq!(parse_deadline_ms(Some("-1")), GPU_INIT_DEADLINE_DEFAULT_MS);
        // The env-backed reader agrees with the parser on an unset var.
        assert!(gpu_init_deadline_ms() > 0);
    }

    /// The watchdog exists to turn a wedged driver into a diagnostic. It
    /// stands down when init finishes in time, and takes the process down
    /// with a message when it does not: a panic on that thread is the
    /// visible failure, where the alternative is a window that never
    /// appears and a process that never exits.
    #[test]
    fn the_init_watchdog_stands_down_or_fires() {
        let done = Arc::new(AtomicBool::new(false));
        let watchdog = spawn_gpu_init_watchdog(60_000, done.clone());
        done.store(true, Ordering::Release);
        assert!(
            watchdog.join().is_ok(),
            "init finished in time, so the watchdog must exit quietly",
        );

        // Nothing ever lights the flag: the watchdog fires.
        let never = Arc::new(AtomicBool::new(false));
        let watchdog = spawn_gpu_init_watchdog(1, never);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watchdog.join()));
        assert!(
            matches!(outcome, Ok(Err(_))),
            "a deadline that passes with init unfinished must panic the watchdog thread",
        );
    }

    /// The display handle the renderer hands to wgpu delegates to the
    /// window and reports its size, so a window that cannot produce a
    /// handle yet surfaces as an error instead of a wrong handle.
    #[test]
    fn the_display_target_delegates_to_the_window() {
        let target = DisplayTarget(Arc::new(SizedWindow { size: (1280, 720) }));
        assert!(target.display_handle().is_err());
        assert!(target.0.window_handle().is_err());
        assert!(format!("{target:?}").contains("1280"));
    }

    /// A window that knows its size and nothing else. The tests that use
    /// it never dereference a handle, which is the point: everything below
    /// the GPU calls can be exercised with no display attached.
    struct SizedWindow {
        size: (u32, u32),
    }

    impl HasWindowHandle for SizedWindow {
        fn window_handle(&self) -> Result<vello::wgpu::rwh::WindowHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl HasDisplayHandle for SizedWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl RenderTarget for SizedWindow {
        fn physical_size(&self) -> (u32, u32) {
            self.size
        }
    }

    /// A single opaque rect, the smallest tree the diff reports damage for.
    fn one_rect() -> Arc<lumen_core::node_ir::Node> {
        use lumen_core::components::Color;
        use lumen_core::node_ir::Node;
        use lumen_core::render_world::Brush;

        Arc::new(Node::Rect {
            bounds: LumenRect {
                origin: glam::Vec2::new(4.0, 4.0),
                size: glam::Vec2::new(16.0, 16.0),
            },
            brush: Brush::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            corner: 0.0,
            corners: None,
        })
    }

    /// A pending screenshot forces a frame even on a clean tick, because
    /// the readback only happens inside `present`.
    #[test]
    fn capture_request_forces_a_frame() {
        let mut renderer = WgpuSurfaceRenderer::new();
        let mut world = render_world();
        let capture = SurfaceCapture::default();
        capture.request();
        world.insert_resource(capture);
        assert!(renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: false,
                force_full: false,
            }
        ));
    }
}
