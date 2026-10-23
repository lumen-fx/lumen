//! The messages and how they are laid out on the wire.
//!
//! Every message is a little-endian frame: a `u32` length (the bytes after
//! the length field), a `u16` type, a `u16` flag set, then the body. Bodies
//! are fixed layouts followed by variable parts, each a `u32` length and the
//! bytes. A frame is at most [`MAX_FRAME`] bytes.
//!
//! The specification beside this crate (`SPEC.md`) is the reference; this
//! module is its implementation, and the tests hold the two to the same
//! layouts.

use std::fmt;

/// The protocol's major version. A peer on another major is refused.
pub const MAJOR: u16 = 1;
/// The protocol's minor version. Minors add messages and capability bits.
pub const MINOR: u16 = 0;

/// The largest frame, header included.
pub const MAX_FRAME: usize = 64 * 1024;
/// The most meta bytes a frame may carry.
pub const MAX_META: usize = 4 * 1024;
/// The most bytes a [`Message::User`] may carry.
pub const MAX_USER: usize = 60 * 1024;
/// The most buffers in one ring.
pub const MAX_RING: usize = 4;
/// The bytes before a body: length, type, flags.
pub const HEADER: usize = 8;

/// Flag: OS handles travel with this frame.
pub const FLAG_HANDLES: u16 = 1;
/// Flag: a receiver that does not know the type skips the frame.
pub const FLAG_MAY_IGNORE: u16 = 2;

/// The graphics API a producer renders with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Api {
    /// Vulkan.
    Vulkan = 1,
    /// Direct3D 12.
    D3d12 = 2,
    /// Metal.
    Metal = 3,
}

impl Api {
    fn from_u16(n: u16) -> Result<Self, WireError> {
        Ok(match n {
            1 => Api::Vulkan,
            2 => Api::D3d12,
            3 => Api::Metal,
            other => return Err(WireError::Value("api", u64::from(other))),
        })
    }
}

/// A buffer's pixel format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Format {
    /// 8-bit RGBA holding sRGB-encoded colour: `VK_FORMAT_R8G8B8A8_SRGB`,
    /// `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB`.
    Rgba8Srgb = 1,
}

impl Format {
    /// The format's bit in [`Welcome::formats`].
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }

    fn from_u32(n: u32) -> Result<Self, WireError> {
        match n {
            1 => Ok(Format::Rgba8Srgb),
            other => Err(WireError::Value("format", u64::from(other))),
        }
    }
}

/// How the producer paces its frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Pacing {
    /// One frame per [`Message::FrameRequest`].
    #[default]
    Paced = 0,
    /// At the producer's own rate; the consumer shows the newest.
    Free = 1,
}

impl Pacing {
    fn from_u16(n: u16) -> Result<Self, WireError> {
        match n {
            0 => Ok(Pacing::Paced),
            1 => Ok(Pacing::Free),
            other => Err(WireError::Value("pacing", u64::from(other))),
        }
    }
}

/// Why a consumer refused a producer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum RejectReason {
    /// The token did not match.
    Token = 1,
    /// The major version differs.
    Version = 2,
    /// The producer renders with another graphics API.
    Api = 3,
    /// The producer renders on another adapter or driver.
    Adapter = 4,
    /// The channel is not one the consumer offers.
    Channel = 5,
    /// The consumer has no GPU to import into.
    Unsupported = 6,
    /// A producer is already connected on the channel.
    Busy = 7,
}

impl RejectReason {
    fn from_u16(n: u16) -> Result<Self, WireError> {
        Ok(match n {
            1 => RejectReason::Token,
            2 => RejectReason::Version,
            3 => RejectReason::Api,
            4 => RejectReason::Adapter,
            5 => RejectReason::Channel,
            6 => RejectReason::Unsupported,
            7 => RejectReason::Busy,
            other => return Err(WireError::Value("reject reason", u64::from(other))),
        })
    }

    /// A short name: `token`, `version`, `api`, `adapter`, `channel`,
    /// `unsupported`, `busy`.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::Token => "token",
            RejectReason::Version => "version",
            RejectReason::Api => "api",
            RejectReason::Adapter => "adapter",
            RejectReason::Channel => "channel",
            RejectReason::Unsupported => "unsupported",
            RejectReason::Busy => "busy",
        }
    }
}

/// Why a session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum ByeReason {
    /// The side is going away normally.
    Exit = 1,
    /// The side lost its GPU device.
    DeviceLost = 2,
    /// The other side broke the protocol.
    Protocol = 3,
    /// The consumer's element went away.
    Closed = 4,
}

impl ByeReason {
    fn from_u16(n: u16) -> Result<Self, WireError> {
        Ok(match n {
            1 => ByeReason::Exit,
            2 => ByeReason::DeviceLost,
            3 => ByeReason::Protocol,
            4 => ByeReason::Closed,
            other => return Err(WireError::Value("bye reason", u64::from(other))),
        })
    }

