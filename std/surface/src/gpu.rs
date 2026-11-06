//! The GPU half: the renderer's device as the sessions see it, the backend
//! they import into and present through, and the per-API importers.
//!
//! The render backend publishes its device ([`GpuContext`]) and texture
//! table ([`GpuTextures`]) in the render world; a render-world system copies
//! them here, where session threads read them. An import runs on a thread
//! of its own, against a clone of the device, so a slow driver never stalls
//! a frame, and a device busy with a frame never silences a session.

use crate::session::{Backend, Imported};
use lumen_module::gpu::{GpuContext, GpuTextures, wgpu};
use lumen_module::lumen_paint::TextureId;
use lumen_surface_protocol::endpoint::AdapterHint;
use lumen_surface_protocol::transport::Handle;
use lumen_surface_protocol::wire::{AdapterId, Api, Buffers};
use std::sync::{Arc, Mutex, MutexGuard};

/// The device the renderer draws with, as sessions use it.
#[derive(Clone)]
pub struct Gpu {
    /// The device.
    pub context: GpuContext,
    /// The renderer's texture table.
    pub textures: GpuTextures,
    /// The device's identity, for the handshake; `None` when its API has no
    /// importer in this build.
    pub adapter: Option<(Api, AdapterId)>,
}

/// The renderer's device, once a frame has rendered on the GPU backend.
static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

fn slot() -> MutexGuard<'static, Option<Gpu>> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

/// The renderer's device, if the GPU backend has published one.
pub fn current() -> Option<Gpu> {
    slot().clone()
}

/// Take the device the renderer published; returns whether it is new.
pub fn publish(context: &GpuContext, textures: &GpuTextures) -> bool {
    let mut slot = slot();
    if slot
        .as_ref()
        .is_some_and(|g| g.context.generation == context.generation)
    {
        return false;
    }
    *slot = Some(Gpu {
        adapter: identify(context),
        context: context.clone(),
        textures: textures.clone(),
    });
    true
}

/// The device's identity and the hint a producer picks its GPU by.
pub fn adapter_hint(gpu: &Gpu) -> Option<(AdapterHint, String)> {
    let (api, id) = gpu.adapter?;
    let hint = match api {
        Api::Vulkan => AdapterHint::Vulkan {
            device_uuid: id.device_uuid,
            driver_uuid: id.driver_uuid,
        },
        Api::D3d12 => AdapterHint::D3d12 { luid: id.luid },
        Api::Metal => AdapterHint::Metal {
            registry_id: id.registry_id,
        },
    };
    Some((hint, gpu.context.info.name.clone()))
}

/// What a session imports into and presents through: the renderer's device
/// and texture table, and the texture id its element draws.
pub struct GpuBackend {
    /// The id the element paints and frames are presented under.
    pub texture: TextureId,
}

impl Backend for GpuBackend {
    type Texture = wgpu::Texture;

    fn device(&self) -> Option<(Api, AdapterId)> {
        current().and_then(|g| g.adapter)
    }

    fn generation(&self) -> u64 {
        current().map_or(0, |g| g.context.generation)
    }

    fn import(
        &self,
        buffers: &Buffers,
        fds: Vec<Handle>,
    ) -> Result<Imported<wgpu::Texture>, String> {
        let gpu = current().ok_or("the renderer has no GPU device")?;
        import(&gpu.context, buffers, fds)
    }

    fn present(&self, texture: wgpu::Texture, stamp: u64, released: Box<dyn FnOnce() + Send>) {
        let Some(gpu) = current() else {
            released();
            return;
        };
        // The frame's image comes over from the producer's queue family
        // before anything samples it, and goes back once nothing does.
        #[cfg(target_os = "linux")]
        let released = if gpu.context.info.backend == wgpu::Backend::Vulkan {
            vulkan::acquire(&gpu.context, &texture);
            vulkan::release_when_done(gpu.context.clone(), texture.clone(), released)
        } else {
            released
        };
        gpu.textures
            .present(self.texture, texture, stamp, Some(released));
    }
}

/// Make sure frames shown under `id` stop showing.
pub fn forget(id: TextureId) {
    if let Some(gpu) = current() {
        gpu.textures.remove(id);
    }
}

