//! One `<surface>`'s session: the listener a producer connects to, the
//! handshake, the rings of imported buffers, the frames shown and released,
//! pacing, stalls, crashes and reconnects.
//!
//! A session runs on a thread of its own and talks to the rest of the module
//! through [`Shared`]: the element's systems read what changed and write what
//! the element wants (its size, its visibility, input), and the script
//! functions read the state. What a frame is imported into and shown with is
//! a [`Backend`]: the GPU one imports into the renderer's device and presents
//! into its texture table; tests use one that records instead, so the whole
//! protocol runs without a GPU.

use lumen_surface_protocol::consumer::{Expected, check_hello};
use lumen_surface_protocol::endpoint::{AdapterHint, Endpoint, Transport};
use lumen_surface_protocol::transport::{
    Handle, Listener, Received, Receiver, Sender, random_bytes,
};
use lumen_surface_protocol::wire::{
    AdapterId, Api, Buffers, ByeReason, Format, FrameReady, InputEvent, Message, Pacing, Welcome,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often a session thread wakes with nothing to read, to check timers
/// and whether its element went away.
const POLL: Duration = Duration::from_millis(50);
/// How long a quiet connection goes before the consumer pings.
const PING_EVERY: Duration = Duration::from_secs(1);
/// How long without any message after a ping counts as hung.
const HUNG_AFTER: Duration = Duration::from_secs(3);
/// How long the producer has to say hello once connected.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a surface stands, as its `state` attribute and class say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SurfaceStateKind {
    /// No producer yet, or none has shown a frame.
    #[default]
    Waiting,
    /// Frames arrive.
    Live,
    /// A frame was asked for and has not come within the stall timeout.
    Stalled,
    /// The producer went away; the last frame stays.
    Lost,
    /// The renderer cannot show shared frames (no GPU).
    Unsupported,
}

impl SurfaceStateKind {
    /// The name the `state` attribute and the script functions use.
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceStateKind::Waiting => "waiting",
            SurfaceStateKind::Live => "live",
            SurfaceStateKind::Stalled => "stalled",
            SurfaceStateKind::Lost => "lost",
            SurfaceStateKind::Unsupported => "unsupported",
        }
    }
}

/// Something that happened in a session, for the element's systems to raise
/// as an event or act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Happened {
    /// A producer finished the handshake.
    Connected {
        /// Its engine name.
        engine: String,
        /// Its process id.
        pid: u32,
    },
    /// A frame request went unanswered past the stall timeout.
    Stalled {
        /// How long it has waited.
        waited_ms: u64,
    },
    /// Frames arrive again after a stall.
    Resumed,
    /// The session ended.
    Disconnected {
        /// `exit`, `crash`, `hung`, `rejected`, `device-lost` or `closed`.
        reason: &'static str,
        /// Words for a person.
        detail: String,
    },
    /// A handshake or an import failed.
    Error {
        /// `rejected`, `import`, `protocol`, `unsupported` or `effect`.
        reason: &'static str,
        /// Words for a person.
        detail: String,
    },
    /// The producer asked for a cursor (a CSS keyword).
    Cursor(&'static str),
    /// The producer asked for the pointer lock, or to let it go.
    PointerLock(bool),
    /// Bytes from the producer's application.
    User(Vec<u8>),
}

/// The frame on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// Its number, as the producer counts.
    pub frame: u64,
    /// The ring generation it came from.
    pub generation: u32,
    /// Its size in texture pixels.
    pub size: (u32, u32),
    /// The producer's bytes about it.
    pub meta: Vec<u8>,
    /// The newest input it reflects.
    pub input_seq: u64,
    /// When the consumer received it.
    pub received_at: Option<Instant>,
}

/// What the element wants of its producer: the frame size and pacing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wanted {
    /// Width in texture pixels.
    pub width: u32,
    /// Height in texture pixels.
    pub height: u32,
    /// Device pixels per logical pixel.
    pub scale: f32,
}

impl Default for Wanted {
    fn default() -> Self {
        Self {
            width: 300,
            height: 150,
            scale: 1.0,
        }
    }
}

/// A ring the producer handed over, imported.
struct Ring<T> {
    size: (u32, u32),
    textures: Vec<T>,
}