    /// A short name: `exit`, `device-lost`, `protocol`, `closed`.
    pub fn as_str(self) -> &'static str {
        match self {
            ByeReason::Exit => "exit",
            ByeReason::DeviceLost => "device-lost",
            ByeReason::Protocol => "protocol",
            ByeReason::Closed => "closed",
        }
    }
}

/// Which physical GPU and driver a side renders on. Fields the API does not
/// have are zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AdapterId {
    /// Vulkan `deviceUUID`.
    pub device_uuid: [u8; 16],
    /// Vulkan `driverUUID`.
    pub driver_uuid: [u8; 16],
    /// Direct3D adapter LUID.
    pub luid: [u8; 8],
    /// Metal `registryID`.
    pub registry_id: u64,
    /// PCI vendor id.
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
}

impl AdapterId {
    /// Whether both name the same device and driver: every identity field
    /// the API fills agrees. PCI ids are not compared; two cards of one model
    /// share them.
    pub fn same_device(&self, other: &AdapterId) -> bool {
        self.device_uuid == other.device_uuid
            && self.driver_uuid == other.driver_uuid
            && self.luid == other.luid
            && self.registry_id == other.registry_id
    }
}

/// `Hello`: the producer introduces itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Its protocol major version.
    pub major: u16,
    /// Its protocol minor version.
    pub minor: u16,
    /// The API it renders with.
    pub api: Api,
    /// Its process id.
    pub pid: u32,
    /// Capability bits; none are defined in version 1.
    pub caps: u32,
    /// The token from the endpoint string.
    pub token: [u8; 16],
    /// The GPU it renders on.
    pub adapter: AdapterId,
    /// The channel it asks for.
    pub channel: String,
    /// Engine name and version, for diagnostics.
    pub engine: String,
}

/// `Welcome`: the consumer accepts the producer and states the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    /// Session id, unique for the consumer's lifetime.
    pub session: u64,
    /// The API the consumer imports with.
    pub api: Api,
    /// How the producer paces frames.
    pub pacing: Pacing,
    /// Fewest buffers in a ring.
    pub ring_min: u8,
    /// Most buffers in a ring.
    pub ring_max: u8,
    /// Preferred buffers in a ring.
    pub ring_pref: u8,
    /// Width the consumer wants frames at, in texture pixels.
    pub width: u32,
    /// Height the consumer wants frames at.
    pub height: u32,
    /// Device pixels per logical pixel where the frame shows.
    pub scale: f32,
    /// [`Format`] bits the consumer accepts.
    pub formats: u32,
    /// Capability bits; none are defined in version 1.
    pub caps: u32,
}

/// `Reject`: the consumer refuses the producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reject {
    /// Why.
    pub reason: RejectReason,
    /// The API the consumer imports with.
    pub api: Api,
    /// The consumer's GPU, so a producer can say which one to start on.
    pub adapter: AdapterId,
    /// Words for a person.
    pub detail: String,
}

/// One buffer of a ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferDesc {
    /// Allocation size in bytes.
    pub alloc_size: u64,
    /// The memory type index the exporter allocated from (Vulkan).
    pub memory_type: u32,
    /// Offset of the image in its allocation.
    pub offset: u32,
    /// Bytes per row for a linear layout; 0 for an optimal one.
    pub row_pitch: u32,
    /// DRM format modifier, for a dma-buf export; 0 otherwise.
    pub modifier: u64,
    /// Linux: the index of this buffer's descriptor among those the frame
    /// carries. Windows: the producer's handle value. macOS: the index of
    /// its port.
    pub handle: u64,
}

/// `Buffers`: the producer's ring for one generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffers {
    /// The ring's generation, from 1, one more for each new ring.
    pub generation: u32,
    /// Width in texture pixels.
    pub width: u32,
    /// Height in texture pixels.
    pub height: u32,
    /// Pixel format.
    pub format: Format,
    /// Usage bits the images were made with, in the API's own terms (Vulkan
    /// `VkImageUsageFlags`); the importer makes its images with the same.
    pub usage: u32,
    /// [`BUFFER_DEDICATED`] and later bits.
    pub flags: u32,
    /// The buffers, one per slot.
    pub buffers: Vec<BufferDesc>,
}

/// [`Buffers::flags`]: each buffer is a dedicated allocation.
pub const BUFFER_DEDICATED: u32 = 1;

/// `FrameReady`: a finished frame sits in a slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameReady {
    /// Frame number, increasing.
    pub frame: u64,
    /// The newest [`Message::Input`] sequence number the frame reflects.
    pub input_seq: u64,
    /// When the frame's simulation ran, monotonic nanoseconds.
    pub t_sim: u64,
    /// When its GPU work was submitted.
    pub t_submit: u64,
    /// When its GPU work finished.
    pub t_gpu_done: u64,
    /// The ring generation of the slot.
    pub generation: u32,
    /// The slot.
    pub slot: u32,
    /// Producer-defined bytes about the frame, at most [`MAX_META`].
    pub meta: Vec<u8>,
}

