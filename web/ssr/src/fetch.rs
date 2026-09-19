//! What a render is allowed to ask the network, and how it knows when the
//! answers are in.
//!
//! A document that waits for nothing renders empty, because a page whose data
//! comes from an API has none of it on the tick the request goes out. So the
//! render counts what is on the wire: a dispatcher wrapped around whatever
//! transport the embedder installed, incrementing before it delegates and
//! decrementing once the reply is queued for the app to read. A render is
//! over when that count is zero and the app's state has stopped moving.
//!
//! The count belongs to one render, and the epoch is what says so. A reply
//! that arrives after its render gave up is dropped rather than handed to
//! whatever app is running now: without that, a slow upstream would put one
//! visitor's data in the next visitor's page.
//!
//! The same wrapper is where a render's network policy lives, because it is
//! the one place every request passes through. An app asking for an address
//! is an app asking a server to make a request on its behalf, so the hosts it
//! may reach are named up front and the number of requests one render may
//! make is capped.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use lumen_script::{HttpDispatch, HttpDone, HttpRequest};

/// Which addresses a render may ask for, and how many times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPolicy {
    /// The hosts a render may request from, lowercased and without a port.
    ///
    /// Empty allows nothing. A render answers a request for anything else
    /// with an error the app reads as a failed reply, so a page whose data
    /// did not arrive renders the way it does in a browser with no network.
    pub hosts: BTreeSet<String>,
    /// How many requests one render may make.
    pub max_requests: usize,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            hosts: BTreeSet::new(),
            max_requests: 8,
        }
    }
}

impl FetchPolicy {
    /// Allow requests to `host`.
    ///
    /// A host is a name, without a scheme, a port or a path:
    /// `api.example.com`. Subdomains are not implied; name each one.
    pub fn allow_host(mut self, host: &str) -> Self {
        self.hosts.insert(host.trim().to_ascii_lowercase());
        self
    }

    /// Whether `url` names a host this policy allows.
    fn allows(&self, url: &str) -> bool {
        host_of(url).is_some_and(|host| self.hosts.contains(&host))
    }
}

/// The host part of an absolute `http` or `https` URL, lowercased and without
/// userinfo or port.
///
/// Anything else is `None`: a relative address has no host to check, and a
/// scheme that is not HTTP is not a request a render makes.
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        // An IPv6 literal keeps its brackets, so the port split below cannot
        // mistake an address for one.
        Some(inside) => inside.split(']').next()?,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// What one renderer knows about the requests its renders have out.
///
/// Shared between the renderer and the dispatcher it installs in each app.
#[derive(Debug, Default)]
pub struct Flight {
    epoch: AtomicU64,
    outstanding: AtomicUsize,
    started: AtomicUsize,
}

