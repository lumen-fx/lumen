//! The GPU core every target shares: device bring-up, the texture vello
//! renders into, the frame encode, and readback.
//!
//! Vello's compute pipeline binds its render target as a storage texture
//! with the format hard-pinned to `Rgba8Unorm`, so every frame lands in an
//! `Rgba8Unorm` texture first, whatever the target. An offscreen target reads
//! it back; a window blits it onto the swap chain (see `surface.rs`). The
//! texture carries a second, sRGB view of the same bytes for that blit.

use crate::sink::{FinishedLayer, GpuLayer};
use crate::{NATIVE_BACKENDS, VelloPainter, vello_options};
use bevy_ecs::world::World;
use lumen_core::traits::RenderTarget;
use lumen_paint::pixels::TileView;
use lumen_paint::{FragmentCache, LayerCache, PaintTarget, PixelImage};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Weak};
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

/// Upload the tiles of `image` whose stamps moved since `kept` last took
/// them. Returns whether anything was written.
///
/// Most of an image changing is one write of the whole image rather than one
/// per tile: the per-call cost of a write is what dominates once the tile
/// count is large.
fn upload_tiles(queue: &wgpu::Queue, kept: &mut PixelTexture, image: &PixelImage) -> bool {
    let changed = image
        .tiles()
        .filter(|tile| kept.stamps[tile.index] != tile.stamp)
        .count();
    if changed == 0 {
        return false;
    }
    let (width, height) = (image.width(), image.height());
    let write = |origin: (u32, u32), size: (u32, u32), bytes: &[u8], stride: u32| {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &kept.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin.0,
                    y: origin.1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
    };
    if changed * 2 > image.tile_count() {
        write((0, 0), (width, height), &image.to_rgba8(), width * 4);
    } else {
        for tile in image.tiles() {
            if kept.stamps[tile.index] != tile.stamp {
                write(
                    (tile.x, tile.y),
                    (tile.width, tile.height),
                    tile.pixels,
                    TileView::STRIDE as u32,
                );
            }
        }
    }
    for tile in image.tiles() {
        kept.stamps[tile.index] = tile.stamp;
    }
    true
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
    /// Faded groups' layers, kept from frame to frame.
    layers: LayerCache,
    /// The textures standing in for kept layers' images, each with the layer
    /// it belongs to; a texture goes once its layer is dropped.
    textures: Vec<(Weak<GpuLayer>, vello::peniko::ImageData)>,
    /// The textures standing in for the pixel images frames draw, by image
    /// id, kept while frames keep drawing them.
    pixels: HashMap<u64, PixelTexture>,
    frame: FrameTexture,
}