/// How often a ring's import thread checks on its first-use work, and how
/// often a session with imports in flight checks for their results.
const PREPARE_POLL: Duration = Duration::from_millis(5);

/// A ring whose import thread finished: its textures once their first-use
/// work is done, or why the import failed.
struct Prepared<T> {
    generation: u32,
    /// The device generation the import was made on.
    device_generation: u64,
    size: (u32, u32),
    result: Result<Vec<T>, String>,
}

/// The connection to the producer, while there is one.
struct Link {
    sender: Sender,
    /// Counts sessions, so a release from an older connection goes nowhere.
    session: u64,
}

/// Mutable session state, behind [`Shared`]'s lock.
struct Inner {
    state: SurfaceStateKind,
    link: Option<Link>,
    happened: Vec<Happened>,
    frame: Option<FrameInfo>,
    /// A frame arrived the element's systems have not seen yet.
    fresh: bool,
    wanted: Wanted,
    /// The size last asked of the producer.
    asked: Option<Wanted>,
    pacing: Pacing,
    visible: bool,
    /// When the frame request in flight was sent.
    requested_at: Option<Instant>,
    /// A frame is wanted once this tick's input has gone (see
    /// [`Shared::ask_after_input`]).
    ask_after_input: bool,
    request_seq: u64,
    /// A ring was accepted on this connection, so frames may be asked for.
    ring_ready: bool,
    input_seq: u64,
    stall_timeout: Duration,
    stalled: bool,
    closing: bool,
    /// A producer is being served (from its hello to its end).
    claimed: bool,
    /// The newest generation, and the generations whose slots are still
    /// held, with how many.
    current_generation: u32,
    held: HashMap<u32, usize>,
}

/// A session's state, shared between its thread, the element's systems and
/// the script functions.
pub struct Shared {
    /// The element's id, which the script functions name it by.
    pub id: String,
    /// The channel producers ask for.
    pub channel: String,
    token: [u8; 16],
    /// Where producers connect; `None` on a platform with no transport,
    /// where only an in-process producer feeds the surface.
    path: Option<std::path::PathBuf>,
    adapter_hint: Mutex<Option<(AdapterHint, String)>>,
    /// Where the element showed in the last frame painted, in logical
    /// pixels: `x, y, width, height`. `None` when it was not drawn.
    placement: Mutex<Option<[f32; 4]>>,
    /// The last reason a draw of the surface was refused, so each is
    /// reported once.
    refused: Mutex<Option<&'static str>>,
    /// The OS identity (device and inode) of each handle of the newest ring.
    buffers: Mutex<Vec<(u64, u64)>>,
    inner: Mutex<Inner>,
    /// Wakes the app when something happened; set once a window backend
    /// runs.
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

static SESSIONS: AtomicU64 = AtomicU64::new(1);

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The endpoint string a producer connects with, or `None` on a
    /// platform with no transport for one.
    pub fn endpoint(&self) -> Option<String> {
        let path = self.path.clone()?;
        let hint = self
            .adapter_hint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Some(
            Endpoint {
                transport: Transport::Unix(path),
                channel: self.channel.clone(),
                token: self.token,
                adapter: hint.as_ref().map(|(h, _)| *h),
                adapter_name: hint.map(|(_, name)| name),
            }
            .to_string(),
        )
    }

    /// Record the GPU the consumer renders on, for the endpoint string.
    pub fn set_adapter_hint(&self, hint: Option<(AdapterHint, String)>) {
        *self.adapter_hint.lock().unwrap_or_else(|e| e.into_inner()) = hint;
    }