/// Pointer readings in an input message.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerInput {
    /// Position in texture pixels.
    pub x: f32,
    /// See [`Self::x`].
    pub y: f32,
    /// Motion since the previous move, texture pixels; raw under a lock.
    pub dx: f32,
    /// See [`Self::dx`].
    pub dy: f32,
    /// Buttons held after the event, as a DOM `buttons` mask.
    pub buttons: u32,
    /// The pointer's id; the mouse is 1.
    pub pointer_id: u32,
    /// 0 mouse, 1 pen, 2 touch.
    pub pointer_type: u16,
    /// The DOM `button` that changed, -1 for none.
    pub button: i16,
    /// Normalized pressure.
    pub pressure: f32,
    /// Pen tilt toward the right, degrees.
    pub tilt_x: f32,
    /// Pen tilt toward the user, degrees.
    pub tilt_y: f32,
    /// Pen rotation, degrees.
    pub twist: f32,
}

/// Modifier bits on a key input.
pub mod modifiers {
    /// Shift.
    pub const SHIFT: u16 = 1;
    /// Control.
    pub const CTRL: u16 = 2;
    /// Alt or Option.
    pub const ALT: u16 = 4;
    /// Meta, Super, Command or Windows.
    pub const META: u16 = 8;
}

/// What happened, in an [`Message::Input`].
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved.
    PointerMove(PointerInput),
    /// A button went down.
    PointerDown(PointerInput),
    /// A button came up.
    PointerUp(PointerInput),
    /// The wheel turned, at a position.
    Wheel {
        /// Where, in texture pixels.
        x: f32,
        /// See `x`.
        y: f32,
        /// Scroll across.
        dx: f32,
        /// Scroll down.
        dy: f32,
        /// 0 pixels, 1 lines.
        unit: u16,
    },
    /// A key went down or up.
    Key {
        /// Where the key sits: a number from the key-code table.
        code: u16,
        /// Down (`true`) or up.
        pressed: bool,
        /// The platform repeating a held key.
        repeat: bool,
        /// [`modifiers`] bits.
        modifiers: u16,
        /// What the key means under the user's layout, the DOM `key`.
        key: String,
    },
    /// Text the user committed (typed, or from an input method).
    Text(String),
    /// The surface gained (`true`) or lost focus. On a loss the producer
    /// releases every key and button it holds.
    Focus(bool),
    /// The pointer lock began (`true`) or ended.
    Lock(bool),
    /// The pointer left the surface.
    Leave,
}

/// A CSS cursor keyword a producer asks for, by number.
pub mod cursor {
    /// `default`.
    pub const DEFAULT: u16 = 0;
    /// `none`: no cursor.
    pub const NONE: u16 = 1;
    /// `pointer`.
    pub const POINTER: u16 = 2;
    /// `crosshair`.
    pub const CROSSHAIR: u16 = 3;
    /// `move`.
    pub const MOVE: u16 = 4;
    /// `grab`.
    pub const GRAB: u16 = 5;
    /// `grabbing`.
    pub const GRABBING: u16 = 6;
    /// `text`.
    pub const TEXT: u16 = 7;
    /// `wait`.
    pub const WAIT: u16 = 8;
    /// `not-allowed`.
    pub const NOT_ALLOWED: u16 = 9;

    /// The keyword a number names, `default` for one no keyword has.
    pub fn keyword(n: u16) -> &'static str {
        match n {
            NONE => "none",
            POINTER => "pointer",
            CROSSHAIR => "crosshair",
            MOVE => "move",
            GRAB => "grab",
            GRABBING => "grabbing",
            TEXT => "text",
            WAIT => "wait",
            NOT_ALLOWED => "not-allowed",
            _ => "default",
        }
    }
}