/// One pixel image's pixels on the GPU, the image vello draws it through,
/// and the stamp each tile carried when it was last uploaded.
struct PixelTexture {
    texture: wgpu::Texture,
    stand_in: vello::peniko::ImageData,
    stamps: Vec<u64>,
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
        let mut sink = VelloPainter::new();
        sink.limit_layer_side(device.limits().max_texture_dimension_2d);
        let painter: PaintTarget = Box::new(sink);
        Ok((
            Self {
                adapter,
                adapter_info,
                device,
                queue,
                vello,
                painter,
                cache: FragmentCache::default(),
                layers: LayerCache::default(),
                textures: Vec::new(),
                pixels: HashMap::new(),
                frame,
            },
            surface,
        ))
    }

    /// What the kept layers of faded groups hold and how often they were
    /// reused.
    pub(crate) fn layer_stats(&self) -> lumen_paint::LayerStats {
        self.layers.stats()
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
        let (width, height) = (self.frame.width, self.frame.height);
        let sink = self
            .painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the GPU renderer paints through a VelloPainter");
        sink.reset();
        sink.set_target_size(width, height);
        lumen_paint::paint_frame(
            render_world,
            &mut self.painter,
            Some(&mut self.cache),
            Some(&mut self.layers),
        );
        self.upload_pixels();
        self.render_offscreen()?;
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
            .map_err(|e| format!("vello render: {e:?}"))?;
        self.release_textures();
        Ok(())
    }

    /// Render the offscreen layers the frame closed, and stand a region of a
    /// texture in for the image the frame draws each layer through.
    ///
    /// Each render costs a full pass of vello's pipeline however little it
    /// draws, so layers share textures: those closed at the same depth pack
    /// into as few textures as fit, and each texture is one render. The
    /// deepest go first, since a layer may draw a deeper one.
    fn render_offscreen(&mut self) -> Result<(), String> {
        let mut finished = self
            .painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the GPU renderer paints through a VelloPainter")
            .take_offscreen();
        if finished.is_empty() {
            return Ok(());
        }
        let max_side = self.device.limits().max_texture_dimension_2d;
        finished.sort_by(|a, b| {
            b.depth
                .cmp(&a.depth)
                .then(b.image.height.cmp(&a.image.height))
        });
        let mut rest = finished.as_slice();
        while !rest.is_empty() {
            let depth = rest[0].depth;
            let same = rest.iter().take_while(|l| l.depth == depth).count();
            let (wave, after) = rest.split_at(same);
            for batch in pack_layers(wave, max_side) {
                self.render_batch(wave, &batch)?;
            }
            rest = after;
        }
        Ok(())
    }

    /// Render one packed texture of offscreen layers and point each layer's
    /// image at its region.
    fn render_batch(&mut self, layers: &[FinishedLayer], batch: &Batch) -> Result<(), String> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lumen offscreen layers"),
            size: wgpu::Extent3d {
                width: batch.width,
                height: batch.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Vello renders into it as storage and copies each layer's region
            // into its image atlas from it.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut scene = vello::Scene::new();
        for &(index, x, y) in &batch.places {
            let layer = &layers[index];
            let (w, h) = (f64::from(layer.image.width), f64::from(layer.image.height));
            let (x, y) = (f64::from(x), f64::from(y));
            // Clip each layer to its own region so nothing it paints past its
            // bounds lands in a neighbour's.
            scene.push_clip_layer(
                vello::peniko::Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                &vello::kurbo::Rect::new(x, y, x + w, y + h),
            );
            scene.append(
                &layer.scene,
                Some(vello::kurbo::Affine::translate((
                    x - layer.bounds.x0,
                    y - layer.bounds.y0,
                ))),
            );
            scene.pop_layer();
        }
        let params = RenderParams {
            base_color: vello::peniko::Color::TRANSPARENT,
            width: batch.width,
            height: batch.height,
            antialiasing_method: AaConfig::Area,
        };
        self.vello
            .render_to_texture(&self.device, &self.queue, &scene, &view, &params)
            .map_err(|e| format!("vello offscreen layer render: {e:?}"))?;
        for &(index, x, y) in &batch.places {
            let layer = &layers[index];
            self.vello.override_image(
                &layer.image,
                Some(wgpu::TexelCopyTextureInfoBase {
                    texture: texture.clone(),
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                }),
            );
            self.textures
                .push((layer.layer.clone(), layer.image.clone()));
        }
        Ok(())
    }

    /// Bring the texture of every pixel image the frame drew up to the state
    /// it drew, uploading only the tiles written since the last upload, and
    /// stand each texture in for the image vello draws it through.
    ///
    /// The upload is the part that crosses to the GPU, and it is only the
    /// written tiles. Vello then copies the whole texture into its image atlas
    /// on the GPU (it has no partial copy), which is a device-local copy and
    /// no transfer.
    fn upload_pixels(&mut self) {
        let (drawn, gone) = self
            .painter
            .native()
            .downcast_mut::<VelloPainter>()
            .expect("the GPU renderer paints through a VelloPainter")
            .take_pixels();
        for stand_in in gone {
            self.vello.override_image(&stand_in, None);
            self.pixels
                .retain(|_, kept| kept.stand_in.data.id() != stand_in.data.id());
        }
        for (image, stand_in) in drawn {
            let (width, height) = (image.width(), image.height());
            let fresh = !self
                .pixels
                .get(&image.id())
                .is_some_and(|kept| kept.stand_in.data.id() == stand_in.data.id());
            if fresh {
                if let Some(old) = self.pixels.remove(&image.id()) {
                    self.vello.override_image(&old.stand_in, None);
                }
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("lumen pixel image"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    // Written from the CPU, copied into vello's atlas.
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                // wgpu clears a new texture, which is what a tile that was
                // never written holds: stamp 0 needs no upload.
                self.vello.override_image(
                    &stand_in,
                    Some(wgpu::TexelCopyTextureInfoBase {
                        texture: texture.clone(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    }),
                );
                self.pixels.insert(
                    image.id(),
                    PixelTexture {
                        texture,
                        stand_in: stand_in.clone(),
                        stamps: vec![0; image.tile_count()],
                    },
                );
            }
            let kept = self
                .pixels
                .get_mut(&image.id())
                .expect("inserted above when missing");
            if upload_tiles(&self.queue, kept, &image) {
                self.vello.mark_override_image_dirty(&kept.stand_in);
            }
        }
    }

    /// Drop the texture of every offscreen layer nothing holds any more: the
    /// layer cache let it go, so no later frame draws it. Runs after the
    /// frame rendered, which may have drawn it one last time.
    fn release_textures(&mut self) {
        let vello = &mut self.vello;
        self.textures.retain(|(layer, image)| {
            let live = layer.strong_count() > 0;
            if !live {
                vello.override_image(image, None);
            }
            live
        });
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

/// One texture's worth of offscreen layers: its size, and where each layer
/// (by index into the slice it was packed from) sits in it.
#[derive(Debug, PartialEq)]
struct Batch {
    width: u32,
    height: u32,
    places: Vec<(usize, u32, u32)>,
}

/// Pack `layers` into shelves of textures no wider or taller than
/// `max_side`, in the order given (tallest first packs tightest). A layer
/// larger than `max_side` either way gets no place and is left undrawn.
fn pack_layers(layers: &[FinishedLayer], max_side: u32) -> Vec<Batch> {
    pack_sizes(
        &layers
            .iter()
            .map(|l| (l.image.width, l.image.height))
            .collect::<Vec<_>>(),
        max_side,
    )
}

/// [`pack_layers`] over bare sizes.
fn pack_sizes(sizes: &[(u32, u32)], max_side: u32) -> Vec<Batch> {
    let area: u64 = sizes
        .iter()
        .map(|&(w, h)| u64::from(w) * u64::from(h))
        .sum();
    let widest = sizes.iter().map(|&(w, _)| w).max().unwrap_or(1);
    // About square, and never narrower than the widest layer.
    let width = ((area as f64).sqrt().ceil() as u32)
        .max(widest)
        .clamp(1, max_side);
    let mut batches = Vec::new();
    let mut batch = Batch {
        width,
        height: 0,
        places: Vec::new(),
    };
    let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
    for (index, &(w, h)) in sizes.iter().enumerate() {
        if w > max_side || h > max_side || w == 0 || h == 0 {
            continue;
        }
        if x + w > width {
            (x, y, shelf) = (0, y + shelf, 0);
        }
        if y + h > max_side {
            batch.height = y;
            batches.push(std::mem::replace(
                &mut batch,
                Batch {
                    width,
                    height: 0,
                    places: Vec::new(),
                },
            ));
            (x, y, shelf) = (0, 0, 0);
        }
        batch.places.push((index, x, y));
        x += w;
        shelf = shelf.max(h);
    }
    batch.height = y + shelf;
    if !batch.places.is_empty() {
        batches.push(batch);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::wgpu::rwh::{HasWindowHandle, WindowHandle};

    /// Layers pack side by side on shelves, a new shelf when one is full and
    /// a new texture when the shelves reach the size limit; a layer too big
    /// for any texture gets no place.
    #[test]
    fn layers_pack_into_shelves_and_textures() {
        let batches = pack_sizes(&[(4, 4), (4, 4), (4, 4), (4, 4)], 8);
        assert_eq!(
            batches,
            vec![Batch {
                width: 8,
                height: 8,
                places: vec![(0, 0, 0), (1, 4, 0), (2, 0, 4), (3, 4, 4)],
            }]
        );

        let batches = pack_sizes(&[(8, 6), (8, 6), (9, 1)], 8);
        assert_eq!(batches.len(), 2, "one layer per texture at that height");
        assert_eq!(batches[0].places, vec![(0, 0, 0)]);
        assert_eq!((batches[0].width, batches[0].height), (8, 6));
        assert_eq!(batches[1].places, vec![(1, 0, 0)]);
        assert!(pack_sizes(&[], 8).is_empty());
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