/// The device's API and identity, where this build has an importer for it.
fn identify(context: &GpuContext) -> Option<(Api, AdapterId)> {
    #[cfg(target_os = "linux")]
    if context.info.backend == wgpu::Backend::Vulkan {
        return vulkan::identify(context).map(|id| (Api::Vulkan, id));
    }
    #[cfg(windows)]
    if context.info.backend == wgpu::Backend::Dx12 {
        return dx12::identify(context).map(|id| (Api::D3d12, id));
    }
    #[cfg(target_os = "macos")]
    if context.info.backend == wgpu::Backend::Metal {
        return metal::identify(context).map(|id| (Api::Metal, id));
    }
    let _ = context;
    None
}

/// Import a ring into the device.
fn import(
    context: &GpuContext,
    buffers: &Buffers,
    handles: Vec<Handle>,
) -> Result<Imported<wgpu::Texture>, String> {
    #[cfg(target_os = "linux")]
    if context.info.backend == wgpu::Backend::Vulkan {
        return vulkan::import(context, buffers, handles);
    }
    #[cfg(windows)]
    if context.info.backend == wgpu::Backend::Dx12 {
        return dx12::import(context, buffers, handles);
    }
    #[cfg(target_os = "macos")]
    if context.info.backend == wgpu::Backend::Metal {
        return metal::import(context, buffers, handles);
    }
    let _ = (context, buffers, handles);
    Err(format!(
        "this build imports shared frames from Vulkan on Linux, Direct3D 12 on Windows and \
         Metal on macOS, not from {:?}",
        context.info.backend
    ))
}

/// The checks every importer makes before touching the GPU: the ring's
/// format is the one its API's rings have, and its size fits the device.
fn check_ring(context: &GpuContext, buffers: &Buffers, api: Api) -> Result<(), String> {
    let want = lumen_surface_protocol::wire::Format::for_api(api);
    if buffers.format != want {
        return Err(format!(
            "a {api:?} ring is {want:?}, not {:?}",
            buffers.format
        ));
    }
    if buffers.width == 0 || buffers.height == 0 {
        return Err("the buffers have no area".into());
    }
    let max = context.device.limits().max_texture_dimension_2d;
    if buffers.width > max || buffers.height > max {
        return Err(format!("the buffers are larger than {max} pixels a side"));
    }
    Ok(())
}

/// The texture an imported image becomes: sampled only, one mip, one layer.
fn frame_descriptor(
    buffers: &Buffers,
    format: wgpu::TextureFormat,
) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("lumen surface frame"),
        size: wgpu::Extent3d {
            width: buffers.width,
            height: buffers.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    }
}

#[cfg(windows)]
mod dx12 {
    //! Direct3D 12 import: each buffer is a committed resource the producer
    //! made on a shared heap and exported as an NT handle, which the
    //! transport has copied into this process. `OpenSharedHandle` gives a
    //! resource that aliases the producer's allocation, wrapped as a texture
    //! on the renderer's device.
    //!
    //! The resources are made with `ALLOW_SIMULTANEOUS_ACCESS`: each decays
    //! to `COMMON` after every command list that used it and is promoted
    //! implicitly on first use, so two devices that each track state on
    //! their own agree on it at every hand-over, with no barrier between
    //! the processes. The frame messages order the two GPUs' work: each side
    //! finishes its work on an image before handing it over.

    use super::*;
    use std::os::windows::io::AsRawHandle;
    use wgpu::hal::api::Dx12;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        ID3D12Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;

    pub(super) fn identify(context: &GpuContext) -> Option<AdapterId> {
        // SAFETY: the hal device is only read here.
        let hal = unsafe { context.device.as_hal::<Dx12>() }?;
        // SAFETY: a plain query on a live device.
        let luid = unsafe { hal.raw_device().GetAdapterLuid() };
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
        bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
        Some(AdapterId {
            luid: bytes,
            vendor: context.info.vendor,
            device: context.info.device,
            ..AdapterId::default()
        })
    }

