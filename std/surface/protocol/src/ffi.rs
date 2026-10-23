//! The producer role as a C ABI, declared in
//! `include/lumen_surface_protocol.h`.
//!
//! An engine written in C or C++ exports its images with its own graphics
//! code and drives the session through these calls. Every function returns
//! an [`LspStatus`] (or a null pointer with the status written out), catches
//! any panic before it reaches the caller, and records a message
//! [`lsp_last_error`] reads. Structs are `#[repr(C)]`, never packed, with
//! explicit reserved fields; those an engine fills begin with `struct_size`
//! so a later minor version can grow them.

use crate::producer::{FrameTimes, Producer, ProducerError, ProducerEvent, ProducerHello, Slot};
use crate::wire::{AdapterId, Api, BufferDesc, Buffers, ByeReason, Format, InputEvent};
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

/// A call's outcome. `LSP_OK` is 0; anything else is a failure
/// [`lsp_last_error`] describes.
pub type LspStatus = u32;
/// Success.
pub const LSP_OK: LspStatus = 0;
/// An argument was null, too small, or out of range.
pub const LSP_ERR_ARG: LspStatus = 1;
/// The endpoint string does not parse.
pub const LSP_ERR_ENDPOINT: LspStatus = 2;
/// The socket failed.
pub const LSP_ERR_IO: LspStatus = 3;
/// The consumer refused this producer or its ring.
pub const LSP_ERR_REJECTED: LspStatus = 4;
/// Nothing came in time.
pub const LSP_ERR_TIMEOUT: LspStatus = 5;
/// The session is over.
pub const LSP_ERR_CLOSED: LspStatus = 6;
/// A call broke the protocol's rules.
pub const LSP_ERR_MISUSE: LspStatus = 7;
/// A panic was caught; the producer should not be used again.
pub const LSP_ERR_PANIC: LspStatus = 8;

/// `LspHello::api`: Vulkan.
pub const LSP_API_VULKAN: u16 = 1;
/// `LspBuffers::format`: 8-bit RGBA holding sRGB-encoded colour.
pub const LSP_FORMAT_RGBA8_SRGB: u32 = 1;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(message: impl std::fmt::Display) {
    let text = CString::new(message.to_string().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = text);
}

fn status_of(e: &ProducerError) -> LspStatus {
    set_error(e);
    match e {
        ProducerError::Endpoint(_) => LSP_ERR_ENDPOINT,
        ProducerError::Transport(_) => LSP_ERR_IO,
        ProducerError::Rejected(_) => LSP_ERR_REJECTED,
        ProducerError::Timeout => LSP_ERR_TIMEOUT,
        ProducerError::Closed => LSP_ERR_CLOSED,
        ProducerError::Misuse(_) => LSP_ERR_MISUSE,
    }
}

fn guard(f: impl FnOnce() -> LspStatus) -> LspStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(_) => {
            set_error("a panic inside lumen-surface-protocol was caught");
            LSP_ERR_PANIC
        }
    }
}

/// The producer behind an `LspProducer *`, with the strings and bytes the
/// last event points at.
pub struct LspProducer {
    producer: Producer,
    text: CString,
    data: Vec<u8>,
}

/// What a producer says about itself. `struct_size` is
/// `sizeof(LspHello)`.
#[repr(C)]
pub struct LspHello {
    /// `sizeof(LspHello)`.
    pub struct_size: u32,
    /// `LSP_API_VULKAN`.
    pub api: u16,
    /// Zero.
    pub reserved0: u16,
    /// Capability bits; zero in version 1.
    pub caps: u32,
    /// Zero.
    pub reserved1: u32,
    /// Vulkan `deviceUUID`.
    pub device_uuid: [u8; 16],
    /// Vulkan `driverUUID`.
    pub driver_uuid: [u8; 16],
    /// Direct3D adapter LUID; zero elsewhere.
    pub luid: [u8; 8],
    /// Metal `registryID`; zero elsewhere.
    pub registry_id: u64,
    /// PCI vendor id.
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
    /// Engine name and version, NUL-terminated; may be null.
    pub engine: *const c_char,
}