    /// The OS identity of each shared buffer of the ring in use: the device
    /// and inode of the handle the producer sent. The producer's own handles
    /// to the same buffers have the same identity, which is how a test (or a
    /// person) sees that frames are shared rather than copied.
    pub fn buffer_identities(&self) -> Vec<(u64, u64)> {
        self.buffers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The size the element wants frames at.
    pub fn wanted(&self) -> Wanted {
        self.lock().wanted
    }

    /// Record where the element was drawn (`None`: it was not).
    pub fn set_placement(&self, placement: Option<[f32; 4]>) {
        *self.placement.lock().unwrap_or_else(|e| e.into_inner()) = placement;
    }

    /// Where the element was drawn in the last frame painted.
    pub fn placement(&self) -> Option<[f32; 4]> {
        *self.placement.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A draw of the surface was refused for `reason`; reported once until
    /// a draw succeeds or another reason comes.
    pub fn report_refusal(&self, reason: Option<(&'static str, String)>) {
        let mut last = self.refused.lock().unwrap_or_else(|e| e.into_inner());
        let name = reason.as_ref().map(|(r, _)| *r);
        if *last == name {
            return;
        }
        *last = name;
        drop(last);
        if let Some((_, detail)) = reason {
            self.happen(Happened::Error {
                reason: "effect",
                detail,
            });
        }
    }

    /// Where the surface stands.
    pub fn state(&self) -> SurfaceStateKind {
        self.lock().state
    }

    /// The frame on screen, if any.
    pub fn frame(&self) -> Option<FrameInfo> {
        self.lock().frame.clone()
    }

    /// Take what happened since the last call, and whether a new frame
    /// arrived.
    pub fn take(&self) -> (Vec<Happened>, bool) {
        let mut inner = self.lock();
        (
            std::mem::take(&mut inner.happened),
            std::mem::take(&mut inner.fresh),
        )
    }

    /// Set the wake-up the session calls when something happened.
    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker);
    }

    fn wake(&self) {
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    fn happen(&self, what: Happened) {
        self.lock().happened.push(what);
        self.wake();
    }

    /// The size the element wants frames at. Sends a resize to a connected
    /// producer when it changed; returns whether it did.
    pub fn set_wanted(&self, wanted: Wanted) -> bool {
        let mut inner = self.lock();
        inner.wanted = wanted;
        if inner.asked == Some(wanted) {
            return false;
        }
        let Some(link) = &inner.link else {
            return false;
        };
        let sent = link
            .sender
            .try_send(
                &Message::Resize {
                    width: wanted.width,
                    height: wanted.height,
                    scale: wanted.scale,
                },
                &[],
            )
            .is_ok();
        if sent {
            inner.asked = Some(wanted);
        }
        sent
    }

    /// Whether anyone can see the surface. Tells a connected producer when it
    /// changes.
    pub fn set_visible(&self, visible: bool) {
        let mut inner = self.lock();
        if inner.visible == visible {
            return;
        }
        inner.visible = visible;
        if let Some(link) = &inner.link {
            let _ = link.sender.try_send(&Message::Visibility { visible }, &[]);
        }
        if visible {
            Self::ask_locked(&mut inner);
        }
    }

    /// Set the pacing new producers are welcomed with.
    pub fn set_pacing(&self, pacing: Pacing) {
        self.lock().pacing = pacing;
    }

    /// Set how long a frame request may go unanswered before the surface
    /// counts as stalled.
    pub fn set_stall_timeout(&self, timeout: Duration) {
        self.lock().stall_timeout = timeout;
    }

    /// Ask a paced producer for its next frame, unless one is asked for
    /// already or nobody can see it.
    pub fn request_frame(&self) {
        Self::ask_locked(&mut self.lock());
    }

    /// Ask for the next frame after the input of this tick has gone: the
    /// request goes out with the next [`Self::send_input`], or at
    /// [`Self::send_asked`] when there is none. A producer that starts a
    /// frame the moment it is asked for then draws it with that input,
    /// instead of showing it a frame later.
    pub fn ask_after_input(&self) {
        self.lock().ask_after_input = true;
    }

    /// Send the request [`Self::ask_after_input`] held back, if input did not
    /// carry it already.
    pub fn send_asked(&self) {
        let mut inner = self.lock();
        if std::mem::take(&mut inner.ask_after_input) {
            Self::ask_locked(&mut inner);
        }
    }

    fn ask_locked(inner: &mut Inner) {
        inner.ask_after_input = false;
        if inner.pacing != Pacing::Paced
            || !inner.visible
            || !inner.ring_ready
            || inner.requested_at.is_some()
        {
            return;
        }
        let Some(link) = &inner.link else {
            return;
        };
        inner.request_seq += 1;
        let request = Message::FrameRequest {
            seq: inner.request_seq,
            target_present_ns: 0,
        };
        if link.sender.try_send(&request, &[]).is_ok() {
            inner.requested_at = Some(Instant::now());
        }
    }

    /// Forward an input event to a connected producer. Returns its sequence
    /// number, or `None` when no producer is connected or it is not reading.
    pub fn send_input(&self, event: InputEvent) -> Option<u64> {
        let mut inner = self.lock();
        let link = inner.link.as_ref()?;
        let seq = inner.input_seq + 1;
        let message = Message::Input {
            seq,
            time: lumen_surface_protocol::monotonic_ns(),
            event,
        };
        link.sender.try_send(&message, &[]).ok()?;
        inner.input_seq = seq;
        Self::ask_locked(&mut inner);
        Some(seq)
    }

    /// Send bytes to the producer's application. Returns whether they went.
    pub fn send_user(&self, bytes: &[u8]) -> bool {
        let inner = self.lock();
        inner.link.as_ref().is_some_and(|link| {
            link.sender
                .try_send(&Message::User(bytes.to_vec()), &[])
                .is_ok()
        })
    }

    /// End the connection to the producer, if there is one. The surface
    /// keeps listening for a new one.
    pub fn disconnect(&self) {
        let inner = self.lock();
        if let Some(link) = &inner.link {
            let _ = link.sender.try_send(
                &Message::Bye {
                    reason: ByeReason::Closed,
                    detail: "the app ended the session".into(),
                },
                &[],
            );
        }
    }

    /// Mark the surface unsupported: there is no GPU to show frames with.
    pub fn set_unsupported(&self, detail: &str) {
        let mut inner = self.lock();
        if inner.state == SurfaceStateKind::Unsupported {
            return;
        }
        inner.state = SurfaceStateKind::Unsupported;
        inner.happened.push(Happened::Error {
            reason: "unsupported",
            detail: detail.to_string(),
        });
        drop(inner);
        self.wake();
    }

    /// Record a frame shown in-process (the loopback producer): what
    /// [`Self::frame`] reports and what the element's systems pick up.
    pub fn record_local_frame(&self, info: FrameInfo) {
        let mut inner = self.lock();
        let first = inner.state != SurfaceStateKind::Live;
        inner.frame = Some(info);
        inner.fresh = true;
        inner.state = SurfaceStateKind::Live;
        if first {
            inner.happened.push(Happened::Connected {
                engine: "in-process".into(),
                pid: std::process::id(),
            });
        }
        drop(inner);
        self.wake();
    }

    /// Stop the session thread. It ends its connection and its listener.
    pub fn close(&self) {
        self.lock().closing = true;
        self.disconnect();
    }

    fn closing(&self) -> bool {
        self.lock().closing
    }
}

/// An imported ring: its textures, and whether the work that makes them
/// usable has finished.
pub struct Imported<T> {
    /// One texture per buffer, in order.
    pub textures: Vec<T>,
    /// Polled until it answers `true`; the ring is accepted then.
    pub ready: Box<dyn FnMut() -> bool + Send>,
}

impl<T> Imported<T> {
    /// A ring usable as soon as it is imported.
    pub fn now(textures: Vec<T>) -> Self {
        Self {
            textures,
            ready: Box::new(|| true),
        }
    }
}

/// What a session imports buffers into and shows frames with.
pub trait Backend: Send + Sync + 'static {
    /// One imported buffer.
    type Texture: Clone + Send + 'static;

    /// The API and GPU the consumer imports with, or `None` when it has
    /// none.
    fn device(&self) -> Option<(Api, AdapterId)>;

    /// Counts device bring-ups; a change drops every import.
    fn generation(&self) -> u64;

    /// Import a ring's buffers from their descriptors, one texture each.
    /// Runs on a thread of its own, which may block (on a GPU busy with
    /// another frame, say), and the ring is accepted once [`Imported::ready`]
    /// says the first-use work the import started has finished. The session
    /// keeps serving meanwhile, so a slow GPU delays the answer without
    /// stalling the connection.
    ///
    /// # Errors
    ///
    /// The import failed; the reason goes to the producer.
    fn import(
        &self,
        buffers: &Buffers,
        fds: Vec<Handle>,
    ) -> Result<Imported<Self::Texture>, String>;

    /// Show `texture` from the next frame on; `released` runs once nothing
    /// reads it any more.
    fn present(&self, texture: Self::Texture, stamp: u64, released: Box<dyn FnOnce() + Send>);
}

/// Start a session for an element: bind its listener and run its thread.
/// On a platform with no transport there is no listener and no thread, and
/// the session serves in-process producers only.
///
/// # Errors
///
/// The listener could not be bound.
pub fn start<B: Backend>(id: &str, channel: &str, backend: Arc<B>) -> std::io::Result<Arc<Shared>> {
    let listener = match Listener::bind_fresh() {
        Ok(listener) => Some(listener),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
        Err(e) => return Err(e),
    };
    let token = match &listener {
        Some(_) => random_bytes()?,
        None => [0; 16],
    };
    let shared = Arc::new(Shared {
        id: id.to_string(),
        channel: channel.to_string(),
        token,
        path: listener.as_ref().map(|l| l.path().to_path_buf()),
        adapter_hint: Mutex::new(None),
        placement: Mutex::new(None),
        refused: Mutex::new(None),
        buffers: Mutex::new(Vec::new()),
        inner: Mutex::new(Inner {
            state: SurfaceStateKind::Waiting,
            link: None,
            happened: Vec::new(),
            frame: None,
            fresh: false,
            wanted: Wanted::default(),
            asked: None,
            pacing: Pacing::Paced,
            visible: true,
            requested_at: None,
            ask_after_input: false,
            request_seq: 0,
            ring_ready: false,
            input_seq: 0,
            stall_timeout: Duration::from_millis(1000),
            stalled: false,
            closing: false,
            claimed: false,
            current_generation: 0,
            held: HashMap::new(),
        }),
        waker: Mutex::new(None),
    });
    if let Some(listener) = listener {
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("lumen-surface {id}"))
            .spawn(move || run(listener, thread_shared, backend))?;
    }
    Ok(shared)
}

