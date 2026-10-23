//! The device this backend renders with, and the textures a frame shows by
//! [`TextureId`], published for plugins that make or import textures.
//!
//! [`GpuContext`] and [`GpuTextures`] are render-world resources the renderer
//! publishes when it attaches (see [`lumen_core::traits::Renderer::publish`]).
//! A plugin that has pixels the CPU never sees (a frame another process
//! renders, a decoded video, an in-process 3D view) makes or imports a
//! `wgpu::Texture` on [`GpuContext::device`], hands it to
//! [`GpuTextures::present`] under a [`TextureId`], and paints that id with
//! [`lumen_paint::Painter::draw_texture`]. The renderer shows the texture
//! beneath the frame in place, with no copy.
//!
//! [`GpuTextures`] is a handle: clones share one table, and every method
//! takes `&self`, so a plugin keeps a clone on a thread of its own and
//! presents from there. The renderer reads the table when it composes a
//! frame.

use bevy_ecs::prelude::Resource;
use lumen_paint::TextureId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use vello::wgpu;

/// The device every frame renders on, as the renderer brought it up.
///
/// Published into the render world when the renderer attaches; absent under a
/// renderer with no GPU. A texture made for [`GpuTextures`] has to be made on
/// this device.
#[derive(Resource, Clone, Debug)]
pub struct GpuContext {
    /// The device frames render on.
    pub device: wgpu::Device,
    /// Its queue.
    pub queue: wgpu::Queue,
    /// The adapter the device came from.
    pub adapter: wgpu::Adapter,
    /// What the adapter reports about itself: name, vendor and device ids,
    /// the graphics API (`backend`), the driver.
    pub info: wgpu::AdapterInfo,
    /// Counts device bring-ups. A texture made on an earlier generation's
    /// device is not valid on this one.
    pub generation: u64,
}

/// Called once the GPU has finished every read of a texture
/// [`GpuTextures`] showed, so its owner may write it again. Runs on whichever
/// thread notices: a wgpu callback thread, the render thread, or the thread
/// that replaced or removed the texture.
pub type Sampled = Box<dyn FnOnce() + Send>;

/// One texture as presented. The view holds the texture.
struct Shown {
    view: wgpu::TextureView,
    stamp: u64,
    sampled: Option<Sampled>,
}

impl Shown {
    fn release(mut self) {
        if let Some(sampled) = self.sampled.take() {
            sampled();
        }
    }
}

/// One id's state: what frames show now, and what the next frame shows
/// instead.
#[derive(Default)]
struct Slot {
    current: Option<Shown>,
    pending: Option<Shown>,
}

/// The table behind every clone of a [`GpuTextures`].
#[derive(Default)]
struct Table {
    slots: HashMap<TextureId, Slot>,
    /// Textures no frame shows any more that the GPU may still be reading:
    /// released once the work submitted so far finishes.
    retired: Vec<Shown>,
    /// The device generation the textures were made on.
    generation: u64,
}

/// The textures a frame may show by [`TextureId`], shared between the
/// renderer and whoever presents them.
///
/// Presenting replaces what an id shows from the next frame on; the texture
/// it replaces is released (its [`Sampled`] callback runs) once no frame
/// reads it any more: at once when no frame ever showed it, otherwise once
/// the GPU has finished the frames that did.
#[derive(Resource, Clone, Default)]
pub struct GpuTextures(Arc<Mutex<Table>>);

/// A texture as the renderer draws it this frame.
pub(crate) struct Resolved {
    pub(crate) view: wgpu::TextureView,
    pub(crate) stamp: u64,
}

