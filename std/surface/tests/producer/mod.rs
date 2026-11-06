//! The reference producer for Direct3D 12 (Windows) and Metal (macOS), in
//! Rust: what `protocol/producer/reference.c` is for Vulkan. The GPU suite
//! runs it in a process of its own by starting its own test binary again
//! with [`ENV`] set (see `the_producer` in `gpu.rs`).
//!
//! It does what every producer does: pick the GPU the endpoint names, make a
//! device, connect, allocate and export a ring, then render into each slot
//! the consumer frees and announce it once the GPU has finished. It draws
//! the same pattern as the C one: the frame cleared to the background, and
//! a square of [`SQUARE`] texture pixels at the last pointer position (the
//! middle until a pointer event arrives); each key press swaps the
//! background between blue and green.
//!
//! - Direct3D 12: each buffer is a committed resource on a default heap
//!   with `D3D12_HEAP_FLAG_SHARED`, `ALLOW_RENDER_TARGET` and
//!   `ALLOW_SIMULTANEOUS_ACCESS`, in `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB`,
//!   exported with `CreateSharedHandle`. The handles stay open until the
//!   consumer answers the ring, which copies them out of this process.
//! - Metal: each buffer is a `'BGRA'` IOSurface, drawn through a texture
//!   made with `newTextureWithDescriptor:iosurface:plane:` in
//!   `BGRA8Unorm_sRGB`, and sent as the port `IOSurfaceCreateMachPort`
//!   gives.
//!
//! Environment, as for the C producer: `LUMEN_SURFACE` (the endpoint),
//! `LSP_REF_CRASH_AFTER` (exit without a goodbye after this many frames).

use lumen_surface::protocol::endpoint::{AdapterHint, Endpoint};
use lumen_surface::protocol::producer::{FrameTimes, Producer, ProducerEvent, ProducerHello};
use lumen_surface::protocol::transport::RawHandle;
use lumen_surface::protocol::wire::{AdapterId, Api, BufferDesc, Buffers, Format, InputEvent};
use lumen_surface::wgpu;
use std::time::Duration;

/// Set in the environment of the process that runs the producer.
pub const ENV: &str = "LSP_TEST_PRODUCER";

/// The square's side, in texture pixels.
const SQUARE: u32 = 16;

#[cfg(windows)]
const API: Api = Api::D3d12;
#[cfg(target_os = "macos")]
const API: Api = Api::Metal;

#[cfg(windows)]
const BACKENDS: wgpu::Backends = wgpu::Backends::DX12;
#[cfg(target_os = "macos")]
const BACKENDS: wgpu::Backends = wgpu::Backends::METAL;

#[cfg(windows)]
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
#[cfg(target_os = "macos")]
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

fn log(line: impl std::fmt::Display) {
    eprintln!("lsp-producer: {line}");
}

/// The device's identity, as the handshake compares it.
#[cfg(windows)]
fn identify(device: &wgpu::Device, info: &wgpu::AdapterInfo) -> AdapterId {
    // SAFETY: the hal device is only read here.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }.expect("a D3D12 device");
    // SAFETY: a plain query on a live device.
    let luid = unsafe { hal.raw_device().GetAdapterLuid() };
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
    bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
    AdapterId {
        luid: bytes,
        vendor: info.vendor,
        device: info.device,
        ..AdapterId::default()
    }
}

/// The device's identity, as the handshake compares it.
#[cfg(target_os = "macos")]
fn identify(device: &wgpu::Device, info: &wgpu::AdapterInfo) -> AdapterId {
    use objc2_metal::MTLDevice;
    // SAFETY: the hal device is only read here.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.expect("a Metal device");
    AdapterId {
        registry_id: hal.raw_device().registryID(),
        vendor: info.vendor,
        device: info.device,
        ..AdapterId::default()
    }
}

/// Whether `id` is the GPU the endpoint names.
fn matches(hint: Option<&AdapterHint>, id: &AdapterId) -> bool {
    match hint {
        None => true,
        Some(AdapterHint::D3d12 { luid }) => *luid == id.luid,
        Some(AdapterHint::Metal { registry_id }) => *registry_id == id.registry_id,
        Some(AdapterHint::Vulkan { .. }) => false,
    }
}