/// What the consumer said when it welcomed the producer.
#[repr(C)]
pub struct LspWelcome {
    /// Set to `sizeof(LspWelcome)` by the library.
    pub struct_size: u32,
    /// 0 paced: render one frame per request; 1 free: render at will.
    pub pacing: u16,
    /// Fewest buffers in a ring.
    pub ring_min: u8,
    /// Most buffers in a ring.
    pub ring_max: u8,
    /// Preferred buffers in a ring.
    pub ring_pref: u8,
    /// Zero.
    pub reserved0: [u8; 3],
    /// Width the consumer wants frames at.
    pub width: u32,
    /// Height the consumer wants frames at.
    pub height: u32,
    /// Device pixels per logical pixel where frames show.
    pub scale: f32,
    /// Format bits the consumer takes: `1 << LSP_FORMAT_RGBA8_SRGB`.
    pub formats: u32,
    /// Zero.
    pub reserved1: u32,
    /// The session id.
    pub session: u64,
}

/// One buffer of a ring and its exported handle.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LspBuffer {
    /// Allocation size in bytes.
    pub alloc_size: u64,
    /// Memory type index the allocation came from.
    pub memory_type: u32,
    /// Offset of the image in the allocation.
    pub offset: u32,
    /// Bytes per row for a linear layout, 0 for optimal tiling.
    pub row_pitch: u32,
    /// Zero.
    pub reserved0: u32,
    /// DRM format modifier, 0 when not a dma-buf export.
    pub modifier: u64,
    /// The exported file descriptor (Vulkan `OPAQUE_FD`). The library sends
    /// a duplicate; the caller keeps its own.
    pub fd: i32,
    /// Zero.
    pub reserved1: u32,
}

/// A ring to offer. `struct_size` is `sizeof(LspBuffers)`.
#[repr(C)]
pub struct LspBuffers {
    /// `sizeof(LspBuffers)`.
    pub struct_size: u32,
    /// The ring's generation: 1 for the first, one more for each new ring.
    pub generation: u32,
    /// Width in texture pixels.
    pub width: u32,
    /// Height in texture pixels.
    pub height: u32,
    /// `LSP_FORMAT_RGBA8_SRGB`.
    pub format: u32,
    /// `VkImageUsageFlags` the images were made with.
    pub usage: u32,
    /// `LSP_BUFFER_DEDICATED` when each buffer is a dedicated allocation.
    pub flags: u32,
    /// How many buffers `buffers` points at.
    pub count: u32,
    /// The buffers.
    pub buffers: *const LspBuffer,
}

/// A slot to render into.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LspSlot {
    /// The ring generation.
    pub generation: u32,
    /// The slot index.
    pub slot: u32,
}

/// A finished frame. `struct_size` is `sizeof(LspFrame)`.
#[repr(C)]
pub struct LspFrame {
    /// `sizeof(LspFrame)`.
    pub struct_size: u32,
    /// The slot's ring generation.
    pub generation: u32,
    /// The slot.
    pub slot: u32,
    /// How many bytes `meta` points at, at most 4096.
    pub meta_len: u32,
    /// Bytes the consumer hands its app with the frame; may be null.
    pub meta: *const u8,
    /// When the frame's simulation ran, monotonic nanoseconds; 0 unknown.
    pub t_sim: u64,
    /// When its GPU work was submitted; 0 unknown.
    pub t_submit: u64,
    /// When its GPU work finished; 0 takes the moment of the call.
    pub t_gpu_done: u64,
    /// The newest input sequence the frame reflects; 0 takes the newest the
    /// library has handed out.
    pub input_seq: u64,
}

/// `LspEvent::kind`: nothing waiting.
pub const LSP_EVENT_NONE: u32 = 0;
/// Make frames at `width` x `height` (`scale`): offer a new ring.
pub const LSP_EVENT_RESIZE: u32 = 1;
/// Input; `input_kind` says which.
pub const LSP_EVENT_INPUT: u32 = 2;
/// `flag` says whether anyone can see the frames.
pub const LSP_EVENT_VISIBILITY: u32 = 3;
/// Bytes from the application in `data` / `data_len`.
pub const LSP_EVENT_USER: u32 = 4;
/// Every buffer is gone; offer a new ring.
pub const LSP_EVENT_RESET: u32 = 5;
/// Every slot of `generation` is back; free its buffers.
pub const LSP_EVENT_RETIRED: u32 = 6;
/// The consumer refused ring `generation`; `text` says why.
pub const LSP_EVENT_BUFFERS_REJECTED: u32 = 7;
/// The session ended; `reason` and `text` say why.
pub const LSP_EVENT_CLOSED: u32 = 8;

