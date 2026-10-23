//! The Unix transport: a stream socket in a private per-user directory, with
//! file descriptors passed alongside the frames that name them.
//!
//! Frames are written whole with one `sendmsg` each; a frame that carries
//! descriptors attaches them to that call, so they arrive with the frame's
//! first bytes. The reader keeps the descriptors that arrived in order and
//! hands a frame the ones it says it carries.

pub use crate::transport::{Received, TransportError};
use crate::wire::{FLAG_HANDLES, Message, WireError};
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most descriptors one frame carries.
pub const MAX_FDS: usize = 8;

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn sockaddr(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: an all-zero sockaddr_un is a valid value of the type.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the socket path is too long",
        ));
    }
    for (slot, b) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = *b as libc::c_char;
    }
    Ok((
        addr,
        std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
    ))
}

/// Linux opens descriptors close-on-exec atomically; macOS has no flag for
/// it on `socket`, `accept` or `recvmsg`, so each descriptor is marked right
/// after it is made, through [`close_on_exec`].
#[cfg(target_os = "linux")]
const SOCKET_CLOEXEC: libc::c_int = libc::SOCK_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const SOCKET_CLOEXEC: libc::c_int = 0;
#[cfg(target_os = "linux")]
const RECV_CLOEXEC: libc::c_int = libc::MSG_CMSG_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const RECV_CLOEXEC: libc::c_int = 0;

/// Mark `fd` close-on-exec where the call that made it could not.
#[cfg(not(target_os = "linux"))]
fn close_on_exec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor this process owns.
    unsafe {
        let flags = cvt(libc::fcntl(fd, libc::F_GETFD))?;
        cvt(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn close_on_exec(_fd: RawFd) -> io::Result<()> {
    Ok(())
}

fn stream_socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the descriptor is owned from here on.
    let fd = unsafe {
        OwnedFd::from_raw_fd(cvt(libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | SOCKET_CLOEXEC,
            0,
        ))?)
    };
    close_on_exec(fd.as_raw_fd())?;
    Ok(fd)
}

/// Wait until `fd` is readable, for at most `timeout` (`None`: no limit).
/// Returns whether it is.
fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as libc::c_int);
    loop {
        // SAFETY: one valid pollfd, owned on this stack frame.
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
}

/// The private directory for sockets, `lumen-surface-<uid>` under
/// `$XDG_RUNTIME_DIR` (or the temporary directory when there is no runtime
/// directory), made if missing and opened to this user only.
pub fn socket_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("lumen-surface-{}", current_uid()));
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn current_uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// 16 bytes from the system's random source.
pub fn random_bytes() -> io::Result<[u8; 16]> {
    use std::io::Read;
    let mut out = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut out)?;
    Ok(out)
}

/// A socket a consumer listens on. The socket file goes with it.
pub struct Listener {
    fd: OwnedFd,
    path: PathBuf,
}

impl Listener {
    /// Listen at a fresh path in [`socket_dir`].
    pub fn bind_fresh() -> io::Result<Self> {
        let dir = socket_dir()?;
        let name = format!(
            "{}-{}.sock",
            std::process::id(),
            crate::endpoint::hex(&random_bytes()?[..6])
        );
        Self::bind(&dir.join(name))
    }

    /// Listen at `path`, replacing a stale socket file there.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let _ = std::fs::remove_file(path);
        let fd = stream_socket()?;
        let (addr, len) = sockaddr(path)?;
        // SAFETY: a valid socket and a sockaddr_un of the stated length.
        unsafe {
            cvt(libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_un).cast(),
                len,
            ))?;
            cvt(libc::listen(fd.as_raw_fd(), 4))?;
        }
        Ok(Self {
            fd,
            path: path.to_path_buf(),
        })
    }

    /// Where the socket is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the next connection, waiting at most `timeout`. A connection from
    /// another user is closed and not returned.
    pub fn accept(&self, timeout: Option<Duration>) -> io::Result<Option<Connection>> {
        if !wait_readable(self.fd.as_raw_fd(), timeout)? {
            return Ok(None);
        }
        let fd = accept(self.fd.as_raw_fd())?;
        if peer_uid(&fd).is_some_and(|uid| uid != current_uid()) {
            return Ok(None);
        }
        Ok(Some(Connection::from_fd(fd)))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Accept a connection on a listening socket, close-on-exec.
#[cfg(target_os = "linux")]
fn accept(listener: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: accept4 on a listening socket; the result is owned.
    unsafe {
        Ok(OwnedFd::from_raw_fd(cvt(libc::accept4(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        ))?))
    }
}

/// Accept a connection on a listening socket, close-on-exec.
#[cfg(not(target_os = "linux"))]
fn accept(listener: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: accept on a listening socket; the result is owned.
    let fd = unsafe {
        OwnedFd::from_raw_fd(cvt(libc::accept(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ))?)
    };
    close_on_exec(fd.as_raw_fd())?;
    Ok(fd)
}

/// The user id of the process on the other end, where the platform says.
#[cfg(target_os = "linux")]
fn peer_uid(fd: &OwnedFd) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the given length.
    let r = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

/// The user id of the process on the other end.
#[cfg(not(target_os = "linux"))]
fn peer_uid(fd: &OwnedFd) -> Option<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes two ids for a connected socket.
    let r = unsafe { libc::getpeereid(fd.as_raw_fd(), &mut uid, &mut gid) };
    (r == 0).then_some(uid)
}

