//! The Windows transport: a named pipe only the current user can open.
//!
//! The consumer creates `\\.\pipe\lumen-surface-<pid>-<random>` with a
//! protected DACL that grants the current user, and nobody else, access, and
//! refuses remote clients. Frames travel in byte mode, each written whole.
//!
//! Handles do not travel through the pipe. A producer names each buffer's
//! NT handle by its value in its own process (`BufferDesc::handle`), and the
//! consumer copies it out with `DuplicateHandle`, using the process the pipe
//! reports as its client (`GetNamedPipeClientProcessId`), never a number the
//! producer claims. A producer therefore only has to keep its handles open
//! until the consumer has answered the ring; nothing is published under a
//! name another process could open.
//!
//! Writes go through a thread per connection, so a sender never blocks on a
//! peer that stopped reading for longer than it takes to queue a frame, and
//! [`Sender::try_send`] can refuse at once when the queue is full.

use crate::endpoint::Transport;
pub use crate::transport::{Received, TransportError};
use crate::wire::{FLAG_HANDLES, Message};
use std::collections::VecDeque;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
    WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcess, OpenProcessToken, PROCESS_DUP_HANDLE,
    ResetEvent, WaitForSingleObject,
};

/// An OS handle to shared memory, as it arrives with a frame: an NT handle
/// this process owns, copied out of the producer's process.
pub type Handle = OwnedHandle;
/// An OS handle as a sender names it: a handle value in the sender's own
/// process, which it keeps open until its ring is answered.
pub type RawHandle = std::os::windows::io::RawHandle;

/// The handle a C caller passes as a number, or `None` for zero.
pub fn raw_handle(value: u64) -> Option<RawHandle> {
    (value != 0).then_some(value as usize as RawHandle)
}

/// The most bytes queued for a peer before [`Sender::try_send`] refuses and
/// [`Sender::send`] waits.
const QUEUE_LIMIT: usize = 256 * 1024;

/// The pipe's buffer each way, a hint to the system.
const PIPE_BUFFER: u32 = 64 * 1024;

/// How long a shutdown lets queued frames drain to a peer that stopped
/// reading.
const DRAIN: Duration = Duration::from_secs(1);

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn millis(timeout: Option<Duration>) -> u32 {
    // Rounded up, so a wait under a millisecond sleeps rather than spins.
    timeout.map_or(INFINITE, |t| {
        t.as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(INFINITE - 1)) as u32
    })
}

/// 16 bytes from the system's random source.
pub fn random_bytes() -> io::Result<[u8; 16]> {
    let mut out = [0u8; 16];
    // SAFETY: fills the 16 bytes given.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            out.as_mut_ptr(),
            out.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "BCryptGenRandom failed: {status:#x}"
        )));
    }
    Ok(out)
}

/// A manual-reset event for one overlapped operation at a time.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: an unnamed manual-reset event, owned from here on.
        let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if h.is_null() {
            return Err(last_error());
        }
        // SAFETY: a fresh handle this process owns.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(h) }))
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
}

/// How an overlapped operation ended.
enum Io {
    /// It moved this many bytes.
    Done(u32),
    /// The timeout passed first; it was cancelled with nothing moved.
    TimedOut,
}

/// Run one overlapped operation on `h` and wait at most `timeout` for it.
/// The `OVERLAPPED` lives on this stack frame, and the call returns only
/// once the operation is complete or cancelled, so nothing refers to it
/// afterwards.
fn overlapped(
    h: HANDLE,
    event: &Event,
    timeout: Option<Duration>,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<Io> {
    // SAFETY: plain data; all-zero is a valid OVERLAPPED.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = event.raw();
    // SAFETY: an event this process owns.
    unsafe { ResetEvent(event.raw()) };
    let ok = start(&mut ov);
    if ok == 0 {
        // SAFETY: reads the calling thread's last error.
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        // SAFETY: an event this process owns.
        let waited = unsafe { WaitForSingleObject(event.raw(), millis(timeout)) };
        if waited != WAIT_OBJECT_0 {
            // Timed out (or the wait failed): stop the operation before
            // `ov` goes away.
            // SAFETY: cancels this operation only; `ov` is still live.
            unsafe { CancelIoEx(h, &ov) };
        }
    }
    let mut n = 0u32;
    // SAFETY: waits for the operation on `ov` to finish, which it does
    // promptly once complete or cancelled.
    let ok = unsafe { GetOverlappedResult(h, &ov, &mut n, 1) };
    if ok != 0 {
        return Ok(Io::Done(n));
    }
    // SAFETY: reads the calling thread's last error.
    let err = unsafe { GetLastError() };
    if err == ERROR_OPERATION_ABORTED {
        return Ok(Io::TimedOut);
    }
    Err(io::Error::from_raw_os_error(err as i32))
}

/// Whether `e` means the other end has gone.
fn is_closed(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
    )
}

