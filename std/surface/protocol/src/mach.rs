//! The macOS half of the Unix transport: IOSurface ports in Mach messages.
//!
//! An IOSurface crosses processes as a Mach send right
//! (`IOSurfaceCreateMachPort`), and a Mach right cannot travel over a Unix
//! socket. So the consumer's listener also checks in a Mach bootstrap
//! service, named in the endpoint string (`mach=<service>`), and a producer
//! that offers a ring first sends one Mach message to that service carrying
//! a send right per buffer, then the `Buffers` frame on the socket. The
//! consumer matches the two by the producer's process id, which the kernel
//! stamps on both (the socket's `LOCAL_PEERPID`, the message's audit
//! trailer), and by the ring's generation, which the message carries. A
//! message from any other process never matches a connection's ring.
//!
//! The message: a complex Mach message whose descriptors are the ring's
//! ports in buffer order (`MACH_MSG_TYPE_COPY_SEND`, so the sender keeps its
//! own rights), followed by three `u32`s: [`MAGIC`], the generation and the
//! port count.

use crate::transport::TransportError;
use mach2::bootstrap::{bootstrap_check_in, bootstrap_look_up, bootstrap_port};
use mach2::kern_return::KERN_SUCCESS;
use mach2::mach_port::{mach_port_deallocate, mach_port_mod_refs};
use mach2::message::{
    MACH_MSG_PORT_DESCRIPTOR, MACH_MSG_SUCCESS, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MOVE_SEND,
    MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX, MACH_RCV_MSG, MACH_RCV_TIMED_OUT, MACH_RCV_TIMEOUT,
    MACH_RCV_TOO_LARGE, MACH_RCV_TRAILER_AUDIT, MACH_SEND_MSG, MACH_SEND_TIMEOUT, mach_msg,
    mach_msg_audit_trailer_t, mach_msg_body_t, mach_msg_destroy, mach_msg_header_t,
    mach_msg_port_descriptor_t,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
use mach2::traps::mach_task_self;
use std::ffi::CString;
use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The first inline word of a ring's message: `'LSPR'`.
pub const MAGIC: u32 = 0x4c53_5052;

/// The most ports one message carries: a ring's most buffers.
const MAX_PORTS: usize = crate::wire::MAX_RING;

/// How long a ring's ports wait to be claimed before they are dropped.
const KEEP: Duration = Duration::from_secs(10);

/// A Mach port name as a sender attaches it: a send right the caller keeps.
pub type RawPort = mach_port_t;

/// A send right this process owns, released when dropped.
#[derive(Debug)]
pub struct Port(mach_port_t);

impl Port {
    /// Own `name`, a send right of this task.
    ///
    /// # Safety
    ///
    /// `name` names a send right this task holds and nothing else releases.
    pub unsafe fn from_raw(name: mach_port_t) -> Self {
        Self(name)
    }

    /// The port's name in this task.
    pub fn as_raw(&self) -> mach_port_t {
        self.0
    }

    /// Give up ownership: the caller releases the right.
    pub fn into_raw(self) -> mach_port_t {
        let name = self.0;
        std::mem::forget(self);
        name
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        if self.0 != MACH_PORT_NULL {
            // SAFETY: a send right this task owns, released once.
            unsafe {
                mach_port_deallocate(mach_task_self(), self.0);
            }
        }
    }
}

// SAFETY: a port name is a plain integer naming a right of the task, usable
// from any of its threads.
unsafe impl Send for Port {}
// SAFETY: as above; the type has no interior mutability.
unsafe impl Sync for Port {}

fn kern_error(call: &str, kr: i32) -> io::Error {
    io::Error::other(format!("{call} failed: {kr:#x}"))
}

/// A ring's ports that arrived and wait for their `Buffers` frame.
struct Arrived {
    pid: i32,
    generation: u32,
    ports: Vec<Port>,
    at: Instant,
}

/// The bootstrap service a consumer's listener receives ring ports on.
pub struct Service {
    name: String,
    port: mach_port_t,
    /// Messages received and not claimed yet. Held while receiving, so one
    /// thread reads the port at a time.
    inbox: Mutex<Vec<Arrived>>,
}

impl Service {
    /// Check in the service `name` in this process's bootstrap namespace,
    /// creating its receive right.
    pub fn check_in(name: &str) -> io::Result<Self> {
        let c = CString::new(name).map_err(|_| io::Error::other("a NUL in the service name"))?;
        let mut port: mach_port_t = MACH_PORT_NULL;
        // SAFETY: the bootstrap port is the task's; `c` is NUL-terminated.
        let kr = unsafe { bootstrap_check_in(bootstrap_port, c.as_ptr(), &mut port) };
        if kr != KERN_SUCCESS {
            return Err(kern_error("bootstrap_check_in", kr));
        }
        Ok(Self {
            name: name.to_string(),
            port,
            inbox: Mutex::new(Vec::new()),
        })
    }

    /// The service's name, for the endpoint string.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `count` ports process `pid` sent for ring `generation`, waiting
    /// at most `timeout` for them to arrive. Messages from other processes,
    /// or for other rings, stay for the connection they belong to.
    pub fn take(
        &self,
        pid: i32,
        generation: u32,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<Port>, TransportError> {
        let deadline = Instant::now() + timeout;
        let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            inbox.retain(|a| a.at.elapsed() < KEEP);
            if let Some(i) = inbox
                .iter()
                .position(|a| a.pid == pid && a.generation == generation)
            {
                let arrived = inbox.swap_remove(i);
                if arrived.ports.len() != count {
                    return Err(TransportError::MissingHandles);
                }
                return Ok(arrived.ports);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.receive(left)? {
                Some(arrived) => inbox.push(arrived),
                None => return Err(TransportError::MissingHandles),
            }
        }
    }

    /// One message off the port, waiting at most `timeout`; `None` when none
    /// came. A message that is not a ring's is destroyed and skipped.
    fn receive(&self, timeout: Duration) -> io::Result<Option<Arrived>> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut buffer = Received::zeroed();
            let ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u128::from(u32::MAX)) as u32;
            let options = MACH_RCV_MSG
                | MACH_RCV_TIMEOUT
                | rcv_trailer_type(0)
                | rcv_trailer_elements(MACH_RCV_TRAILER_AUDIT as i32);
            // SAFETY: a receive into a buffer of the stated size.
            let kr = unsafe {
                mach_msg(
                    &mut buffer.header,
                    options,
                    0,
                    std::mem::size_of::<Received>() as u32,
                    self.port,
                    ms,
                    MACH_PORT_NULL,
                )
            };
            if kr == MACH_RCV_TIMED_OUT {
                return Ok(None);
            }
            if kr == MACH_RCV_TOO_LARGE {
                // The kernel destroyed it: nothing a producer sends is that
                // large, so it was not a ring's.
                continue;
            }
            if kr != MACH_MSG_SUCCESS {
                return Err(kern_error("mach_msg (receive)", kr));
            }
            if let Some(arrived) = buffer.parse() {
                return Ok(Some(arrived));
            }
            // SAFETY: a message received into `buffer`, destroyed once; its
            // rights go with it.
            unsafe { mach_msg_destroy(&mut buffer.header) };
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: the receive right this service checked in, dropped once;
        // the bootstrap entry goes with it.
        unsafe {
            mach_port_mod_refs(mach_task_self(), self.port, MACH_PORT_RIGHT_RECEIVE, -1);
        }
    }
}