/// The writing half of a connection. Clones share the socket; each frame is
/// written whole under a lock, so threads may send at once.
#[derive(Clone)]
pub struct Sender {
    fd: Arc<Mutex<OwnedFd>>,
}

impl Sender {
    /// Write `message`, with `fds` attached when it carries handles.
    ///
    /// # Errors
    ///
    /// The message does not encode, or the socket failed (the other side
    /// went away).
    pub fn send(&self, message: &Message, fds: &[RawFd]) -> Result<(), TransportError> {
        let frame = message.encode()?;
        if fds.len() > MAX_FDS {
            return Err(TransportError::Wire(WireError::TooLarge("the handle list")));
        }
        let fd = self.fd.lock().unwrap_or_else(|e| e.into_inner());
        send_all(fd.as_raw_fd(), &frame, fds, false)?;
        Ok(())
    }

    /// [`Self::send`], unless the other side has stopped reading and the
    /// socket is full, in which case nothing is written and the error is
    /// [`io::ErrorKind::WouldBlock`]. A thread that must not stall on a
    /// peer that hangs (a UI thread) sends this way.
    ///
    /// # Errors
    ///
    /// As [`Self::send`], plus the would-block case.
    pub fn try_send(&self, message: &Message, fds: &[RawFd]) -> Result<(), TransportError> {
        let frame = message.encode()?;
        let fd = self.fd.lock().unwrap_or_else(|e| e.into_inner());
        send_all(fd.as_raw_fd(), &frame, fds, true)?;
        Ok(())
    }
}

/// Write all of `bytes`, attaching `fds` to the first write. With
/// `nonblocking`, a socket too full to take the first byte fails with
/// `WouldBlock`; once part of a frame is out, the rest is written whatever
/// it takes, so frames never interleave or tear.
fn send_all(fd: RawFd, bytes: &[u8], fds: &[RawFd], nonblocking: bool) -> io::Result<()> {
    let mut sent = 0;
    let mut attach = fds;
    while sent < bytes.len() {
        let rest = &bytes[sent..];
        let mut iov = libc::iovec {
            iov_base: rest.as_ptr() as *mut libc::c_void,
            iov_len: rest.len(),
        };
        // SAFETY: a zeroed msghdr is valid; the fields set below point at
        // buffers that outlive the call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let payload = std::mem::size_of_val(attach);
        // SAFETY: CMSG_SPACE is a pure size computation.
        let space = unsafe { libc::CMSG_SPACE(payload as u32) } as usize;
        let mut control = vec![0u8; space.max(1)];
        if !attach.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            // SAFETY: the control buffer holds one header of `space` bytes.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&msg);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
                std::ptr::copy_nonoverlapping(
                    attach.as_ptr(),
                    libc::CMSG_DATA(header).cast::<RawFd>(),
                    attach.len(),
                );
            }
        }
        let flags = if nonblocking && sent == 0 {
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT
        } else {
            libc::MSG_NOSIGNAL
        };
        // SAFETY: the msghdr describes valid buffers.
        let n = unsafe { libc::sendmsg(fd, &msg, flags) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        sent += n as usize;
        attach = &[];
    }
    Ok(())
}

/// The reading half of a connection.
pub struct Receiver {
    fd: Arc<Mutex<OwnedFd>>,
    raw: RawFd,
    buffer: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

impl Receiver {
    /// The next message, waiting at most `timeout` (`None`: no limit) for
    /// bytes to arrive.
    ///
    /// # Errors
    ///
    /// The socket failed, or a frame does not read.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Received, TransportError> {
        loop {
            if let Some((message, flags, used)) = Message::read_frame(&self.buffer)? {
                self.buffer.drain(..used);
                let Some(message) = message else {
                    continue;
                };
                let wanted = match (&message, flags & FLAG_HANDLES != 0) {
                    (Message::Buffers(b), true) => b.buffers.len(),
                    _ => 0,
                };
                if self.fds.len() < wanted {
                    return Err(TransportError::MissingHandles);
                }
                let fds = self.fds.drain(..wanted).collect();
                return Ok(Received::Message(message, fds));
            }
            if !wait_readable(self.raw, timeout)? {
                return Ok(Received::Timeout);
            }
            if !self.fill()? {
                return Ok(Received::Closed);
            }
        }
    }