/// A device on the GPU the endpoint names.
fn open_gpu(hint: Option<&AdapterHint>) -> (wgpu::Device, wgpu::Queue, AdapterId) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: BACKENDS,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    for adapter in pollster::block_on(instance.enumerate_adapters(BACKENDS)) {
        let info = adapter.get_info();
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("lsp-producer"),
                required_limits: adapter.limits(),
                ..Default::default()
            }))
        else {
            continue;
        };
        let id = identify(&device, &info);
        if matches(hint, &id) {
            log(format!("rendering on {} ({:?})", info.name, info.backend));
            return (device, queue, id);
        }
    }
    log("no adapter matches the endpoint");
    std::process::exit(2);
}

/// One exported image, as this process keeps it.
struct Slot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The IOSurface, which the texture also holds.
    #[cfg(target_os = "macos")]
    _surface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
}

/// A ring of exported images.
struct Ring {
    generation: u32,
    width: u32,
    height: u32,
    slots: Vec<Slot>,
}

/// The texture an exported image is drawn through.
fn slot_descriptor(width: u32, height: u32) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("lsp-producer slot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    }
}

/// Allocate and export `count` images; returns each slot, the handle to
/// offer, and the usage bits to state.
#[cfg(windows)]
fn export(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    count: u32,
) -> (Vec<Slot>, Vec<RawHandle>, u32) {
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_SAMPLE_DESC,
    };
    use windows::core::PCWSTR;
    // SAFETY: the hal device is only used through its raw device below.
    let raw = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
        .expect("a D3D12 device")
        .raw_device()
        .clone();
    let flags =
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS;
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(width),
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    };
    let mut slots = Vec::new();
    let mut handles = Vec::new();
    for _ in 0..count {
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: valid descriptions; the resource is owned from here on.
        unsafe {
            raw.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
        }
        .expect("CreateCommittedResource");
        let resource = resource.expect("a resource");
        // SAFETY: an unnamed NT handle to a shareable resource.
        let handle =
            unsafe { raw.CreateSharedHandle(&resource, None, GENERIC_ALL.0, PCWSTR::null()) }
                .expect("CreateSharedHandle");
        let descriptor = slot_descriptor(width, height);
        // SAFETY: the resource matches the descriptor.
        let hal = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                FORMAT,
                wgpu::TextureDimension::D2,
                descriptor.size,
                1,
                1,
            )
        };
        // SAFETY: the hal texture was made for this device.
        let texture =
            unsafe { device.create_texture_from_hal::<wgpu::hal::api::Dx12>(hal, &descriptor) };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        slots.push(Slot { texture, view });
        handles.push(handle.0 as RawHandle);
    }
    (slots, handles, flags.0 as u32)
}

/// Close the handles a ring was offered with, once the consumer answered.
#[cfg(windows)]
fn release_handles(handles: &[RawHandle]) {
    for h in handles {
        // SAFETY: handles `export` made and nothing else closes.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(*h));
        }
    }
}