/// The protected DACL every pipe instance is made with: full access for the
/// current user, nobody else.
struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable once made, and only read.
unsafe impl Send for Security {}
// SAFETY: as above.
unsafe impl Sync for Security {}

impl Security {
    fn current_user_only() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string; the descriptor is allocated
        // by the call and freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(Self { descriptor })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor..., freed once.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The current user's SID as a string (`S-1-5-21-...`).
fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: opens this process's own token for query.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    // SAFETY: a fresh handle this process owns.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut len = 0u32;
    // SAFETY: a size query; fails with the size needed.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };
    // u64s keep the buffer aligned for TOKEN_USER.
    let mut buffer = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: a buffer of at least `len` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    // SAFETY: the call filled a TOKEN_USER at the start of the buffer.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: the SID lives in `buffer`; the string is allocated by the call.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(last_error());
    }
    // SAFETY: a NUL-terminated wide string the call allocated.
    let sid = unsafe {
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
    };
    // SAFETY: allocated by ConvertSidToStringSidW, freed once.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

/// One pipe instance waiting for a client.
struct Pending {
    pipe: OwnedHandle,
    /// Boxed so its address stays put while the connect is in flight.
    ov: Box<OVERLAPPED>,
    /// A connect is in flight on `ov`.
    connecting: bool,
}

// SAFETY: the OVERLAPPED is only touched under the listener's lock.
unsafe impl Send for Pending {}

/// A named pipe a consumer listens on.
pub struct Listener {
    name: String,
    security: Security,
    event: Event,
    pending: Mutex<Option<Pending>>,
}

impl Listener {
    /// Listen at a fresh name.
    pub fn bind_fresh() -> io::Result<Self> {
        let name = format!(
            r"\\.\pipe\lumen-surface-{}-{}",
            std::process::id(),
            crate::endpoint::hex(&random_bytes()?[..6])
        );
        let listener = Self {
            name,
            security: Security::current_user_only()?,
            event: Event::new()?,
            pending: Mutex::new(None),
        };
        // The first instance claims the name: creating it fails if another
        // process made a pipe of that name first.
        let first = listener.instance(true)?;
        *listener.lock() = Some(listener.start_connect(first)?);
        Ok(listener)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where a producer reaches this listener, for its endpoint string.
    pub fn transport(&self) -> Transport {
        Transport::Pipe(self.name.clone())
    }

    fn instance(&self, first: bool) -> io::Result<OwnedHandle> {
        let name = wide(&self.name);
        let attributes = self.security.attributes();
        let mut mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: a NUL-terminated name and valid security attributes.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &attributes,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        // SAFETY: a fresh handle this process owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }

    /// Start waiting for a client on `pipe`.
    fn start_connect(&self, pipe: OwnedHandle) -> io::Result<Pending> {
        // SAFETY: plain data; all-zero is a valid OVERLAPPED.
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = self.event.raw();
        // SAFETY: an event this process owns.
        unsafe { ResetEvent(self.event.raw()) };
        // SAFETY: the OVERLAPPED is boxed and outlives the operation (it is
        // waited for or cancelled before the box goes).
        let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), &mut *ov) };
        let connecting = if ok != 0 {
            false
        } else {
            // SAFETY: reads the calling thread's last error.
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => true,
                // A client connected between creation and this call.
                ERROR_PIPE_CONNECTED => false,
                err => return Err(io::Error::from_raw_os_error(err as i32)),
            }
        };
        Ok(Pending {
            pipe,
            ov,
            connecting,
        })
    }

    /// Take the next connection, waiting at most `timeout`.
    pub fn accept(&self, timeout: Option<Duration>) -> io::Result<Option<Connection>> {
        let mut pending = self.lock();
        if pending.is_none() {
            let pipe = self.instance(false)?;
            *pending = Some(self.start_connect(pipe)?);
        }
        let p = pending.as_mut().expect("just made");
        if p.connecting {
            // SAFETY: the listener's event.
            let waited = unsafe { WaitForSingleObject(self.event.raw(), millis(timeout)) };
            if waited != WAIT_OBJECT_0 {
                // Timed out; the connect stays in flight for the next call.
                return Ok(None);
            }
            let mut n = 0u32;
            // SAFETY: the connect on `ov` has completed.
            let ok = unsafe { GetOverlappedResult(p.pipe.as_raw_handle(), &*p.ov, &mut n, 0) };
            if ok == 0 {
                let e = last_error();
                // The instance is unusable; the next accept makes another.
                *pending = None;
                return Err(e);
            }
            p.connecting = false;
        }
        let taken = pending.take().expect("pending");
        // The next instance, so the name never disappears between clients.
        // A failure here leaves the next accept to try again.
        if let Ok(pipe) = self.instance(false) {
            *pending = self.start_connect(pipe).ok();
        }
        drop(pending);
        Ok(Some(Connection::server(taken.pipe)))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut pending = self.lock();
        if let Some(p) = pending.as_mut()
            && p.connecting
        {
            let mut n = 0u32;
            // SAFETY: cancels the connect on `ov` and waits for it to finish
            // before the box goes.
            unsafe {
                CancelIoEx(p.pipe.as_raw_handle(), &*p.ov);
                GetOverlappedResult(p.pipe.as_raw_handle(), &*p.ov, &mut n, 1);
            }
        }
        *pending = None;
    }
}

