# The Lumen surface protocol, version 1.1

The Lumen surface protocol lets a renderer in one process (the producer: a
game, an engine, a video decoder) show its frames in a `<surface>` element of
a Lumen app (the consumer) with no copy. The producer allocates a small ring
of GPU images, exports them as OS handles once, and from then on renders into
a free image and announces it. The consumer imports the images into its own
GPU device and shows the newest finished one in place. Input on the element
travels back.

This document is the contract. The `lumen-surface-protocol` crate beside it
implements it for both roles, in Rust and as a C library
(`include/lumen_surface_protocol.h`), and Lumen's tests hold the two
together. An engine may speak the protocol from this document alone.

Version 1.1 covers three platforms, each with its own graphics API and
transport: Linux with Vulkan over a Unix socket, Windows with Direct3D 12 over
a named pipe, and macOS with Metal over a Unix socket with Mach ports beside
it. Everything but the transport and the buffers is the same on all three.

## Roles and connection

The consumer listens; the producer connects. The consumer hands the producer
one line, the *endpoint string*, by whatever means the app likes. By
convention the producer reads it from the environment variable
`LUMEN_SURFACE`.

```text
v1;transport=unix:/run/user/1000/lumen-surface-1000/4242-9f3c0b.sock;channel=scene;token=4be1...;adapter=vk:<device uuid>:<driver uuid>;adapter-name=AMD Radeon RX 6800 XT
v1;transport=pipe:\\.\pipe\lumen-surface-4242-9f3c0b1d2e3f;channel=scene;token=4be1...;adapter=dx:<luid>;adapter-name=...
v1;transport=unix:/var/folders/.../lumen-surface-501/4242-9f3c0b.sock;mach=dev.lumenfx.surface.4242.1d2e3f4a5b6c;channel=scene;token=4be1...;adapter=mtl:<registry id>;adapter-name=Apple M2
```

- The first field is the version, `v1`. Other fields are `key=value`,
  separated by `;`. A value escapes `%`, `;` and `=` as `%25`, `%3B` and
  `%3D`. A producer skips keys it does not know.
- `transport=unix:<path>` (Linux, macOS): a Unix stream socket. The consumer
  keeps it in a directory only its user can open
  (`$XDG_RUNTIME_DIR/lumen-surface-<uid>`, or the temporary directory when
  there is no runtime directory) and refuses connections from other users.
- `mach=<service>` (macOS): the Mach bootstrap service, in the user's
  bootstrap namespace, that the producer sends a ring's IOSurface ports to
  (see "Buffers on Metal"). Present with every macOS socket.
- `transport=pipe:<name>` (Windows): a named pipe,
  `\\.\pipe\lumen-surface-<pid>-<12 hex digits>`. Its DACL grants the
  current user, and nobody else, access; it refuses remote clients. Open it
  with `CreateFileW` (`GENERIC_READ | GENERIC_WRITE`, `OPEN_EXISTING`), in
  byte mode, and retry while it answers `ERROR_PIPE_BUSY`.
- `channel`: which surface the producer feeds. One process feeding two
  surfaces connects twice, once per channel.
- `token`: 32 hex digits, a 128-bit secret the producer's `Hello` must carry.
- `adapter`: the consumer's GPU. A producer picks this device before it
  creates its own, so the two can share memory. Present once the consumer's
  renderer has started. One form per API:
  - `vk:<device uuid>:<driver uuid>`: Vulkan's
    `VkPhysicalDeviceIDProperties`, 32 hex digits each;
  - `dx:<luid>`: the Direct3D 12 adapter LUID (`ID3D12Device::GetAdapterLuid`,
    `IDXGIFactory4::EnumAdapterByLuid`), 16 hex digits: the bytes of
    `LowPart` then `HighPart`, each little-endian;
  - `mtl:<registry id>`: Metal's `MTLDevice.registryID`, 16 hex digits, most
    significant first.
- `adapter-name`: the same GPU by name, for a person or a setting that picks
  an adapter by name.

