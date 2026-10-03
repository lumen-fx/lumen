//! The GPU core every target shares: device bring-up, the texture vello
//! renders into, the frame encode, and readback.
//!
//! Vello's compute pipeline binds its render target as a storage texture
//! with the format hard-pinned to `Rgba8Unorm`, so every frame lands in an
//! `Rgba8Unorm` texture first, whatever the target. An offscreen target reads
//! it back; a window blits it onto the swap chain (see `surface.rs`). The
//! texture carries a second, sRGB view of the same bytes for that blit.

use crate::{NATIVE_BACKENDS, VelloPainter, vello_options};
use bevy_ecs::world::World;
use lumen_core::traits::RenderTarget;
use lumen_paint::{FragmentCache, PaintTarget};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use vello::wgpu;
// wgpu re-exports the handle crate it was built against, so the seam
// cannot drift from it.
use vello::wgpu::rwh::{DisplayHandle, HandleError, HasDisplayHandle};
use vello::{AaConfig, RenderParams};

/// Environment variable controlling the GPU adapter / device init
/// deadline, in milliseconds. Defaults to
/// [`GPU_INIT_DEADLINE_DEFAULT_MS`] when unset or unparsable. When the
/// deadline is exceeded, a watchdog thread panics with a diagnostic instead
/// of leaving the launch frozen without a word.
pub const GPU_INIT_DEADLINE_ENV: &str = "LUMEN_GPU_INIT_DEADLINE_MS";

/// Default GPU init deadline, in milliseconds. Surfaces driver hangs (a
/// broken Vulkan loader, a blocked Wayland compositor) within a bounded
/// wall-clock budget.
pub const GPU_INIT_DEADLINE_DEFAULT_MS: u64 = 5000;

/// The tracing target GPU bring-up reports its stages under.
const GPU_INIT_TARGET: &str = "lumen::render::gpu_init";

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

/// A thread that panics with a diagnostic if GPU bring-up has not finished
/// within its deadline. Dropping the watchdog stands it down at once, on
/// success or on an early error return alike.
struct InitWatchdog {
    /// Dropped to stand the watchdog down: its receive returns the moment
    /// the channel disconnects.
    done: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl InitWatchdog {
    fn spawn(deadline_ms: u64) -> Self {
        let (done, wait) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("lumen-gpu-init-watchdog".into())
            .spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) =
                    wait.recv_timeout(Duration::from_millis(deadline_ms))
                {
                    // The bring-up thread is wedged inside `pollster::block_on`
                    // waiting for a driver callback that will never come; this
                    // panic is the diagnostic.
                    panic!(
                        "lumen-render-wgpu: GPU init exceeded {deadline_ms} ms deadline (\
                         set {GPU_INIT_DEADLINE_ENV}=<ms> to tune). This usually means the GPU \
                         adapter / device request is blocked at the driver \
                         level (Vulkan loader, Wayland compositor handshake, \
                         or device reset). Re-run with `lumenc run --profile stderr` \
                         (a lumenc built with the `profiling` feature) to print each \
                         `{GPU_INIT_TARGET}` stage as it finishes; \
                         `RUST_LOG={GPU_INIT_TARGET}=trace` narrows the output to them.",
                    );
                }
            })
            .ok();
        Self {
            done: Some(done),
            thread,
        }
    }
}