/// The listening thread: take connections until the element goes, each on a
/// thread of its own, so a second producer on a busy channel is told so at
/// once rather than left waiting.
fn run<B: Backend>(listener: Listener, shared: Arc<Shared>, backend: Arc<B>) {
    while !shared.closing() {
        match listener.accept(Some(POLL * 4)) {
            Ok(Some(connection)) => {
                let (sender, receiver) = connection.split();
                let (shared, backend) = (shared.clone(), backend.clone());
                let spawned = std::thread::Builder::new()
                    .name(format!("lumen-surface {} producer", shared.id))
                    .spawn(move || serve(&shared, &backend, sender, receiver));
                if spawned.is_err() {
                    std::thread::sleep(POLL);
                }
            }
            Ok(None) => {}
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

/// The right to serve the channel's one producer, given back when dropped.
struct Claim<'a>(&'a Shared);

impl<'a> Claim<'a> {
    fn take(shared: &'a Shared) -> Option<Self> {
        let mut inner = shared.lock();
        if inner.claimed {
            return None;
        }
        inner.claimed = true;
        Some(Self(shared))
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.lock().claimed = false;
    }
}

/// How a connection ended.
struct Ended {
    reason: &'static str,
    detail: String,
}

/// Serve one producer from its hello to its end.
fn serve<B: Backend>(
    shared: &Arc<Shared>,
    backend: &Arc<B>,
    sender: Sender,
    mut receiver: Receiver,
) {
    let hello = match receiver.recv(Some(HELLO_TIMEOUT)) {
        Ok(Received::Message(Message::Hello(hello), _)) => hello,
        _ => return,
    };
    let device = backend.device();
    let expected = Expected {
        token: shared.token,
        channel: shared.channel.clone(),
        api: device.map(|(api, _)| api),
        adapter: device.map(|(_, a)| a).unwrap_or_default(),
    };
    let Some(_claim) = Claim::take(shared) else {
        let _ = sender.send(
            &Message::Reject(lumen_surface_protocol::wire::Reject {
                reason: lumen_surface_protocol::wire::RejectReason::Busy,
                api: expected.api.unwrap_or(hello.api),
                adapter: expected.adapter,
                detail: "another producer is connected on this channel".into(),
            }),
            &[],
        );
        return;
    };
    if let Err(reject) = check_hello(&hello, &expected) {
        let detail = reject.detail.clone();
        let _ = sender.send(&Message::Reject(reject), &[]);
        shared.happen(Happened::Error {
            reason: "rejected",
            detail,
        });
        return;
    }
    let session = SESSIONS.fetch_add(1, Ordering::Relaxed);
    let welcome = {
        let mut inner = shared.lock();
        let wanted = inner.wanted;
        inner.asked = Some(wanted);
        inner.ring_ready = false;
        inner.requested_at = None;
        inner.stalled = false;
        // Generations count from 1 again on each connection.
        inner.current_generation = 0;
        inner.held.clear();
        inner.link = Some(Link {
            sender: sender.clone(),
            session,
        });
        Welcome {
            session,
            api: expected.api.expect("checked: a device"),
            pacing: inner.pacing,
            ring_min: 2,
            ring_max: lumen_surface_protocol::wire::MAX_RING as u8,
            ring_pref: if inner.pacing == Pacing::Free { 4 } else { 3 },
            width: wanted.width,
            height: wanted.height,
            scale: wanted.scale,
            formats: Format::Rgba8Srgb.bit(),
            caps: 0,
        }
    };
    if sender.send(&Message::Welcome(welcome), &[]).is_err() {
        shared.lock().link = None;
        return;
    }
    if !shared.lock().visible {
        let _ = sender.send(&Message::Visibility { visible: false }, &[]);
    }
    shared.happen(Happened::Connected {
        engine: hello.engine.clone(),
        pid: hello.pid,
    });
    let ended = pump(shared, backend, &sender, &mut receiver, session);
    receiver.shutdown();
    {
        let mut inner = shared.lock();
        inner.link = None;
        inner.ring_ready = false;
        inner.requested_at = None;
        inner.held.clear();
        if inner.state != SurfaceStateKind::Unsupported {
            inner.state = SurfaceStateKind::Lost;
        }
        inner.happened.push(Happened::Disconnected {
            reason: ended.reason,
            detail: ended.detail,
        });
    }
    shared.wake();
}

/// Sets a flag when dropped: tells a session's import threads that nobody
/// waits for their results any more.
struct Abandon(Arc<AtomicBool>);

impl Drop for Abandon {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Import a ring on a thread of its own and wait there for its first-use
/// work, sending the result to `done`. An import talks to the GPU, and a
/// device busy with the renderer's frame (a software rasterizer compiling
/// its shaders, say) can hold every submission back for seconds; the
/// session's own thread keeps reading and pinging meanwhile, so the waiting
/// producer hears from it and does not give up.
fn start_import<B: Backend>(
    backend: &Arc<B>,
    buffers: Buffers,
    fds: Vec<Handle>,
    device_generation: u64,
    gone: &Arc<AtomicBool>,
    done: &mpsc::Sender<Prepared<B::Texture>>,
) -> Result<(), String> {
    let (backend, gone, done) = (backend.clone(), gone.clone(), done.clone());
    std::thread::Builder::new()
        .name("lumen-surface import".into())
        .spawn(move || {
            let result = backend.import(&buffers, fds).and_then(|mut imported| {
                while !(imported.ready)() {
                    if gone.load(Ordering::Acquire) {
                        return Err("the session ended".into());
                    }
                    std::thread::sleep(PREPARE_POLL);
                }
                Ok(imported.textures)
            });
            let _ = done.send(Prepared {
                generation: buffers.generation,
                device_generation,
                size: (buffers.width, buffers.height),
                result,
            });
        })
        .map(drop)
        .map_err(|e| format!("could not start importing the ring: {e}"))
}

/// Read the producer's messages until the connection ends.
fn pump<B: Backend>(
    shared: &Arc<Shared>,
    backend: &Arc<B>,
    sender: &Sender,
    receiver: &mut Receiver,
    session: u64,
) -> Ended {
    let mut rings: HashMap<u32, Ring<B::Texture>> = HashMap::new();
    let (done, prepared) = mpsc::channel::<Prepared<B::Texture>>();
    let abandon = Abandon(Arc::new(AtomicBool::new(false)));
    let mut importing = 0usize;
    let mut device_generation = backend.generation();
    let mut last_heard = Instant::now();
    let mut pinged: Option<Instant> = None;
    loop {
        if shared.closing() {
            let _ = sender.send(
                &Message::Bye {
                    reason: ByeReason::Closed,
                    detail: "the surface went away".into(),
                },
                &[],
            );
            return Ended {
                reason: "closed",
                detail: "the surface went away".into(),
            };
        }
        if backend.generation() != device_generation {
            // Every import was made on the old device: drop them all and ask
            // for a new ring on the new one.
            device_generation = backend.generation();
            rings.clear();
            {
                let mut inner = shared.lock();
                inner.ring_ready = false;
                inner.requested_at = None;
                inner.held.clear();
            }
            let _ = sender.send(&Message::Reset { device_generation }, &[]);
        }
        // Answer each ring whose import has finished. One made on a device
        // that has gone since is dropped: the reset asked for a new ring.
        while let Ok(ring) = prepared.try_recv() {
            importing -= 1;
            if ring.device_generation != device_generation {
                continue;
            }
            let generation = ring.generation;
            match ring.result {
                Ok(textures) => {
                    rings.insert(
                        generation,
                        Ring {
                            size: ring.size,
                            textures,
                        },
                    );
                    if sender
                        .send(&Message::BuffersAccepted { generation }, &[])
                        .is_ok()
                    {
                        let mut inner = shared.lock();
                        inner.ring_ready = true;
                        Shared::ask_locked(&mut inner);
                    }
                }
                Err(detail) => reject_ring(shared, sender, generation, detail),
            }
        }
        let wait = if importing == 0 { POLL } else { PREPARE_POLL };
        let received = match receiver.recv(Some(wait)) {
            Ok(r) => r,
            Err(e) => {
                return Ended {
                    reason: "crash",
                    detail: e.to_string(),
                };
            }
        };
        let now = Instant::now();
        match received {
            Received::Closed => {
                return Ended {
                    reason: "crash",
                    detail: "the producer's connection closed without a goodbye".into(),
                };
            }
            Received::Timeout => {
                housekeeping(shared, sender, &mut pinged, last_heard, now);
                if pinged.is_some_and(|at| now.duration_since(at) > HUNG_AFTER) {
                    return Ended {
                        reason: "hung",
                        detail: "the producer stopped answering".into(),
                    };
                }
                continue;
            }
            Received::Message(message, fds) => {
                last_heard = now;
                pinged = None;
                match message {
                    Message::Buffers(buffers) => {
                        let generation = buffers.generation;
                        let identities = fds.iter().filter_map(identity).collect();
                        *shared.buffers.lock().unwrap_or_else(|e| e.into_inner()) = identities;
                        // Accepted at the top of the loop once its import
                        // thread is done.
                        match start_import(
                            backend,
                            buffers,
                            fds,
                            device_generation,
                            &abandon.0,
                            &done,
                        ) {
                            Ok(()) => importing += 1,
                            Err(detail) => reject_ring(shared, sender, generation, detail),
                        }
                    }
                    Message::FrameReady(frame) => {
                        show(shared, &**backend, sender, &mut rings, frame, session, now);
                    }
                    Message::Cursor { keyword } => shared.happen(Happened::Cursor(
                        lumen_surface_protocol::wire::cursor::keyword(keyword),
                    )),
                    Message::PointerLock { wanted } => {
                        shared.happen(Happened::PointerLock(wanted));
                    }
                    Message::User(bytes) => shared.happen(Happened::User(bytes)),
                    Message::Ping(t) => {
                        let _ = sender.send(&Message::Pong(t), &[]);
                    }
                    Message::Pong(_) => {}
                    Message::Bye { reason, detail } => {
                        return Ended {
                            reason: match reason {
                                ByeReason::DeviceLost => "device-lost",
                                ByeReason::Protocol => "rejected",
                                _ => "exit",
                            },
                            detail,
                        };
                    }
                    // Messages a producer does not send.
                    _ => {
                        let _ = sender.send(
                            &Message::Bye {
                                reason: ByeReason::Protocol,
                                detail: "a consumer-only message from the producer".into(),
                            },
                            &[],
                        );
                        return Ended {
                            reason: "rejected",
                            detail: "the producer broke the protocol".into(),
                        };
                    }
                }
            }
        }
        // Retire every older ring all of whose slots have come back.
        retire(shared, sender, &mut rings);
    }
}

/// Refuse a ring the consumer could not import.
fn reject_ring(shared: &Shared, sender: &Sender, generation: u32, detail: String) {
    let _ = sender.send(
        &Message::BuffersRejected {
            generation,
            reason: 1,
            detail: detail.clone(),
        },
        &[],
    );
    shared.happen(Happened::Error {
        reason: "import",
        detail,
    });
}

/// Show a finished frame and record it.
fn show<B: Backend>(
    shared: &Arc<Shared>,
    backend: &B,
    sender: &Sender,
    rings: &mut HashMap<u32, Ring<B::Texture>>,
    frame: FrameReady,
    session: u64,
    now: Instant,
) {
    let Some(ring) = rings.get_mut(&frame.generation) else {
        // A frame of a ring that never arrived: hand the slot straight back.
        let _ = sender.send(
            &Message::Release {
                frame: frame.frame,
                generation: frame.generation,
                slot: frame.slot,
            },
            &[],
        );
        return;
    };
    let Some(texture) = ring.textures.get(frame.slot as usize).cloned() else {
        return;
    };
    let size = ring.size;
    let release_to = sender.clone();
    let weak = Arc::downgrade(shared);
    let (number, generation, slot) = (frame.frame, frame.generation, frame.slot);
    let released: Box<dyn FnOnce() + Send> = Box::new(move || {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        {
            let mut inner = shared.lock();
            if inner.link.as_ref().map(|l| l.session) != Some(session) {
                return;
            }
            if let Some(n) = inner.held.get_mut(&generation) {
                *n = n.saturating_sub(1);
            }
        }
        let _ = release_to.send(
            &Message::Release {
                frame: number,
                generation,
                slot,
            },
            &[],
        );
    });
    {
        let mut inner = shared.lock();
        *inner.held.entry(generation).or_insert(0) += 1;
        inner.current_generation = inner.current_generation.max(generation);
        inner.requested_at = None;
        let resumed = std::mem::take(&mut inner.stalled);
        if resumed {
            inner.happened.push(Happened::Resumed);
        }
        inner.state = SurfaceStateKind::Live;
        inner.frame = Some(FrameInfo {
            frame: number,
            generation,
            size,
            meta: frame.meta,
            input_seq: frame.input_seq,
            received_at: Some(now),
        });
        inner.fresh = true;
    }
    backend.present(texture, number, released);
    shared.wake();
}

/// Retire rings older than the newest shown whose slots are all back.
fn retire<T>(shared: &Shared, sender: &Sender, rings: &mut HashMap<u32, Ring<T>>) {
    let (current, held) = {
        let inner = shared.lock();
        (inner.current_generation, inner.held.clone())
    };
    let old: Vec<u32> = rings
        .keys()
        .copied()
        .filter(|g| *g < current && held.get(g).copied().unwrap_or(0) == 0)
        .collect();
    for generation in old {
        rings.remove(&generation);
        shared.lock().held.remove(&generation);
        let _ = sender.send(&Message::RetireGeneration { generation }, &[]);
    }
}

/// Timers for a quiet connection: stall detection and pings.
fn housekeeping(
    shared: &Shared,
    sender: &Sender,
    pinged: &mut Option<Instant>,
    last_heard: Instant,
    now: Instant,
) {
    let stalled = {
        let mut inner = shared.lock();
        match inner.requested_at {
            Some(at) if !inner.stalled && now.duration_since(at) > inner.stall_timeout => {
                inner.stalled = true;
                inner.state = SurfaceStateKind::Stalled;
                Some(now.duration_since(at))
            }
            _ => None,
        }
    };
    if let Some(waited) = stalled {
        shared.happen(Happened::Stalled {
            waited_ms: waited.as_millis() as u64,
        });
    }
    if pinged.is_none() && now.duration_since(last_heard) > PING_EVERY {
        let t = lumen_surface_protocol::monotonic_ns();
        if sender.send(&Message::Ping(t), &[]).is_ok() {
            *pinged = Some(now);
        }
    }
}

/// The OS identity of a shared buffer's handle: its device and inode.
#[cfg(unix)]
fn identity(handle: &Handle) -> Option<(u64, u64)> {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;
    let file = std::fs::File::from(handle.as_fd().try_clone_to_owned().ok()?);
    let meta = file.metadata().ok()?;
    Some((meta.dev(), meta.ino()))
}

/// The OS identity of a shared buffer's handle.
#[cfg(not(unix))]
fn identity(handle: &Handle) -> Option<(u64, u64)> {
    match *handle {}
}