fn rcv_trailer_type(kind: i32) -> i32 {
    (kind & 0xf) << 28
}

fn rcv_trailer_elements(elements: i32) -> i32 {
    (elements & 0xf) << 24
}

/// The message as sent: header, body, the descriptors in use, then the
/// inline words. Built in a byte buffer, since its size depends on the
/// port count.
#[repr(C)]
struct Sent {
    header: mach_msg_header_t,
    body: mach_msg_body_t,
    ports: [mach_msg_port_descriptor_t; MAX_PORTS],
    words: [u32; 3],
}

/// A buffer a ring's message is received into, with room for the trailer.
#[repr(C)]
struct Received {
    header: mach_msg_header_t,
    rest: [u8; 512],
}

impl Received {
    fn zeroed() -> Self {
        // SAFETY: plain data; all-zero is a valid value.
        unsafe { std::mem::zeroed() }
    }

    /// The ring this message carries, or `None` when it is not a ring's
    /// message (its rights are then still in it, for the caller to destroy).
    fn parse(&mut self) -> Option<Arrived> {
        let size = self.header.msgh_size as usize;
        let total = std::mem::size_of::<mach_msg_header_t>() + self.rest.len();
        if self.header.msgh_bits & MACH_MSGH_BITS_COMPLEX == 0 || size > total {
            return None;
        }
        let base = (&self.header as *const mach_msg_header_t).cast::<u8>();
        let read_u32 = |at: usize| -> Option<u32> {
            if at + 4 > size {
                return None;
            }
            // SAFETY: inside the received message, which `size` bounds.
            Some(unsafe { base.add(at).cast::<u32>().read_unaligned() })
        };
        let header = std::mem::size_of::<mach_msg_header_t>();
        let count = read_u32(header)? as usize;
        if count == 0 || count > MAX_PORTS {
            return None;
        }
        let descriptor = std::mem::size_of::<mach_msg_port_descriptor_t>();
        let first = header + std::mem::size_of::<mach_msg_body_t>();
        let words = first + count * descriptor;
        if read_u32(words)? != MAGIC || read_u32(words + 8)? as usize != count {
            return None;
        }
        let generation = read_u32(words + 4)?;
        let mut names = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: inside the message: `words` lies past every descriptor.
            let d = unsafe {
                base.add(first + i * descriptor)
                    .cast::<mach_msg_port_descriptor_t>()
                    .read_unaligned()
            };
            if u32::from(d.type_) != MACH_MSG_PORT_DESCRIPTOR
                // A send right arrives as MACH_MSG_TYPE_PORT_SEND, which
                // has the value of MOVE_SEND.
                || u32::from(d.disposition) != MACH_MSG_TYPE_MOVE_SEND
            {
                return None;
            }
            names.push(d.name);
        }
        // The trailer follows the message, rounded to four bytes.
        let trailer_at = size.div_ceil(4) * 4;
        let trailer = std::mem::size_of::<mach_msg_audit_trailer_t>();
        if trailer_at + trailer > total {
            return None;
        }
        // SAFETY: inside the buffer, checked just above.
        let trailer = unsafe {
            base.add(trailer_at)
                .cast::<mach_msg_audit_trailer_t>()
                .read_unaligned()
        };
        if (trailer.msgh_trailer_size as usize) < std::mem::size_of::<mach_msg_audit_trailer_t>() {
            return None;
        }
        // The audit token's sixth word is the sender's process id.
        let pid = trailer.msgh_audit.val[5] as i32;
        Some(Arrived {
            pid,
            generation,
            // SAFETY: each name is a send right the message moved into this
            // task, owned from here on.
            ports: names
                .into_iter()
                .map(|n| unsafe { Port::from_raw(n) })
                .collect(),
            at: Instant::now(),
        })
    }
}

