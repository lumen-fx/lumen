//! Frames another renderer draws on the GPU, shown in a Lumen app with no
//! copy.
//!
//! The `<surface>` element shows the frames of a producer: a game in another
//! process, an engine linked into the app, a video decoder. The producer
//! renders into GPU images it shares once, and the app's renderer shows each
//! finished frame in place, beneath the window's other content, so an editor
//! around a game viewport costs nothing per game frame while the editor
//! itself is still. Input on the element goes back to the producer.
//!
//! ```lmn
//! <surface id="scene" />
//! ```
//!
//! ```candela
//! let endpoint = surface::endpoint("scene");
//! // start the game with LUMEN_SURFACE=<endpoint> in its environment
//! ```
//!
//! A producer in another process speaks the Lumen surface protocol, through
//! the `lumen-surface-protocol` crate or its C library; one linked into the
//! app takes a [`LoopbackProducer`] from [`Surfaces::loopback`] and presents
//! textures it makes on the renderer's own device.
//!
//! One implementation, two link shapes, like every std module: the `cdylib`
//! is the bundled `lumen-surface` module an app declares in `lumen.toml`
//! (`lumen-surface = { bundled = true }` under `[dependencies]`), and the
//! `lib` is [`SurfacePlugin`] for a build that installs it itself.
//!
//! # What a surface cannot do
//!
//! The frame is shown as it is: opaque, upright, unblended. Under an
//! `opacity` below 1 (its own or a fading ancestor's), a `mask-image`, or a
//! rotation, the frame is not drawn and the element raises `surfaceerror`
//! with the reason `effect`. Clipping by ancestors (`overflow: hidden`) and
//! content painted over it (overlay children) work.
//!
//! Shared frames need the GPU renderer: under the CPU renderer a surface
//! stays `unsupported`, and in a browser it is a placeholder.

#![warn(missing_docs)]

pub mod api;
mod gpu;
mod input;
mod paint;
mod plugin;
pub mod session;

pub use api::{
    LoopbackProducer, SurfaceElement, SurfaceEvent, SurfaceFrame, SurfaceState, Surfaces,
};
pub use input::ForwardState;
/// The renderer's device and the `wgpu` it was made with, for an engine that
/// presents through a [`LoopbackProducer`]: textures made with this `wgpu` are
/// the renderer's own types.
pub use lumen_module::gpu::{GpuContext, wgpu};
pub use lumen_surface_protocol as protocol;
pub use lumen_surface_protocol::wire::Pacing;
pub use paint::{EXTENSION_ID, SurfaceLeaf, SurfacePainter, extract_surfaces};
pub use plugin::{DESCRIPTOR, NAME, SurfacePlugin, SurfaceSet, TAG};
pub use session::{FrameInfo, SurfaceStateKind};

// The module entry: the loader constructs the plugin from the app's `config`
// table, whether it opened this crate's library or found it linked in.
lumen_module::lumen_module!("lumen-surface", |config: lumen_module::ModuleConfig| {
    SurfacePlugin::new(config)
});