impl Drop for InitWatchdog {
    fn drop(&mut self) {
        drop(self.done.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
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

/// An adapter found on one backend set, and the window surface created on
/// the same instance when there is a window.
struct Found {
    adapter: wgpu::Adapter,
    surface: Option<wgpu::Surface<'static>>,
}

/// Look for an adapter on `backends`. With a window, the instance is given
/// the window's display connection (what GLES needs to present on Wayland;
/// Vulkan, Metal, and DX12 ignore it) and the adapter must be able to
/// present to the window's surface. Without one, the instance declares no
/// display, so a host with no display socket still finds a device.
fn find_adapter(
    backends: wgpu::Backends,
    window: Option<&Arc<dyn RenderTarget>>,
) -> Result<Found, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..match window {
            Some(window) => wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                DisplayTarget(window.clone()),
            )),
            None => wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        }
    });
    let surface = window
        .map(|window| instance.create_surface(window.clone()))
        .transpose()
        .map_err(|e| format!("create_surface: {e:?}"))?;
    let adapter = {
        let _span = tracing::info_span!(target: GPU_INIT_TARGET, "request_adapter").entered();
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface.as_ref(),
            force_fallback_adapter: false,
        }))
        .map_err(|e| format!("request_adapter: {e}"))?
    };
    Ok(Found { adapter, surface })
}

/// An adapter on this OS's native backend, or, in a `gl-fallback` build,
/// on GL when the native backend has none.
fn find_any_adapter(window: Option<&Arc<dyn RenderTarget>>) -> Result<Found, String> {
    match find_adapter(NATIVE_BACKENDS, window) {
        Ok(found) => Ok(found),
        Err(primary) => {
            #[cfg(feature = "gl-fallback")]
            {
                find_adapter(wgpu::Backends::GL, window).map_err(|gl| {
                    format!(
                        "no adapter on the {NATIVE_BACKENDS:?} backend (primary: {primary}; \
                         GL fallback: {gl})"
                    )
                })
            }
            #[cfg(not(feature = "gl-fallback"))]
            {
                Err(format!(
                    "no adapter on the {NATIVE_BACKENDS:?} backend ({primary}); rebuild \
                     lumen-render-wgpu with --features gl-fallback for the GL compat path"
                ))
            }
        }
    }
}

/// The `Rgba8Unorm` texture a frame is rendered into, with a linear view
/// vello writes through and an sRGB view of the same bytes a blit samples.
struct FrameTexture {
    texture: wgpu::Texture,
    /// Storage-bindable view vello writes through.
    linear: wgpu::TextureView,
    /// The same bytes re-read as sRGB-encoded, so a blit into an sRGB
    /// surface does not encode them twice.
    srgb: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl FrameTexture {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lumen vello target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // STORAGE for vello, TEXTURE_BINDING for the surface blit,
            // COPY_SRC for readback.
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let linear = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("lumen vello target (linear write)"),
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            usage: Some(wgpu::TextureUsages::STORAGE_BINDING),
            ..Default::default()
        });
        let srgb = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("lumen vello target (sRGB sample)"),
            format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            // sRGB does not support STORAGE; this view is sample-only.
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        });
        Self {
            texture,
            linear,
            srgb,
            width,
            height,
        }
    }
}

/// Everything a frame needs on the GPU, whatever its target: the device,
/// the vello renderer, the sink frames are painted into, the fragment
/// cache, and the texture the frame lands in.
pub(crate) struct GpuCore {
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) adapter_info: wgpu::AdapterInfo,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    vello: vello::Renderer,
    /// The sink a frame is painted into; a [`VelloPainter`].
    painter: PaintTarget,
    /// Position-independent encoded paths for rects, shadows, outlines,
    /// and SVGs keyed by appearance, each reused across every position it
    /// appears at and across frames.
    cache: FragmentCache,
    frame: FrameTexture,
}

