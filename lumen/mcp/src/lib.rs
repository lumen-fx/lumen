//! Runtime introspection plugin for the Lumen UI framework.
//!
//! Installs [`LumenMcpPlugin`] in a Lumen app. The plugin spins up a dedicated
//! OS thread running a tokio current-thread runtime, which binds a localhost
//! TCP listener and speaks **line-delimited JSON-RPC 2.0**. MCP clients (via
//! the companion `lumen-mcp-server` binary) connect to this socket and
//! introspect entities, components, resources, messages, and screenshots of
//! the running app.
//!
//! ## Architecture
//!
//! - Per tick, two main-world systems and one render-world system update a
//!   shared `Arc<RwLock<Snapshot>>` resource. The TCP handler only ever reads
//!   from the snapshot - it never touches the live worlds. This avoids any
//!   `!Send`/cross-thread soundness gymnastics around `World`,
//!   `taffy::TaffyTree`, the `HeadlessRenderer`, etc.
//! - Messages are drained from `MessageReader<T>` into bounded ring buffers
//!   (cap 256) for the message types listed in the plan.
//! - Screenshot: there are two paths.
//!   * **Headless** - if the render world holds a `HeadlessRenderer`
//!     non-send resource, the last frame's RGBA8 framebuffer is encoded to
//!     PNG and stored in the snapshot. Always-on; no client signalling.
//!   * **On-screen WGPU surface** - `LumenMcpPlugin` inserts a
//!     [`SurfaceCapture`](lumen_core::render_world::SurfaceCapture) into
//!     both worlds and shares an `Arc`-cloned handle with the JSON-RPC
//!     server thread. When a client calls `lumen.screenshot`, the handler
//!     sets the request flag and polls the capture's frame store for up to
//!     ~500 ms. The on-screen renderer (`lumen-window-winit`) checks the
//!     flag once per frame; when set, it copies the intermediate
//!     `Rgba8Unorm` texture to CPU, fills the frame store, and clears the
//!     flag. **No GPU readback on the no-screenshot path** - the renderer
//!     just performs one atomic load per frame.
//!
//!   We pick the "flag-on-demand" design over a permanently-up-to-date
//!   framebuffer because the latter would force one GPU->CPU copy per frame
//!   (~10 MB/frame at 1080p x 60 Hz) regardless of whether an MCP client is
//!   listening. The atomic flag costs effectively nothing in the common
//!   case.
//!
//! Why: the snapshot model is one-way (worlds -> snapshot -> TCP). Mutations
//! flow through the existing command queue if/when we add write-side tools.
//! Trying to expose live `&World` to a tokio task would cross the !Send
//! boundary of `taffy::TaffyTree`, `wgpu::Device`, and the rest of the render
//! backends. A snapshot is the only sane V1 design.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod mcp_protocol;
mod methods;
mod plugin;
mod server;
mod simulate;
mod snapshot;

pub use mcp_protocol::{MCP_PROTOCOL_VERSION, tool_name_to_legacy};
pub use plugin::{LumenMcpPlugin, McpSnapshotSchedule, McpTransport};
pub use simulate::{SimulateKind, SimulateQueue, SimulateRequest};
pub use snapshot::{
    ColorView, EntityInspect, EntityView, ExtractedRectView, ExtractedTextView, FillView,
    FocusOutlineView, InteractionView, LoadedImageView, MessageRing, RecordedClickEvent,
    RecordedFocusedKey, RecordedKeyPressed, RecordedKeyReleased, RecordedMouseWheel,
    RecordedPointerMoved, RecordedPointerPressed, RecordedPointerReleased, ShadowView, SignalView,
    SliderValueView, Snapshot, SnapshotHandle, StyleView, TextStyleView, TransformView, V2,
    ViewportView, VisualsView,
};