/// `LspEvent::input_kind` values.
pub const LSP_INPUT_POINTER_MOVE: u32 = 1;
/// A button went down.
pub const LSP_INPUT_POINTER_DOWN: u32 = 2;
/// A button came up.
pub const LSP_INPUT_POINTER_UP: u32 = 3;
/// The wheel turned.
pub const LSP_INPUT_WHEEL: u32 = 4;
/// A key went down or up.
pub const LSP_INPUT_KEY: u32 = 5;
/// Committed text in `text`.
pub const LSP_INPUT_TEXT: u32 = 6;
/// Focus gained (`flag` 1) or lost.
pub const LSP_INPUT_FOCUS: u32 = 7;
/// Pointer lock began (`flag` 1) or ended.
pub const LSP_INPUT_LOCK: u32 = 8;
/// The pointer left the surface.
pub const LSP_INPUT_LEAVE: u32 = 9;

/// One event, flattened: the fields its kind uses are set, the rest zero.
/// `text` and `data` stay valid until the next call on the producer.
#[repr(C)]
pub struct LspEvent {
    /// Set to `sizeof(LspEvent)` by the library.
    pub struct_size: u32,
    /// `LSP_EVENT_*`.
    pub kind: u32,
    /// Input sequence number.
    pub seq: u64,
    /// When the input happened, monotonic nanoseconds.
    pub time: u64,
    /// `LSP_INPUT_*` for an input event.
    pub input_kind: u32,
    /// Resize width.
    pub width: u32,
    /// Resize height.
    pub height: u32,
    /// Resize scale.
    pub scale: f32,
    /// Generation of a retired or rejected ring.
    pub generation: u32,
    /// Visibility, focus or lock state.
    pub flag: u32,
    /// Pointer or wheel position, texture pixels.
    pub x: f32,
    /// See `x`.
    pub y: f32,
    /// Pointer motion, or wheel scroll.
    pub dx: f32,
    /// See `dx`.
    pub dy: f32,
    /// Buttons held, a DOM `buttons` mask.
    pub buttons: u32,
    /// The pointer's id; the mouse is 1.
    pub pointer_id: u32,
    /// 0 mouse, 1 pen, 2 touch.
    pub pointer_type: u16,
    /// The DOM `button` that changed, -1 none.
    pub button: i16,
    /// Pen pressure.
    pub pressure: f32,
    /// Pen tilt toward the right.
    pub tilt_x: f32,
    /// Pen tilt toward the user.
    pub tilt_y: f32,
    /// Pen rotation.
    pub twist: f32,
    /// Wheel unit: 0 pixels, 1 lines.
    pub wheel_unit: u16,
    /// Key code, from the key-code table (`lsp_key_code_name`).
    pub key_code: u16,
    /// Key down (1) or up (0).
    pub pressed: u8,
    /// The platform repeating a held key.
    pub repeat: u8,
    /// Modifier bits: 1 shift, 2 ctrl, 4 alt, 8 meta.
    pub modifiers: u16,
    /// Why a session closed: 0 unknown, else a `Bye` reason.
    pub reason: u32,
    /// Zero.
    pub reserved0: u32,
    /// Zero.
    pub reserved1: u32,
    /// Key name, committed text, or a reason, NUL-terminated; may be null.
    pub text: *const c_char,
    /// User bytes; may be null.
    pub data: *const u8,
    /// How many bytes `data` points at.
    pub data_len: u64,
}

/// `LspBuffers::flags`: each buffer is a dedicated allocation.
pub const LSP_BUFFER_DEDICATED: u32 = crate::wire::BUFFER_DEDICATED;

/// The protocol version this library speaks: `major << 16 | minor`.
#[unsafe(no_mangle)]
pub extern "C" fn lsp_version() -> u32 {
    (u32::from(crate::wire::MAJOR) << 16) | u32::from(crate::wire::MINOR)
}

/// Copy the calling thread's last error message into `buf` (NUL-terminated,
/// truncated to `len`). Returns the message's full length without the NUL.
///
/// # Safety
///
/// `buf` is null or points at `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_last_error(buf: *mut c_char, len: usize) -> usize {
    LAST_ERROR.with(|e| {
        let e = e.borrow();
        let bytes = e.as_bytes();
        if !buf.is_null() && len > 0 {
            let n = bytes.len().min(len - 1);
            // SAFETY: the caller gives `len` writable bytes; `n < len`.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr().cast(), buf, n);
                *buf.add(n) = 0;
            }
        }
        bytes.len()
    })
}

