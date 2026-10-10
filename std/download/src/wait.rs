//! Every wait for the server: an idle limit, and a way to stop.
//!
//! ureq bounds each phase of a request with a deadline counted from when that
//! phase began, and the body inherits the deadline of the response headers.
//! That is the wrong shape for a download: a large file arriving steadily
//! would fail once the headers' budget ran out. So the agent carries no
//! receive timeout of its own, and this transport caps every wait for input
//! at the idle limit instead. A server that keeps sending is waited on for as
//! long as it takes; one that goes quiet for longer than the limit fails.
//!
//! The same transport is what makes a transfer cancellable. A blocked socket
//! read cannot be interrupted from another thread, so a wait is taken in
//! slices of [`POLL`], and the transfer's [`Control`] is looked at between
//! them: a cancelled transfer stops within one slice, however quiet the
//! server is.
//!
//! The transport and connector traits live in ureq's `unversioned` module,
//! which does not follow semver; a ureq upgrade that moves them breaks the
//! build here rather than changing behavior silently.

use std::fmt;
use std::time::Instant;

use ureq::Timeout;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

use crate::transfer::Control;

/// The longest one wait for input runs before the transfer's [`Control`] is
/// looked at again: how long a cancel can take to reach a quiet connection.
pub(crate) const POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Wraps the transport the rest of the chain built in a [`WaitTransport`].
#[derive(Debug)]
pub(crate) struct WaitConnector {
    /// The idle limit, or `None` to wait on a quiet server indefinitely.
    pub(crate) limit: Option<std::time::Duration>,
    pub(crate) control: Control,
}

impl Connector<Box<dyn Transport>> for WaitConnector {
    type Out = WaitTransport;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| WaitTransport {
            inner,
            limit: self.limit.map_or(Duration::NotHappening, Duration::Exact),
            control: self.control.clone(),
            poll: POLL,
            heard_from: false,
        }))
    }
}

/// A transport whose every wait for input ends after `limit` with nothing
/// read, or as soon as its transfer is cancelled.
pub(crate) struct WaitTransport {
    inner: Box<dyn Transport>,
    limit: Duration,
    control: Control,
    /// The slice one wait is taken in; [`POLL`] outside the tests.
    poll: std::time::Duration,
    /// Whether the server has sent anything yet, so a timeout names the
    /// reply starting or the body as the thing that went quiet.
    heard_from: bool,
}

impl fmt::Debug for WaitTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaitTransport")
            .field("inner", &self.inner)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// The error a cancelled wait ends with. The transfer reports the cancel
/// itself; this only has to unwind ureq's read.
fn cancelled() -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "cancelled",
    ))
}

/// Whether an error is a wait running out rather than the connection failing.
fn is_timeout(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ),
        _ => false,
    }
}