The consumer does not start the producer. An app starts it (or attaches to
one already running) and passes the endpoint string.

## Framing

Every message is a frame:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `length`: the bytes after this field |
| 4 | 2 | `type` |
| 6 | 2 | `flags`: bit 0, handles travel with this frame; bit 1, a receiver that does not know `type` skips the frame |
| 8 | ... | body |

- Integers and floats are little-endian. A frame is at most 64 KiB.
- Bodies are fixed layouts with explicit `reserved` fields (always zero),
  followed by variable parts. A variable part (`str`, `bytes`) is a `u32`
  length and that many bytes. Strings are UTF-8 without a terminator.
- Timestamps are nanoseconds on the machine's monotonic clock, which both
  processes read: `CLOCK_MONOTONIC` on Linux and macOS,
  `QueryPerformanceCounter` scaled to nanoseconds on Windows.
- An unknown `type` without bit 1 set ends the session.

## Messages

`P` is the producer, `C` the consumer. Adapter fields (`adapter` below) are
`device_uuid [16]`, `driver_uuid [16]`, `luid [8]`, `registry_id u64`,
`vendor u32`, `device u32`: 56 bytes. Fields an API does not have are zero.

| Type | Name | Direction | Body |
| --- | --- | --- | --- |
| 1 | `Hello` | P to C | `major u16`, `minor u16`, `api u16`, `reserved u16`, `pid u32`, `caps u32`, `token [16]`, `adapter`, `channel str`, `engine str` |
| 2 | `Welcome` | C to P | `session u64`, `api u16`, `pacing u16`, `ring_min u8`, `ring_max u8`, `ring_pref u8`, `reserved u8`, `width u32`, `height u32`, `scale f32`, `formats u32`, `caps u32` |
| 3 | `Reject` | C to P | `reason u16`, `api u16`, `adapter`, `detail str` |
| 4 | `Buffers` | P to C, with handles | `generation u32`, `count u32`, `width u32`, `height u32`, `format u32`, `reserved u32`, `usage u32`, `flags u32`, then `count` times: `alloc_size u64`, `memory_type u32`, `offset u32`, `row_pitch u32`, `reserved u32`, `modifier u64`, `handle u64` |
| 5 | `BuffersAccepted` | C to P | `generation u32` |
| 6 | `BuffersRejected` | C to P | `generation u32`, `reason u16`, `reserved u16`, `detail str` |
| 7 | `FrameRequest` | C to P | `seq u64`, `target_present_ns u64` |
| 8 | `FrameReady` | P to C | `frame u64`, `input_seq u64`, `t_sim u64`, `t_submit u64`, `t_gpu_done u64`, `generation u32`, `slot u32`, `meta bytes` (at most 4096) |
| 9 | `Release` | C to P | `frame u64`, `generation u32`, `slot u32` |
| 10 | `Resize` | C to P | `width u32`, `height u32`, `scale f32`, `reserved u32` |
| 11 | `RetireGeneration` | C to P | `generation u32` |
| 12 | `Input` | C to P | `seq u64`, `time u64`, `kind u16`, `reserved u16`, then the event (below) |
| 13 | `Visibility` | C to P | `visible u8` |
| 14 | `Cursor` | P to C | `keyword u16` |
| 15 | `PointerLock` | P to C | `wanted u8` |
| 16 | `User` | either | `bytes` (at most 61440) |
| 17 | `Ping` | either | `t u64` |
| 18 | `Pong` | either | `t u64` |
| 19 | `Reset` | C to P | `device_generation u64` |
| 20 | `Bye` | either | `reason u16`, `detail str` |

Enumerations:

- `api`: 1 Vulkan, 2 Direct3D 12, 3 Metal.
- `format`: 1 `RGBA8_SRGB` (`VK_FORMAT_R8G8B8A8_SRGB`,
  `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB`), 2 `BGRA8_SRGB`
  (`MTLPixelFormatBGRA8Unorm_sRGB` on an IOSurface of pixel format `'BGRA'`).
  A ring is `RGBA8_SRGB` on Vulkan and Direct3D 12 and `BGRA8_SRGB` on Metal.
  `Welcome.formats` has bit `1 << format` set for each format the consumer
  takes.