/// The pipe both halves of a connection use, closed once for both.
struct Pipe {
    state: Mutex<PipeState>,
    idle: Condvar,
}

struct PipeState {
    handle: Option<OwnedHandle>,
    /// Operations in flight on the handle.
    users: usize,
}

impl Pipe {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            state: Mutex::new(PipeState {
                handle: Some(handle),
                users: 0,
            }),
            idle: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PipeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` on the open handle; `None` once the pipe is closed.
    fn with<T>(&self, f: impl FnOnce(HANDLE) -> T) -> Option<T> {
        let raw = {
            let mut state = self.lock();
            let raw = state.handle.as_ref()?.as_raw_handle();
            state.users += 1;
            raw
        };
        let out = f(raw);
        self.lock().users -= 1;
        self.idle.notify_all();
        Some(out)
    }

    /// Close the handle now: operations in flight are cancelled, and the
    /// other end sees the pipe end once it has read what was written.
    fn close(&self) {
        let mut state = self.lock();
        let Some(raw) = state.handle.as_ref().map(AsRawHandle::as_raw_handle) else {
            return;
        };
        // No new operation starts from here (see `with`); the handle stays
        // open until the ones in flight are out.
        let handle = state.handle.take();
        while state.users > 0 {
            // SAFETY: cancels every operation on the handle, which `handle`
            // keeps open. Repeated, as one may have started just after.
            unsafe { CancelIoEx(raw, std::ptr::null()) };
            state = self
                .idle
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(state);
        drop(handle);
    }
}

/// Frames waiting for the writer thread.
struct Outbox {
    state: Mutex<OutState>,
    changed: Condvar,
}

struct OutState {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    /// The writer thread is writing a frame it took off the queue.
    busy: bool,
    /// Why writing stopped, once it has.
    failed: Option<io::ErrorKind>,
    /// No sender is left, or the connection was shut down: write what is
    /// queued and stop.
    done: bool,
}

impl Outbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, OutState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn finish(&self) {
        self.lock().done = true;
        self.changed.notify_all();
    }
}

/// The writer thread: writes queued frames in order until the senders are
/// gone and the queue is empty, or the pipe fails.
fn write_loop(pipe: Arc<Pipe>, outbox: Arc<Outbox>) {
    let Ok(event) = Event::new() else {
        outbox.lock().failed = Some(io::ErrorKind::Other);
        outbox.changed.notify_all();
        return;
    };
    loop {
        let frame = {
            let mut state = outbox.lock();
            loop {
                if state.failed.is_some() {
                    return;
                }
                if let Some(frame) = state.frames.pop_front() {
                    state.busy = true;
                    break frame;
                }
                if state.done {
                    return;
                }
                state = outbox
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = write_all(&pipe, &event, &frame);
        let mut state = outbox.lock();
        state.bytes -= frame.len();
        state.busy = false;
        if let Err(e) = result {
            state.failed = Some(e.kind());
            state.frames.clear();
            state.bytes = 0;
        }
        drop(state);
        outbox.changed.notify_all();
    }
}

fn write_all(pipe: &Pipe, event: &Event, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let n = pipe
            .with(|h| {
                overlapped(h, event, None, |ov| {
                    // SAFETY: writes from `bytes`, which outlives the
                    // operation (it is waited for).
                    unsafe {
                        WriteFile(
                            h,
                            bytes.as_ptr(),
                            bytes.len().min(u32::MAX as usize) as u32,
                            std::ptr::null_mut(),
                            ov,
                        )
                    }
                })
            })
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))??;
        match n {
            Io::Done(0) | Io::TimedOut => {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Io::Done(n) => bytes = &bytes[n as usize..],
        }
    }
    Ok(())
}

/// What senders share: the queue. When the last sender goes, the writer
/// finishes the queue and stops.
struct SenderShared {
    outbox: Arc<Outbox>,
}

impl Drop for SenderShared {
    fn drop(&mut self) {
        self.outbox.finish();
    }
}

/// The writing half of a connection. Clones share one queue; frames are
/// written whole and in order by the connection's writer thread.
#[derive(Clone)]
pub struct Sender {
    shared: Arc<SenderShared>,
}

impl Sender {
    fn queue(&self, message: &Message, wait: bool) -> Result<(), TransportError> {
        let frame = message.encode()?;
        let outbox = &self.shared.outbox;
        let mut state = outbox.lock();
        loop {
            if let Some(kind) = state.failed {
                return Err(TransportError::Io(kind.into()));
            }
            if state.done {
                return Err(TransportError::Io(io::ErrorKind::BrokenPipe.into()));
            }
            if state.bytes <= QUEUE_LIMIT {
                break;
            }
            if !wait {
                return Err(TransportError::Io(io::ErrorKind::WouldBlock.into()));
            }
            state = outbox
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        state.bytes += frame.len();
        state.frames.push_back(frame);
        drop(state);
        outbox.changed.notify_all();
        Ok(())
    }

    /// Queue `message` to be written. A ring's handles are not sent: the
    /// message names them by value (`BufferDesc::handle`), and the consumer
    /// copies them out of this process; `handles` must be those values.
    ///
    /// # Errors
    ///
    /// The message does not encode, or the pipe failed (the other side went
    /// away).
    pub fn send(&self, message: &Message, handles: &[RawHandle]) -> Result<(), TransportError> {
        if let Message::Buffers(b) = message
            && (handles.len() != b.buffers.len()
                || b.buffers
                    .iter()
                    .zip(handles)
                    .any(|(d, h)| d.handle != *h as usize as u64))
        {
            return Err(TransportError::Io(io::Error::other(
                "each buffer names its handle by value",
            )));
        }
        self.queue(message, true)
    }

    /// [`Self::send`], unless the other side has stopped reading and the
    /// queue is full, in which case nothing is queued and the error is
    /// [`io::ErrorKind::WouldBlock`].
    ///
    /// # Errors
    ///
    /// As [`Self::send`], plus the would-block case.
    pub fn try_send(&self, message: &Message, handles: &[RawHandle]) -> Result<(), TransportError> {
        if !handles.is_empty() {
            return Err(TransportError::Io(io::Error::other(
                "a ring's handles are sent with `send`",
            )));
        }
        self.queue(message, false)
    }
}

/// The reading half of a connection.
pub struct Receiver {
    pipe: Arc<Pipe>,
    outbox: Arc<Outbox>,
    /// The event reads wait on; `None` when none could be made, which fails
    /// every read.
    event: Option<Event>,
    buffer: Vec<u8>,
    /// The process on the other end, opened to copy handles out of (the
    /// consumer's end only).
    peer: Option<OwnedHandle>,
}

impl Receiver {
    /// The next message, waiting at most `timeout` (`None`: no limit) for
    /// bytes to arrive.
    ///
    /// # Errors
    ///
    /// The pipe failed, a frame does not read, or a handle it names could
    /// not be copied out of the producer's process.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Received, TransportError> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if let Some((message, flags, used)) = Message::read_frame(&self.buffer)? {
                self.buffer.drain(..used);
                let Some(message) = message else {
                    continue;
                };
                let handles = self.handles_for(&message, flags & FLAG_HANDLES != 0)?;
                return Ok(Received::Message(message, handles));
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            match self.fill(left)? {
                Some(true) => {}
                Some(false) => return Ok(Received::Closed),
                None => return Ok(Received::Timeout),
            }
        }
    }

    /// Copy each handle a ring names out of the producer's process.
    fn handles_for(
        &mut self,
        message: &Message,
        carries: bool,
    ) -> Result<Vec<Handle>, TransportError> {
        let (Message::Buffers(b), true) = (message, carries) else {
            return Ok(Vec::new());
        };
        let Some(peer) = &self.peer else {
            return Err(TransportError::MissingHandles);
        };
        let mut out = Vec::with_capacity(b.buffers.len());
        for desc in &b.buffers {
            let mut copy: HANDLE = std::ptr::null_mut();
            // SAFETY: copies a handle out of the peer process, which `peer`
            // was opened for; the copy belongs to this process.
            let ok = unsafe {
                DuplicateHandle(
                    peer.as_raw_handle(),
                    desc.handle as usize as HANDLE,
                    GetCurrentProcess(),
                    &mut copy,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                return Err(TransportError::Io(io::Error::other(format!(
                    "could not copy handle {:#x} out of the producer: {}",
                    desc.handle,
                    last_error()
                ))));
            }
            // SAFETY: a fresh handle this process owns.
            out.push(unsafe { OwnedHandle::from_raw_handle(copy) });
        }
        Ok(out)
    }

    /// Read what the pipe holds, waiting at most `timeout` for the first
    /// byte. `Some(true)`: bytes came; `Some(false)`: end of stream;
    /// `None`: nothing came in time.
    fn fill(&mut self, timeout: Option<Duration>) -> io::Result<Option<bool>> {
        if timeout == Some(Duration::ZERO) {
            // Look without waiting: a read cancelled at once would cost a
            // round trip through the kernel for nothing.
            let mut available = 0u32;
            let peeked = self.pipe.with(|h| {
                // SAFETY: a peek that reads no bytes.
                unsafe {
                    PeekNamedPipe(
                        h,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                }
            });
            match peeked {
                None => return Ok(Some(false)),
                Some(0) => {
                    let e = last_error();
                    return if is_closed(&e) {
                        Ok(Some(false))
                    } else {
                        Err(e)
                    };
                }
                Some(_) if available == 0 => return Ok(None),
                Some(_) => {}
            }
        }
        let Some(event) = &self.event else {
            return Err(io::Error::other("the pipe has no event to read with"));
        };
        let mut chunk = vec![0u8; 64 * 1024];
        let read = self.pipe.with(|h| {
            overlapped(h, event, timeout, |ov| {
                // SAFETY: reads into `chunk`, which outlives the operation
                // (it is waited for or cancelled).
                unsafe {
                    ReadFile(
                        h,
                        chunk.as_mut_ptr(),
                        chunk.len() as u32,
                        std::ptr::null_mut(),
                        ov,
                    )
                }
            })
        });
        match read {
            None => Ok(Some(false)),
            Some(Ok(Io::Done(0))) => Ok(Some(false)),
            Some(Ok(Io::Done(n))) => {
                self.buffer.extend_from_slice(&chunk[..n as usize]);
                Ok(Some(true))
            }
            Some(Ok(Io::TimedOut)) => {
                // A shutdown also cancels the read; then the pipe is closed.
                if self.pipe.lock().handle.is_none() {
                    Ok(Some(false))
                } else {
                    Ok(None)
                }
            }
            Some(Err(e)) if is_closed(&e) => Ok(Some(false)),
            Some(Err(e)) => Err(e),
        }
    }

    /// Close the connection for both halves: what is queued is written (for
    /// at most a second, against a peer that stopped reading), then the pipe
    /// closes; a reader waiting on the other end sees it end, and this
    /// side's sender fails from here on.
    pub fn shutdown(&self) {
        let deadline = Instant::now() + DRAIN;
        {
            let mut state = self.outbox.lock();
            state.done = true;
            self.outbox.changed.notify_all();
            while (!state.frames.is_empty() || state.busy) && state.failed.is_none() {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                state = self
                    .outbox
                    .changed
                    .wait_timeout(state, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        self.pipe.close();
    }
}

/// One connection, before it is split into its halves.
pub struct Connection {
    pipe: OwnedHandle,
    peer: Option<OwnedHandle>,
}

impl Connection {
    /// The consumer's end of a client that connected: the client's process
    /// is opened for copying handles out of.
    fn server(pipe: OwnedHandle) -> Self {
        let mut pid = 0u32;
        // SAFETY: a connected pipe instance this process owns.
        let peer = if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } != 0 {
            // SAFETY: opens the client process for handle duplication only.
            let h = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) };
            // SAFETY: a fresh handle this process owns.
            (!h.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(h) })
        } else {
            None
        };
        Self { pipe, peer }
    }

    /// Connect to the listener `transport` names.
    ///
    /// # Errors
    ///
    /// The transport is not a named pipe, or the pipe cannot be opened (no
    /// consumer there, or another user's).
    pub fn connect(transport: &Transport) -> io::Result<Self> {
        let Transport::Pipe(name) = transport else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a Unix socket is not a Windows transport",
            ));
        };
        let wname = wide(name);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut missing = 0;
        loop {
            // SAFETY: a NUL-terminated name. SECURITY_IDENTIFICATION keeps
            // the consumer from acting as this process.
            let h = unsafe {
                CreateFileW(
                    wname.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                return Ok(Self {
                    // SAFETY: a fresh handle this process owns.
                    pipe: unsafe { OwnedHandle::from_raw_handle(h) },
                    peer: None,
                });
            }
            // SAFETY: reads the calling thread's last error.
            let err = unsafe { GetLastError() };
            if Instant::now() >= deadline {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            match err {
                // Every instance is taken: wait for the listener's next.
                ERROR_PIPE_BUSY => {
                    // SAFETY: a NUL-terminated name.
                    unsafe { WaitNamedPipeW(wname.as_ptr(), 1000) };
                }
                // Between one client and the next instance there is a
                // moment with none; a name that stays missing is wrong.
                ERROR_FILE_NOT_FOUND if missing < 20 => {
                    missing += 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
                err => return Err(io::Error::from_raw_os_error(err as i32)),
            }
        }
    }

    /// The two halves; the writer thread starts here.
    pub fn split(self) -> (Sender, Receiver) {
        let pipe = Arc::new(Pipe::new(self.pipe));
        let outbox = Arc::new(Outbox {
            state: Mutex::new(OutState {
                frames: VecDeque::new(),
                bytes: 0,
                busy: false,
                failed: None,
                done: false,
            }),
            changed: Condvar::new(),
        });
        let (thread_pipe, thread_outbox) = (pipe.clone(), outbox.clone());
        let spawned = std::thread::Builder::new()
            .name("lumen-surface pipe writer".into())
            .spawn(move || write_loop(thread_pipe, thread_outbox));
        if spawned.is_err() {
            outbox.lock().failed = Some(io::ErrorKind::Other);
        }
        let event = Event::new().ok();
        (
            Sender {
                shared: Arc::new(SenderShared {
                    outbox: outbox.clone(),
                }),
            },
            Receiver {
                pipe,
                outbox,
                event,
                buffer: Vec::new(),
                peer: self.peer,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{BufferDesc, Buffers, Format};
    use std::io::{Read, Seek, Write};
    use std::os::windows::io::IntoRawHandle;

    fn pair() -> (Sender, Receiver, Sender, Receiver, Listener) {
        let listener = Listener::bind_fresh().unwrap();
        let client = Connection::connect(&listener.transport()).unwrap();
        let server = listener
            .accept(Some(Duration::from_secs(5)))
            .unwrap()
            .expect("a connection");
        let (cs, cr) = client.split();
        let (ss, sr) = server.split();
        (cs, cr, ss, sr, listener)
    }

    /// Frames cross in order; the handles a ring names by value are copied
    /// out of the sending process and open the same files.
    #[test]
    fn frames_and_handles_cross() {
        let (client, _cr, _ss, mut server, _l) = pair();
        let dir = std::env::temp_dir();
        let mut files = Vec::new();
        for i in 0..2u8 {
            let path = dir.join(format!(
                "lsp-pipe-{}-{i}",
                crate::endpoint::hex(&random_bytes().unwrap()[..6])
            ));
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            f.write_all(&[i; 4]).unwrap();
            files.push((f, path));
        }
        let raws: Vec<RawHandle> = files.iter().map(|(f, _)| f.as_raw_handle()).collect();
        client.send(&Message::Ping(1), &[]).unwrap();
        let buffers = Buffers {
            generation: 1,
            width: 4,
            height: 4,
            format: Format::Rgba8Srgb,
            usage: 0,
            flags: 0,
            buffers: raws
                .iter()
                .map(|h| BufferDesc {
                    handle: *h as usize as u64,
                    ..BufferDesc::default()
                })
                .collect(),
        };
        client
            .send(&Message::Buffers(buffers.clone()), &raws)
            .unwrap();
        client.send(&Message::Pong(2), &[]).unwrap();

        let timeout = Some(Duration::from_secs(5));
        assert!(matches!(
            server.recv(timeout).unwrap(),
            Received::Message(Message::Ping(1), h) if h.is_empty()
        ));
        let Received::Message(Message::Buffers(got), handles) = server.recv(timeout).unwrap()
        else {
            panic!("buffers")
        };
        assert_eq!(got, buffers);
        assert_eq!(handles.len(), 2);
        for (i, handle) in handles.into_iter().enumerate() {
            // SAFETY: a file handle this process owns.
            let mut f = unsafe { std::fs::File::from_raw_handle(handle.into_raw_handle()) };
            f.rewind().unwrap();
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, vec![i as u8; 4]);
        }
        assert!(matches!(
            server.recv(timeout).unwrap(),
            Received::Message(Message::Pong(2), _)
        ));
        assert!(matches!(
            server.recv(Some(Duration::from_millis(10))).unwrap(),
            Received::Timeout
        ));
        assert!(matches!(
            server.recv(Some(Duration::ZERO)).unwrap(),
            Received::Timeout
        ));
        drop(client);
        drop(_cr);
        assert!(matches!(server.recv(timeout).unwrap(), Received::Closed));
        for (f, path) in files {
            drop(f);
            let _ = std::fs::remove_file(path);
        }
    }

    /// A shutdown writes what was queued, then the other end sees the
    /// connection end; this side's sender fails from then on.
    #[test]
    fn a_shutdown_delivers_the_goodbye_then_ends() {
        let (client, mut client_rx, server, server_rx, _l) = pair();
        server
            .send(
                &Message::Bye {
                    reason: crate::wire::ByeReason::Closed,
                    detail: "done".into(),
                },
                &[],
            )
            .unwrap();
        server_rx.shutdown();
        let timeout = Some(Duration::from_secs(5));
        assert!(matches!(
            client_rx.recv(timeout).unwrap(),
            Received::Message(Message::Bye { .. }, _)
        ));
        assert!(matches!(client_rx.recv(timeout).unwrap(), Received::Closed));
        assert!(server.send(&Message::Ping(1), &[]).is_err());
        drop(client);
    }

    /// A second pipe of the same name cannot be squatted first: the first
    /// instance claims it.
    #[test]
    fn the_name_is_claimed_and_found() {
        let listener = Listener::bind_fresh().unwrap();
        let Transport::Pipe(name) = listener.transport() else {
            panic!("a pipe")
        };
        assert!(name.starts_with(r"\\.\pipe\lumen-surface-"), "{name}");
        assert!(listener.instance(true).is_err(), "the name is taken");
        assert!(Connection::connect(&Transport::Pipe(format!("{name}-missing"))).is_err());
    }
}
