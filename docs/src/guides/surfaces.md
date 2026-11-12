# Showing another renderer's frames

A `<surface>` shows frames that another renderer draws on the GPU: a game or
an engine running in its own process, an engine linked into your app, a video
decoder. The frames are shared with your app's renderer, not copied, and each
one is shown in place beneath the window's other content. An editor built
around a game viewport costs nothing extra per game frame while the editor
itself is still, and input on the element goes back to the game.

## Quick start

Declare the module and put the element where the frames should show:

```toml
[dependencies]
lumen-surface = { bundled = true }
```

```lmn
<surface id="scene" />
```

```css
#scene { width: 100%; height: 100%; }
```

Then start the producer with the surface's endpoint string, which tells it
where to connect, which channel to feed and the secret it must present:

```rust
fn on_ready() {
    let endpoint = surface::endpoint("scene");
    process::start("./game", [], "game",
        process::StartOptions { env: {"LUMEN_SURFACE": endpoint}, .. });
}
```

Lumen does not start the producer itself: your app decides when to run it,
and a producer already running can be handed the endpoint any other way.

## Writing a producer

The producer speaks the Lumen surface protocol. It allocates a small ring of
GPU images, exports them, and hands them over once; from then on it renders
into an image the surface has given back, waits for its own GPU to finish, and
announces the frame. The specification is
[`std/surface/protocol/SPEC.md`](https://github.com/lumen-fx/lumen/blob/main/std/surface/protocol/SPEC.md).

The producer renders with the graphics API your app's renderer uses on that
platform, and exports its images the way the specification says for it:

| Platform | API | Images | Format |
| --- | --- | --- | --- |
| Linux | Vulkan | memory exported as `OPAQUE_FD`; each image changes hands with a queue family ownership transfer ("Ownership and layout") | RGBA |
| Windows | Direct3D 12 | committed resources on a shared heap with `ALLOW_SIMULTANEOUS_ACCESS`, exported with `CreateSharedHandle` | RGBA |
| macOS | Metal | IOSurfaces, sent as the ports `IOSurfaceCreateMachPort` gives | BGRA |

- A Rust engine depends on the `lumen-surface-protocol` crate, which carries
  the connection, the handshake and the bookkeeping, and does its own GPU
  work.
- An engine in C or C++ links the same crate's C library and includes
  `lumen_surface_protocol.h`.
  [`producer/reference.c`](https://github.com/lumen-fx/lumen/blob/main/std/surface/protocol/producer/reference.c)
  is a complete producer in plain C and Vulkan: it is the shortest path to
  your own on Linux.
  [`tests/producer/mod.rs`](https://github.com/lumen-fx/lumen/blob/main/std/surface/tests/producer/mod.rs)
  is the same producer in Rust for Direct3D 12 and Metal.

The producer must render on the same GPU as your app. The endpoint string
names it, and a producer started on another one is refused with the reason.

## Size, pacing and input

The surface asks for frames at the size of its box in device pixels, so each
frame pixel lands on one screen pixel, and asks again when the box changes;
`surfaceresize` says when it did. Until the new frames arrive the surface
shows the last one, placed by `object-fit`. A surface with no box
(`display: none`) keeps the size it asked for last, so the producer's frames
fit again as soon as it shows.

By default the producer renders one frame for each frame the surface shows,
and none while nobody can see the surface: a hidden element, a covered or
minimized window. `pacing="free"` lets it render at its own rate instead,
with the surface showing the newest frame. `surface::request_frame(id)` asks
for a frame now, so a still scene shows an edit your app made.

Pointer and key input on the surface goes to the producer, with positions in
the frame's own pixels. Children of the surface (labels, selection boxes, a
toolbar) take the pointer first; give them `pointer-events: none` to let it
through. While the surface has focus its keys go to the producer, except Tab
and the shortcuts your app answers app-wide (the menu bar's, and its
[commands'](os-integration.md#commands-and-shortcuts)); `keyboard="all"` sends
those too, and then the app's shortcuts leave the surface's keys alone. A game that
takes the mouse asks for the pointer lock, which the surface grants when it
has focus and was just clicked; Escape always gives the mouse back. When the
surface loses focus the producer hears it and lets go of every key and button
it held.

### From the window to the frame

Events on the surface element say where the pointer was in window and
element coordinates, not in the frame's pixels. `surface::to_texture(id, x,
y)` maps a window position into the frame: it undoes where the element is
drawn (scrolled with its containers), its `object-fit` and the device pixel
ratio, and returns `[x, y]` in frame pixels, or an empty list before the
first frame. A point outside the drawn frame maps outside its width and
height, so check the range before you use it. This is how a drop lands on
what is under it in the scene:

```rust
fn on_scene_drop(ev: int) {
    let at = surface::to_texture("scene", lumen::event_client_x(ev), lumen::event_client_y(ev));
    if at.len() == 2 {
        let asset = lumen::event_get_data(ev, "application/x-asset");
        surface::send("scene", "place " + asset + " " + string(at[0]) + " " + string(at[1]));
    }
}
```

See [Drag and drop](os-integration.md#drag-and-drop) for the drop event. A
Rust app calls `Surfaces::to_texture(id, position)`.

## States and events

The element's `state` attribute and its class say where it stands:
`waiting` (`surface-waiting`), `live`, `stalled`, `lost` and `unsupported`.
Style them like any class:

```css
surface.surface-waiting { background: #202020; }
surface.surface-lost { border: 2px solid #c04040; }
```

When the producer exits or crashes the surface keeps its last frame up and
waits for a new producer on the same channel, which takes over without your
app doing anything. Events on the element report each step:
`surfaceconnect`, `surfaceresize`, `surfacestall`, `surfaceresume`,
`surfacedisconnect`, `surfaceerror` and `surfacemessage`. See
[`<surface>`](../reference/tags.md#surface) for what each carries.

`surface::send(id, message)` and `surfacemessage` carry messages between your
app and the producer's, for an engine with no other channel to it.

## An engine in your app

A Rust app that links an engine in shows its frames without a second
process: `Surfaces.loopback(id)` gives the renderer's own device, and the
engine presents the textures it renders there. See
[Surfaces](../reference/ffi.md#surfaces) in the Rust SDK reference.

## Limits

- Shared frames need the GPU renderer. Under the CPU renderer a surface is
  `unsupported`, and in a web build it is a placeholder.
- A producer in another process renders with the same API as your app's
  renderer: Vulkan on Linux, Direct3D 12 on Windows, Metal on macOS. A
  producer on another API is refused with the reason `api`.
- The frame is shown as it is: opaque, upright and unblended. Clipping by an
  ancestor and content painted over the surface work; under an `opacity`
  below 1 or a `mask-image` the frame is not drawn, and `surfaceerror` says
  why.