/// Connect to the consumer `endpoint` names (the `LUMEN_SURFACE` string),
/// introduce the producer with `hello`, and wait for the answer. Returns the
/// producer, with `welcome` filled, or null with `status` saying why
/// (`LSP_ERR_REJECTED`: the consumer refused; the error message says why and
/// which GPU it renders on).
///
/// # Safety
///
/// `endpoint` is a NUL-terminated string; `hello` points at an `LspHello`
/// whose `engine` is null or NUL-terminated; `welcome` and `status` are null
/// or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_connect(
    endpoint: *const c_char,
    hello: *const LspHello,
    welcome: *mut LspWelcome,
    status: *mut LspStatus,
) -> *mut LspProducer {
    let mut out: *mut LspProducer = std::ptr::null_mut();
    let result = guard(|| {
        if endpoint.is_null() || hello.is_null() {
            set_error("lsp_connect: null endpoint or hello");
            return LSP_ERR_ARG;
        }
        // SAFETY: checked non-null; the caller promises a valid string and
        // struct.
        let (endpoint, hello) = unsafe { (CStr::from_ptr(endpoint), &*hello) };
        if (hello.struct_size as usize) < std::mem::size_of::<LspHello>() {
            set_error("lsp_connect: hello.struct_size is too small");
            return LSP_ERR_ARG;
        }
        let Ok(endpoint) = endpoint.to_str() else {
            set_error("lsp_connect: the endpoint is not UTF-8");
            return LSP_ERR_ENDPOINT;
        };
        let api = match hello.api {
            LSP_API_VULKAN => Api::Vulkan,
            2 => Api::D3d12,
            3 => Api::Metal,
            _ => {
                set_error("lsp_connect: unknown api");
                return LSP_ERR_ARG;
            }
        };
        let engine = if hello.engine.is_null() {
            String::new()
        } else {
            // SAFETY: non-null, NUL-terminated by contract.
            unsafe { CStr::from_ptr(hello.engine) }
                .to_string_lossy()
                .into_owned()
        };
        let producer = match Producer::connect(
            endpoint,
            &ProducerHello {
                api,
                adapter: AdapterId {
                    device_uuid: hello.device_uuid,
                    driver_uuid: hello.driver_uuid,
                    luid: hello.luid,
                    registry_id: hello.registry_id,
                    vendor: hello.vendor,
                    device: hello.device,
                },
                engine,
                caps: hello.caps,
            },
        ) {
            Ok(p) => p,
            Err(e) => return status_of(&e),
        };
        if !welcome.is_null() {
            let w = producer.welcome();
            // SAFETY: non-null and writable by contract.
            unsafe {
                *welcome = LspWelcome {
                    struct_size: std::mem::size_of::<LspWelcome>() as u32,
                    pacing: w.pacing as u16,
                    ring_min: w.ring_min,
                    ring_max: w.ring_max,
                    ring_pref: w.ring_pref,
                    reserved0: [0; 3],
                    width: w.width,
                    height: w.height,
                    scale: w.scale,
                    formats: w.formats,
                    reserved1: 0,
                    session: w.session,
                };
            }
        }
        out = Box::into_raw(Box::new(LspProducer {
            producer,
            text: CString::default(),
            data: Vec::new(),
        }));
        LSP_OK
    });
    if !status.is_null() {
        // SAFETY: non-null and writable by contract.
        unsafe { *status = result };
    }
    out
}

/// Borrow the producer behind `p`, or fail with `LSP_ERR_ARG`.
fn with_producer(p: *mut LspProducer, f: impl FnOnce(&mut LspProducer) -> LspStatus) -> LspStatus {
    guard(|| {
        if p.is_null() {
            set_error("null producer");
            return LSP_ERR_ARG;
        }
        // SAFETY: non-null; the caller owns the producer and does not share
        // it across threads during the call.
        f(unsafe { &mut *p })
    })
}

