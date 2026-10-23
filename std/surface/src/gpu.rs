//! The GPU half: the renderer's device as the sessions see it, the backend
//! they import into and present through, and the per-API importers.
//!
//! The render backend publishes its device ([`GpuContext`]) and texture
//! table ([`GpuTextures`]) in the render world; a render-world system copies
//! them here, where session threads read them. An import runs on the
//! session's thread, against a clone of the device, so a slow driver never
//! stalls a frame.

use crate::session::Backend;
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
    match gpu.adapter {
        Some((Api::Vulkan, id)) => Some((
            AdapterHint::Vulkan {
                device_uuid: id.device_uuid,
                driver_uuid: id.driver_uuid,
            },
            gpu.context.info.name.clone(),
        )),
        _ => None,
    }
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

    fn import(&self, buffers: &Buffers, fds: Vec<Handle>) -> Result<Vec<wgpu::Texture>, String> {
        let gpu = current().ok_or("the renderer has no GPU device")?;
        import(&gpu.context, buffers, fds)
    }

    fn present(&self, texture: wgpu::Texture, stamp: u64, released: Box<dyn FnOnce() + Send>) {
        match current() {
            Some(gpu) => gpu
                .textures
                .present(self.texture, texture, stamp, Some(released)),
            None => released(),
        }
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
    let _ = context;
    None
}

/// Import a ring into the device.
fn import(
    context: &GpuContext,
    buffers: &Buffers,
    fds: Vec<Handle>,
) -> Result<Vec<wgpu::Texture>, String> {
    #[cfg(target_os = "linux")]
    if context.info.backend == wgpu::Backend::Vulkan {
        return vulkan::import(context, buffers, fds);
    }
    let _ = (context, buffers, fds);
    Err("this build imports shared frames only from Vulkan".into())
}

#[cfg(target_os = "linux")]
mod vulkan {
    //! Vulkan import: each buffer is an image the producer exported as an
    //! `OPAQUE_FD` memory handle. The importer makes an image with the
    //! producer's parameters, binds the imported memory to it, and wraps it
    //! as a texture on the renderer's device.

    use super::*;
    use ash::vk;
    use lumen_surface_protocol::wire::{BUFFER_DEDICATED, Format};
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
    ) -> Result<Vec<wgpu::Texture>, String> {
        let Format::Rgba8Srgb = buffers.format;
        let usage = vk::ImageUsageFlags::from_raw(buffers.usage);
        if !usage.contains(REQUIRED_USAGE) {
            return Err(
                "the buffers were not made for sampling (VK_IMAGE_USAGE_SAMPLED_BIT)".into(),
            );
        }
        if buffers.width == 0 || buffers.height == 0 {
            return Err("the buffers have no area".into());
        }
        let max = context.device.limits().max_texture_dimension_2d;
        if buffers.width > max || buffers.height > max {
            return Err(format!("the buffers are larger than {max} pixels a side"));
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
                    &wgpu::TextureDescriptor {
                        label: Some("lumen surface frame"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                )
            };
            textures.push(texture);
        }
        Ok(textures)
    }
}

/// Shared ownership of the backend for session threads.
pub fn backend(texture: TextureId) -> Arc<GpuBackend> {
    Arc::new(GpuBackend { texture })
}
