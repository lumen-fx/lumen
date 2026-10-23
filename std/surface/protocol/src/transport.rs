//! The transport a session runs over, whichever the platform has.
//!
//! On Unix it is the [`unix`](crate::unix) stream socket, with handles passed
//! as file descriptors. A platform with no transport yet builds the same
//! names, so a consumer compiles everywhere: a listener there does not bind,
//! failing with [`io::ErrorKind::Unsupported`], so no connection is made.

use crate::wire::{Message, WireError};
use std::io;

/// An OS handle to shared memory, as it arrives with a frame: a file
/// descriptor on Unix.
#[cfg(unix)]
pub type Handle = std::os::fd::OwnedFd;
/// An OS handle as a sender attaches it, borrowed for the call.
#[cfg(unix)]
pub type RawHandle = std::os::fd::RawFd;

#[cfg(unix)]
pub use crate::unix::{Connection, Listener, Receiver, Sender, random_bytes};

/// What went wrong on a connection.
#[derive(Debug)]
pub enum TransportError {
    /// The socket failed.
    Io(io::Error),
    /// A frame did not read.
    Wire(WireError),
    /// A frame said it carries handles that did not arrive.
    MissingHandles,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "{e}"),
            TransportError::Wire(e) => write!(f, "{e}"),
            TransportError::MissingHandles => {
                f.write_str("a frame named handles that did not arrive")
            }
        }
    }
}

impl std::error::Error for TransportError {}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<WireError> for TransportError {
    fn from(e: WireError) -> Self {
        TransportError::Wire(e)
    }
}

/// What a read returned.
#[derive(Debug)]
pub enum Received {
    /// A message, with the handles that came with it.
    Message(Message, Vec<Handle>),
    /// Nothing arrived before the timeout.
    Timeout,
    /// The other side closed the connection.
    Closed,
}

#[cfg(not(unix))]
pub use none::*;

/// The names of a transport, on a platform that has none yet. Nothing here
/// can connect: a listener does not bind, so no connection is ever made.
#[cfg(not(unix))]
mod none {
    use super::{Received, TransportError};
    use crate::wire::Message;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    /// No handle can arrive.
    #[derive(Debug)]
    pub enum Handle {}
    /// No handle can be attached.
    #[derive(Clone, Copy, Debug)]
    pub enum RawHandle {}

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the surface protocol has no transport on this platform yet",
        )
    }

    /// 16 random bytes.
    ///
    /// # Errors
    ///
    /// Always: there is no transport whose token they would be.
    pub fn random_bytes() -> io::Result<[u8; 16]> {
        Err(unsupported())
    }

    /// A listener. None binds.
    pub struct Listener(());

    impl Listener {
        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn bind_fresh() -> io::Result<Self> {
            Err(unsupported())
        }

        /// Where the listener is: nowhere.
        pub fn path(&self) -> &Path {
            Path::new("")
        }

        /// No connection ever arrives.
        ///
        /// # Errors
        ///
        /// None.
        pub fn accept(&self, _timeout: Option<Duration>) -> io::Result<Option<Connection>> {
            Ok(None)
        }
    }

    /// A connection. None is made.
    pub struct Connection(());

    impl Connection {
        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn connect(_path: &Path) -> io::Result<Self> {
            Err(unsupported())
        }

        /// The two halves.
        pub fn split(self) -> (Sender, Receiver) {
            (Sender(()), Receiver(()))
        }
    }

    /// The writing half of a connection.
    #[derive(Clone)]
    pub struct Sender(());

    impl Sender {
        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn send(&self, _message: &Message, _fds: &[RawHandle]) -> Result<(), TransportError> {
            Err(unsupported().into())
        }

        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn try_send(
            &self,
            _message: &Message,
            _fds: &[RawHandle],
        ) -> Result<(), TransportError> {
            Err(unsupported().into())
        }
    }

    /// The reading half of a connection.
    pub struct Receiver(());

    impl Receiver {
        /// The connection is closed.
        ///
        /// # Errors
        ///
        /// None.
        pub fn recv(&mut self, _timeout: Option<Duration>) -> Result<Received, TransportError> {
            Ok(Received::Closed)
        }

        /// Close the connection.
        pub fn shutdown(&self) {}
    }
}