    pub(super) fn import(
        context: &GpuContext,
        buffers: &Buffers,
        handles: Vec<Handle>,
    ) -> Result<Imported<wgpu::Texture>, String> {
        check_ring(context, buffers, Api::D3d12)?;
        // SAFETY: the hal device outlives the calls below.
        let hal = unsafe { context.device.as_hal::<Dx12>() }
            .ok_or("the renderer's device is not Direct3D 12")?;
        let device = hal.raw_device().clone();
        drop(hal);
        let mut textures = Vec::with_capacity(handles.len());
        for handle in handles {
            let mut resource: Option<ID3D12Resource> = None;
            // SAFETY: an NT handle this process owns; the resource keeps
            // the allocation alive on its own, so the handle closes after.
            unsafe { device.OpenSharedHandle(HANDLE(handle.as_raw_handle()), &mut resource) }
                .map_err(|e| format!("OpenSharedHandle: {e}"))?;
            drop(handle);
            let resource = resource.ok_or("OpenSharedHandle gave no resource")?;
            // SAFETY: a plain query on a live resource.
            let desc = unsafe { resource.GetDesc() };
            if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
                || desc.Width != u64::from(buffers.width)
                || desc.Height != buffers.height
                || desc.DepthOrArraySize != 1
                || desc.MipLevels != 1
                || desc.SampleDesc.Count != 1
                || desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            {
                return Err("a buffer is not a single 2D image of the ring's size and \
                     DXGI_FORMAT_R8G8B8A8_UNORM_SRGB"
                    .into());
            }
            if !desc
                .Flags
                .contains(D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS)
            {
                return Err(
                    "a buffer was not made with D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS"
                        .into(),
                );
            }
            let descriptor = frame_descriptor(buffers, wgpu::TextureFormat::Rgba8UnormSrgb);
            // SAFETY: the resource matches the descriptor, checked above.
            let hal_texture = unsafe {
                wgpu::hal::dx12::Device::texture_from_raw(
                    resource,
                    descriptor.format,
                    descriptor.dimension,
                    descriptor.size,
                    1,
                    1,
                )
            };
            // SAFETY: the hal texture was made for this device.
            textures.push(unsafe {
                context
                    .device
                    .create_texture_from_hal::<Dx12>(hal_texture, &descriptor)
            });
        }
        Ok(Imported::now(textures))
    }
}

#[cfg(target_os = "macos")]
mod metal {
    //! Metal import: each buffer is an IOSurface whose Mach port the
    //! transport received. `IOSurfaceLookupFromMachPort` finds the surface,
    //! and a texture made on the renderer's device with
    //! `newTextureWithDescriptor:iosurface:plane:` shares its memory with
    //! the producer's. The frame messages order the two GPUs' work: each
    //! side's command buffers on an image have completed before it hands
    //! the image over.

    use super::*;
    use objc2_io_surface::IOSurfaceRef;
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
        MTLTextureUsage,
    };
    use wgpu::hal::api::Metal;

    /// The IOSurface pixel format of a ring's buffers: `'BGRA'`.
    const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

    pub(super) fn identify(context: &GpuContext) -> Option<AdapterId> {
        // SAFETY: the hal device is only read here.
        let hal = unsafe { context.device.as_hal::<Metal>() }?;
        Some(AdapterId {
            registry_id: hal.raw_device().registryID(),
            vendor: context.info.vendor,
            device: context.info.device,
            ..AdapterId::default()
        })
    }

    pub(super) fn import(
        context: &GpuContext,
        buffers: &Buffers,
        handles: Vec<Handle>,
    ) -> Result<Imported<wgpu::Texture>, String> {
        check_ring(context, buffers, Api::Metal)?;
        // SAFETY: the hal device outlives the calls below.
        let hal = unsafe { context.device.as_hal::<Metal>() }
            .ok_or("the renderer's device is not Metal")?;
        let device = hal.raw_device().clone();
        drop(hal);
        let (width, height) = (buffers.width as usize, buffers.height as usize);
        let mut textures = Vec::with_capacity(handles.len());
        for port in handles {
            let surface = IOSurfaceRef::lookup_from_mach_port(port.as_raw())
                .ok_or("IOSurfaceLookupFromMachPort found no surface for a buffer's port")?;
            if surface.width() != width
                || surface.height() != height
                || surface.pixel_format() != PIXEL_FORMAT_BGRA
            {
                return Err("a buffer is not a 'BGRA' IOSurface of the ring's size".into());
            }
            // SAFETY: plain descriptor setup.
            let descriptor = unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::BGRA8Unorm_sRGB,
                    width,
                    height,
                    false,
                )
            };
            descriptor.setUsage(MTLTextureUsage::ShaderRead);
            // An IOSurface texture cannot be private: shared on Apple
            // silicon's unified memory, managed on a Mac with its own VRAM.
            descriptor.setStorageMode(if cfg!(target_arch = "aarch64") {
                MTLStorageMode::Shared
            } else {
                MTLStorageMode::Managed
            });
            let texture = device
                .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
                .ok_or("newTextureWithDescriptor:iosurface:plane: failed")?;
            let wgpu_descriptor = frame_descriptor(buffers, wgpu::TextureFormat::Bgra8UnormSrgb);
            // SAFETY: the texture matches the descriptor: one 2D level and
            // layer of the ring's size in BGRA8Unorm_sRGB.
            let hal_texture = unsafe {
                wgpu::hal::metal::Device::texture_from_raw(
                    texture,
                    wgpu_descriptor.format,
                    MTLTextureType::Type2D,
                    1,
                    1,
                    wgpu::hal::CopyExtent {
                        width: buffers.width,
                        height: buffers.height,
                        depth: 1,
                    },
                )
            };
            // SAFETY: the hal texture was made for this device.
            textures.push(unsafe {
                context
                    .device
                    .create_texture_from_hal::<Metal>(hal_texture, &wgpu_descriptor)
            });
        }
        Ok(Imported::now(textures))
    }
}