/// Allocate and export `count` images; returns each slot, the port to
/// offer, and the usage bits to state.
#[cfg(target_os = "macos")]
fn export(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    count: u32,
) -> (Vec<Slot>, Vec<RawHandle>, u32) {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
    use objc2_io_surface::{
        IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
        kIOSurfaceWidth,
    };
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
        MTLTextureUsage,
    };
    // SAFETY: the hal device is only used through its raw device below.
    let raw = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
        .expect("a Metal device")
        .raw_device()
        .clone();
    let usage = MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead;
    let mut slots = Vec::new();
    let mut ports = Vec::new();
    for _ in 0..count {
        let w = CFNumber::new_i32(width as i32);
        let h = CFNumber::new_i32(height as i32);
        let bpe = CFNumber::new_i32(4);
        let pf = CFNumber::new_i32(i32::from_be_bytes(*b"BGRA"));
        // SAFETY: the keys are the framework's own constants.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let values: [&CFType; 4] = [&w, &h, &bpe, &pf];
        let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: a dictionary of IOSurface properties.
        let surface = unsafe { IOSurfaceRef::new(properties.as_ref()) }.expect("IOSurfaceCreate");
        // SAFETY: plain descriptor setup.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm_sRGB,
                width as usize,
                height as usize,
                false,
            )
        };
        descriptor.setUsage(usage);
        descriptor.setStorageMode(if cfg!(target_arch = "aarch64") {
            MTLStorageMode::Shared
        } else {
            MTLStorageMode::Managed
        });
        let texture = raw
            .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
            .expect("newTextureWithDescriptor:iosurface:plane:");
        let wgpu_descriptor = slot_descriptor(width, height);
        // SAFETY: the texture matches the descriptor.
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                texture,
                FORMAT,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
            )
        };
        // SAFETY: the hal texture was made for this device.
        let texture = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(hal, &wgpu_descriptor)
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        ports.push(surface.create_mach_port());
        slots.push(Slot {
            texture,
            view,
            _surface: surface,
        });
    }
    (slots, ports, usage.0 as u32)
}

/// Release the ports a ring was offered with, once the consumer answered.
#[cfg(target_os = "macos")]
fn release_handles(ports: &[RawHandle]) {
    for port in ports {
        // SAFETY: send rights IOSurfaceCreateMachPort made for this task,
        // released once; the consumer holds its own.
        unsafe {
            mach2::mach_port::mach_port_deallocate(mach2::traps::mach_task_self(), *port);
        }
    }
}

/// Allocate, export and offer ring `generation`.
fn make_ring(
    device: &wgpu::Device,
    producer: &mut Producer,
    generation: u32,
    width: u32,
    height: u32,
    count: u32,
) -> Option<Ring> {
    let (slots, handles, usage) = export(device, width, height, count);
    for (i, slot) in slots.iter().enumerate() {
        #[cfg(target_os = "macos")]
        log(format!(
            "buffer {generation}.{i} surface {}",
            slot._surface.id()
        ));
        #[cfg(windows)]
        log(format!("buffer {generation}.{i} handle {:?}", handles[i]));
        let _ = slot;
    }
    let offered = producer.offer_buffers(
        Buffers {
            generation,
            width,
            height,
            format: Format::for_api(API),
            usage,
            flags: 0,
            buffers: vec![BufferDesc::default(); slots.len()],
        },
        &handles,
    );
    // The consumer holds its own copies now.
    release_handles(&handles);
    match offered {
        Ok(()) => {
            log(format!(
                "ring {generation} is {width}x{height}, {count} buffers"
            ));
            Some(Ring {
                generation,
                width,
                height,
                slots,
            })
        }
        Err(e) => {
            log(format!("the consumer refused the ring: {e}"));
            None
        }
    }
}

/// Draws the square: one full-target triangle in orange, cut down by the
/// scissor rectangle.
fn square_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lsp-producer square"),
        source: wgpu::ShaderSource::Wgsl(
            "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                 let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
                 return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
             }
             @fragment fn fs() -> @location(0) vec4<f32> {
                 return vec4<f32>(1.0, 0.5, 0.0, 1.0);
             }"
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("lsp-producer square"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Render the pattern into `slot` and wait for the GPU to finish it.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    ring: &Ring,
    slot: usize,
    background: wgpu::Color,
    at: (f32, f32),
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lsp-producer frame"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lsp-producer frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &ring.slots[slot].view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(background),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let x = (at.0 as i64 - i64::from(SQUARE / 2)).max(0) as u32;
        let y = (at.1 as i64 - i64::from(SQUARE / 2)).max(0) as u32;
        if x + SQUARE <= ring.width && y + SQUARE <= ring.height {
            pass.set_pipeline(pipeline);
            pass.set_scissor_rect(x, y, SQUARE, SQUARE);
            pass.draw(0..3, 0..1);
        }
    }
    let index = queue.submit([encoder.finish()]);
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: Some(index),
        timeout: None,
    });
    let _ = &ring.slots[slot].texture;
}