/// Offer a ring and wait for the consumer to import it. The descriptors are
/// duplicated into the consumer; the caller keeps its own.
///
/// # Safety
///
/// `p` came from [`lsp_connect`]; `buffers` points at an `LspBuffers` whose
/// `buffers` points at `count` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_offer_buffers(
    p: *mut LspProducer,
    buffers: *const LspBuffers,
) -> LspStatus {
    with_producer(p, |p| {
        if buffers.is_null() {
            set_error("lsp_offer_buffers: null buffers");
            return LSP_ERR_ARG;
        }
        // SAFETY: non-null by the check above, valid by contract.
        let b = unsafe { &*buffers };
        if (b.struct_size as usize) < std::mem::size_of::<LspBuffers>()
            || b.buffers.is_null()
            || b.count == 0
            || b.count as usize > crate::wire::MAX_RING
        {
            set_error("lsp_offer_buffers: a bad ring description");
            return LSP_ERR_ARG;
        }
        if b.format != LSP_FORMAT_RGBA8_SRGB {
            set_error("lsp_offer_buffers: unknown format");
            return LSP_ERR_ARG;
        }
        // SAFETY: `count` entries by contract.
        let list = unsafe { std::slice::from_raw_parts(b.buffers, b.count as usize) };
        let descs = list
            .iter()
            .enumerate()
            .map(|(i, d)| BufferDesc {
                alloc_size: d.alloc_size,
                memory_type: d.memory_type,
                offset: d.offset,
                row_pitch: d.row_pitch,
                modifier: d.modifier,
                handle: i as u64,
            })
            .collect();
        let fds: Vec<i32> = list.iter().map(|d| d.fd).collect();
        match p.producer.offer_buffers(
            Buffers {
                generation: b.generation,
                width: b.width,
                height: b.height,
                format: Format::Rgba8Srgb,
                usage: b.usage,
                flags: b.flags,
                buffers: descs,
            },
            &fds,
        ) {
            Ok(()) => LSP_OK,
            Err(ProducerError::Misuse(m)) if m.contains("refused") => {
                set_error(m);
                LSP_ERR_REJECTED
            }
            Err(e) => status_of(&e),
        }
    })
}

/// Wait at most `timeout_ms` for a slot to render the next frame into: one
/// that is free and, in paced mode, asked for. `LSP_ERR_TIMEOUT` when none
/// came; call [`lsp_poll`] meanwhile for input and resizes.
///
/// # Safety
///
/// `p` came from [`lsp_connect`]; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_next_slot(
    p: *mut LspProducer,
    timeout_ms: u32,
    out: *mut LspSlot,
) -> LspStatus {
    with_producer(p, |p| {
        if out.is_null() {
            set_error("lsp_next_slot: null out");
            return LSP_ERR_ARG;
        }
        match p
            .producer
            .next_slot(Duration::from_millis(u64::from(timeout_ms)))
        {
            Ok(Some(slot)) => {
                // SAFETY: checked non-null.
                unsafe {
                    *out = LspSlot {
                        generation: slot.generation,
                        slot: slot.slot,
                    };
                }
                LSP_OK
            }
            Ok(None) => {
                set_error("no slot came in time");
                LSP_ERR_TIMEOUT
            }
            Err(e) => status_of(&e),
        }
    })
}