impl GpuCore {
    /// Bring up a device and a `width` x `height` frame texture. With a
    /// window, also returns the surface for it, created on the instance the
    /// adapter came from.
    ///
    /// The adapter and device requests run under tracing spans on the
    /// `lumen::render::gpu_init` target and under a watchdog that turns a
    /// wedged driver into a diagnostic.
    pub(crate) fn bring_up(
        window: Option<&Arc<dyn RenderTarget>>,
        width: u32,
        height: u32,
    ) -> Result<(Self, Option<wgpu::Surface<'static>>), String> {
        let _init_span = tracing::info_span!(target: GPU_INIT_TARGET, "lumen_gpu_init").entered();
        let watchdog = InitWatchdog::spawn(gpu_init_deadline_ms());
        let Found { adapter, surface } = find_any_adapter(window)?;
        let (device, queue) = {
            let _span = tracing::info_span!(target: GPU_INIT_TARGET, "request_device").entered();
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("lumen-render-wgpu device"),
                required_features: wgpu::Features::empty(),
                // vello needs more storage buffers per stage than the
                // downlevel defaults allow.
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            }))
            .map_err(|e| format!("request_device: {e}"))?
        };
        drop(watchdog);

        let adapter_info = adapter.get_info();
        let vello = vello::Renderer::new(&device, vello_options(&adapter_info))
            .map_err(|e| format!("vello renderer init: {e:?}"))?;
        let frame = FrameTexture::new(&device, width, height);
        Ok((
            Self {
                adapter,
                adapter_info,
                device,
                queue,
                vello,
                painter: Box::new(VelloPainter::new()),
                cache: FragmentCache::default(),
                frame,
            },
            surface,
        ))
    }

    /// Size of the frame texture in physical pixels.
    pub(crate) fn size(&self) -> (u32, u32) {
        (self.frame.width, self.frame.height)
    }

    /// Reallocate the frame texture at a new size, at least one pixel each
    /// way. Returns `true` when the size changed; the new texture holds no
    /// frame.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> bool {
        if (width.max(1), height.max(1)) == self.size() {
            return false;
        }
        self.frame = FrameTexture::new(&self.device, width, height);
        true
    }

    /// The frame texture as a blit reads it.
    pub(crate) fn srgb_view(&self) -> &wgpu::TextureView {
        &self.frame.srgb
    }

    /// Paint the render world's retained scene and render it into the
    /// frame texture.
    pub(crate) fn render(&mut self, render_world: &mut World) -> Result<(), String> {
        self.painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the GPU renderer paints through a VelloPainter")
            .reset();
        lumen_paint::paint_frame(render_world, &mut self.painter, Some(&mut self.cache));
        let params = RenderParams {
            base_color: lumen_paint::clear_color(render_world),
            width: self.frame.width,
            height: self.frame.height,
            antialiasing_method: AaConfig::Area,
        };
        let scene = self
            .painter
            .native()
            .downcast_ref::<VelloPainter>()
            .expect("the GPU renderer paints through a VelloPainter")
            .scene();
        self.vello
            .render_to_texture(
                &self.device,
                &self.queue,
                scene,
                &self.frame.linear,
                &params,
            )
            .map_err(|e| format!("vello render: {e:?}"))
    }

    /// Copy the frame texture to the CPU as tightly packed RGBA8: exactly
    /// the bytes vello wrote, with no colour conversion. Rows are padded to
    /// wgpu's copy alignment on the GPU side and unpadded here.
    pub(crate) fn read_rgba8(&self) -> Result<Vec<u8>, String> {
        let (width, height) = self.size();
        let unpadded = width as usize * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumen frame readback"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen frame readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.frame.texture,
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
        let (tx, rx) = mpsc::channel();
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

    /// Wait for every submission to finish. wgpu-core's `Queue::drop` waits
    /// on the last submission with a fixed timeout and panics when it
    /// expires; a software rasterizer on a loaded machine can hold a frame
    /// past it, and a panic inside drop aborts the process. Draining first,
    /// without a deadline, turns that abort into a quiet finish.
    pub(crate) fn drain(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::wgpu::rwh::{HasWindowHandle, WindowHandle};

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
    /// stands down at once when init finishes, well inside its deadline,
    /// and panics with a message when init never does.
    #[test]
    fn the_init_watchdog_stands_down_or_fires() {
        let started = std::time::Instant::now();
        drop(InitWatchdog::spawn(60_000));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "standing down must not wait out the deadline",
        );

        // Nothing ever stands it down: the watchdog fires.
        let mut watchdog = InitWatchdog::spawn(1);
        let thread = watchdog.thread.take().expect("spawned");
        let outcome = thread.join();
        assert!(
            outcome.is_err(),
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
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
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
}