impl Transport for WaitTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let wanted = if timeout.after <= self.limit {
            timeout
        } else {
            NextTimeout {
                after: self.limit,
                reason: if self.heard_from {
                    Timeout::RecvBody
                } else {
                    Timeout::RecvResponse
                },
            }
        };
        let started = Instant::now();
        loop {
            if self.control.is_cancelled() {
                return Err(cancelled());
            }
            let left = wanted.after.saturating_sub(started.elapsed());
            let last = left <= self.poll;
            let attempt = if !last {
                NextTimeout {
                    after: Duration::Exact(self.poll),
                    reason: wanted.reason,
                }
            } else if *wanted.after <= self.poll {
                // A wait that fits in one slice goes to the inner transport as
                // it was asked for.
                wanted
            } else {
                NextTimeout {
                    after: Duration::Exact(left),
                    reason: wanted.reason,
                }
            };
            match self.inner.await_input(attempt) {
                Ok(progressed) => {
                    self.heard_from |= progressed;
                    return Ok(progressed);
                }
                Err(e) if !last && is_timeout(&e) => {}
                Err(_) if self.control.is_cancelled() => return Err(cancelled()),
                Err(e) => return Err(e),
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ureq::unversioned::transport::LazyBuffers;

    use super::*;

    /// Records every timeout it is handed and reports the progress it is told
    /// to.
    #[derive(Debug)]
    struct Recording {
        buffers: LazyBuffers,
        seen: Arc<Mutex<Vec<NextTimeout>>>,
        progress: bool,
    }

    impl Transport for Recording {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }
        fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
            Ok(())
        }
        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
            self.seen.lock().unwrap().push(timeout);
            Ok(self.progress)
        }
        fn is_open(&mut self) -> bool {
            true
        }
    }

    /// A transport over [`Recording`] whose slice is longer than any wait the
    /// tests ask for, so each wait reaches the inner transport whole.
    fn idle(limit_ms: u64, progress: bool) -> (WaitTransport, Arc<Mutex<Vec<NextTimeout>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inner = Recording {
            buffers: LazyBuffers::new(64, 64),
            seen: seen.clone(),
            progress,
        };
        let transport = WaitTransport {
            inner: Box::new(inner),
            limit: Duration::from_millis(limit_ms),
            control: Control::default(),
            poll: std::time::Duration::from_secs(3600),
            heard_from: false,
        };
        (transport, seen)
    }

    fn wait(after: Duration) -> NextTimeout {
        NextTimeout {
            after,
            reason: Timeout::Global,
        }
    }

    #[test]
    fn every_wait_is_capped_at_the_limit_however_long_the_request_has_run() {
        let (mut transport, seen) = idle(500, true);
        for _ in 0..3 {
            transport.await_input(wait(Duration::NotHappening)).unwrap();
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|t| t.after == Duration::from_millis(500)));
    }

    #[test]
    fn a_timeout_names_the_reply_until_the_server_has_sent_something() {
        let (mut quiet, seen) = idle(500, false);
        quiet.await_input(wait(Duration::NotHappening)).unwrap();
        quiet.await_input(wait(Duration::NotHappening)).unwrap();
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|t| t.reason == Timeout::RecvResponse)
        );

        let (mut talking, seen) = idle(500, true);
        talking.await_input(wait(Duration::NotHappening)).unwrap();
        talking.await_input(wait(Duration::NotHappening)).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].reason, Timeout::RecvResponse);
        assert_eq!(seen[1].reason, Timeout::RecvBody);
    }

    #[test]
    fn a_shorter_deadline_of_ureq_s_own_is_kept() {
        let (mut transport, seen) = idle(500, true);
        transport
            .await_input(wait(Duration::from_millis(100)))
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].after, Duration::from_millis(100));
        assert_eq!(seen[0].reason, Timeout::Global);
    }

    /// A server that never sends: every wait runs its whole length and ends
    /// in a timeout, the way a socket read does.
    #[derive(Debug)]
    struct Silent {
        buffers: LazyBuffers,
        waits: Arc<Mutex<u32>>,
    }

    impl Transport for Silent {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }
        fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
            Ok(())
        }
        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
            *self.waits.lock().unwrap() += 1;
            std::thread::sleep(*timeout.after);
            Err(ureq::Error::Timeout(timeout.reason))
        }
        fn is_open(&mut self) -> bool {
            true
        }
    }

    fn silent(limit: Duration, control: Control) -> (WaitTransport, Arc<Mutex<u32>>) {
        let waits = Arc::new(Mutex::new(0));
        let transport = WaitTransport {
            inner: Box::new(Silent {
                buffers: LazyBuffers::new(64, 64),
                waits: waits.clone(),
            }),
            limit,
            control,
            poll: std::time::Duration::from_millis(10),
            heard_from: false,
        };
        (transport, waits)
    }

    #[test]
    fn a_cancel_ends_a_wait_on_a_server_that_never_answers() {
        let control = Control::default();
        let (mut transport, waits) = silent(Duration::NotHappening, control.clone());
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            control.cancel()
        });
        let started = std::time::Instant::now();
        let err = transport
            .await_input(wait(Duration::NotHappening))
            .expect_err("a cancelled wait fails");
        assert!(
            canceller.join().unwrap(),
            "the cancel found the transfer running"
        );
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(*waits.lock().unwrap() > 1, "the wait was taken in slices");
    }

    #[test]
    fn the_slices_of_a_wait_still_add_up_to_the_idle_limit() {
        let (mut transport, waits) = silent(Duration::from_millis(50), Control::default());
        let started = std::time::Instant::now();
        let err = transport
            .await_input(wait(Duration::NotHappening))
            .expect_err("a quiet server times out");
        assert!(
            matches!(err, ureq::Error::Timeout(Timeout::RecvResponse)),
            "{err}"
        );
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));
        assert!(*waits.lock().unwrap() >= 5, "a 50 ms limit in 10 ms slices");
    }
}