/// Announce a finished frame: its GPU work has completed. Writes the frame
/// number to `frame_number` when it is not null.
///
/// # Safety
///
/// `p` came from [`lsp_connect`]; `frame` points at an `LspFrame` whose
/// `meta` points at `meta_len` bytes or is null; `frame_number` is null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_frame_ready(
    p: *mut LspProducer,
    frame: *const LspFrame,
    frame_number: *mut u64,
) -> LspStatus {
    with_producer(p, |p| {
        if frame.is_null() {
            set_error("lsp_frame_ready: null frame");
            return LSP_ERR_ARG;
        }
        // SAFETY: non-null, valid by contract.
        let f = unsafe { &*frame };
        if (f.struct_size as usize) < std::mem::size_of::<LspFrame>()
            || f.meta_len as usize > crate::wire::MAX_META
        {
            set_error("lsp_frame_ready: a bad frame description");
            return LSP_ERR_ARG;
        }
        let meta = if f.meta.is_null() || f.meta_len == 0 {
            &[][..]
        } else {
            // SAFETY: `meta_len` bytes by contract.
            unsafe { std::slice::from_raw_parts(f.meta, f.meta_len as usize) }
        };
        match p.producer.frame_ready(
            Slot {
                generation: f.generation,
                slot: f.slot,
            },
            meta,
            FrameTimes {
                sim: f.t_sim,
                submit: f.t_submit,
                gpu_done: f.t_gpu_done,
                input_seq: (f.input_seq != 0).then_some(f.input_seq),
            },
        ) {
            Ok(n) => {
                if !frame_number.is_null() {
                    // SAFETY: non-null and writable by contract.
                    unsafe { *frame_number = n };
                }
                LSP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// Give back a slot [`lsp_next_slot`] handed out, without a frame in it.
///
/// # Safety
///
/// `p` came from [`lsp_connect`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_abandon_slot(p: *mut LspProducer, slot: LspSlot) -> LspStatus {
    with_producer(p, |p| {
        p.producer.abandon(Slot {
            generation: slot.generation,
            slot: slot.slot,
        });
        LSP_OK
    })
}

/// Take the next event without waiting; `out->kind` is `LSP_EVENT_NONE`
/// when nothing is waiting. Its `text` and `data` stay valid until the next
/// call on `p`.
///
/// # Safety
///
/// `p` came from [`lsp_connect`]; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_poll(p: *mut LspProducer, out: *mut LspEvent) -> LspStatus {
    with_producer(p, |p| {
        if out.is_null() {
            set_error("lsp_poll: null out");
            return LSP_ERR_ARG;
        }
        let event = match p.producer.poll_event() {
            Ok(e) => e,
            Err(e) => return status_of(&e),
        };
        let mut e = blank_event();
        if let Some(event) = event {
            fill_event(p, event, &mut e);
        }
        // SAFETY: checked non-null.
        unsafe { *out = e };
        LSP_OK
    })
}

fn blank_event() -> LspEvent {
    LspEvent {
        struct_size: std::mem::size_of::<LspEvent>() as u32,
        kind: LSP_EVENT_NONE,
        seq: 0,
        time: 0,
        input_kind: 0,
        width: 0,
        height: 0,
        scale: 0.0,
        generation: 0,
        flag: 0,
        x: 0.0,
        y: 0.0,
        dx: 0.0,
        dy: 0.0,
        buttons: 0,
        pointer_id: 0,
        pointer_type: 0,
        button: -1,
        pressure: 0.0,
        tilt_x: 0.0,
        tilt_y: 0.0,
        twist: 0.0,
        wheel_unit: 0,
        key_code: 0,
        pressed: 0,
        repeat: 0,
        modifiers: 0,
        reason: 0,
        reserved0: 0,
        reserved1: 0,
        text: std::ptr::null(),
        data: std::ptr::null(),
        data_len: 0,
    }
}

fn set_text(p: &mut LspProducer, text: &str, e: &mut LspEvent) {
    p.text = CString::new(text.replace('\0', " ")).unwrap_or_default();
    e.text = p.text.as_ptr();
}

fn fill_event(p: &mut LspProducer, event: ProducerEvent, e: &mut LspEvent) {
    match event {
        ProducerEvent::Resize {
            width,
            height,
            scale,
        } => {
            e.kind = LSP_EVENT_RESIZE;
            (e.width, e.height, e.scale) = (width, height, scale);
        }
        ProducerEvent::Input { seq, time, event } => {
            e.kind = LSP_EVENT_INPUT;
            (e.seq, e.time) = (seq, time);
            match event {
                InputEvent::PointerMove(i)
                | InputEvent::PointerDown(i)
                | InputEvent::PointerUp(i) => {
                    e.input_kind = match event {
                        InputEvent::PointerMove(_) => LSP_INPUT_POINTER_MOVE,
                        InputEvent::PointerDown(_) => LSP_INPUT_POINTER_DOWN,
                        _ => LSP_INPUT_POINTER_UP,
                    };
                    (e.x, e.y, e.dx, e.dy) = (i.x, i.y, i.dx, i.dy);
                    (e.buttons, e.pointer_id) = (i.buttons, i.pointer_id);
                    (e.pointer_type, e.button) = (i.pointer_type, i.button);
                    (e.pressure, e.tilt_x, e.tilt_y, e.twist) =
                        (i.pressure, i.tilt_x, i.tilt_y, i.twist);
                }
                InputEvent::Wheel { x, y, dx, dy, unit } => {
                    e.input_kind = LSP_INPUT_WHEEL;
                    (e.x, e.y, e.dx, e.dy, e.wheel_unit) = (x, y, dx, dy, unit);
                }
                InputEvent::Key {
                    code,
                    pressed,
                    repeat,
                    modifiers,
                    key,
                } => {
                    e.input_kind = LSP_INPUT_KEY;
                    e.key_code = code;
                    e.pressed = u8::from(pressed);
                    e.repeat = u8::from(repeat);
                    e.modifiers = modifiers;
                    set_text(p, &key, e);
                }
                InputEvent::Text(text) => {
                    e.input_kind = LSP_INPUT_TEXT;
                    set_text(p, &text, e);
                }
                InputEvent::Focus(on) => {
                    e.input_kind = LSP_INPUT_FOCUS;
                    e.flag = u32::from(on);
                }
                InputEvent::Lock(on) => {
                    e.input_kind = LSP_INPUT_LOCK;
                    e.flag = u32::from(on);
                }
                InputEvent::Leave => e.input_kind = LSP_INPUT_LEAVE,
            }
        }
        ProducerEvent::Visibility(visible) => {
            e.kind = LSP_EVENT_VISIBILITY;
            e.flag = u32::from(visible);
        }
        ProducerEvent::User(bytes) => {
            e.kind = LSP_EVENT_USER;
            p.data = bytes;
            e.data = p.data.as_ptr();
            e.data_len = p.data.len() as u64;
        }
        ProducerEvent::Reset => e.kind = LSP_EVENT_RESET,
        ProducerEvent::Retired(generation) => {
            e.kind = LSP_EVENT_RETIRED;
            e.generation = generation;
        }
        ProducerEvent::BuffersRejected { generation, detail } => {
            e.kind = LSP_EVENT_BUFFERS_REJECTED;
            e.generation = generation;
            set_text(p, &detail, e);
        }
        ProducerEvent::Closed { reason, detail } => {
            e.kind = LSP_EVENT_CLOSED;
            e.reason = reason.map_or(0, |r| r as u32);
            set_text(p, &detail, e);
        }
    }
}

/// Ask for the cursor over the surface (`LSP_CURSOR_*`).
///
/// # Safety
///
/// `p` came from [`lsp_connect`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_set_cursor(p: *mut LspProducer, keyword: u16) -> LspStatus {
    with_producer(p, |p| match p.producer.set_cursor(keyword) {
        Ok(()) => LSP_OK,
        Err(e) => status_of(&e),
    })
}

/// Ask for the pointer to be locked to the surface (`wanted` 1) or let go
/// (0). An `LSP_INPUT_LOCK` input says when the lock took effect.
///
/// # Safety
///
/// `p` came from [`lsp_connect`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_request_pointer_lock(p: *mut LspProducer, wanted: u8) -> LspStatus {
    with_producer(p, |p| match p.producer.request_pointer_lock(wanted != 0) {
        Ok(()) => LSP_OK,
        Err(e) => status_of(&e),
    })
}

/// Send `len` bytes to the consumer's application, at most 61440.
///
/// # Safety
///
/// `p` came from [`lsp_connect`]; `bytes` points at `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_send_user(
    p: *mut LspProducer,
    bytes: *const u8,
    len: u32,
) -> LspStatus {
    with_producer(p, |p| {
        if bytes.is_null() && len > 0 {
            set_error("lsp_send_user: null bytes");
            return LSP_ERR_ARG;
        }
        let slice = if len == 0 {
            &[][..]
        } else {
            // SAFETY: `len` bytes by contract.
            unsafe { std::slice::from_raw_parts(bytes, len as usize) }
        };
        match p.producer.send_user(slice) {
            Ok(()) => LSP_OK,
            Err(e) => status_of(&e),
        }
    })
}