/// Every message of version 1.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// 1, producer to consumer.
    Hello(Hello),
    /// 2, consumer to producer.
    Welcome(Welcome),
    /// 3, consumer to producer.
    Reject(Reject),
    /// 4, producer to consumer, with handles.
    Buffers(Buffers),
    /// 5, consumer to producer.
    BuffersAccepted {
        /// The generation accepted.
        generation: u32,
    },
    /// 6, consumer to producer.
    BuffersRejected {
        /// The generation refused.
        generation: u32,
        /// 1: import failed, 2: bad description, 3: too large.
        reason: u16,
        /// Words for a person.
        detail: String,
    },
    /// 7, consumer to producer: render one frame.
    FrameRequest {
        /// Request number.
        seq: u64,
        /// When the consumer means to show it, monotonic nanoseconds; 0 for
        /// as soon as possible.
        target_present_ns: u64,
    },
    /// 8, producer to consumer.
    FrameReady(FrameReady),
    /// 9, consumer to producer: the slot may be written again.
    Release {
        /// The frame that was in it.
        frame: u64,
        /// Its generation.
        generation: u32,
        /// The slot.
        slot: u32,
    },
    /// 10, consumer to producer: make frames at this size.
    Resize {
        /// Width in texture pixels.
        width: u32,
        /// Height in texture pixels.
        height: u32,
        /// Device pixels per logical pixel.
        scale: f32,
    },
    /// 11, consumer to producer: every slot of the generation is back and
    /// none will be shown again; free it.
    RetireGeneration {
        /// The generation.
        generation: u32,
    },
    /// 12, consumer to producer.
    Input {
        /// Sequence number, increasing.
        seq: u64,
        /// When it happened, monotonic nanoseconds.
        time: u64,
        /// What happened.
        event: InputEvent,
    },
    /// 13, consumer to producer: whether anyone can see the frames.
    Visibility {
        /// Seen.
        visible: bool,
    },
    /// 14, producer to consumer: the cursor over the surface.
    Cursor {
        /// A [`cursor`] number.
        keyword: u16,
    },
    /// 15, producer to consumer: lock the pointer, or let it go. The
    /// consumer decides.
    PointerLock {
        /// Wanted.
        wanted: bool,
    },
    /// 16, either way: bytes for the application.
    User(Vec<u8>),
    /// 17, either way.
    Ping(u64),
    /// 18, either way, answering a ping.
    Pong(u64),
    /// 19, consumer to producer: the consumer's device changed; every
    /// buffer is gone, offer a new ring.
    Reset {
        /// The consumer's device generation.
        device_generation: u64,
    },
    /// 20, either way: the session ends.
    Bye {
        /// Why.
        reason: ByeReason,
        /// Words for a person.
        detail: String,
    },
}

/// A frame that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The frame ended before its fields did.
    Short,
    /// A length or size is out of range.
    TooLarge(&'static str),
    /// A field holds a value the protocol does not define.
    Value(&'static str, u64),
    /// A string is not UTF-8.
    Utf8,
    /// A message type this version does not know, without the may-ignore
    /// flag.
    UnknownType(u16),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Short => f.write_str("the frame is shorter than its fields"),
            WireError::TooLarge(what) => write!(f, "{what} is too large"),
            WireError::Value(what, v) => write!(f, "{v} is not a valid {what}"),
            WireError::Utf8 => f.write_str("a string is not UTF-8"),
            WireError::UnknownType(t) => write!(f, "message type {t} is unknown"),
        }
    }
}

impl std::error::Error for WireError {}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }
    fn var(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.bytes(v);
    }
    fn adapter(&mut self, a: &AdapterId) {
        self.bytes(&a.device_uuid);
        self.bytes(&a.driver_uuid);
        self.bytes(&a.luid);
        self.u64(a.registry_id);
        self.u32(a.vendor);
        self.u32(a.device);
    }
    fn pointer(&mut self, p: &PointerInput) {
        self.f32(p.x);
        self.f32(p.y);
        self.f32(p.dx);
        self.f32(p.dy);
        self.u32(p.buttons);
        self.u32(p.pointer_id);
        self.u16(p.pointer_type);
        self.i16(p.button);
        self.f32(p.pressure);
        self.f32(p.tilt_x);
        self.f32(p.tilt_y);
        self.f32(p.twist);
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.0.len() < n {
            return Err(WireError::Short);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        Ok(self.take(N)?.try_into().expect("took N"))
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn i16(&mut self) -> Result<i16, WireError> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f32(&mut self) -> Result<f32, WireError> {
        Ok(f32::from_le_bytes(self.array()?))
    }
    fn var(&mut self, max: usize, what: &'static str) -> Result<&'a [u8], WireError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(WireError::TooLarge(what));
        }
        self.take(n)
    }
    fn string(&mut self, what: &'static str) -> Result<String, WireError> {
        let bytes = self.var(MAX_FRAME, what)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::Utf8)
    }
    fn adapter(&mut self) -> Result<AdapterId, WireError> {
        Ok(AdapterId {
            device_uuid: self.array()?,
            driver_uuid: self.array()?,
            luid: self.array()?,
            registry_id: self.u64()?,
            vendor: self.u32()?,
            device: self.u32()?,
        })
    }
    fn pointer(&mut self) -> Result<PointerInput, WireError> {
        Ok(PointerInput {
            x: self.f32()?,
            y: self.f32()?,
            dx: self.f32()?,
            dy: self.f32()?,
            buttons: self.u32()?,
            pointer_id: self.u32()?,
            pointer_type: self.u16()?,
            button: self.i16()?,
            pressure: self.f32()?,
            tilt_x: self.f32()?,
            tilt_y: self.f32()?,
            twist: self.f32()?,
        })
    }
    fn bool(&mut self) -> Result<bool, WireError> {
        Ok(self.u8()? != 0)
    }
}

impl Message {
    /// The message's type number.
    pub fn kind(&self) -> u16 {
        match self {
            Message::Hello(_) => 1,
            Message::Welcome(_) => 2,
            Message::Reject(_) => 3,
            Message::Buffers(_) => 4,
            Message::BuffersAccepted { .. } => 5,
            Message::BuffersRejected { .. } => 6,
            Message::FrameRequest { .. } => 7,
            Message::FrameReady(_) => 8,
            Message::Release { .. } => 9,
            Message::Resize { .. } => 10,
            Message::RetireGeneration { .. } => 11,
            Message::Input { .. } => 12,
            Message::Visibility { .. } => 13,
            Message::Cursor { .. } => 14,
            Message::PointerLock { .. } => 15,
            Message::User(_) => 16,
            Message::Ping(_) => 17,
            Message::Pong(_) => 18,
            Message::Reset { .. } => 19,
            Message::Bye { .. } => 20,
        }
    }