#[cfg(target_os = "linux")]
mod vulkan {
    //! Vulkan import: each buffer is an image the producer exported as an
    //! `OPAQUE_FD` memory handle. The importer makes an image with the
    //! producer's parameters, binds the imported memory to it, and wraps it
    //! as a texture on the renderer's device.

    use super::*;
    use ash::vk;
    use lumen_surface_protocol::wire::BUFFER_DEDICATED;
    use std::os::fd::IntoRawFd;
    use wgpu::hal::api::Vulkan;

    /// The usage every buffer must have been made with: the consumer
    /// samples it.
    const REQUIRED_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::SAMPLED;

    pub(super) fn identify(context: &GpuContext) -> Option<AdapterId> {
        // SAFETY: the hal device is only read here.
        let hal = unsafe { context.device.as_hal::<Vulkan>() }?;
        let instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        // SAFETY: a valid physical device of this instance.
        unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
        let (vendor, device) = (
            properties.properties.vendor_id,
            properties.properties.device_id,
        );
        Some(AdapterId {
            device_uuid: id.device_uuid,
            driver_uuid: id.driver_uuid,
            luid: [0; 8],
            registry_id: 0,
            vendor,
            device,
        })
    }

    pub(super) fn import(
        context: &GpuContext,
        buffers: &Buffers,
        fds: Vec<Handle>,
    ) -> Result<Imported<wgpu::Texture>, String> {
        check_ring(context, buffers, Api::Vulkan)?;
        let usage = vk::ImageUsageFlags::from_raw(buffers.usage);
        if !usage.contains(REQUIRED_USAGE) {
            return Err(
                "the buffers were not made for sampling (VK_IMAGE_USAGE_SAMPLED_BIT)".into(),
            );
        }
        // SAFETY: the hal device outlives the calls below, which only use
        // it through the guard.
        let hal = unsafe { context.device.as_hal::<Vulkan>() }
            .ok_or("the renderer's device is not Vulkan")?;
        if !hal
            .enabled_device_extensions()
            .contains(&ash::khr::external_memory_fd::NAME)
        {
            return Err("the renderer's device has no VK_KHR_external_memory_fd".into());
        }
        let device = hal.raw_device().clone();
        let mut textures = Vec::with_capacity(buffers.buffers.len());
        for (desc, fd) in buffers.buffers.iter().zip(fds) {
            let mut external = vk::ExternalMemoryImageCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_SRGB)
                .extent(vk::Extent3D {
                    width: buffers.width,
                    height: buffers.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external);
            // SAFETY: a valid create info on a live device.
            let image = unsafe { device.create_image(&info, None) }
                .map_err(|e| format!("vkCreateImage: {e}"))?;
            // SAFETY: the image was just made on this device.
            let requirements = unsafe { device.get_image_memory_requirements(image) };
            let fits = requirements.size <= desc.alloc_size
                && requirements.memory_type_bits & (1u32 << desc.memory_type.min(31)) != 0;
            if !fits {
                // SAFETY: the image is unused and owned here.
                unsafe { device.destroy_image(image, None) };
                return Err(
                    "the buffer's allocation does not fit an image of the producer's parameters \
                     here: the producer renders on another device or driver"
                        .into(),
                );
            }
            let raw_fd = fd.into_raw_fd();
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .fd(raw_fd);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let mut allocate = vk::MemoryAllocateInfo::default()
                .allocation_size(desc.alloc_size)
                .memory_type_index(desc.memory_type)
                .push_next(&mut import);
            if buffers.flags & BUFFER_DEDICATED != 0 {
                allocate = allocate.push_next(&mut dedicated);
            }
            // SAFETY: a valid allocate info; on success Vulkan owns the fd.
            let memory = match unsafe { device.allocate_memory(&allocate, None) } {
                Ok(memory) => memory,
                Err(e) => {
                    // SAFETY: the import failed, so the fd is still ours, and
                    // the image is unused.
                    unsafe {
                        libc::close(raw_fd);
                        device.destroy_image(image, None);
                    }
                    return Err(format!("vkAllocateMemory (import): {e}"));
                }
            };
            // SAFETY: memory and image are fresh and compatible.
            if let Err(e) =
                unsafe { device.bind_image_memory(image, memory, u64::from(desc.offset)) }
            {
                // SAFETY: both are unused and owned here.
                unsafe {
                    device.free_memory(memory, None);
                    device.destroy_image(image, None);
                }
                return Err(format!("vkBindImageMemory: {e}"));
            }
            let size = wgpu::Extent3d {
                width: buffers.width,
                height: buffers.height,
                depth_or_array_layers: 1,
            };
            let hal_desc = wgpu::hal::TextureDescriptor {
                label: Some("lumen surface frame"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            };
            // SAFETY: the image matches the descriptor; with no drop callback
            // and dedicated memory, the hal texture owns both from here.
            let hal_texture = unsafe {
                hal.texture_from_raw(
                    image,
                    &hal_desc,
                    None,
                    wgpu::hal::vulkan::TextureMemory::Dedicated(memory),
                )
            };
            // SAFETY: the hal texture was made on this device.
            let texture = unsafe {
                context.device.create_texture_from_hal::<Vulkan>(
                    hal_texture,
                    &frame_descriptor(buffers, wgpu::TextureFormat::Rgba8UnormSrgb),
                )
            };
            textures.push(texture);
        }
        let ready = prime(context, &textures);
        Ok(Imported { textures, ready })
    }

    /// The images' first use on this side: the layout they are sampled in,
    /// out of `UNDEFINED`. wgpu tracks a texture made from a raw image as
    /// holding nothing, and would make this transition on the first frame
    /// that samples it, after the producer had drawn into it, which may
    /// discard what it drew. Submitted here instead; the returned check says
    /// when the GPU has finished it, and the producer is told the ring is
    /// accepted, and draws into it, only then. The GPU runs the transition
    /// after whatever the renderer queued before it, which on a software
    /// rasterizer under load can take a while; the import thread polls the
    /// check rather than blocking on it, so it can stop when the session
    /// ends.
    fn prime(context: &GpuContext, textures: &[wgpu::Texture]) -> Box<dyn FnMut() -> bool + Send> {
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen surface import"),
            });
        encoder.transition_resources(
            std::iter::empty(),
            textures.iter().map(|texture| wgpu::TextureTransition {
                texture,
                selector: None,
                state: wgpu::TextureUses::RESOURCE,
            }),
        );
        context.queue.submit([encoder.finish()]);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        context.queue.on_submitted_work_done(move || {
            flag.store(true, std::sync::atomic::Ordering::Release);
        });
        let device = context.device.clone();
        Box::new(move || {
            if !done.load(std::sync::atomic::Ordering::Acquire) {
                // Runs the callbacks of finished work without waiting.
                let _ = device.poll(wgpu::PollType::Poll);
            }
            done.load(std::sync::atomic::Ordering::Acquire)
        })
    }

    /// Block until the GPU has finished `index`, or a hung device has had
    /// long enough.
    fn wait(context: &GpuContext, index: wgpu::SubmissionIndex) {
        let _ = context.device.poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(std::time::Duration::from_secs(5)),
        });
    }

    /// Which way an image's ownership moves.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Transfer {
        /// From the producer (`VK_QUEUE_FAMILY_EXTERNAL`) to the renderer's
        /// queue family, before sampling.
        Acquire,
        /// From the renderer's queue family back to the producer, after
        /// sampling.
        Release,
    }

    /// Record and submit the ownership transfer of `texture`'s image. A
    /// transfer never changes the layout: both sides name
    /// `SHADER_READ_ONLY_OPTIMAL`, the layout an image has between the
    /// processes, as the specification requires.
    fn transfer(
        context: &GpuContext,
        texture: &wgpu::Texture,
        way: Transfer,
    ) -> Option<wgpu::SubmissionIndex> {
        // SAFETY: the hal device and texture are only read here.
        let (device, family) = {
            let hal = unsafe { context.device.as_hal::<Vulkan>() }?;
            (hal.raw_device().clone(), hal.queue_family_index())
        };
        // SAFETY: the image stays alive with `texture`, which outlives the
        // submission (it is waited for, or held by the frame it belongs to).
        let image = unsafe { texture.as_hal::<Vulkan>().map(|t| t.raw_handle()) }?;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image(image)
            .subresource_range(range);
        let (barrier, from, to) = match way {
            Transfer::Acquire => (
                barrier
                    .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .dst_queue_family_index(family)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ),
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
            ),
            Transfer::Release => (
                barrier
                    .src_queue_family_index(family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL),
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            ),
        };
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen surface ownership"),
            });
        // SAFETY: the barrier is recorded into the encoder's open command
        // buffer on the device it was made on; wgpu tracks the texture as
        // sampled already (see `prime`), so nothing it records disagrees.
        let recorded = unsafe {
            encoder.as_hal_mut::<Vulkan, _, _>(|hal| {
                hal.map(|hal| {
                    device.cmd_pipeline_barrier(
                        hal.raw_handle(),
                        from,
                        to,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[barrier],
                    );
                })
            })
        };
        recorded?;
        Some(context.queue.submit([encoder.finish()]))
    }

    /// Take `texture`'s image from the producer. Submitted before the frame
    /// goes into the texture table, so every frame that samples it is
    /// submitted after.
    pub(super) fn acquire(context: &GpuContext, texture: &wgpu::Texture) {
        transfer(context, texture, Transfer::Acquire);
    }

    /// A release callback for the texture table that first gives the image
    /// back to the producer: the release transfer is submitted and waited
    /// for, then `released` tells the producer. The table runs its
    /// callbacks from a device poll, where nothing may submit, so the work
    /// goes to a thread of its own.
    pub(super) fn release_when_done(
        context: GpuContext,
        texture: wgpu::Texture,
        released: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(move || {
            let job = Release {
                context,
                texture,
                released,
            };
            if let Err(std::sync::mpsc::SendError(job)) = releaser().send(job) {
                (job.released)();
            }
        })
    }

    /// One image to hand back.
    struct Release {
        context: GpuContext,
        texture: wgpu::Texture,
        released: Box<dyn FnOnce() + Send>,
    }

    /// The thread that hands images back, started on first use.
    fn releaser() -> std::sync::mpsc::Sender<Release> {
        static SENDER: Mutex<Option<std::sync::mpsc::Sender<Release>>> = Mutex::new(None);
        let mut sender = SENDER.lock().unwrap_or_else(|e| e.into_inner());
        sender
            .get_or_insert_with(|| {
                let (tx, rx) = std::sync::mpsc::channel::<Release>();
                // Without the thread the receiver is gone: every send fails
                // and each image is released at once.
                let _ = std::thread::Builder::new()
                    .name("lumen-surface release".into())
                    .spawn(move || {
                        for job in rx {
                            // A device that went away took the image with it.
                            let live = current()
                                .is_some_and(|g| g.context.generation == job.context.generation);
                            if live
                                && let Some(index) =
                                    transfer(&job.context, &job.texture, Transfer::Release)
                            {
                                wait(&job.context, index);
                            }
                            drop(job.texture);
                            (job.released)();
                        }
                    });
                tx
            })
            .clone()
    }
}

/// Shared ownership of the backend for session threads.
pub fn backend(texture: TextureId) -> Arc<GpuBackend> {
    Arc::new(GpuBackend { texture })
}