/// End the session and free the producer. Null is ignored.
///
/// # Safety
///
/// `p` came from [`lsp_connect`] and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_close(p: *mut LspProducer) {
    if p.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: from `lsp_connect` (Box::into_raw), released once.
        let boxed = unsafe { Box::from_raw(p) };
        boxed.producer.close(ByeReason::Exit, "the producer closed");
    }));
}

/// Read the consumer's GPU from an endpoint string: the Vulkan device and
/// driver UUIDs to pick a physical device by before making the producer's
/// own device. `LSP_ERR_ARG` when the string names none (the consumer's
/// renderer had not started when it was made); `LSP_ERR_ENDPOINT` when it
/// does not parse.
///
/// # Safety
///
/// `endpoint` is a NUL-terminated string; `device_uuid` and `driver_uuid`
/// point at 16 writable bytes each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lsp_endpoint_adapter(
    endpoint: *const c_char,
    device_uuid: *mut u8,
    driver_uuid: *mut u8,
) -> LspStatus {
    guard(|| {
        if endpoint.is_null() || device_uuid.is_null() || driver_uuid.is_null() {
            set_error("lsp_endpoint_adapter: a null argument");
            return LSP_ERR_ARG;
        }
        // SAFETY: non-null, NUL-terminated by contract.
        let text = unsafe { CStr::from_ptr(endpoint) }.to_string_lossy();
        let parsed: crate::endpoint::Endpoint = match text.parse() {
            Ok(e) => e,
            Err(e) => {
                set_error(e);
                return LSP_ERR_ENDPOINT;
            }
        };
        match parsed.adapter {
            Some(crate::endpoint::AdapterHint::Vulkan {
                device_uuid: device,
                driver_uuid: driver,
            }) => {
                // SAFETY: 16 writable bytes each, by contract.
                unsafe {
                    std::ptr::copy_nonoverlapping(device.as_ptr(), device_uuid, 16);
                    std::ptr::copy_nonoverlapping(driver.as_ptr(), driver_uuid, 16);
                }
                LSP_OK
            }
            None => {
                set_error("the endpoint names no GPU");
                LSP_ERR_ARG
            }
        }
    })
}

