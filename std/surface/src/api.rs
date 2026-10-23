//! What a Rust app reaches: the components on a surface element, the
//! [`SurfaceEvent`] message, the [`Surfaces`] resource, and the in-process
//! [`LoopbackProducer`].

use crate::gpu;
use crate::session::{FrameInfo, Shared, SurfaceStateKind};
use bevy_ecs::prelude::*;
use lumen_module::gpu::{GpuContext, wgpu};
use lumen_module::lumen_paint::TextureId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// One registered surface: its session and the texture id its element
/// paints.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) shared: Arc<Shared>,
    pub(crate) texture: TextureId,
}

/// Every surface element, by its id.
static REGISTRY: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

pub(crate) fn registry() -> MutexGuard<'static, Option<HashMap<String, Entry>>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// The surface element with `id`.
pub(crate) fn find(id: &str) -> Option<Entry> {
    registry().as_ref().and_then(|r| r.get(id).cloned())
}

/// The session of a `<surface>` element, on the element's entity.
#[derive(Component, Clone)]
pub struct SurfaceElement {
    pub(crate) shared: Arc<Shared>,
    pub(crate) texture: TextureId,
}

impl SurfaceElement {
    /// The endpoint string a producer connects with.
    pub fn endpoint(&self) -> String {
        self.shared.endpoint()
    }

    /// The texture id the element paints.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// Where the surface stands.
    pub fn state(&self) -> SurfaceStateKind {
        self.shared.state()
    }

    /// The frame on screen, if any.
    pub fn frame(&self) -> Option<FrameInfo> {
        self.shared.frame()
    }

    /// The OS identity (device, inode) of each shared buffer of the ring in
    /// use; the producer's handles to the same buffers have the same one.
    pub fn buffer_identities(&self) -> Vec<(u64, u64)> {
        self.shared.buffer_identities()
    }
}

/// Where a surface stands, on the element's entity: `waiting`, `live`,
/// `stalled`, `lost` or `unsupported`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceState(pub SurfaceStateKind);

/// The frame a surface shows, on the element's entity, updated as frames
/// arrive.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceFrame(pub FrameInfo);

impl SurfaceFrame {
    /// The producer's bytes about the frame on screen, such as the camera it
    /// was drawn with, in whatever layout the producer and the app agree on.
    pub fn meta(&self) -> &[u8] {
        &self.0.meta
    }
}

/// What happened to a surface, for a Rust app. Script and C handlers hear
/// the same through the element's `surface*` DOM events.
#[derive(Message, Clone, Debug, PartialEq)]
pub enum SurfaceEvent {
    /// A producer finished the handshake.
    Connected {
        /// The element.
        entity: Entity,
        /// The producer's engine name.
        engine: String,
        /// Its process id.
        pid: u32,
    },
    /// The size asked of the producer changed.
    Resized {
        /// The element.
        entity: Entity,
        /// Width in texture pixels.
        width: u32,
        /// Height in texture pixels.
        height: u32,
    },
    /// No frame came within the stall timeout.
    Stalled {
        /// The element.
        entity: Entity,
        /// How long it waited, in milliseconds.
        waited_ms: u64,
    },
    /// Frames arrive again.
    Resumed {
        /// The element.
        entity: Entity,
    },
    /// The session ended; the last frame stays.
    Disconnected {
        /// The element.
        entity: Entity,
        /// `exit`, `crash`, `hung`, `rejected`, `device-lost` or `closed`.
        reason: String,
        /// Words for a person.
        detail: String,
    },
    /// A handshake, an import or a draw failed.
    Error {
        /// The element.
        entity: Entity,
        /// `rejected`, `import`, `unsupported` or `effect`.
        reason: String,
        /// Words for a person.
        detail: String,
    },
    /// Bytes from the producer's application.
    Message {
        /// The element.
        entity: Entity,
        /// The bytes.
        bytes: Vec<u8>,
    },
}

/// The app's surfaces, by element id: the endpoint a producer needs, and the
/// calls a script makes through the `surface` namespace.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct Surfaces;

impl Surfaces {
    /// The endpoint string for the surface with `id`: hand it to the
    /// producer, by the `LUMEN_SURFACE` environment variable by convention.
    pub fn endpoint(&self, id: &str) -> Option<String> {
        find(id).map(|e| e.shared.endpoint())
    }

    /// Where the surface with `id` stands.
    pub fn state(&self, id: &str) -> Option<SurfaceStateKind> {
        find(id).map(|e| e.shared.state())
    }

    /// The frame the surface with `id` shows.
    pub fn frame(&self, id: &str) -> Option<FrameInfo> {
        find(id).and_then(|e| e.shared.frame())
    }

    /// Ask a paced producer for a frame now: after an edit the app made, so
    /// a still scene shows it.
    pub fn request_frame(&self, id: &str) {
        if let Some(e) = find(id) {
            e.shared.request_frame();
        }
    }

    /// Send bytes to the producer's application. Returns whether they went.
    pub fn send(&self, id: &str, bytes: &[u8]) -> bool {
        find(id).is_some_and(|e| e.shared.send_user(bytes))
    }

    /// End the session with the producer; the surface keeps listening.
    pub fn disconnect(&self, id: &str) {
        if let Some(e) = find(id) {
            e.shared.disconnect();
        }
    }

    /// A producer for the surface with `id` in this process: an engine
    /// linked into the app renders on the renderer's own device and presents
    /// its textures directly, with no socket and no import.
    pub fn loopback(&self, id: &str) -> Option<LoopbackProducer> {
        find(id).map(|e| LoopbackProducer {
            shared: e.shared,
            texture: e.texture,
            next: 1,
        })
    }
}

/// An in-process producer for one surface. Make textures on
/// [`Self::context`]'s device and [`present`](Self::present) them; the
/// surface shows each from the next frame on, the same way it shows frames
/// from another process.
pub struct LoopbackProducer {
    shared: Arc<Shared>,
    texture: TextureId,
    next: u64,
}

impl LoopbackProducer {
    /// The renderer's device, once the GPU backend has drawn a frame. `None`
    /// under the CPU backend.
    pub fn context(&self) -> Option<GpuContext> {
        gpu::current().map(|g| g.context)
    }

    /// The size, in texture pixels, the surface wants frames at.
    pub fn wanted_size(&self) -> (u32, u32) {
        let w = self.shared.wanted();
        (w.width, w.height)
    }

    /// Show `texture`: an `Rgba8UnormSrgb` texture (or `Rgba8Unorm` with an
    /// sRGB view format) made on [`Self::context`]'s device with
    /// `TEXTURE_BINDING` usage. `meta` is what [`SurfaceFrame::meta`] reads
    /// for it. Returns the frame number, or `None` when the renderer has no
    /// GPU.
    pub fn present(&mut self, texture: wgpu::Texture, meta: &[u8]) -> Option<u64> {
        let gpu = gpu::current()?;
        let frame = self.next;
        self.next += 1;
        let size = (texture.width(), texture.height());
        gpu.textures.present(self.texture, texture, frame, None);
        self.shared.record_local_frame(FrameInfo {
            frame,
            generation: 1,
            size,
            meta: meta.to_vec(),
            input_seq: 0,
            received_at: Some(std::time::Instant::now()),
        });
        Some(frame)
    }
}