impl Flight {
    /// Start a render, and return the epoch its requests belong to.
    pub fn open(&self) -> u64 {
        self.outstanding.store(0, Ordering::SeqCst);
        self.started.store(0, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// End a render. Every reply still out belongs to nobody from here on.
    pub fn close(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether something this render asked for is still on its way.
    pub fn outstanding(&self) -> bool {
        self.outstanding.load(Ordering::SeqCst) > 0
    }

    /// How many requests this render has asked for, counting the ones the cap
    /// refused.
    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

/// The dispatcher a render installs, wrapped around the embedder's transport.
///
/// It counts, it enforces [`FetchPolicy`], and it drops what arrives too
/// late. What performs the request is whatever was passed in, so an embedder
/// keeps their own client, their own proxy settings and their own test double.
pub struct CountingDispatch {
    inner: Arc<dyn HttpDispatch>,
    flight: Arc<Flight>,
    epoch: u64,
    policy: FetchPolicy,
}

impl CountingDispatch {
    /// Count and police the requests of the render holding `epoch`.
    pub fn new(
        inner: Arc<dyn HttpDispatch>,
        flight: Arc<Flight>,
        epoch: u64,
        policy: FetchPolicy,
    ) -> Self {
        Self {
            inner,
            flight,
            epoch,
            policy,
        }
    }
}

impl HttpDispatch for CountingDispatch {
    fn dispatch(&self, label: &str, request: HttpRequest, body_limit: u64, done: HttpDone) {
        if self.flight.epoch() != self.epoch {
            done(Err(format!(
                "cannot request {}: the render that asked for it is over",
                request.url
            )));
            return;
        }
        if !self.policy.allows(&request.url) {
            done(Err(format!(
                "cannot request {}: a render reaches only the hosts it was \
                 given, and this one is not among them",
                request.url
            )));
            return;
        }
        if self.flight.started.fetch_add(1, Ordering::SeqCst) >= self.policy.max_requests {
            done(Err(format!(
                "cannot request {}: one render makes at most {} requests",
                request.url, self.policy.max_requests
            )));
            return;
        }

        // Counted before the request leaves, so there is no moment where it is
        // on the wire and the render believes it has arrived.
        self.flight.outstanding.fetch_add(1, Ordering::SeqCst);
        let flight = Arc::clone(&self.flight);
        let epoch = self.epoch;
        self.inner.dispatch(
            label,
            request,
            body_limit,
            Box::new(move |outcome| {
                if flight.epoch() != epoch {
                    return;
                }
                // Queued for the app first, and only then counted as arrived:
                // a render that reads the count as zero has to find the reply
                // waiting for it on the next tick.
                done(outcome);
                flight.outstanding.fetch_sub(1, Ordering::SeqCst);
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc;

    use lumen_script::HttpResponse;

    use super::*;

    /// Holds every completion callback it is given, so a test decides when a
    /// request finishes.
    #[derive(Default)]
    struct Held(Mutex<Vec<HttpDone>>);

    impl HttpDispatch for Held {
        fn dispatch(&self, _label: &str, _request: HttpRequest, _body_limit: u64, done: HttpDone) {
            self.0.lock().expect("the test holds the lock").push(done);
        }
    }

    impl Held {
        fn finish(&self) {
            for done in self.0.lock().expect("the test holds the lock").drain(..) {
                done(Ok(HttpResponse {
                    status: 200,
                    ..HttpResponse::default()
                }));
            }
        }
    }

    fn ask(dispatch: &CountingDispatch, url: &str) -> mpsc::Receiver<Result<HttpResponse, String>> {
        let (tx, rx) = mpsc::channel();
        dispatch.dispatch(
            "tag",
            HttpRequest {
                url: url.to_string(),
                ..HttpRequest::default()
            },
            0,
            Box::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
        );
        rx
    }

    fn policy() -> FetchPolicy {
        FetchPolicy::default().allow_host("api.example.com")
    }

    #[test]
    fn a_host_is_read_out_of_an_address() {
        assert_eq!(
            host_of("https://API.example.com/items"),
            Some("api.example.com".into())
        );
        assert_eq!(
            host_of("http://user:pw@example.com:8080/x"),
            Some("example.com".into())
        );
        assert_eq!(host_of("https://[::1]:9000/x"), Some("::1".into()));
        assert_eq!(host_of("/items.json"), None);
        assert_eq!(host_of("file:///etc/passwd"), None);
    }

    #[test]
    fn a_request_in_flight_holds_the_render_open() {
        let held = Arc::new(Held::default());
        let flight = Arc::new(Flight::default());
        let epoch = flight.open();
        let dispatch = CountingDispatch::new(
            Arc::clone(&held) as Arc<dyn HttpDispatch>,
            Arc::clone(&flight),
            epoch,
            policy(),
        );

        let reply = ask(&dispatch, "https://api.example.com/items");
        assert!(flight.outstanding(), "the request is on the wire");
        held.finish();
        assert!(reply.try_recv().is_ok(), "the reply reached the app");
        assert!(!flight.outstanding(), "and only then was it counted in");
    }

    #[test]
    fn a_reply_that_arrives_after_its_render_is_dropped() {
        let held = Arc::new(Held::default());
        let flight = Arc::new(Flight::default());
        let epoch = flight.open();
        let dispatch = CountingDispatch::new(
            Arc::clone(&held) as Arc<dyn HttpDispatch>,
            Arc::clone(&flight),
            epoch,
            policy(),
        );

        let reply = ask(&dispatch, "https://api.example.com/items");
        flight.close();
        held.finish();
        assert!(
            reply.try_recv().is_err(),
            "the render it belonged to is over"
        );
    }

    #[test]
    fn a_host_that_was_not_named_is_refused_without_being_asked() {
        let held = Arc::new(Held::default());
        let flight = Arc::new(Flight::default());
        let epoch = flight.open();
        let dispatch = CountingDispatch::new(
            Arc::clone(&held) as Arc<dyn HttpDispatch>,
            Arc::clone(&flight),
            epoch,
            policy(),
        );

        let reply = ask(&dispatch, "https://elsewhere.example/items");
        assert!(reply.try_recv().expect("answered on the spot").is_err());
        assert!(!flight.outstanding());
        assert!(held.0.lock().expect("the test holds the lock").is_empty());
    }

    #[test]
    fn a_render_makes_only_so_many_requests() {
        let held = Arc::new(Held::default());
        let flight = Arc::new(Flight::default());
        let epoch = flight.open();
        let policy = FetchPolicy {
            max_requests: 2,
            ..policy()
        };
        let dispatch = CountingDispatch::new(
            Arc::clone(&held) as Arc<dyn HttpDispatch>,
            Arc::clone(&flight),
            epoch,
            policy,
        );

        for _ in 0..2 {
            let reply = ask(&dispatch, "https://api.example.com/items");
            assert!(reply.try_recv().is_err(), "still on the wire");
        }
        let refused = ask(&dispatch, "https://api.example.com/items");
        assert!(refused.try_recv().expect("answered on the spot").is_err());
        assert_eq!(flight.started(), 3);
    }
}