/// Run the producer until the consumer ends the session; returns the exit
/// code.
pub fn run() -> i32 {
    let Ok(endpoint) = std::env::var("LUMEN_SURFACE") else {
        log("LUMEN_SURFACE is not set");
        return 2;
    };
    let crash_after: u32 = std::env::var("LSP_REF_CRASH_AFTER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let hint = endpoint.parse::<Endpoint>().ok().and_then(|e| e.adapter);
    let (device, queue, adapter) = open_gpu(hint.as_ref());
    let mut producer = match Producer::connect(
        &endpoint,
        &ProducerHello {
            api: API,
            adapter,
            engine: "lsp-producer 1".into(),
            caps: 0,
        },
    ) {
        Ok(p) => p,
        Err(e) => {
            log(format!("connect failed: {e}"));
            return 3;
        }
    };
    let welcome = producer.welcome().clone();
    log(format!("welcomed at {}x{}", welcome.width, welcome.height));
    let count = u32::from(
        welcome
            .ring_pref
            .clamp(welcome.ring_min, welcome.ring_max)
            .min(4),
    );
    let pipeline = square_pipeline(&device);
    let mut generation = 1;
    let Some(mut ring) = make_ring(
        &device,
        &mut producer,
        generation,
        welcome.width,
        welcome.height,
        count,
    ) else {
        return 4;
    };
    // The ring before the current one, until the consumer retires it.
    let mut previous: Option<Ring> = None;
    let blue = wgpu::Color {
        r: 0.0,
        g: 0.0,
        b: 0.8,
        a: 1.0,
    };
    let green = wgpu::Color {
        r: 0.0,
        g: 0.8,
        b: 0.0,
        a: 1.0,
    };
    let mut green_background = false;
    let mut at = (ring.width as f32 / 2.0, ring.height as f32 / 2.0);
    let mut frames = 0u32;
    loop {
        while let Ok(Some(event)) = producer.poll_event() {
            match event {
                ProducerEvent::Input { event, .. } => match event {
                    InputEvent::PointerMove(p) | InputEvent::PointerDown(p) => at = (p.x, p.y),
                    InputEvent::Key {
                        pressed: true,
                        repeat: false,
                        ..
                    } => green_background = !green_background,
                    _ => {}
                },
                ProducerEvent::Resize { width, height, .. } => {
                    generation += 1;
                    if let Some(next) =
                        make_ring(&device, &mut producer, generation, width, height, count)
                    {
                        previous = Some(std::mem::replace(&mut ring, next));
                        at = (width as f32 / 2.0, height as f32 / 2.0);
                    }
                }
                ProducerEvent::Retired(g) => {
                    if previous.as_ref().is_some_and(|r| r.generation == g) {
                        previous = None;
                    }
                }
                ProducerEvent::Reset => {
                    generation += 1;
                    previous = None;
                    match make_ring(
                        &device,
                        &mut producer,
                        generation,
                        welcome.width,
                        welcome.height,
                        count,
                    ) {
                        Some(next) => ring = next,
                        None => return 4,
                    }
                }
                ProducerEvent::Closed { .. } => {
                    log("the consumer ended the session");
                    return 0;
                }
                _ => {}
            }
        }
        let slot = match producer.next_slot(Duration::from_millis(50)) {
            Ok(Some(slot)) => slot,
            Ok(None) => continue,
            Err(e) => {
                log(e);
                return 0;
            }
        };
        if slot.generation != ring.generation || slot.slot as usize >= ring.slots.len() {
            producer.abandon(slot);
            continue;
        }
        render(
            &device,
            &queue,
            &pipeline,
            &ring,
            slot.slot as usize,
            if green_background { green } else { blue },
            at,
        );
        if producer
            .frame_ready(slot, &frames.to_le_bytes(), FrameTimes::default())
            .is_err()
        {
            return 0;
        }
        frames += 1;
        if crash_after > 0 && frames >= crash_after {
            log(format!("exiting without a goodbye after {frames} frames"));
            // At once, as a crash would: nothing is dropped or said.
            std::process::exit(7);
        }
    }
}