impl GpuTextures {
    fn lock(&self) -> MutexGuard<'_, Table> {
        // A panic while the table was held leaves it consistent: every
        // method writes it in one step.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Show `texture` for `id` from the next frame on.
    ///
    /// The texture must be made on [`GpuContext::device`] at the current
    /// generation, with `TEXTURE_BINDING` usage, in a format whose sRGB view
    /// the renderer samples: `Rgba8UnormSrgb`, or `Rgba8Unorm` /
    /// `Bgra8Unorm` with the matching sRGB view format declared. `stamp`
    /// names its content; a frame is put up again only when an id the frame
    /// shows has a stamp it has not shown. `sampled` runs once no frame reads
    /// the texture any more.
    pub fn present(
        &self,
        id: TextureId,
        texture: wgpu::Texture,
        stamp: u64,
        sampled: Option<Sampled>,
    ) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("lumen presented texture"),
            format: Some(texture.format().add_srgb_suffix()),
            ..Default::default()
        });
        let skipped = {
            let mut table = self.lock();
            table.slots.entry(id).or_default().pending.replace(Shown {
                view,
                stamp,
                sampled,
            })
        };
        // Replaced before any frame showed it: nothing reads it.
        if let Some(skipped) = skipped {
            skipped.release();
        }
    }

    /// Stop showing anything for `id`. What it showed is released once no
    /// frame reads it.
    pub fn remove(&self, id: TextureId) {
        let skipped = {
            let mut table = self.lock();
            let Some(slot) = table.slots.remove(&id) else {
                return;
            };
            if let Some(current) = slot.current {
                table.retired.push(current);
            }
            slot.pending
        };
        if let Some(skipped) = skipped {
            skipped.release();
        }
    }

    /// Whether `id` has a texture to show, presented or about to be.
    pub fn contains(&self, id: TextureId) -> bool {
        self.lock()
            .slots
            .get(&id)
            .is_some_and(|s| s.current.is_some() || s.pending.is_some())
    }

    /// The stamp `id` shows next frame, if it shows anything.
    pub fn stamp(&self, id: TextureId) -> Option<u64> {
        let table = self.lock();
        let slot = table.slots.get(&id)?;
        slot.pending
            .as_ref()
            .or(slot.current.as_ref())
            .map(|s| s.stamp)
    }

    /// Whether any id has a texture presented that no frame has shown yet.
    pub fn changed(&self) -> bool {
        self.lock().slots.values().any(|s| s.pending.is_some())
    }

    /// The texture each of `ids` shows this frame, making a presented one the
    /// shown one. What it replaces waits in the retired list for the work
    /// submitted so far to finish.
    pub(crate) fn resolve(&self, ids: &[TextureId]) -> HashMap<TextureId, Resolved> {
        let mut table = self.lock();
        let mut out = HashMap::with_capacity(ids.len());
        let Table { slots, retired, .. } = &mut *table;
        for id in ids {
            let Some(slot) = slots.get_mut(id) else {
                continue;
            };
            if let Some(next) = slot.pending.take()
                && let Some(old) = slot.current.replace(next)
            {
                retired.push(old);
            }
            if let Some(current) = slot.current.as_ref() {
                out.insert(
                    *id,
                    Resolved {
                        view: current.view.clone(),
                        stamp: current.stamp,
                    },
                );
            }
        }
        out
    }

    /// Release every retired texture once the work submitted to `queue` so
    /// far has finished.
    pub(crate) fn release_retired(&self, queue: &wgpu::Queue) {
        let retired = std::mem::take(&mut self.lock().retired);
        if retired.is_empty() {
            return;
        }
        queue.on_submitted_work_done(move || {
            for shown in retired {
                shown.release();
            }
        });
    }

    /// Forget every texture: they were made on a device generation before
    /// `generation`. Each is released at once, since the device that could
    /// still read them is gone.
    pub(crate) fn reset(&self, generation: u64) {
        let gone: Vec<Shown> = {
            let mut table = self.lock();
            if table.generation == generation {
                return;
            }
            table.generation = generation;
            let mut gone: Vec<Shown> = std::mem::take(&mut table.retired);
            for (_, slot) in table.slots.drain() {
                gone.extend(slot.current);
                gone.extend(slot.pending);
            }
            gone
        };
        for shown in gone {
            shown.release();
        }
    }
}

impl std::fmt::Debug for GpuTextures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.lock();
        f.debug_struct("GpuTextures")
            .field("ids", &table.slots.len())
            .field("retired", &table.retired.len())
            .field("generation", &table.generation)
            .finish()
    }
}
