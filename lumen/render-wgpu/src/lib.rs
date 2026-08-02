//! WGPU + vello renderer backend.
//!
//! - Registers a render-world system in [`RenderStage::Render`] that queries [`ExtractedRect`] and [`ExtractedText`], encodes them into a [`vello::Scene`], and renders into an `Rgba8Unorm` texture.
//! - Offscreen-texture path; tests read back the framebuffer to CPU for cross-platform parity.
//! - The on-screen winit-surface path lives in `lumen-window-winit` and reuses [`translate_rects`] and [`draw_text_into_vello`] with its own surface texture view.

#![warn(missing_docs)]

pub mod scene_cache;
pub mod walker;
pub use scene_cache::{CacheStats, FragmentKey, SceneFragmentCache, append_translated};
pub use walker::{
    ClipStack, WalkContext, damage_union, diff_retained_scenes, walk_node, walk_retained_scene,
};

use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use lumen_core::components::Color as LumenColor;
use lumen_core::prelude::*;
use lumen_core::render_world::DEFAULT_SELECTION_BG;
use thiserror::Error;
use vello::peniko;
use vello::peniko::color::{AlphaColor, Srgb};
use vello::peniko::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Blob, Color as PenikoColor, Fill, FontData};
use vello::wgpu;
use vello::{AaConfig, RenderParams, RendererOptions};

/// The single GPU backend this per-OS build compiles and probes (Part A of
/// runtime-tree-shaking). The Cargo manifest already trims wgpu/naga to one
/// backend per OS; pinning the instance's `Backends` to the same bit is
/// defense in depth -- it keeps the requested set honest and avoids a surprise
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
/// out with a reason instead of running. Direct3D's WARP rasterizer faults the
/// process partway through offscreen rendering, and lavapipe's output is close
/// to but not interchangeable with a GPU's, so neither is a substrate for
/// pixel-level checks.
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