/// A send right to the consumer's service named `name`.
pub fn look_up(name: &str) -> io::Result<Port> {
    let c = CString::new(name).map_err(|_| io::Error::other("a NUL in the service name"))?;
    let mut port: mach_port_t = MACH_PORT_NULL;
    // SAFETY: the bootstrap port is the task's; `c` is NUL-terminated.
    let kr = unsafe { bootstrap_look_up(bootstrap_port, c.as_ptr(), &mut port) };
    if kr != KERN_SUCCESS {
        return Err(kern_error("bootstrap_look_up", kr));
    }
    // SAFETY: bootstrap_look_up hands this task a send right.
    Ok(unsafe { Port::from_raw(port) })
}

/// Send ring `generation`'s `ports` to the service `to`, copying the
/// rights so the caller keeps its own.
pub fn send_ports(to: &Port, generation: u32, ports: &[RawPort]) -> io::Result<()> {
    if ports.is_empty() || ports.len() > MAX_PORTS {
        return Err(io::Error::other("a ring carries one to four ports"));
    }
    // SAFETY: plain data; all-zero is a valid value.
    let mut message: Sent = unsafe { std::mem::zeroed() };
    for (slot, &port) in message.ports.iter_mut().zip(ports) {
        *slot = mach_msg_port_descriptor_t::new(port, MACH_MSG_TYPE_COPY_SEND);
    }
    // The inline words follow the descriptors in use, so they move up when
    // fewer than four ports are sent.
    let descriptor = std::mem::size_of::<mach_msg_port_descriptor_t>();
    let header = std::mem::size_of::<mach_msg_header_t>();
    let words_at = header + std::mem::size_of::<mach_msg_body_t>() + ports.len() * descriptor;
    let size = words_at + 12;
    message.body.msgh_descriptor_count = ports.len() as u32;
    message.header = mach_msg_header_t {
        msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0) | MACH_MSGH_BITS_COMPLEX,
        msgh_size: size as u32,
        msgh_remote_port: to.as_raw(),
        msgh_local_port: MACH_PORT_NULL,
        msgh_voucher_port: MACH_PORT_NULL,
        msgh_id: MAGIC as i32,
    };
    let base = (&mut message.header as *mut mach_msg_header_t).cast::<u8>();
    for (i, word) in [MAGIC, generation, ports.len() as u32]
        .into_iter()
        .enumerate()
    {
        // SAFETY: inside `message`: at most four descriptors plus three
        // words fit `Sent`.
        unsafe {
            base.add(words_at + i * 4)
                .cast::<u32>()
                .write_unaligned(word)
        };
    }
    // SAFETY: a well-formed complex message of `size` bytes.
    let kr = unsafe {
        mach_msg(
            &mut message.header,
            MACH_SEND_MSG | MACH_SEND_TIMEOUT,
            size as u32,
            0,
            MACH_PORT_NULL,
            5_000,
            MACH_PORT_NULL,
        )
    };
    if kr != MACH_MSG_SUCCESS {
        return Err(kern_error("mach_msg (send)", kr));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
    use mach2::message::MACH_MSG_TYPE_MAKE_SEND;

    /// A fresh port this task holds a send right to (and the receive right,
    /// which the test leaks).
    pub(crate) fn port() -> mach_port_t {
        let mut name = MACH_PORT_NULL;
        // SAFETY: plain port allocation in this task.
        unsafe {
            assert_eq!(
                mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut name),
                KERN_SUCCESS
            );
            assert_eq!(
                mach_port_insert_right(mach_task_self(), name, name, MACH_MSG_TYPE_MAKE_SEND),
                KERN_SUCCESS
            );
        }
        name
    }

    /// Ports sent to a service arrive with the sender's process id and the
    /// ring's generation; a ring nobody sent is missing.
    #[test]
    fn ports_reach_the_service() {
        let name = format!(
            "dev.lumenfx.surface.test.{}.{}",
            std::process::id(),
            crate::endpoint::hex(&crate::unix::random_bytes().unwrap()[..6])
        );
        let service = Service::check_in(&name).expect("checked in");
        let to = look_up(&name).expect("found");
        let ports = [port(), port(), port()];
        send_ports(&to, 7, &ports).expect("sent");
        let pid = std::process::id() as i32;
        let got = service
            .take(pid, 7, 3, Duration::from_secs(5))
            .expect("the ring's ports");
        assert_eq!(got.len(), 3);
        // Copied rights to ports this task already names arrive under the
        // same names.
        assert_eq!(
            got.iter().map(Port::as_raw).collect::<Vec<_>>(),
            ports.to_vec()
        );
        assert!(matches!(
            service.take(pid, 8, 3, Duration::from_millis(50)),
            Err(TransportError::MissingHandles)
        ));
    }
}
