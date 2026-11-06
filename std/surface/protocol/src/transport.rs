//! The transport a session runs over, whichever the platform has.
//!
//! - Linux: the [`unix`](crate::unix) stream socket, with handles passed as
//!   file descriptors alongside the frames that name them.
//! - macOS: the same socket for the messages; the IOSurface ports a ring
//!   names travel in a Mach message to a bootstrap service the consumer
//!   checked in ([`mach`](crate::mach)).
//! - Windows: a [`pipe`](crate::pipe), a named pipe only the current user can
//!   open; the consumer copies each handle a ring names out of the
//!   producer's process.
//!
//! A platform with none of these builds the same names, so a consumer
//! compiles everywhere: a listener there does not bind, failing with
//! [`io::ErrorKind::Unsupported`], so no connection is made.
//!
//! Every platform's transport has the same shape: a [`Listener`] the
//! consumer binds and names in its endpoint string, a [`Connection`] a
//! producer opens to it, split into a [`Sender`] (cloneable, each frame
//! written whole) and a [`Receiver`] that returns each message with the OS
//! handles it carries, as [`Handle`]s this process owns.

use crate::wire::{Message, WireError};
use std::io;

#[cfg(windows)]
pub use crate::pipe::{
    Connection, Handle, Listener, RawHandle, Receiver, Sender, random_bytes, raw_handle,
};
#[cfg(unix)]
pub use crate::unix::{
    Connection, Handle, Listener, RawHandle, Receiver, Sender, random_bytes, raw_handle,
};

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

#[cfg(not(any(unix, windows)))]
pub use none::*;

/// The names of a transport, on a platform that has none. Nothing here can
/// connect: a listener does not bind, so no connection is ever made.
#[cfg(not(any(unix, windows)))]
mod none {
    use super::{Received, TransportError};
    use crate::endpoint::Transport;
    use crate::wire::Message;
    use std::io;
    use std::time::Duration;

    /// No handle can arrive.
    #[derive(Debug)]
    pub enum Handle {}
    /// No handle can be attached.
    #[derive(Clone, Copy, Debug)]
    pub enum RawHandle {}

    /// No handle can be attached.
    pub fn raw_handle(_value: u64) -> Option<RawHandle> {
        None
    }

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the surface protocol has no transport on this platform",
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

    /// No value: nothing here is ever made.
    enum Never {}

    /// A listener. None binds.
    pub struct Listener(Never);

    impl Listener {
        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn bind_fresh() -> io::Result<Self> {
            Err(unsupported())
        }

        /// Where the listener is; never called, as none binds.
        pub fn transport(&self) -> Transport {
            match self.0 {}
        }

        /// No connection ever arrives.
        ///
        /// # Errors
        ///
        /// None.
        pub fn accept(&self, _timeout: Option<Duration>) -> io::Result<Option<Connection>> {
            match self.0 {}
        }
    }

    /// A connection. None is made.
    pub struct Connection(Never);

    impl Connection {
        /// # Errors
        ///
        /// Always [`io::ErrorKind::Unsupported`].
        pub fn connect(_transport: &Transport) -> io::Result<Self> {
            Err(unsupported())
        }

        /// The two halves.
        pub fn split(self) -> (Sender, Receiver) {
            match self.0 {}
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
