//! An idle limit on every wait for the server.
//!
//! ureq bounds each phase of a request with a deadline counted from when that
//! phase began, and the body inherits the deadline of the response headers.
//! That is the wrong shape for a download: a large file arriving steadily
//! would fail once the headers' budget ran out. So the agent carries no
//! receive timeout of its own, and this transport caps every wait for input
//! at the idle limit instead. A server that keeps sending is waited on for as
//! long as it takes; one that goes quiet for longer than the limit fails.
//!
//! The transport and connector traits live in ureq's `unversioned` module,
//! which does not follow semver; a ureq upgrade that moves them breaks the
//! build here rather than changing behavior silently.

use std::fmt;

use ureq::Timeout;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

/// Wraps the transport the rest of the chain built in an [`IdleTransport`].
#[derive(Debug)]
pub(crate) struct IdleConnector {
    pub(crate) limit: std::time::Duration,
}

impl Connector<Box<dyn Transport>> for IdleConnector {
    type Out = IdleTransport;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| IdleTransport {
            inner,
            limit: Duration::Exact(self.limit),
            heard_from: false,
        }))
    }
}

/// A transport whose every wait for input ends after `limit` with nothing
/// read.
pub(crate) struct IdleTransport {
    inner: Box<dyn Transport>,
    limit: Duration,
    /// Whether the server has sent anything yet, so a timeout names the
    /// reply starting or the body as the thing that went quiet.
    heard_from: bool,
}

impl fmt::Debug for IdleTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleTransport")
            .field("inner", &self.inner)
            .field("limit", &self.limit)
            .finish()
    }
}

impl Transport for IdleTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let capped = if timeout.after <= self.limit {
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
        let progressed = self.inner.await_input(capped)?;
        self.heard_from |= progressed;
        Ok(progressed)
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

    fn idle(limit_ms: u64, progress: bool) -> (IdleTransport, Arc<Mutex<Vec<NextTimeout>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inner = Recording {
            buffers: LazyBuffers::new(64, 64),
            seen: seen.clone(),
            progress,
        };
        let transport = IdleTransport {
            inner: Box::new(inner),
            limit: Duration::from_millis(limit_ms),
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
}