- `pacing`: 0 paced, 1 free.
- `Reject.reason`: 1 token, 2 version, 3 api, 4 adapter, 5 channel,
  6 unsupported (the consumer has no GPU to import into), 7 busy (another
  producer is connected on the channel).
- `BuffersRejected.reason`: 1 import failed, 2 bad description, 3 too large.
- `Bye.reason`: 1 exit, 2 device lost, 3 protocol error, 4 closed (the
  consumer's element went away, or its app ended the session).
- `Buffers.flags`: bit 0, each buffer is a dedicated allocation.
- `Cursor.keyword`: 0 `default`, 1 `none`, 2 `pointer`, 3 `crosshair`,
  4 `move`, 5 `grab`, 6 `grabbing`, 7 `text`, 8 `wait`, 9 `not-allowed`.

Input events, by `kind`:

| Kind | Event | Body |
| --- | --- | --- |
| 1 | pointer move | pointer |
| 2 | pointer down | pointer |
| 3 | pointer up | pointer |
| 4 | wheel | `x f32`, `y f32`, `dx f32`, `dy f32`, `unit u16` (0 pixels, 1 lines), `reserved u16` |
| 5 | key | `code u16`, `pressed u8`, `repeat u8`, `modifiers u16`, `reserved u16`, `key str` |
| 6 | text | `text str` |
| 7 | focus | `focused u8` |
| 8 | lock | `locked u8` |
| 9 | leave | nothing |

A pointer body is `x f32`, `y f32`, `dx f32`, `dy f32`, `buttons u32`,
`pointer_id u32`, `pointer_type u16` (0 mouse, 1 pen, 2 touch), `button i16`
(the DOM `button` that changed, -1 for none), `pressure f32`, `tilt_x f32`,
`tilt_y f32`, `twist f32`. Positions are in the frame's texture pixels: the
consumer has already undone where the element is, how the frame fits it and
the display's pixel ratio. `dx`, `dy` are the motion since the previous move,
also in texture pixels; under a pointer lock the position stays put and they
carry the device's raw motion.

A key's `code` is where it sits, numbered from the key-code table below;
`key` is what it means under the user's layout (the DOM `key`). `modifiers`
bits: 1 shift, 2 control, 4 alt, 8 meta.

## The session

### Handshake

1. P sends `Hello` with the token from the endpoint string, the channel, the
   API it renders with, and its GPU.
2. C checks the token, the major version, the channel, the API and the GPU
   (device and driver UUIDs on Vulkan, the adapter LUID on Direct3D 12, the
   registry id on Metal; `Hello.adapter` fields an API does not have are
   zero). Any mismatch is answered with
   `Reject`, which carries the consumer's own GPU so the producer can say
   which one to start on; then the consumer closes the connection. Otherwise
   C answers `Welcome`: the frame size it wants, the pacing, the ring sizes it
   takes (`ring_min` to `ring_max`, `ring_pref` preferred) and the formats.
3. P allocates a ring at that size, exports it, and sends `Buffers` with
   generation 1.
4. C imports every buffer and answers `BuffersAccepted`, or
   `BuffersRejected` with the reason. The answer can take a while: the
   import and the first-use work on the images (see "Ownership and layout")
   both wait on C's GPU, which may be busy with C's own frames. C keeps serving the connection meanwhile, pinging
   a quiet producer as usual, so P waits for the answer for as long as C
   keeps sending, and gives up only after five seconds in which C sent
   nothing at all.

The producer allocates and exports; the consumer imports.

### Buffers on Vulkan

Each buffer is a `VkImage` created with exactly these parameters, which the
consumer repeats to import it:

- `VK_IMAGE_TYPE_2D`, the format of `Buffers.format`, `width` x `height` x 1,
  one mip level, one layer, one sample, `VK_IMAGE_TILING_OPTIMAL`,
  `VK_SHARING_MODE_EXCLUSIVE`, no create flags;
- `usage` exactly as `Buffers.usage` says; it must include
  `VK_IMAGE_USAGE_SAMPLED_BIT`;
- `VkExternalMemoryImageCreateInfo` with
  `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT`.

Its memory is allocated with `VkExportMemoryAllocateInfo` (`OPAQUE_FD`) and,
when `Buffers.flags` bit 0 is set, `VkMemoryDedicatedAllocateInfo` for the
image. `alloc_size` and `memory_type` are the allocation's size and memory
type index; `offset` is where the image is bound in it. The handle is the
file descriptor `vkGetMemoryFdKHR` returns. Descriptors travel as
`SCM_RIGHTS` ancillary data on the `sendmsg` that carries the frame's first
byte, in buffer order; each buffer's `handle` is its index among them.
`modifier` and `row_pitch` are zero.

Both sides must run on the same physical device and driver, which the
handshake checks. Each side keeps its own reference to the memory, so a
buffer stays valid for the consumer after the producer exits.

#### Ownership and layout

The images are `VK_SHARING_MODE_EXCLUSIVE`, so each side takes an image from
the other with a queue family ownership transfer through
`VK_QUEUE_FAMILY_EXTERNAL`. Between the processes an image is in
`VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL`, and a transfer never changes the
layout: the release and the acquire both name that layout as `oldLayout` and
`newLayout`, so neither side has to know the layout the other works in. A
layout change is a barrier of its own, on the side's own queue family.

- **P, each frame.** After drawing: a barrier into
  `SHADER_READ_ONLY_OPTIMAL` on P's family, then the release, P's family to
  `VK_QUEUE_FAMILY_EXTERNAL`. P waits for the GPU to finish both, then sends
  `FrameReady`.
- **C, on `FrameReady`.** The acquire, `VK_QUEUE_FAMILY_EXTERNAL` to C's
  family, before anything samples the image.
- **C, giving a slot back.** Once the GPU work that sampled the image has
  completed: the release, C's family to `VK_QUEUE_FAMILY_EXTERNAL`. C waits
  for the GPU to finish it, then sends `Release`. A frame replaced before it
  was shown is acquired and released the same way.
- **P, reusing a slot.** Before drawing into a released slot: the acquire,
  `VK_QUEUE_FAMILY_EXTERNAL` to P's family, then a barrier from
  `SHADER_READ_ONLY_OPTIMAL` into the layout P draws in.
- **First use.** Between importing a ring and sending `BuffersAccepted`, C
  may move its images out of `VK_IMAGE_LAYOUT_UNDEFINED` on its own family,
  and waits for the GPU to finish that before it answers; a renderer that tracks layouts needs to, so
  it never makes that transition later over a frame P drew. C touches the
  images in no other way before the first `FrameReady` that names them. P
  draws into each image the first time from `VK_IMAGE_LAYOUT_UNDEFINED`,
  with no acquire: the image holds nothing to keep yet.

The waits are what order the two GPUs' work: each side finishes its barriers
before the message that hands the image over, so no semaphore crosses the
processes.

### Buffers on Direct3D 12

Each buffer is a committed resource the producer creates with
`ID3D12Device::CreateCommittedResource` on a `D3D12_HEAP_TYPE_DEFAULT` heap
with `D3D12_HEAP_FLAG_SHARED`, in state `D3D12_RESOURCE_STATE_COMMON`:

- `D3D12_RESOURCE_DIMENSION_TEXTURE2D`, `width` x `height`, one array layer,
  one mip level, one sample, `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB`,
  `D3D12_TEXTURE_LAYOUT_UNKNOWN`;
- flags `D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS` and whatever the
  producer draws with (`ALLOW_RENDER_TARGET`, `ALLOW_UNORDERED_ACCESS`);
  `Buffers.usage` is these `D3D12_RESOURCE_FLAGS`.

The producer exports each with `ID3D12Device::CreateSharedHandle`
(`GENERIC_ALL`, no name) and puts the handle's value in the buffer's `handle`.
The handle does not travel: the consumer copies it out of the producer's
process with `DuplicateHandle` (`DUPLICATE_SAME_ACCESS`), from the process the
pipe names as its client (`GetNamedPipeClientProcessId`), and opens it with
`ID3D12Device::OpenSharedHandle`. So the producer keeps every handle open until
`BuffersAccepted` or `BuffersRejected` for its ring has arrived, and may close
them then; the consumer's resources keep the memory alive. Nothing is named,
so no other process can open the frames. `alloc_size`, `memory_type`,
`offset`, `row_pitch` and `modifier` are zero, and so is `Buffers.flags`.

Both sides must run on the same adapter, which the handshake checks by LUID.

`ALLOW_SIMULTANEOUS_ACCESS` is what lets two devices that each track resource
state on their own agree on it: such a resource decays to `COMMON` after every
`ExecuteCommandLists` that used it and is promoted implicitly on its next use,
so no barrier crosses the processes, and the image is never compressed. The
order of the two GPUs' work comes from the messages, as on every API: each
side waits for its GPU to finish its work on an image before the message that
hands the image over.

### Buffers on Metal

Each buffer is an IOSurface the producer creates with `IOSurfaceCreate`:
`width` x `height`, 4 bytes per element, pixel format `'BGRA'`
(`0x42475241`), no other plane. The producer draws into it through a texture
made with `-[MTLDevice newTextureWithDescriptor:iosurface:plane:]`
(`MTLPixelFormatBGRA8Unorm_sRGB`, plane 0); `Buffers.usage` is that texture's
`MTLTextureUsage`, and must include `MTLTextureUsageShaderRead`. The consumer
makes its own texture on the same IOSurface the same way.

A Mach port cannot travel over a Unix socket, so a ring's buffers go beside
it. Before it sends `Buffers`, the producer looks up the endpoint's `mach`
service (`bootstrap_look_up`) and sends it one Mach message carrying, in
buffer order, a send right per buffer from `IOSurfaceCreateMachPort`:

- a complex message (`MACH_MSGH_BITS_COMPLEX`), one port descriptor per
  buffer with disposition `MACH_MSG_TYPE_COPY_SEND`;
- after the descriptors, three `u32`s: `0x4c535052` (`'LSPR'`), the ring's
  `generation`, and the number of ports.

Each buffer's `handle` is its index among the ports. The consumer receives the
message with an audit trailer and takes it as the ring's only when the sender's
process id there is the process at the other end of the socket
(`LOCAL_PEERPID`) and the generation matches; it finds each surface with
`IOSurfaceLookupFromMachPort`. Rings from any other process are ignored. The
producer may release its own send rights once the message is sent. The
consumer keeps the IOSurfaces alive on its own. `alloc_size`, `memory_type`,
`offset`, `row_pitch`, `modifier` and `Buffers.flags` are zero.

Both sides should run on the same GPU, which the handshake checks by registry
id. Each side waits for its command buffers on an image to complete before the
message that hands the image over; no event crosses the processes.

### Frames

- P renders into a free slot of the newest generation, waits until its GPU
  has finished the frame's work, then sends `FrameReady`. Only finished
  frames are announced, so the consumer never reads a half-drawn image.
- C shows the newest frame at its next redraw. When it no longer reads a
  slot (a newer frame replaced it and the GPU work that sampled it has
  completed), it sends `Release` for it. A slot whose frame was replaced
  before it was ever shown is released at once.
- P never writes a slot it has announced until it is released.
- `input_seq` names the newest `Input` the frame reflects, which is how the
  consumer measures input latency. `meta` is the producer's own bytes about
  the frame (a camera, for instance); the consumer hands them to its app
  with the frame they belong to.

### Pacing

- *Paced* (the default): C sends `FrameRequest` when it wants a frame, at
  most one outstanding at a time, and P renders one frame per request. The
  consumer asks once per frame it shows while the surface can be seen, and
  again when input arrives or its app asks, so the producer renders at the
  consumer's display rate and stops when nobody looks. C sends the input it
  has before the request that follows it, so a producer that starts a frame
  as soon as it is asked draws that input into it.
- *Free*: P renders at its own rate into a ring of up to four; C shows the
  newest and releases the rest. No requests are sent.
- `Visibility`: C says when nobody can see the surface (hidden, scrolled
  out, its window covered or minimized) and when somebody can again. In
  either mode P may stop rendering while it is not visible.

### Resize

C sends `Resize` when the size it wants changes, at most once per frame it
shows. An element that is hidden and has no size sends only `Visibility`; the
size it wants stays what it was. P allocates a new ring at the next generation and sends `Buffers`.
C keeps showing the old generation's newest frame until the first frame of
the new one arrives. Once every slot of an older generation is back and none
will be shown again, C sends `RetireGeneration` and P frees that ring.

### Input

C sends `Input` in the order things happened. Pointer events reach the
producer when they land on the surface itself; children of the element
(overlays the app draws over the frame) take them first. A press keeps the
pointer for the surface until the buttons come up. Keys reach the producer
while the surface has keyboard focus; the consumer keeps Tab and its menu
shortcuts, except under a pointer lock, when it keeps only Escape. `focus`
with 0 means the producer must release every key and button it holds.

`Cursor` asks for the cursor shown over the surface. `PointerLock` asks the
consumer to lock the pointer to the surface; the consumer grants it only the
way a browser does (to a focused surface, during or just after a press) and
reports what took effect with a `lock` input. Escape always ends a lock.

### Liveness, crashes and reconnects

- A side that has heard nothing for a second sends `Ping`; the other answers
  `Pong` with the same `t`. A producer must read its socket at least once a
  second. A producer that answers nothing for three seconds after a ping is
  hung, and the consumer ends the session.
- A `FrameRequest` unanswered past the consumer's stall timeout makes the
  surface *stalled*; the next frame resumes it.
- End of stream without `Bye` is a crash. The consumer keeps showing the
  last frame and listens for a new producer on the same channel, which goes
  through the handshake again.
- When the consumer's device changes, it drops every import and sends
  `Reset`; the producer offers a new ring at the next generation on the same
  connection.
- Either side ends a session with `Bye`.

## Versions

The major version changes when a message's layout does. A consumer refuses a
producer on another major. Minor versions add message types, capability bits
and enumeration values; a message of a type the receiver does not know is
skipped when its sender set flag bit 1, and ends the session otherwise.

- 1.0: Linux, Vulkan.
- 1.1: Windows with Direct3D 12 over a named pipe, macOS with Metal over a
  Unix socket and Mach ports; the `BGRA8_SRGB` format; the `dx` and `mtl`
  adapter forms and the `pipe` transport and `mach` key of the endpoint
  string.

## Key codes

Key codes number the DOM `KeyboardEvent.code` names. 0 is a key the platform
did not place (`Unidentified`). The library's `lsp_key_code_name` returns
the name for a number.

| Code | Name | Code | Name | Code | Name | Code | Name |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Backquote | 2 | Backslash | 3 | BracketLeft | 4 | BracketRight |
| 5 | Comma | 6 | Digit0 | 7 | Digit1 | 8 | Digit2 |
| 9 | Digit3 | 10 | Digit4 | 11 | Digit5 | 12 | Digit6 |
| 13 | Digit7 | 14 | Digit8 | 15 | Digit9 | 16 | Equal |
| 17 | IntlBackslash | 18 | IntlRo | 19 | IntlYen | 20 | KeyA |
| 21 | KeyB | 22 | KeyC | 23 | KeyD | 24 | KeyE |
| 25 | KeyF | 26 | KeyG | 27 | KeyH | 28 | KeyI |
| 29 | KeyJ | 30 | KeyK | 31 | KeyL | 32 | KeyM |
| 33 | KeyN | 34 | KeyO | 35 | KeyP | 36 | KeyQ |
| 37 | KeyR | 38 | KeyS | 39 | KeyT | 40 | KeyU |
| 41 | KeyV | 42 | KeyW | 43 | KeyX | 44 | KeyY |
| 45 | KeyZ | 46 | Minus | 47 | Period | 48 | Quote |
| 49 | Semicolon | 50 | Slash | 51 | AltLeft | 52 | AltRight |
| 53 | Backspace | 54 | CapsLock | 55 | ContextMenu | 56 | ControlLeft |
| 57 | ControlRight | 58 | Enter | 59 | MetaLeft | 60 | MetaRight |
| 61 | ShiftLeft | 62 | ShiftRight | 63 | Space | 64 | Tab |
| 65 | Convert | 66 | KanaMode | 67 | Lang1 | 68 | Lang2 |
| 69 | Lang3 | 70 | Lang4 | 71 | Lang5 | 72 | NonConvert |
| 73 | Delete | 74 | End | 75 | Help | 76 | Home |
| 77 | Insert | 78 | PageDown | 79 | PageUp | 80 | ArrowDown |
| 81 | ArrowLeft | 82 | ArrowRight | 83 | ArrowUp | 84 | NumLock |
| 85 | Numpad0 | 86 | Numpad1 | 87 | Numpad2 | 88 | Numpad3 |
| 89 | Numpad4 | 90 | Numpad5 | 91 | Numpad6 | 92 | Numpad7 |
| 93 | Numpad8 | 94 | Numpad9 | 95 | NumpadAdd | 96 | NumpadBackspace |
| 97 | NumpadClear | 98 | NumpadClearEntry | 99 | NumpadComma | 100 | NumpadDecimal |
| 101 | NumpadDivide | 102 | NumpadEnter | 103 | NumpadEqual | 104 | NumpadHash |
| 105 | NumpadMemoryAdd | 106 | NumpadMemoryClear | 107 | NumpadMemoryRecall | 108 | NumpadMemoryStore |
| 109 | NumpadMemorySubtract | 110 | NumpadMultiply | 111 | NumpadParenLeft | 112 | NumpadParenRight |
| 113 | NumpadStar | 114 | NumpadSubtract | 115 | Escape | 116 | F1 |
| 117 | F2 | 118 | F3 | 119 | F4 | 120 | F5 |
| 121 | F6 | 122 | F7 | 123 | F8 | 124 | F9 |
| 125 | F10 | 126 | F11 | 127 | F12 | 128 | F13 |
| 129 | F14 | 130 | F15 | 131 | F16 | 132 | F17 |
| 133 | F18 | 134 | F19 | 135 | F20 | 136 | F21 |
| 137 | F22 | 138 | F23 | 139 | F24 | 140 | Fn |
| 141 | FnLock | 142 | PrintScreen | 143 | ScrollLock | 144 | Pause |
| 145 | BrowserBack | 146 | BrowserFavorites | 147 | BrowserForward | 148 | BrowserHome |
| 149 | BrowserRefresh | 150 | BrowserSearch | 151 | BrowserStop | 152 | Eject |
| 153 | LaunchApp1 | 154 | LaunchApp2 | 155 | LaunchMail | 156 | MediaPlayPause |
| 157 | MediaSelect | 158 | MediaStop | 159 | MediaTrackNext | 160 | MediaTrackPrevious |
| 161 | Power | 162 | Sleep | 163 | AudioVolumeDown | 164 | AudioVolumeMute |
| 165 | AudioVolumeUp | 166 | WakeUp | 167 | Hyper | 168 | Super |
| 169 | Turbo | 170 | Abort | 171 | Resume | 172 | Suspend |
| 173 | Again | 174 | Copy | 175 | Cut | 176 | Find |
| 177 | Open | 178 | Paste | 179 | Props | 180 | Select |
| 181 | Undo | 182 | Hiragana | 183 | Katakana |  |  |