    /// Read what the socket holds. Returns `false` at end of stream.
    fn fill(&mut self) -> io::Result<bool> {
        let mut chunk = vec![0u8; 64 * 1024];
        let mut iov = libc::iovec {
            iov_base: chunk.as_mut_ptr().cast(),
            iov_len: chunk.len(),
        };
        // SAFETY: a zeroed msghdr is valid; the fields set point at owned
        // buffers.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        // SAFETY: a pure size computation.
        let space =
            unsafe { libc::CMSG_SPACE((MAX_FDS * std::mem::size_of::<RawFd>()) as u32) } as usize;
        let mut control = vec![0u8; space];
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let n = loop {
            // SAFETY: the msghdr describes valid owned buffers.
            let n = unsafe { libc::recvmsg(self.raw, &mut msg, RECV_CLOEXEC) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if e.kind() == io::ErrorKind::ConnectionReset {
                    return Ok(false);
                }
                return Err(e);
            }
            break n as usize;
        };
        // SAFETY: walking the control headers recvmsg filled in.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&msg);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let bytes = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    let data = libc::CMSG_DATA(header).cast::<RawFd>();
                    for i in 0..bytes / std::mem::size_of::<RawFd>() {
                        let fd = OwnedFd::from_raw_fd(data.add(i).read_unaligned());
                        // Best effort: a descriptor that cannot be marked is
                        // still the buffer the frame names.
                        let _ = close_on_exec(fd.as_raw_fd());
                        self.fds.push_back(fd);
                    }
                }
                header = libc::CMSG_NXTHDR(&msg, header);
            }
        }
        if n == 0 {
            return Ok(false);
        }
        self.buffer.extend_from_slice(&chunk[..n]);
        Ok(true)
    }

    /// Close the connection for both halves: a reader waiting on the other
    /// end sees it end, and so does this side's sender.
    pub fn shutdown(&self) {
        let fd = self.fd.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: shutdown on an open socket.
        unsafe {
            libc::shutdown(fd.as_raw_fd(), libc::SHUT_RDWR);
        }
    }
}

/// One connection, before it is split into its halves.
pub struct Connection {
    fd: OwnedFd,
}

impl Connection {
    fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Connect to a listener at `path`.
    pub fn connect(path: &Path) -> io::Result<Self> {
        let fd = stream_socket()?;
        let (addr, len) = sockaddr(path)?;
        // SAFETY: a valid socket and a sockaddr_un of the stated length.
        unsafe {
            cvt(libc::connect(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_un).cast(),
                len,
            ))?;
        }
        Ok(Self { fd })
    }

    /// The two halves.
    pub fn split(self) -> (Sender, Receiver) {
        let raw = self.fd.as_raw_fd();
        let fd = Arc::new(Mutex::new(self.fd));
        (
            Sender { fd: fd.clone() },
            Receiver {
                fd,
                raw,
                buffer: Vec::new(),
                fds: VecDeque::new(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{BufferDesc, Buffers, Format};
    use std::io::{Read, Seek, Write};

    fn pair() -> (Sender, Receiver, Sender, Receiver, Listener) {
        let dir = std::env::temp_dir().join(format!("lsp-test-{}", random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let listener = Listener::bind(&dir.join("s.sock")).unwrap();
        let client = Connection::connect(listener.path()).unwrap();
        let server = listener
            .accept(Some(Duration::from_secs(5)))
            .unwrap()
            .expect("a connection");
        let (cs, cr) = client.split();
        let (ss, sr) = server.split();
        (cs, cr, ss, sr, listener)
    }

    fn random_hex() -> String {
        crate::endpoint::hex(&random_bytes().unwrap()[..6])
    }

    /// Frames cross in order; descriptors arrive with the frame that names
    /// them and read the same file.
    #[test]
    fn frames_and_descriptors_cross() {
        let (client, _cr, _ss, mut server, _l) = pair();
        let mut files = Vec::new();
        for i in 0..2u8 {
            let mut f = tempfile_with(&[i; 4]);
            f.rewind().unwrap();
            files.push(f);
        }
        let fds: Vec<RawFd> = files.iter().map(|f| f.as_raw_fd()).collect();
        client.send(&Message::Ping(1), &[]).unwrap();
        let buffers = Buffers {
            generation: 1,
            width: 4,
            height: 4,
            format: Format::Rgba8Srgb,
            usage: 0,
            flags: 0,
            buffers: vec![BufferDesc::default(); 2],
        };
        client
            .send(&Message::Buffers(buffers.clone()), &fds)
            .unwrap();
        client.send(&Message::Pong(2), &[]).unwrap();

        let timeout = Some(Duration::from_secs(5));
        assert!(
            matches!(server.recv(timeout).unwrap(), Received::Message(Message::Ping(1), f) if f.is_empty())
        );
        let Received::Message(Message::Buffers(got), got_fds) = server.recv(timeout).unwrap()
        else {
            panic!("buffers")
        };
        assert_eq!(got, buffers);
        assert_eq!(got_fds.len(), 2);
        for (i, fd) in got_fds.into_iter().enumerate() {
            let mut f = std::fs::File::from(fd);
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
        drop(client);
        drop(_cr);
        assert!(matches!(server.recv(timeout).unwrap(), Received::Closed));
    }

    fn tempfile_with(bytes: &[u8]) -> std::fs::File {
        let path = std::env::temp_dir().join(format!("lsp-fd-{}", random_hex()));
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        f.write_all(bytes).unwrap();
        f
    }
}