    /// The whole frame: header and body. `handles` sets the flag that OS
    /// handles travel with it.
    ///
    /// # Errors
    ///
    /// A variable part, or the whole frame, is larger than the protocol
    /// allows.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let mut w = Writer(Vec::with_capacity(64));
        w.u32(0);
        w.u16(self.kind());
        let flags = if matches!(self, Message::Buffers(_)) {
            FLAG_HANDLES
        } else {
            0
        };
        w.u16(flags);
        match self {
            Message::Hello(h) => {
                w.u16(h.major);
                w.u16(h.minor);
                w.u16(h.api as u16);
                w.u16(0);
                w.u32(h.pid);
                w.u32(h.caps);
                w.bytes(&h.token);
                w.adapter(&h.adapter);
                w.var(h.channel.as_bytes());
                w.var(h.engine.as_bytes());
            }
            Message::Welcome(m) => {
                w.u64(m.session);
                w.u16(m.api as u16);
                w.u16(m.pacing as u16);
                w.u8(m.ring_min);
                w.u8(m.ring_max);
                w.u8(m.ring_pref);
                w.u8(0);
                w.u32(m.width);
                w.u32(m.height);
                w.f32(m.scale);
                w.u32(m.formats);
                w.u32(m.caps);
            }
            Message::Reject(r) => {
                w.u16(r.reason as u16);
                w.u16(r.api as u16);
                w.adapter(&r.adapter);
                w.var(r.detail.as_bytes());
            }
            Message::Buffers(b) => {
                if b.buffers.is_empty() || b.buffers.len() > MAX_RING {
                    return Err(WireError::TooLarge("the ring"));
                }
                w.u32(b.generation);
                w.u32(b.buffers.len() as u32);
                w.u32(b.width);
                w.u32(b.height);
                w.u32(b.format as u32);
                w.u32(0);
                w.u32(b.usage);
                w.u32(b.flags);
                for d in &b.buffers {
                    w.u64(d.alloc_size);
                    w.u32(d.memory_type);
                    w.u32(d.offset);
                    w.u32(d.row_pitch);
                    w.u32(0);
                    w.u64(d.modifier);
                    w.u64(d.handle);
                }
            }
            Message::BuffersAccepted { generation } => w.u32(*generation),
            Message::BuffersRejected {
                generation,
                reason,
                detail,
            } => {
                w.u32(*generation);
                w.u16(*reason);
                w.u16(0);
                w.var(detail.as_bytes());
            }
            Message::FrameRequest {
                seq,
                target_present_ns,
            } => {
                w.u64(*seq);
                w.u64(*target_present_ns);
            }
            Message::FrameReady(f) => {
                if f.meta.len() > MAX_META {
                    return Err(WireError::TooLarge("frame meta"));
                }
                w.u64(f.frame);
                w.u64(f.input_seq);
                w.u64(f.t_sim);
                w.u64(f.t_submit);
                w.u64(f.t_gpu_done);
                w.u32(f.generation);
                w.u32(f.slot);
                w.var(&f.meta);
            }
            Message::Release {
                frame,
                generation,
                slot,
            } => {
                w.u64(*frame);
                w.u32(*generation);
                w.u32(*slot);
            }
            Message::Resize {
                width,
                height,
                scale,
            } => {
                w.u32(*width);
                w.u32(*height);
                w.f32(*scale);
                w.u32(0);
            }
            Message::RetireGeneration { generation } => w.u32(*generation),
            Message::Input { seq, time, event } => {
                w.u64(*seq);
                w.u64(*time);
                let kind: u16 = match event {
                    InputEvent::PointerMove(_) => 1,
                    InputEvent::PointerDown(_) => 2,
                    InputEvent::PointerUp(_) => 3,
                    InputEvent::Wheel { .. } => 4,
                    InputEvent::Key { .. } => 5,
                    InputEvent::Text(_) => 6,
                    InputEvent::Focus(_) => 7,
                    InputEvent::Lock(_) => 8,
                    InputEvent::Leave => 9,
                };
                w.u16(kind);
                w.u16(0);
                match event {
                    InputEvent::PointerMove(p)
                    | InputEvent::PointerDown(p)
                    | InputEvent::PointerUp(p) => w.pointer(p),
                    InputEvent::Wheel { x, y, dx, dy, unit } => {
                        w.f32(*x);
                        w.f32(*y);
                        w.f32(*dx);
                        w.f32(*dy);
                        w.u16(*unit);
                        w.u16(0);
                    }
                    InputEvent::Key {
                        code,
                        pressed,
                        repeat,
                        modifiers,
                        key,
                    } => {
                        w.u16(*code);
                        w.u8(u8::from(*pressed));
                        w.u8(u8::from(*repeat));
                        w.u16(*modifiers);
                        w.u16(0);
                        w.var(key.as_bytes());
                    }
                    InputEvent::Text(text) => w.var(text.as_bytes()),
                    InputEvent::Focus(on) | InputEvent::Lock(on) => w.u8(u8::from(*on)),
                    InputEvent::Leave => {}
                }
            }
            Message::Visibility { visible } => w.u8(u8::from(*visible)),
            Message::Cursor { keyword } => w.u16(*keyword),
            Message::PointerLock { wanted } => w.u8(u8::from(*wanted)),
            Message::User(bytes) => {
                if bytes.len() > MAX_USER {
                    return Err(WireError::TooLarge("a user message"));
                }
                w.var(bytes);
            }
            Message::Ping(t) | Message::Pong(t) => w.u64(*t),
            Message::Reset { device_generation } => w.u64(*device_generation),
            Message::Bye { reason, detail } => {
                w.u16(*reason as u16);
                w.var(detail.as_bytes());
            }
        }
        let mut frame = w.0;
        if frame.len() > MAX_FRAME {
            return Err(WireError::TooLarge("the frame"));
        }
        let len = (frame.len() - 4) as u32;
        frame[..4].copy_from_slice(&len.to_le_bytes());
        Ok(frame)
    }

    /// Read one message from a frame's type, flags and body (everything
    /// after the header). `Ok(None)` is an unknown type the sender marked
    /// as one to skip.
    ///
    /// # Errors
    ///
    /// The body does not hold the message its type says.
    pub fn decode(kind: u16, flags: u16, body: &[u8]) -> Result<Option<Message>, WireError> {
        let mut r = Reader(body);
        let m = match kind {
            1 => {
                let major = r.u16()?;
                let minor = r.u16()?;
                let api = Api::from_u16(r.u16()?)?;
                let _ = r.u16()?;
                Message::Hello(Hello {
                    major,
                    minor,
                    api,
                    pid: r.u32()?,
                    caps: r.u32()?,
                    token: r.array()?,
                    adapter: r.adapter()?,
                    channel: r.string("the channel")?,
                    engine: r.string("the engine name")?,
                })
            }
            2 => {
                let session = r.u64()?;
                let api = Api::from_u16(r.u16()?)?;
                let pacing = Pacing::from_u16(r.u16()?)?;
                let ring_min = r.u8()?;
                let ring_max = r.u8()?;
                let ring_pref = r.u8()?;
                let _ = r.u8()?;
                Message::Welcome(Welcome {
                    session,
                    api,
                    pacing,
                    ring_min,
                    ring_max,
                    ring_pref,
                    width: r.u32()?,
                    height: r.u32()?,
                    scale: r.f32()?,
                    formats: r.u32()?,
                    caps: r.u32()?,
                })
            }
            3 => Message::Reject(Reject {
                reason: RejectReason::from_u16(r.u16()?)?,
                api: Api::from_u16(r.u16()?)?,
                adapter: r.adapter()?,
                detail: r.string("the detail")?,
            }),
            4 => {
                let generation = r.u32()?;
                let count = r.u32()? as usize;
                if count == 0 || count > MAX_RING {
                    return Err(WireError::Value("buffer count", count as u64));
                }
                let width = r.u32()?;
                let height = r.u32()?;
                let format = Format::from_u32(r.u32()?)?;
                let _ = r.u32()?;
                let usage = r.u32()?;
                let buffer_flags = r.u32()?;
                let mut buffers = Vec::with_capacity(count);
                for _ in 0..count {
                    let alloc_size = r.u64()?;
                    let memory_type = r.u32()?;
                    let offset = r.u32()?;
                    let row_pitch = r.u32()?;
                    let _ = r.u32()?;
                    buffers.push(BufferDesc {
                        alloc_size,
                        memory_type,
                        offset,
                        row_pitch,
                        modifier: r.u64()?,
                        handle: r.u64()?,
                    });
                }
                Message::Buffers(Buffers {
                    generation,
                    width,
                    height,
                    format,
                    usage,
                    flags: buffer_flags,
                    buffers,
                })
            }
            5 => Message::BuffersAccepted {
                generation: r.u32()?,
            },
            6 => {
                let generation = r.u32()?;
                let reason = r.u16()?;
                let _ = r.u16()?;
                Message::BuffersRejected {
                    generation,
                    reason,
                    detail: r.string("the detail")?,
                }
            }
            7 => Message::FrameRequest {
                seq: r.u64()?,
                target_present_ns: r.u64()?,
            },
            8 => Message::FrameReady(FrameReady {
                frame: r.u64()?,
                input_seq: r.u64()?,
                t_sim: r.u64()?,
                t_submit: r.u64()?,
                t_gpu_done: r.u64()?,
                generation: r.u32()?,
                slot: r.u32()?,
                meta: r.var(MAX_META, "frame meta")?.to_vec(),
            }),
            9 => Message::Release {
                frame: r.u64()?,
                generation: r.u32()?,
                slot: r.u32()?,
            },
            10 => {
                let width = r.u32()?;
                let height = r.u32()?;
                let scale = r.f32()?;
                let _ = r.u32()?;
                Message::Resize {
                    width,
                    height,
                    scale,
                }
            }
            11 => Message::RetireGeneration {
                generation: r.u32()?,
            },
            12 => {
                let seq = r.u64()?;
                let time = r.u64()?;
                let kind = r.u16()?;
                let _ = r.u16()?;
                let event = match kind {
                    1 => InputEvent::PointerMove(r.pointer()?),
                    2 => InputEvent::PointerDown(r.pointer()?),
                    3 => InputEvent::PointerUp(r.pointer()?),
                    4 => {
                        let (x, y, dx, dy) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
                        let unit = r.u16()?;
                        let _ = r.u16()?;
                        InputEvent::Wheel { x, y, dx, dy, unit }
                    }
                    5 => {
                        let code = r.u16()?;
                        let pressed = r.bool()?;
                        let repeat = r.bool()?;
                        let modifiers = r.u16()?;
                        let _ = r.u16()?;
                        InputEvent::Key {
                            code,
                            pressed,
                            repeat,
                            modifiers,
                            key: r.string("the key")?,
                        }
                    }
                    6 => InputEvent::Text(r.string("the text")?),
                    7 => InputEvent::Focus(r.bool()?),
                    8 => InputEvent::Lock(r.bool()?),
                    9 => InputEvent::Leave,
                    other => return Err(WireError::Value("input kind", u64::from(other))),
                };
                Message::Input { seq, time, event }
            }
            13 => Message::Visibility { visible: r.bool()? },
            14 => Message::Cursor { keyword: r.u16()? },
            15 => Message::PointerLock { wanted: r.bool()? },
            16 => Message::User(r.var(MAX_USER, "a user message")?.to_vec()),
            17 => Message::Ping(r.u64()?),
            18 => Message::Pong(r.u64()?),
            19 => Message::Reset {
                device_generation: r.u64()?,
            },
            20 => Message::Bye {
                reason: ByeReason::from_u16(r.u16()?)?,
                detail: r.string("the detail")?,
            },
            other if flags & FLAG_MAY_IGNORE != 0 => {
                let _ = other;
                return Ok(None);
            }
            other => return Err(WireError::UnknownType(other)),
        };
        Ok(Some(m))
    }

    /// Read one frame (header and body) from the front of `bytes`. Returns
    /// the message and how many bytes the frame took, or `Ok(None)` when
    /// `bytes` does not hold a whole frame yet.
    ///
    /// # Errors
    ///
    /// The frame declares a size the protocol does not allow, or its body
    /// does not hold its message.
    pub fn read_frame(bytes: &[u8]) -> Result<Option<(Option<Message>, u16, usize)>, WireError> {
        if bytes.len() < HEADER {
            return Ok(None);
        }
        let len = u32::from_le_bytes(bytes[..4].try_into().expect("4 bytes")) as usize;
        if !(HEADER - 4..=MAX_FRAME - 4).contains(&len) {
            return Err(WireError::TooLarge("the frame"));
        }
        let total = len + 4;
        if bytes.len() < total {
            return Ok(None);
        }
        let kind = u16::from_le_bytes([bytes[4], bytes[5]]);
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        let message = Message::decode(kind, flags, &bytes[HEADER..total])?;
        Ok(Some((message, flags, total)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> AdapterId {
        AdapterId {
            device_uuid: [1; 16],
            driver_uuid: [2; 16],
            luid: [3; 8],
            registry_id: 4,
            vendor: 0x1002,
            device: 0x73bf,
        }
    }

    fn pointer() -> PointerInput {
        PointerInput {
            x: 10.5,
            y: 20.0,
            dx: 1.0,
            dy: -2.0,
            buttons: 1,
            pointer_id: 1,
            pointer_type: 0,
            button: 0,
            pressure: 0.5,
            tilt_x: 0.0,
            tilt_y: 0.0,
            twist: 0.0,
        }
    }

    fn every_message() -> Vec<Message> {
        vec![
            Message::Hello(Hello {
                major: MAJOR,
                minor: MINOR,
                api: Api::Vulkan,
                pid: 42,
                caps: 0,
                token: [9; 16],
                adapter: adapter(),
                channel: "scene".into(),
                engine: "test 1.0".into(),
            }),
            Message::Welcome(Welcome {
                session: 7,
                api: Api::Vulkan,
                pacing: Pacing::Free,
                ring_min: 2,
                ring_max: 4,
                ring_pref: 3,
                width: 640,
                height: 480,
                scale: 1.5,
                formats: Format::Rgba8Srgb.bit(),
                caps: 0,
            }),
            Message::Reject(Reject {
                reason: RejectReason::Adapter,
                api: Api::Vulkan,
                adapter: adapter(),
                detail: "other GPU".into(),
            }),
            Message::Buffers(Buffers {
                generation: 1,
                width: 640,
                height: 480,
                format: Format::Rgba8Srgb,
                usage: 0x17,
                flags: BUFFER_DEDICATED,
                buffers: (0..3)
                    .map(|i| BufferDesc {
                        alloc_size: 1 << 20,
                        memory_type: 2,
                        offset: 0,
                        row_pitch: 0,
                        modifier: 0,
                        handle: i,
                    })
                    .collect(),
            }),
            Message::BuffersAccepted { generation: 1 },
            Message::BuffersRejected {
                generation: 1,
                reason: 1,
                detail: "no".into(),
            },
            Message::FrameRequest {
                seq: 5,
                target_present_ns: 99,
            },
            Message::FrameReady(FrameReady {
                frame: 3,
                input_seq: 2,
                t_sim: 1,
                t_submit: 2,
                t_gpu_done: 3,
                generation: 1,
                slot: 2,
                meta: vec![1, 2, 3],
            }),
            Message::Release {
                frame: 3,
                generation: 1,
                slot: 2,
            },
            Message::Resize {
                width: 800,
                height: 600,
                scale: 2.0,
            },
            Message::RetireGeneration { generation: 1 },
            Message::Input {
                seq: 1,
                time: 2,
                event: InputEvent::PointerMove(pointer()),
            },
            Message::Input {
                seq: 2,
                time: 3,
                event: InputEvent::PointerDown(pointer()),
            },
            Message::Input {
                seq: 3,
                time: 4,
                event: InputEvent::PointerUp(pointer()),
            },
            Message::Input {
                seq: 4,
                time: 5,
                event: InputEvent::Wheel {
                    x: 1.0,
                    y: 2.0,
                    dx: 0.0,
                    dy: -3.0,
                    unit: 1,
                },
            },
            Message::Input {
                seq: 5,
                time: 6,
                event: InputEvent::Key {
                    code: 26,
                    pressed: true,
                    repeat: false,
                    modifiers: modifiers::SHIFT,
                    key: "W".into(),
                },
            },
            Message::Input {
                seq: 6,
                time: 7,
                event: InputEvent::Text("hej".into()),
            },
            Message::Input {
                seq: 7,
                time: 8,
                event: InputEvent::Focus(false),
            },
            Message::Input {
                seq: 8,
                time: 9,
                event: InputEvent::Lock(true),
            },
            Message::Input {
                seq: 9,
                time: 10,
                event: InputEvent::Leave,
            },
            Message::Visibility { visible: false },
            Message::Cursor {
                keyword: cursor::CROSSHAIR,
            },
            Message::PointerLock { wanted: true },
            Message::User(b"hello".to_vec()),
            Message::Ping(11),
            Message::Pong(11),
            Message::Reset {
                device_generation: 2,
            },
            Message::Bye {
                reason: ByeReason::DeviceLost,
                detail: "gone".into(),
            },
        ]
    }

    /// Every message survives encoding and decoding, and its frame says how
    /// long it is.
    #[test]
    fn every_message_round_trips() {
        for message in every_message() {
            let frame = message.encode().expect("encodes");
            let (back, _flags, used) = Message::read_frame(&frame)
                .expect("reads")
                .expect("a whole frame");
            assert_eq!(used, frame.len());
            assert_eq!(back, Some(message));
        }
    }

    /// A partial frame waits for more bytes; an oversize length is refused.
    #[test]
    fn frames_are_bounded() {
        let frame = Message::Ping(1).encode().unwrap();
        assert_eq!(Message::read_frame(&frame[..frame.len() - 1]), Ok(None));
        let mut huge = frame.clone();
        huge[..4].copy_from_slice(&(MAX_FRAME as u32).to_le_bytes());
        assert!(Message::read_frame(&huge).is_err());
        assert!(
            Message::User(vec![0; MAX_USER + 1]).encode().is_err(),
            "user bytes are capped"
        );
    }

    /// An unknown type is skipped only when its sender said it may be.
    #[test]
    fn unknown_types_need_the_may_ignore_flag() {
        assert_eq!(Message::decode(999, FLAG_MAY_IGNORE, &[]), Ok(None));
        assert_eq!(
            Message::decode(999, 0, &[]),
            Err(WireError::UnknownType(999))
        );
    }

    /// The Hello layout is the one the specification gives, field by field:
    /// a C producer that writes the documented offsets is read correctly.
    #[test]
    fn hello_matches_the_documented_layout() {
        let frame = every_message()[0].encode().unwrap();
        // header 8, major 2, minor 2, api 2, reserved 2, pid 4, caps 4,
        // token 16, adapter 16+16+8+8+4+4, channel 4+5, engine 4+8.
        assert_eq!(frame.len(), 8 + 16 + 16 + 56 + 9 + 12);
        assert_eq!(&frame[4..6], &1u16.to_le_bytes());
        assert_eq!(&frame[16..20], &42u32.to_le_bytes());
    }
}
