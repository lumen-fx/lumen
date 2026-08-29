//! Where a canvas lives between the call that draws on it and the tick that
//! encodes it.
//!
//! A script function body has no world: it runs on the host's stack, is
//! handed its arguments and nothing else, and has to answer `canvas::width`
//! in place. So the surfaces live in one process-global store the bodies
//! reach and the module's system drains, which is the same shape the other
//! modules use for state a body must read.
//!
//! A surface is created the first time anything names it. That is what makes
//! `canvas::fill_rect("chart", ..)` work from `on_start`, before the element
//! is adopted or even spawned; the drawing is kept and the element picks it
//! up when it appears.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use lumen_module::lumen_core::app::EventLoopWaker;
use lumen_module::lumen_render_wgpu::vello::Scene;

use crate::buffer::PixBuf;
use crate::ops::{Gfx, Op};

/// How large a `buffer_get_region` / `buffer_put_region` may be, in pixels.
pub const DEFAULT_REGION_CAP: u64 = 1_048_576;
/// The smallest region cap an app can ask for; below it the call is useless
/// rather than careful.
pub const MIN_REGION_CAP: u64 = 1024;
/// The largest region cap an app can ask for. One script value per pixel is
/// what makes this the ceiling.
pub const MAX_REGION_CAP: u64 = 16_777_216;

/// How many pixels one buffer may hold.
pub const DEFAULT_BUFFER_PIXEL_CAP: u64 = 16_777_216;
/// The smallest per-buffer pixel cap an app can ask for.
pub const MIN_BUFFER_PIXEL_CAP: u64 = 1024;
/// The largest per-buffer pixel cap an app can ask for.
pub const MAX_BUFFER_PIXEL_CAP: u64 = 67_108_864;

/// How many buffers may exist at once.
pub const DEFAULT_BUFFER_COUNT_CAP: u64 = 256;
/// The smallest buffer-count cap an app can ask for.
pub const MIN_BUFFER_COUNT_CAP: u64 = 1;
/// The largest buffer-count cap an app can ask for.
pub const MAX_BUFFER_COUNT_CAP: u64 = 4096;

/// The size a `<canvas>` with no `width` / `height` draws at, which is the
/// one the HTML canvas has had since it was introduced.
pub const UA_SIZE: (f32, f32) = (300.0, 150.0);

/// What the caps are set to, resolved once at install.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Largest region, in pixels.
    pub region: u64,
    /// Largest buffer, in pixels.
    pub buffer_pixels: u64,
    /// Most buffers at once.
    pub buffer_count: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            region: DEFAULT_REGION_CAP,
            buffer_pixels: DEFAULT_BUFFER_PIXEL_CAP,
            buffer_count: DEFAULT_BUFFER_COUNT_CAP,
        }
    }
}

/// One canvas.
pub struct Surface {
    /// The drawing space, in canvas units. The element's box may be a
    /// different size; the painter scales one onto the other.
    pub logical: (f32, f32),
    /// Calls recorded since the last encode.
    pub pending: Vec<Op>,
    /// The encoded scene, which is what the painter appends.
    pub scene: std::sync::Arc<Scene>,
    /// The drawing state, which persists across ticks: a fill set in one
    /// handler is still the fill in the next.
    pub gfx: Gfx,
    /// Bumped every time the scene changes, so the extract can tell the
    /// renderer whether anything moved.
    pub revision: u64,
}

impl Default for Surface {
    fn default() -> Self {
        Surface {
            logical: UA_SIZE,
            pending: Vec::new(),
            scene: std::sync::Arc::new(Scene::new()),
            gfx: Gfx::default(),
            revision: 0,
        }
    }
}

/// Every canvas and buffer in the process.
#[derive(Default)]
pub struct CanvasStore {
    /// Surfaces, keyed by the element id they draw into.
    pub surfaces: BTreeMap<String, Surface>,
    /// Buffers, keyed by handle. Handle 0 is never issued, so a script that
    /// kept a handle past `buffer_free` gets the same refusal as one that
    /// invented a number.
    pub buffers: BTreeMap<u32, PixBuf>,
    /// The next handle to issue.
    next_buffer: u32,
    /// The caps this install resolved.
    pub caps: Caps,
    /// How the module wakes a parked event loop after a call that drew.
    pub waker: Option<EventLoopWaker>,
    /// Ids already reported as unmatched, so a call in a loop says it once.
    pub reported: BTreeSet<String>,
    /// Ids an element has been adopted for. Read to tell a canvas waiting
    /// for its element from one whose id nothing will ever match.
    pub answered: BTreeSet<String>,
}

/// How many recorded calls a canvas nothing answers for keeps.
///
/// A script may legitimately draw before its element is mounted, so the
/// journal is kept rather than dropped. A script drawing into an id that is
/// simply a typo would otherwise grow it without bound, so the oldest calls
/// fall off past this point; the module has already said the id matches
/// nothing.
pub const UNANSWERED_JOURNAL_CAP: usize = 4096;

impl CanvasStore {
    /// The surface for an id, created empty if this is the first mention.
    pub fn surface(&mut self, id: &str) -> &mut Surface {
        self.surfaces.entry(id.to_string()).or_default()
    }

    /// Record one call against a surface and wake the loop, which is what
    /// every drawing function body does.
    pub fn record(&mut self, id: &str, op: Op) {
        self.surface(id).pending.push(op);
        self.wake();
    }

    /// Wake a parked event loop so the tick that encodes runs.
    pub fn wake(&self) {
        if let Some(waker) = self.waker.as_ref() {
            waker.wake();
        }
    }

    /// Issue a buffer, or say why not.
    pub fn new_buffer(&mut self, width: u32, height: u32) -> Result<u32, String> {
        if width == 0 || height == 0 {
            return Err(format!("a {width}x{height} buffer holds no pixels"));
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.caps.buffer_pixels {
            return Err(format!(
                "a {width}x{height} buffer is {pixels} pixels, over the \
                 {} the app allows",
                self.caps.buffer_pixels
            ));
        }
        if self.buffers.len() as u64 >= self.caps.buffer_count {
            return Err(format!(
                "{} buffers already exist, which is the cap; free one first",
                self.caps.buffer_count
            ));
        }
        self.next_buffer += 1;
        let handle = self.next_buffer;
        self.buffers.insert(handle, PixBuf::new(width, height));
        Ok(handle)
    }

    /// Report an id nothing answers for, once per id.
    ///
    /// Once, because the call that produced it is usually in a draw loop and
    /// a line per frame buries every other message the app prints.
    pub fn report_once(&mut self, id: &str, message: &str) {
        if self.reported.insert(id.to_string()) {
            lumen_module::lumen_core::warn_line!("lumen-canvas: {message}");
        }
    }
}

/// The one store. `OnceLock` rather than a resource because the script
/// function bodies that read it have no world to read it from.
static STORE: OnceLock<Mutex<CanvasStore>> = OnceLock::new();

/// The store, locked. A poisoned lock is taken anyway: the state behind it is
/// pixels and drawing calls, and refusing to draw for the rest of the run
/// because one call panicked is worse than drawing.
pub fn store() -> MutexGuard<'static, CanvasStore> {
    STORE
        .get_or_init(|| Mutex::new(CanvasStore::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Drop every surface and buffer. Tests that drive several apps in one
/// process call it between them; nothing in a running app does.
pub fn reset() {
    let mut store = store();
    let caps = store.caps;
    *store = CanvasStore {
        caps,
        ..CanvasStore::default()
    };
}