/// Offscreen WGPU + vello renderer.
///
/// Holds the device, queue, vello renderer, vello scene buffer, and the
/// offscreen texture target. The actual render-world system fills the scene
/// and calls [`render_current`].
pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    vello: vello::Renderer,
    /// Per-frame vello scene buffer; reset by the render system each frame.
    pub scene: vello::Scene,
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    /// Number of actual GPU encode+submit passes ([`render_current`]) since
    /// construction. Frames skipped by the empty-damage partial-repaint gate do
    /// NOT increment it, so `render_count` measures real present work - a static
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
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: NATIVE_BACKENDS,
            ..wgpu::InstanceDescriptor::from_env_or_default()
        });
        let adapter = match instance.request_adapter(&opts).await {
            Ok(a) => a,
            Err(primary_err) => {
                #[cfg(feature = "gl-fallback")]
                {
                    let gl_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                        backends: wgpu::Backends::GL,
                        ..wgpu::InstanceDescriptor::from_env_or_default()
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

        let vello = vello::Renderer::new(&device, RendererOptions::default())
            .map_err(|e| WgpuRendererError::Vello(format!("{e:?}")))?;

        let (texture, texture_view) = make_target(&device, width, height);

        Ok(Self {
            device,
            queue,
            vello,
            scene: vello::Scene::new(),
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

    /// Render the current contents of `self.scene` into the offscreen target.
    pub fn render_current(&mut self, clear: LumenColor) {
        self.render_count += 1;
        let params = RenderParams {
            base_color: peniko_color(clear),
            width: self.width,
            height: self.height,
            antialiasing_method: AaConfig::Area,
        };
        if let Err(e) = self.vello.render_to_texture(
            &self.device,
            &self.queue,
            &self.scene,
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

impl lumen_core::traits::Renderer for WgpuRenderer {}

/// Emit a single rect onto an already-reset scene. Callers manage scene
/// lifecycle (reset, append, render) - the renderer interleaves rects /
/// text / outlines through this helper sorted by [`PaintOrder`].
///
/// Origin is baked into the encoding. For cached, position-independent
/// encoding see [`emit_rect_into_fragment`] + [`emit_rect_cached`].
pub fn emit_rect(vello_scene: &mut vello::Scene, cmd: &ExtractedRect) {
    emit_rect_at(vello_scene, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

/// Emit a rect *at the local origin* into a fresh scene fragment that
/// can be cached and re-appended via [`scene_cache::append_translated`].
/// Identical appearance => identical fragment => one encode amortised
/// across every position the rect ever takes.
pub fn emit_rect_into_fragment(cmd: &ExtractedRect) -> vello::Scene {
    let mut frag = vello::Scene::new();
    emit_rect_at(&mut frag, cmd, 0.0, 0.0);
    frag
}

/// Maps Lumen gradient stops straight through to `peniko::ColorStop`,
/// converting each colour via [`peniko_color`]. Shared by the linear /
/// radial / conic arms of [`emit_rect_at`].
fn to_color_stops(stops: &[(f32, LumenColor)]) -> Vec<peniko::ColorStop> {
    stops
        .iter()
        .map(|(offset, color)| peniko::ColorStop {
            offset: *offset,
            color: peniko_color(*color).into(),
        })
        .collect()
}

fn emit_rect_at(vello_scene: &mut vello::Scene, cmd: &ExtractedRect, ox: f64, oy: f64) {
    use lumen_core::render_world::Brush as LumenBrush;
    let x0 = ox;
    let y0 = oy;
    let x1 = x0 + cmd.size.x as f64;
    let y1 = y0 + cmd.size.y as f64;

    let brush: peniko::Brush = match &cmd.brush {
        LumenBrush::Solid(c) => peniko::Brush::Solid(peniko_color(*c)),
        LumenBrush::Linear { angle_deg, stops } => {
            // Convert CSS angle (0deg = left->right, increasing CCW) into
            // start/end points on the bounding box. CSS defines 0deg as
            // bottom-to-top - but we use the more common "0 = right" so
            // authors can think in compass terms. Each stop is mapped
            // straight through to peniko::ColorStop.
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let diag = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt() / 2.0;
            let theta = (*angle_deg as f64).to_radians();
            let dx = theta.cos() * diag;
            let dy = theta.sin() * diag;
            let start = vello::kurbo::Point::new(cx - dx, cy - dy);
            let end = vello::kurbo::Point::new(cx + dx, cy + dy);
            let color_stops = to_color_stops(stops);
            let g = peniko::Gradient::new_linear(start, end).with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
        LumenBrush::Radial { radius, stops } => {
            // Centre on the rect midpoint; multiply the normalised radius by half the rect's min dimension so `1.0` reaches the nearest edge.
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let half_min = ((x1 - x0).min(y1 - y0) / 2.0).max(1.0);
            let r = (*radius as f64 * half_min).max(1.0) as f32;
            let color_stops = to_color_stops(stops);
            let g = peniko::Gradient::new_radial(vello::kurbo::Point::new(cx, cy), r)
                .with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
        LumenBrush::Conic { from_deg, stops } => {
            // Centre on the rect midpoint; the sweep runs clockwise from `from_deg` (0 = north).
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let color_stops = to_color_stops(stops);
            let start = *from_deg;
            let end = start + 360.0;
            let g = peniko::Gradient::new_sweep(vello::kurbo::Point::new(cx, cy), start, end)
                .with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
    };

    if let Some([tl, tr, br, bl]) = cmd.corner_radii {
        use vello::peniko::kurbo::RoundedRectRadii;
        let radii = RoundedRectRadii::new(tl as f64, tr as f64, br as f64, bl as f64);
        let shape = RoundedRect::from_rect(Rect::new(x0, y0, x1, y1), radii);
        vello_scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &shape);
    } else if cmd.radius > 0.0 {
        let shape = RoundedRect::new(x0, y0, x1, y1, cmd.radius as f64);
        vello_scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &shape);
    } else {
        let shape = Rect::new(x0, y0, x1, y1);
        vello_scene.fill(Fill::NonZero, Affine::IDENTITY, &brush, None, &shape);
    }
}

/// Shared cache-lookup skeleton for the cached emitters: on hit, appends the
/// stored fragment translated to `origin`; on miss, runs `encode` to build a
/// position-independent fragment, appends it, and stores it under `key`.
fn emit_cached(
    vello_scene: &mut vello::Scene,
    cache: &mut SceneFragmentCache,
    key: FragmentKey,
    origin: glam::Vec2,
    encode: impl FnOnce() -> vello::Scene,
) {
    if let Some(frag) = cache.get(key) {
        append_translated(vello_scene, &frag, origin);
        return;
    }
    let frag = encode();
    append_translated(vello_scene, &frag, origin);
    cache.insert(key, frag);
}

/// Cache-aware emit: looks up the fragment for this rect's appearance,
/// encodes on miss, appends translated to `cmd.origin`.
pub fn emit_rect_cached(
    vello_scene: &mut vello::Scene,
    cache: &mut SceneFragmentCache,
    cmd: &ExtractedRect,
) {
    emit_cached(
        vello_scene,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        || emit_rect_into_fragment(cmd),
    );
}

/// CSS `object-fit` placement math shared by raster images ([`draw_image_into_vello`])
/// and vector SVGs ([`emit_svg`]). Given the intrinsic size and the layout
/// box, returns the centred `(offset, drawn_size)` in the box's coordinate
/// space (top-left for `None`, centred otherwise). Both axes are guarded
/// against a zero intrinsic dimension; callers already reject zero-sized
/// content before calling, so the guard is a belt-and-braces no-op there.
fn fit_box(
    intrinsic: (f64, f64),
    box_size: (f64, f64),
    fit: lumen_core::components::ImageFit,
) -> ((f64, f64), (f64, f64)) {
    use lumen_core::components::ImageFit;
    let (iw, ih) = intrinsic;
    let (bw, bh) = box_size;
    let box_aspect = if bh > 0.0 { bw / bh } else { 1.0 };
    let img_aspect = if ih > 0.0 { iw / ih } else { 1.0 };
    let (dw, dh) = match fit {
        ImageFit::Fill => (bw, bh),
        ImageFit::None => (iw, ih),
        ImageFit::Contain => {
            if img_aspect > box_aspect {
                (bw, bw / img_aspect)
            } else {
                (bh * img_aspect, bh)
            }
        }
        ImageFit::Cover => {
            if img_aspect > box_aspect {
                (bh * img_aspect, bh)
            } else {
                (bw, bw / img_aspect)
            }
        }
        ImageFit::ScaleDown => {
            if iw <= bw && ih <= bh {
                (iw, ih)
            } else if img_aspect > box_aspect {
                (bw, bw / img_aspect)
            } else {
                (bh * img_aspect, bh)
            }
        }
    };
    let dx = match fit {
        ImageFit::None => 0.0,
        _ => (bw - dw) / 2.0,
    };
    let dy = match fit {
        ImageFit::None => 0.0,
        _ => (bh - dh) / 2.0,
    };
    ((dx, dy), (dw, dh))
}

/// Append a pre-rendered SVG sub-scene to the target with a transform
/// computed from the entity's drawn size + intrinsic SVG size + fit
/// mode. Reuses the same fit-math as raster images so authors get
/// consistent placement across `<image src="*.png">` and
/// `<image src="*.svg">`.
pub fn emit_svg(vello_scene: &mut vello::Scene, cmd: &lumen_assets::ExtractedSvg) {
    let iw = cmd.intrinsic.x as f64;
    let ih = cmd.intrinsic.y as f64;
    let bw = cmd.size.x as f64;
    let bh = cmd.size.y as f64;
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let ((dx, dy), (dw, dh)) = fit_box((iw, ih), (bw, bh), cmd.fit);
    let sx = dw / iw;
    let sy = dh / ih;
    let transform = Affine::translate((cmd.origin.x as f64 + dx, cmd.origin.y as f64 + dy))
        * Affine::scale_non_uniform(sx, sy);
    // Wrap the cached scene in a `push_layer` when opacity < 1.0 so the
    // SVG fades uniformly. Clip rect = drawn target rect.
    let alpha = cmd.alpha.clamp(0.0, 1.0);
    let needs_alpha = alpha < 1.0;
    if needs_alpha {
        let clip = Rect::new(
            cmd.origin.x as f64,
            cmd.origin.y as f64,
            cmd.origin.x as f64 + bw,
            cmd.origin.y as f64 + bh,
        );
        vello_scene.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            Affine::IDENTITY,
            &clip,
        );
    }
    vello_scene.append(&cmd.asset.scene, Some(transform));
    if needs_alpha {
        vello_scene.pop_layer();
    }
}

/// Emit a single drop shadow onto an already-reset scene. Uses vello's
/// native `draw_blurred_rounded_rect` - one GPU primitive per shadow,
/// no stacked-clones approximation.
pub fn emit_shadow(vello_scene: &mut vello::Scene, cmd: &ExtractedShadow) {
    emit_shadow_at(vello_scene, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

/// Emit a shadow into a fresh fragment positioned at local origin
/// (0,0) for caching.
pub fn emit_shadow_into_fragment(cmd: &ExtractedShadow) -> vello::Scene {
    let mut frag = vello::Scene::new();
    emit_shadow_at(&mut frag, cmd, 0.0, 0.0);
    frag
}

fn emit_shadow_at(vello_scene: &mut vello::Scene, cmd: &ExtractedShadow, ox: f64, oy: f64) {
    // CSS spread: inflate (positive) / deflate (negative) the shadow
    // rect on every side before blurring; the corner radius grows /
    // shrinks with it (CSS Backgrounds & Borders section 7.1.1).
    let spread = cmd.spread as f64;
    let x1 = ox + cmd.size.x as f64;
    let y1 = oy + cmd.size.y as f64;
    if !cmd.inner {
        let rect = Rect::new(ox - spread, oy - spread, x1 + spread, y1 + spread);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        vello_scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            rect,
            peniko_color(cmd.color),
            (cmd.radius as f64 + spread).max(0.0),
            cmd.blur.max(0.0) as f64,
        );
        return;
    }
    // Inner (inset) shadow. Clip to the entity rect, then draw a
    // blurred rect at the *negated* offset so the dark edge lands on
    // the inside rim. Grow the inner rect outward by ~3x blur so the
    // gradient covers the whole interior; the clip hides the overflow.
    let rect_x0 = cmd.rect_origin.x as f64;
    let rect_y0 = cmd.rect_origin.y as f64;
    let rect_x1 = rect_x0 + cmd.size.x as f64;
    let rect_y1 = rect_y0 + cmd.size.y as f64;
    let clip = Rect::new(rect_x0, rect_y0, rect_x1, rect_y1);
    let grow = (cmd.blur.max(0.0) as f64 * 3.0).max(1.0);
    let dx = -(cmd.origin.x - cmd.rect_origin.x) as f64;
    let dy = -(cmd.origin.y - cmd.rect_origin.y) as f64;
    // Inset spread moves the shadow's inner edge inward: shrink the
    // blurred rect by `spread` per side (the clip still hides the
    // outer overflow).
    let inset_rect = Rect::new(
        rect_x0 + dx - grow + spread,
        rect_y0 + dy - grow + spread,
        rect_x1 + dx + grow - spread,
        rect_y1 + dy + grow - spread,
    );
    let _ = ox;
    let _ = oy;
    vello_scene.push_layer(
        Fill::NonZero,
        peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &clip,
    );
    vello_scene.draw_blurred_rounded_rect(
        Affine::IDENTITY,
        inset_rect,
        peniko_color(cmd.color),
        cmd.radius as f64,
        cmd.blur.max(0.0) as f64,
    );
    vello_scene.pop_layer();
}

/// Cache-aware shadow emit. Identical appearance shares the cached
/// blurred rounded-rect fragment across all positions.
pub fn emit_shadow_cached(
    vello_scene: &mut vello::Scene,
    cache: &mut SceneFragmentCache,
    cmd: &ExtractedShadow,
) {
    // Inner shadows depend on the entity's clip rect (not a
    // position-independent appearance), so they bypass the cache.
    if cmd.inner {
        emit_shadow_at(vello_scene, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
        return;
    }
    emit_cached(
        vello_scene,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        || emit_shadow_into_fragment(cmd),
    );
}

/// Emit a single outline onto an already-reset scene.
pub fn emit_outline(vello_scene: &mut vello::Scene, cmd: &ExtractedOutline) {
    emit_outline_at(vello_scene, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

/// Emit an outline into a fresh fragment positioned at local origin
/// (0,0) for caching.
pub fn emit_outline_into_fragment(cmd: &ExtractedOutline) -> vello::Scene {
    let mut frag = vello::Scene::new();
    emit_outline_at(&mut frag, cmd, 0.0, 0.0);
    frag
}

fn emit_outline_at(vello_scene: &mut vello::Scene, cmd: &ExtractedOutline, ox: f64, oy: f64) {
    use vello::kurbo::Stroke;
    if cmd.width <= 0.0 {
        return;
    }
    let brush = peniko_color(cmd.stroke);
    let x1 = ox + cmd.size.x as f64;
    let y1 = oy + cmd.size.y as f64;
    let stroke = Stroke::new(cmd.width as f64);
    if cmd.radius > 0.0 {
        let shape = RoundedRect::new(ox, oy, x1, y1, cmd.radius as f64);
        vello_scene.stroke(&stroke, Affine::IDENTITY, brush, None, &shape);
    } else {
        let shape = Rect::new(ox, oy, x1, y1);
        vello_scene.stroke(&stroke, Affine::IDENTITY, brush, None, &shape);
    }
}

/// Emit a CSS border ring onto the scene: the area between the outer
/// border box (rounded by `cmd.radius`) and the inner padding box (each
/// side inset by its width, inner corner radii reduced per CSS
/// `border-radius` background-clip math), filled even-odd with the
/// border color. Handles both uniform and per-side widths exactly.
pub fn emit_border(
    vello_scene: &mut vello::Scene,
    cmd: &lumen_core::render_world::ExtractedBorder,
) {
    use vello::peniko::kurbo::{BezPath, RoundedRectRadii, Shape};
    let [top, right, bottom, left] = cmd.widths;
    if top <= 0.0 && right <= 0.0 && bottom <= 0.0 && left <= 0.0 {
        return;
    }
    let x0 = cmd.origin.x as f64;
    let y0 = cmd.origin.y as f64;
    let x1 = x0 + cmd.size.x as f64;
    let y1 = y0 + cmd.size.y as f64;
    let r = cmd.radius.max(0.0) as f64;
    // Per-corner outer radii `[tl, tr, br, bl]` - uniform `radius`
    // when the entity has no per-corner override.
    let [rtl, rtr, rbr, rbl] = cmd
        .corner_radii
        .map(|cs| cs.map(|c| c.max(0.0) as f64))
        .unwrap_or([r; 4]);
    let rounded = rtl > 0.0 || rtr > 0.0 || rbr > 0.0 || rbl > 0.0;
    let (top, right, bottom, left) = (top as f64, right as f64, bottom as f64, left as f64);

    // Inner box = border box inset by the per-side widths. Degenerate
    // (fully-consumed) inner boxes fill the whole outer shape.
    let ix0 = x0 + left;
    let iy0 = y0 + top;
    let ix1 = (x1 - right).max(ix0);
    let iy1 = (y1 - bottom).max(iy0);

    let mut path = BezPath::new();
    let tol = 0.1;
    if rounded {
        let outer_radii = RoundedRectRadii::new(rtl, rtr, rbr, rbl);
        path.extend(
            RoundedRect::from_rect(Rect::new(x0, y0, x1, y1), outer_radii).path_elements(tol),
        );
    } else {
        path.extend(Rect::new(x0, y0, x1, y1).path_elements(tol));
    }
    if ix1 > ix0 && iy1 > iy0 {
        // CSS: inner corner radius = max(0, outer radius - the two
        // adjacent border widths' relevant component). With one circular
        // radius per corner we take the max of the two adjacent widths.
        let radii = RoundedRectRadii::new(
            (rtl - left.max(top)).max(0.0),
            (rtr - top.max(right)).max(0.0),
            (rbr - right.max(bottom)).max(0.0),
            (rbl - bottom.max(left)).max(0.0),
        );
        let inner = RoundedRect::from_rect(Rect::new(ix0, iy0, ix1, iy1), radii);
        path.extend(inner.path_elements(tol));
    }

    // Uniform-color fast path: one even-odd fill of the ring.
    let uniform = match cmd.side_colors {
        None => true,
        Some([t, rr, b, l]) => t == rr && rr == b && b == l,
    };
    if uniform {
        let color = cmd.side_colors.map(|cs| cs[0]).unwrap_or(cmd.color);
        vello_scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            peniko_color(color),
            None,
            &path,
        );
        return;
    }

    // Per-side colors: clip to the ring, then fill one mitred trapezoid
    // per side (outer edge -> the matching inner-box corner), exactly the
    // corner-diagonal split browsers paint. The clip keeps the rounded
    // corners correct.
    let side_colors = cmd.side_colors.unwrap_or([cmd.color; 4]);
    vello_scene.push_layer(
        Fill::EvenOdd,
        vello::peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &path,
    );
    let quads: [(f64, [(f64, f64); 4]); 4] = [
        // top: outer TL, outer TR, inner TR, inner TL
        (top, [(x0, y0), (x1, y0), (ix1, iy0), (ix0, iy0)]),
        // right: outer TR, outer BR, inner BR, inner TR
        (right, [(x1, y0), (x1, y1), (ix1, iy1), (ix1, iy0)]),
        // bottom: outer BR, outer BL, inner BL, inner BR
        (bottom, [(x1, y1), (x0, y1), (ix0, iy1), (ix1, iy1)]),
        // left: outer BL, outer TL, inner TL, inner BL
        (left, [(x0, y1), (x0, y0), (ix0, iy0), (ix0, iy1)]),
    ];
    for (i, (width, pts)) in quads.iter().enumerate() {
        if *width <= 0.0 {
            continue;
        }
        let mut quad = BezPath::new();
        quad.move_to(pts[0]);
        quad.line_to(pts[1]);
        quad.line_to(pts[2]);
        quad.line_to(pts[3]);
        quad.close_path();
        vello_scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            peniko_color(side_colors[i]),
            None,
            &quad,
        );
    }
    vello_scene.pop_layer();
}

/// Cache-aware outline emit.
pub fn emit_outline_cached(
    vello_scene: &mut vello::Scene,
    cache: &mut SceneFragmentCache,
    cmd: &ExtractedOutline,
) {
    emit_cached(
        vello_scene,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        || emit_outline_into_fragment(cmd),
    );
}

/// Draw one [`ExtractedImage`] into `vello_scene` honouring [`ImageFit`].
/// `blob` is the pre-built peniko Blob from [`lumen_assets::LoadedImage`],
/// cloned (cheap, Arc-internal) each frame so vello can key its GPU
/// upload cache off the stable Blob identity rather than re-uploading
/// the texture every tick.
pub fn draw_image_into_vello(
    vello_scene: &mut vello::Scene,
    cmd: &ExtractedImage,
    blob: &lumen_assets::ExtractedImageBlob,
) {
    use lumen_core::components::ImageFit;
    if cmd.width == 0 || cmd.height == 0 {
        return;
    }
    let image_data = peniko::ImageData {
        data: blob.0.clone(),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: cmd.width,
        height: cmd.height,
    };

    let iw = cmd.width as f64;
    let ih = cmd.height as f64;
    let bw = cmd.size.x as f64;
    let bh = cmd.size.y as f64;

    // Compute scaled (drawn) width + height + per-axis offset inside the
    // layout box according to the fit mode. Top-left default; centered
    // for cover / contain / scale-down because that's what CSS / Flutter
    // / SwiftUI all do.
    let ((dx, dy), (dw, dh)) = fit_box((iw, ih), (bw, bh), cmd.fit);

    let sx = if iw > 0.0 { dw / iw } else { 1.0 };
    let sy = if ih > 0.0 { dh / ih } else { 1.0 };
    let transform = Affine::translate((cmd.origin.x as f64 + dx, cmd.origin.y as f64 + dy))
        * Affine::scale_non_uniform(sx, sy);

    // Cover may overshoot the box - clip to the entity rect so the image
    // doesn't bleed onto sibling boxes. push_layer + pop_layer with a
    // simple Rect clip is cheap on vello 0.8. The same layer carries the
    // `Opacity` alpha so partially-transparent images fade as a whole.
    let needs_clip = matches!(cmd.fit, ImageFit::Cover | ImageFit::None);
    let alpha = cmd.alpha.clamp(0.0, 1.0);
    let needs_alpha = alpha < 1.0;
    let needs_layer = needs_clip || needs_alpha;
    if needs_layer {
        let clip = Rect::new(
            cmd.origin.x as f64,
            cmd.origin.y as f64,
            cmd.origin.x as f64 + bw,
            cmd.origin.y as f64 + bh,
        );
        vello_scene.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            Affine::IDENTITY,
            &clip,
        );
    }
    vello_scene.draw_image(&peniko::ImageBrush::new(image_data), transform);
    if needs_layer {
        vello_scene.pop_layer();
    }
}

thread_local! {
    /// Per-font-id `Blob` cache. Keyed on the shaper's stable `font_id`
    /// hash so the same face reuses one `Blob` (hence one vello glyph-atlas
    /// entry) across frames. Rendering is single-threaded per surface, so a
    /// thread-local keeps the cache lock-free; a second render thread simply
    /// warms its own copy. Fonts are few and long-lived - no eviction.
    static FONT_BLOBS: std::cell::RefCell<rustc_hash::FxHashMap<u64, Blob<u8>>> =
        std::cell::RefCell::new(rustc_hash::FxHashMap::default());
}

/// Fetch (or mint + cache) the `Blob` for a font id. The clone returned is
/// an `Arc` bump - the underlying face bytes are shared, not copied.
fn font_blob(font_id: u64, font_data: &std::sync::Arc<Vec<u8>>) -> Blob<u8> {
    FONT_BLOBS.with(|c| {
        c.borrow_mut()
            .entry(font_id)
            .or_insert_with(|| Blob::new(font_data.clone()))
            .clone()
    })
}

/// Draws one [`ExtractedText`] into `vello_scene` using the supplied shaper.
///
/// The un-scaled run is passed by reference alongside the running device-
/// pixel ratio (`dpr`) and `opacity`; both are folded in locally so the
/// walker never has to deep-clone the run (String and all) per node.
///
/// W3.6/W5.6: shapes the run ONCE per frame; caret + selection x
/// positions come from the same shape via [`lumen_text::TextGeometry`].
/// Multi-segment paths (Latin + Arabic + ...) issue one `draw_glyphs`
/// call per segment so each font is bound to the right glyph slice;
/// selection highlights emit one rectangle per maximal contiguous-
/// level slice the range intersects (HTML / Qt / macOS convention).
/// The pre-W3.6 baseline triggered three additional reshape passes per
/// frame (one for caret prefix, two for selection ends).
pub fn draw_text_into_vello<S: lumen_text::TextShaper + ?Sized>(
    shaper: &mut S,
    vello_scene: &mut vello::Scene,
    text: &ExtractedText,
    dpr: f32,
    opacity: f32,
) {
    use lumen_core::components::TextAlign;
    use lumen_text::{ShapeOptions, WrapMode};
    // The walker passes the un-scaled `ExtractedText` by reference plus the
    // running dpr + opacity; we fold both in here rather than deep-cloning
    // the run (String included) per node just to mutate a few scalars.
    let origin = text.origin * dpr;
    let size_px = text.size_px * dpr;
    let container_width = text.container_width * dpr;
    let fill = folded(text.fill, opacity);
    // Shape the full run for glyph painting using the text's configured wrap and `max_lines`.
    let wrap = WrapMode::from(text.wrap);
    let shape_opts = ShapeOptions {
        width: Some(container_width),
        wrap,
        max_lines: text.max_lines,
        family: text.family.clone(),
        weight: text.weight,
    };
    let shaped = shaper.shape(&text.text, size_px, shape_opts);
    let measured = shaped.as_ref().map(|r| r.width).unwrap_or(0.0);
    let align_dx = match text.align {
        TextAlign::Start => 0.0,
        TextAlign::Center => ((container_width - measured) / 2.0).max(0.0),
        TextAlign::End => (container_width - measured).max(0.0),
    };
    let draw_x = origin.x + align_dx;
    // The geometry index is only consumed by the caret + selection branches;
    // building it for every text node (the common no-caret label) wasted a
    // per-node pass. Build it lazily only when one of those is present.
    // W3.6/D4: this is now `lumen_text::TextGeometry` (relocated from the
    // former private `ShapedRunSegmentIndex`); the render draw path still
    // reshapes here -- the D4-R render-consume dedup is separate.
    let run_index = if text.caret.is_some() || text.selection.is_some() {
        shaped.as_ref().map(lumen_text::TextGeometry::from)
    } else {
        None
    };
    // Selection bands, shared by the highlight fill below and the
    // selected-glyph foreground over-paint further down. BiDi-correct:
    // [`lumen_text::TextGeometry::selection_bands`] returns one band per
    // line-portion of each maximal contiguous-level slice - matches HTML /
    // Qt / macOS selection visualisation. Each band carries its own
    // baseline, so a selection running across a line break paints on both
    // lines instead of collapsing onto the first one.
    let sel_bands: Vec<lumen_text::SelectionBand> = match (text.selection, &run_index) {
        (Some((s, e)), Some(idx)) if e > s => idx.selection_bands(s, e),
        _ => Vec::new(),
    };
    let band_y = |b: &lumen_text::SelectionBand| {
        let base = origin.y as f64 + b.baseline_y as f64;
        (base - size_px as f64 * 0.9, base + size_px as f64 * 0.15)
    };
    // Selection highlight paints first so glyphs sit on top. Styleable
    // via `selection-color` (default skin: `--lumen-selection`); the
    // single built-in fallback is the platform highlight blue
    // ([`DEFAULT_SELECTION_BG`]) - visible on any field color, unlike the
    // old text-fill-at-32%-alpha fallback which vanished on light fields.
    if !sel_bands.is_empty() {
        let sel = folded(
            text.selection_color.unwrap_or(DEFAULT_SELECTION_BG),
            opacity,
        );
        let brush = peniko_color(sel);
        for b in &sel_bands {
            let (y0, y1) = band_y(b);
            let x0 = draw_x as f64 + b.x0 as f64;
            let x1 = draw_x as f64 + b.x1 as f64;
            vello_scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                brush,
                None,
                &Rect::new(x0, y0, x1, y1),
            );
        }
    }
    if let Some(run) = &shaped {
        draw_glyph_run(
            vello_scene,
            run,
            size_px,
            draw_x,
            origin.y,
            peniko_color(fill),
        );
    }
    // Selected-glyph foreground (Qt `QPalette::HighlightedText` / Slint
    // `selection-foreground-color`): re-paint the glyphs that fall inside
    // each selection rect in the override color, clipping to the rect so
    // only the selected span inverts. Opt-in - the default translucent
    // highlight preserves unselected contrast, so most skins never set it.
    if let (Some(run), Some(fg)) = (&shaped, text.selection_foreground)
        && !sel_bands.is_empty()
    {
        let fg = folded(fg, opacity);
        let brush = peniko_color(fg);
        for b in &sel_bands {
            let (y0, y1) = band_y(b);
            let clip = Rect::new(
                draw_x as f64 + b.x0 as f64,
                y0,
                draw_x as f64 + b.x1 as f64,
                y1,
            );
            vello_scene.push_layer(
                Fill::NonZero,
                peniko::BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &clip,
            );
            draw_glyph_run(vello_scene, run, size_px, draw_x, origin.y, brush);
            vello_scene.pop_layer();
        }
    }
    // Caret position computed from the SAME TextGeometry - no extra
    // shape pass. `caret_xy` also yields the baseline offset of
    // the byte's line so multiline carets land on the right line.
    if let Some(byte_offset) = text.caret {
        let line_height = size_px as f64 * 1.2;
        let (caret_x, caret_y) =
            if byte_offset > 0 && text.text.as_bytes().get(byte_offset - 1) == Some(&b'\n') {
                // Caret sits at the start of a (possibly empty) line right
                // after a newline - newlines emit no glyph cluster, so
                // derive the line index from the text instead of the shape.
                let line_idx = text.text[..byte_offset.min(text.text.len())]
                    .matches('\n')
                    .count();
                (0.0, line_idx as f64 * line_height)
            } else if let Some(idx) = &run_index {
                let (x, y) = idx.caret_xy(byte_offset);
                (x as f64, y as f64)
            } else {
                // No shape (empty text or shaper missing): caret at origin.
                (0.0, 0.0)
            };
        let h = size_px as f64 * 0.9;
        let x0 = draw_x as f64 + caret_x;
        let y0 = origin.y as f64 + caret_y - h;
        // Caret width is a logical 2 px scaled to physical pixels (the
        // old fixed `+2.0` rendered a ~1 px sliver on hidpi and was easy
        // to lose against the field). Floor at 1 physical px so it never
        // sub-pixels away entirely.
        let x1 = x0 + (2.0 * dpr as f64).max(1.0);
        let y1 = origin.y as f64 + caret_y + size_px as f64 * 0.15;
        // Caret color: `caret-color` token, else the text fill (web
        // default). Alpha-folds the inherited opacity like the glyphs.
        let caret_col = folded(text.caret_color.unwrap_or(text.fill), opacity);
        let brush = peniko_color(caret_col);
        vello_scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            brush,
            None,
            &Rect::new(x0, y0, x1, y1),
        );
    }
}

/// Paint one shaped run's glyphs at `(draw_x, baseline_y)` in `brush`.
/// W5.6: one `draw_glyphs` call per segment so each font binds its own
/// glyph slice; pure-Latin runs degenerate to a single call (the W3.6
/// fast path). The per-font [`Blob`] is reused via [`font_blob`] so
/// vello's glyph-atlas cache (keyed on `Blob` identity) stays warm.
fn draw_glyph_run(
    vello_scene: &mut vello::Scene,
    run: &lumen_text::ShapedRun,
    size_px: f32,
    draw_x: f32,
    baseline_y: f32,
    brush: PenikoColor,
) {
    for seg in &run.segments {
        if seg.glyphs.is_empty() {
            continue;
        }
        let blob = font_blob(seg.font_id, &seg.font_data);
        let font = FontData::new(blob, seg.font_index);
        vello_scene
            .draw_glyphs(&font)
            .font_size(size_px)
            .brush(brush)
            .transform(Affine::translate((draw_x as f64, baseline_y as f64)))
            .draw(
                Fill::NonZero,
                seg.glyphs.iter().map(|g| vello::Glyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                }),
            );
    }
}

#[allow(dead_code)] // kept for downstream callers that still want a byte-prefix slice
fn safe_prefix(text: &str, byte_offset: usize) -> &str {
    if byte_offset >= text.len() {
        text
    } else if text.is_char_boundary(byte_offset) {
        &text[..byte_offset]
    } else {
        text
    }
}

/// Holder for a boxed text shaper kept as a non-send render-world resource.
///
/// Renderer system pulls this if present and draws [`ExtractedText`] via it.
/// Absent = text silently skipped (current behavior of the offscreen path
/// when constructed without a shaper).
pub struct WgpuTextShaper(pub Box<dyn lumen_text::TextShaper>);

/// Plugin: installs the offscreen [`WgpuRenderer`] into the render world and
/// registers the render-world system in [`RenderStage::Render`].
///
/// Optionally accepts a [`lumen_text::TextShaper`] via
/// [`WgpuRendererPlugin::with_text_shaper`]; without it, text draw commands
/// are skipped.
pub struct WgpuRendererPlugin {
    /// Initial offscreen target width.
    pub width: u32,
    /// Initial offscreen target height.
    pub height: u32,
    /// Optional text shaper; installed as a non-send render-world resource.
    pub text_shaper: Option<Box<dyn lumen_text::TextShaper>>,
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
    pub fn with_text_shaper<S: lumen_text::TextShaper + 'static>(mut self, shaper: S) -> Self {
        self.text_shaper = Some(Box::new(shaper));
        self
    }

    /// Attach an already-boxed text shaper (same effect as
    /// [`Self::with_text_shaper`]; avoids double-boxing when the caller
    /// already holds a `Box<dyn TextShaper>` - e.g. the one built for
    /// `WinitOptions`).
    pub fn with_boxed_text_shaper(mut self, shaper: Box<dyn lumen_text::TextShaper>) -> Self {
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
        app.render_world.insert_non_send_resource(renderer);
        // W2.4: install the SceneFragmentCache on the offscreen path so the cached emit helpers can be
        // shared between the on-screen winit path and the offscreen render system.
        app.render_world
            .insert_resource(SceneFragmentCache::default());
        if let Some(shaper) = self.text_shaper {
            app.render_world
                .insert_non_send_resource(WgpuTextShaper(shaper));
        }
        app.add_render_systems(RenderStage::Render, wgpu_render_system);
    }
}

/// Render-world system that drives the offscreen [`WgpuRenderer`] via the shared Node IR walker.
///
/// W2.2: the legacy `Extracted*`-flat-query path is replaced with a walk over the
/// [`lumen_core::node_ir::RetainedScene`]. Inside the walker (see [`crate::walker::walk_node`]) leaves
/// route through the cached emitters when the [`SceneFragmentCache`] resource is present (W2.4), giving the
/// offscreen path the same encoding-reuse the winit path already enjoyed.
///
/// Damage-driven partial repaint: the system calls [`crate::walker::diff_retained_scenes`] against the
/// previous frame's root and skips the entire encode + submit when the diff is empty (the visual tree is
/// unchanged), keeping the last-rendered target on screen. A non-empty diff re-encodes the whole scene -
/// bounding the encode to the damage rect is not pixel-safe while vello clears the whole target per call.
#[allow(clippy::too_many_arguments)]
fn wgpu_render_system(
    mut renderer: NonSendMut<WgpuRenderer>,
    mut cache: Option<ResMut<SceneFragmentCache>>,
    shaper: Option<NonSendMut<WgpuTextShaper>>,
    viewport: Res<Viewport>,
    retained: Res<lumen_core::node_ir::RetainedScene>,
    mut previous: ResMut<lumen_core::node_ir::PreviousScene>,
    mut damage: ResMut<FrameDamage>,
) {
    // Device pixel ratio: the walker scales every leaf (and clip) from logical to physical pixels
    // at emit time, so the target texture and the damage scissor must be sized in the same physical
    // space. Offscreen viewports default to `scale_factor == 1.0`, making this a no-op there.
    let dpr = viewport.scale_factor.max(0.01);
    let w = (viewport.size.x * dpr).max(1.0) as u32;
    let h = (viewport.size.y * dpr).max(1.0) as u32;
    renderer.resize(w, h);

    let viewport_rect = lumen_core::render_world::Rect {
        origin: glam::Vec2::ZERO,
        size: viewport.size,
    };
    damage.clear();
    crate::walker::diff_retained_scenes(
        previous.root.as_ref(),
        retained.root.as_ref(),
        viewport_rect,
        &mut damage,
    );

    // Partial-repaint gate. The retained Node-IR diff tells us whether the
    // visual tree actually changed this frame. When it did NOT (empty damage)
    // and a previous frame already rendered into the target, skip the whole
    // encode + submit - the offscreen texture still holds the pixel-identical
    // last frame. Mirrors Qt `QWidget::update()` collapsing to no backing-store
    // flush when the computed dirty region is empty, and GTK's damage-region
    // coalescing.
    //
    // When the tree DID change we re-encode the entire scene. A damage-bounded
    // scissor is deliberately NOT applied: `render_to_texture` clears the whole
    // target to `base_color` on every call, so clipping the encode to the
    // damage rect would blank every untouched pixel - not pixel-identical.
    // Pixel-safe partial *encode* needs a preserved backing store (deferred
    // slice; see `diff_retained_scenes` docs). `FrameDamage` is still populated
    // for consumers that only need the dirty-region *size*.
    let first_frame = previous.root.is_none();
    if first_frame || !damage.is_empty() {
        renderer.scene.reset();
        {
            let mut shaper_opt = shaper;
            let shaper_ref: Option<&mut dyn lumen_text::TextShaper> = shaper_opt
                .as_deref_mut()
                .map(|w| &mut *w.0 as &mut dyn lumen_text::TextShaper);
            let mut ctx = WalkContext::new_with_dpr(
                &mut renderer.scene,
                cache.as_deref_mut(),
                shaper_ref,
                dpr,
            );
            crate::walker::walk_retained_scene(&mut ctx, &retained);
        }

        let clear = viewport.clear;
        renderer.render_current(clear);
    }

    // Park the just-walked tree so the next frame's diff has something to compare against.
    previous.root = retained.root.clone();
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

fn peniko_color(c: LumenColor) -> PenikoColor {
    let [r, g, b, a] = c.to_rgba8();
    AlphaColor::<Srgb>::from_rgba8(r, g, b, a)
}

/// Folds an inherited `opacity` multiplier into a colour's alpha. A no-op
/// when `opacity >= 1.0`; otherwise multiplies `a` by the clamped opacity.
/// Centralises the alpha-fold idiom shared by every leaf emitter so no site
/// can forget the clamp.
pub(crate) fn folded(mut c: LumenColor, opacity: f32) -> LumenColor {
    if opacity < 1.0 {
        c.a *= opacity.clamp(0.0, 1.0);
    }
    c
}