/// The DOM `code` name of key code `n` from the key-code table, a static
/// NUL-terminated string (`"Unidentified"` for a number it lacks).
#[unsafe(no_mangle)]
pub extern "C" fn lsp_key_code_name(n: u16) -> *const c_char {
    static NAMES: std::sync::OnceLock<Vec<CString>> = std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let mut v = vec![CString::new("Unidentified").expect("no NUL")];
        v.extend(
            crate::keys::KEY_CODES
                .iter()
                .map(|(_, name)| CString::new(*name).expect("no NUL")),
        );
        v
    });
    names.get(usize::from(n)).unwrap_or(&names[0]).as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every call refuses a null producer or a null argument with a status,
    /// and leaves a message.
    #[test]
    fn nulls_are_refused_not_dereferenced() {
        unsafe {
            let mut status = 99;
            assert!(
                lsp_connect(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut status
                )
                .is_null()
            );
            assert_eq!(status, LSP_ERR_ARG);
            assert_eq!(
                lsp_next_slot(std::ptr::null_mut(), 0, std::ptr::null_mut()),
                LSP_ERR_ARG
            );
            assert_eq!(
                lsp_poll(std::ptr::null_mut(), std::ptr::null_mut()),
                LSP_ERR_ARG
            );
            assert_eq!(lsp_set_cursor(std::ptr::null_mut(), 0), LSP_ERR_ARG);
            lsp_close(std::ptr::null_mut());
            let mut buf = [0 as c_char; 64];
            assert!(lsp_last_error(buf.as_mut_ptr(), buf.len()) > 0);
        }
    }

    /// Key names come from the table.
    #[test]
    fn key_names_come_from_the_table() {
        let name = |n| {
            unsafe { CStr::from_ptr(lsp_key_code_name(n)) }
                .to_str()
                .unwrap()
        };
        assert_eq!(name(0), "Unidentified");
        assert_eq!(name(crate::keys::key_code_number("KeyW")), "KeyW");
        assert_eq!(name(u16::MAX), "Unidentified");
    }

    /// The structs keep the sizes the header states.
    #[test]
    fn struct_sizes_match_the_header() {
        assert_eq!(std::mem::size_of::<LspHello>(), 80);
        assert_eq!(std::mem::size_of::<LspWelcome>(), 40);
        assert_eq!(std::mem::size_of::<LspBuffer>(), 40);
        assert_eq!(std::mem::size_of::<LspBuffers>(), 40);
        assert_eq!(std::mem::size_of::<LspSlot>(), 8);
        assert_eq!(std::mem::size_of::<LspFrame>(), 56);
        assert_eq!(std::mem::size_of::<LspEvent>(), 136);
    }
}
