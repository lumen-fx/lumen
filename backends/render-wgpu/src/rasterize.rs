//! Rasterizing a drawing on demand, on the device the frames render on.
//!
//! A drawing is painted into a scene of its own, rendered into a texture of
//! the requested size, and copied back to the CPU. The frame is not touched:
//! the scene, the texture and the vello renderer are this rasterizer's, and
//! only the device and queue are shared with the frame.

use std::sync::Mutex;

use lumen_paint::kurbo::Affine;
use lumen_paint::{Painter, Rasterize, Recording};
use vello::wgpu;
use vello::{AaConfig, RenderParams, Scene};

use crate::gpu::read_texture;
use crate::{VelloPainter, vello_options};

/// What [`crate::WgpuRenderer`] publishes as the render world's
/// [`lumen_paint::Rasterizer`].
///
/// Its vello renderer is made on the first request, not with the frame's:
/// an app that never reads a drawing back never compiles a second set of
/// vello's pipelines. The first request pays that once.
pub struct GpuRasterizer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
    vello: Mutex<Option<vello::Renderer>>,
}

impl GpuRasterizer {
    pub(crate) fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
        Self {
            device,
            queue,
            adapter_info,
            vello: Mutex::new(None),
        }
    }
}

impl Rasterize for GpuRasterizer {
    fn rasterize(
        &self,
        drawing: &Recording,
        transform: Affine,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }
        let max = self.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return Err(format!(
                "a {width}x{height} image is larger than the {max} pixels a side this GPU renders"
            ));
        }
        // Painted as a fragment, so every pixel image in the drawing is
        // encoded with its own pixels rather than through the frame's
        // resident textures, which this scene cannot reach.
        let mut sink = VelloPainter::new();
        sink.set_target_size(width, height);
        sink.begin_fragment();
        drawing.replay(&mut sink, transform);
        let fragment = sink
            .end_fragment()
            .ok_or_else(|| "the GPU painter recorded nothing".to_string())?;
        let scene = fragment.downcast::<Scene>().ok_or_else(|| {
            "the GPU painter recorded into something other than a scene".to_string()
        })?;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lumen rasterize target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let params = RenderParams {
            base_color: vello::peniko::Color::TRANSPARENT,
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        {
            let mut vello = self.vello.lock().unwrap_or_else(|e| e.into_inner());
            if vello.is_none() {
                *vello = Some(
                    vello::Renderer::new(&self.device, vello_options(&self.adapter_info))
                        .map_err(|e| format!("vello renderer init: {e:?}"))?,
                );
            }
            vello
                .as_mut()
                .expect("made above")
                .render_to_texture(&self.device, &self.queue, scene, &view, &params)
                .map_err(|e| format!("vello render: {e:?}"))?;
        }
        // Vello writes straight alpha, which is what a rasterizer hands out.
        read_texture(&self.device, &self.queue, &texture, width, height)
    }
}
