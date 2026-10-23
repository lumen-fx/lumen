//! Putting backend textures beneath the frame.
//!
//! A frame that shows backend textures (see [`crate::texture`]) leaves a
//! hole where each one goes. Composing draws each texture at its rect, opaque
//! and unblended, then draws the frame over all of them with its own alpha:
//! the frame covers everything but its holes, so clips, rounded corners and
//! whatever painted over a texture show as the frame painted them, and the
//! texture costs one sampled quad and no copy.
//!
//! Compose replaces the blit onto a window's swap chain, and writes an
//! offscreen target's composed image for readback. A frame that shows no
//! backend texture never comes here.

use crate::sink::Placement;
use std::collections::HashMap;
use vello::wgpu;
use vello::wgpu::util::DeviceExt;

/// The shader: one quad per draw, from six vertices and an instance rect.
const SHADER: &str = r#"
struct Instance {
    @location(0) dst: vec4<f32>,
    @location(1) uv: vec4<f32>,
};

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    var out: Out;
    let xy = mix(inst.dst.xy, inst.dst.zw, c);
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = mix(inst.uv.xy, inst.uv.zw, c);
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

@fragment
fn fs_texture(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(tex, smp, in.uv).rgb, 1.0);
}

@fragment
fn fs_frame(in: Out) -> @location(0) vec4<f32> {
    return textureSample(tex, smp, in.uv);
}
"#;

/// One quad: where it lands in normalized device coordinates, and the part
/// of its texture it samples.
#[repr(C)]
#[derive(Clone, Copy)]
struct Instance {
    dst: [f32; 4],
    uv: [f32; 4],
}

impl Instance {
    fn bytes(instances: &[Instance]) -> Vec<u8> {
        let mut out = Vec::with_capacity(std::mem::size_of_val(instances));
        for i in instances {
            for v in i.dst.iter().chain(&i.uv) {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }
}

/// The two pipelines for one target format: textures replace what is under
/// them, the frame blends over them.
struct Pipelines {
    texture: wgpu::RenderPipeline,
    frame: wgpu::RenderPipeline,
}

/// Where a compose pass writes: a view in `format`, `size` pixels.
pub(crate) struct Output<'a> {
    pub(crate) view: &'a wgpu::TextureView,
    pub(crate) format: wgpu::TextureFormat,
    pub(crate) size: (u32, u32),
}

/// One texture to put beneath the frame, resolved to a view.
pub(crate) struct Draw<'a> {
    pub(crate) placement: &'a Placement,
    pub(crate) view: &'a wgpu::TextureView,
}

/// The compose pass and what it keeps between frames.
pub(crate) struct Composer {
    shader: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<wgpu::TextureFormat, Pipelines>,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
}

impl Composer {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lumen compose"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumen compose"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumen compose"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = |filter: wgpu::FilterMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("lumen compose"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        Self {
            shader,
            layout,
            pipeline_layout,
            pipelines: HashMap::new(),
            linear: sampler(wgpu::FilterMode::Linear),
            nearest: sampler(wgpu::FilterMode::Nearest),
        }
    }

    fn pipelines(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &Pipelines {
        let (shader, layout) = (&self.shader, &self.pipeline_layout);
        self.pipelines.entry(format).or_insert_with(|| {
            let make = |entry: &str, blend: Option<wgpu::BlendState>| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("lumen compose"),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<Instance>() as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &wgpu::vertex_attr_array![
                                0 => Float32x4,
                                1 => Float32x4,
                            ],
                        }],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            };
            // The frame's pixels carry straight alpha, as vello writes them.
            let over = wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
            };
            Pipelines {
                texture: make("fs_texture", None),
                frame: make("fs_frame", Some(over)),
            }
        })
    }

    fn bind(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        nearest: bool,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumen compose"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(if nearest {
                        &self.nearest
                    } else {
                        &self.linear
                    }),
                },
            ],
        })
    }

    /// Encode the pass: `draws` beneath `frame` (the frame texture's sRGB
    /// view, the size of `target`), into `target`.
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: Output<'_>,
        frame: &wgpu::TextureView,
        draws: &[Draw<'_>],
    ) {
        let Output {
            view: target,
            format,
            size,
        } = target;
        let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
        // Pixels to normalized device coordinates: y points up there.
        let ndc = |x: f64, y: f64| [(x / w * 2.0 - 1.0) as f32, (1.0 - y / h * 2.0) as f32];
        let mut instances: Vec<Instance> = draws
            .iter()
            .map(|d| {
                let r = d.placement.rect;
                let [x0, y0] = ndc(r.x0, r.y0);
                let [x1, y1] = ndc(r.x1, r.y1);
                Instance {
                    dst: [x0, y0, x1, y1],
                    uv: [0.0, 0.0, 1.0, 1.0],
                }
            })
            .collect();
        instances.push(Instance {
            dst: [-1.0, 1.0, 1.0, -1.0],
            uv: [0.0, 0.0, 1.0, 1.0],
        });
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lumen compose quads"),
            contents: &Instance::bytes(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let texture_groups: Vec<wgpu::BindGroup> = draws
            .iter()
            .map(|d| self.bind(device, d.view, d.placement.nearest))
            .collect();
        // The frame maps one texel to one pixel.
        let frame_group = self.bind(device, frame, true);
        let pipelines = self.pipelines(device, format);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumen compose"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.set_pipeline(&pipelines.texture);
        for (i, group) in texture_groups.iter().enumerate() {
            pass.set_bind_group(0, group, &[]);
            let i = i as u32;
            pass.draw(0..6, i..i + 1);
        }
        pass.set_pipeline(&pipelines.frame);
        pass.set_bind_group(0, &frame_group, &[]);
        let last = draws.len() as u32;
        pass.draw(0..6, last..last + 1);
    }
}

/// An offscreen image a composed frame is written to and read back from:
/// `Rgba8Unorm` bytes written through an sRGB view, so a readback gets the
/// same encoding the frame texture holds.
pub(crate) struct ComposedTexture {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    pub(crate) size: (u32, u32),
}

impl ComposedTexture {
    pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    pub(crate) fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lumen composed frame"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[Self::FORMAT],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("lumen composed frame (sRGB write)"),
            format: Some(Self::FORMAT),
            ..Default::default()
        });
        Self {
            texture,
            view,
            size,
        }
    }
}
