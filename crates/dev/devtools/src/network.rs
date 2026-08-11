//! Dev-only bounded capture of the scripting HTTP layer, feeding the
//! Network tab. The actual events come from [`lumen_core::net_capture`],
//! which `lumen-script` reports to unconditionally (a no-op until
//! the sink is installed by [`crate::DevtoolsPlugin`]).

use std::collections::VecDeque;

use bevy_ecs::prelude::Resource;
use lumen_core::net_capture::{self, NetEvent};

/// Cap on the request ring. Oldest entries drop first on overflow.
pub const NETWORK_RING_CAP: usize = 128;

/// One captured HTTP exchange. Created on the `Started` event and updated
/// in place when the matching `Completed` (same `tag`) lands.
#[derive(Clone, Debug)]
pub struct NetEntry {
    /// Correlation tag (`fetch(url, tag)`).
    pub tag: String,
    /// HTTP method.
    pub method: String,
    /// Target URL.
    pub url: String,
    /// `None` while in-flight; `Some(status)` (or `Some(0)` on transport
    /// error) once the reply lands.
    pub status: Option<u16>,
    /// Transport-level failure message, if any.
    pub error: Option<String>,
}

impl NetEntry {
    /// A one-line human rendering for the Network tab.
    pub fn render(&self) -> String {
        let state = match (&self.error, self.status) {
            (Some(e), _) => format!("ERR {e}"),
            (None, Some(s)) => format!("{s}"),
            (None, None) => "...".to_string(),
        };
        format!("[{state}] {} {}  ({})", self.method, self.url, self.tag)
    }
}

/// Bounded ring of captured HTTP exchanges. Inserted as a main-world
/// resource by [`crate::DevtoolsPlugin`].
#[derive(Resource, Debug)]
pub struct NetworkCapture {
    entries: VecDeque<NetEntry>,
    cap: usize,
}

impl Default for NetworkCapture {
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(NETWORK_RING_CAP),
            cap: NETWORK_RING_CAP,
        }
    }
}

impl NetworkCapture {
    /// True when nothing has been captured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of captured exchanges.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterate entries oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &NetEntry> {
        self.entries.iter()
    }

    /// Apply one capture event: push a new in-flight entry on `Started`,
    /// or fill in the status of the matching (most-recent same-tag,
    /// still-open) entry on `Completed`.
    pub fn apply(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Started { tag, method, url } => {
                if self.entries.len() == self.cap {
                    self.entries.pop_front();
                }
                self.entries.push_back(NetEntry {
                    tag,
                    method,
                    url,
                    status: None,
                    error: None,
                });
            }
            NetEvent::Completed {
                tag,
                ok,
                status,
                error,
            } => {
                if let Some(e) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| e.tag == tag && e.status.is_none())
                {
                    e.status = Some(status);
                    e.error = if ok || error.is_empty() {
                        None
                    } else {
                        Some(error)
                    };
                } else {
                    // Completion with no in-flight start (e.g. capture
                    // installed mid-flight): record a terminal-only row.
                    if self.entries.len() == self.cap {
                        self.entries.pop_front();
                    }
                    self.entries.push_back(NetEntry {
                        tag,
                        method: "?".to_string(),
                        url: String::new(),
                        status: Some(status),
                        error: if ok { None } else { Some(error) },
                    });
                }
            }
        }
    }
}

/// System: drain the process-wide capture sink into the ring each tick.
pub fn drain_network_capture(mut cap: bevy_ecs::system::ResMut<NetworkCapture>) {
    for ev in net_capture::drain(256) {
        cap.apply(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_then_completed_correlates_by_tag() {
        let mut cap = NetworkCapture::default();
        cap.apply(NetEvent::Started {
            tag: "t1".into(),
            method: "GET".into(),
            url: "https://x/a".into(),
        });
        assert_eq!(cap.len(), 1);
        assert!(cap.iter().next().unwrap().status.is_none());

        cap.apply(NetEvent::Completed {
            tag: "t1".into(),
            ok: true,
            status: 200,
            error: String::new(),
        });
        assert_eq!(cap.len(), 1, "completion updates in place, not appends");
        let e = cap.iter().next().unwrap();
        assert_eq!(e.status, Some(200));
        assert!(e.error.is_none());
        assert!(e.render().contains("200"));
    }

    #[test]
    fn transport_error_is_recorded() {
        let mut cap = NetworkCapture::default();
        cap.apply(NetEvent::Started {
            tag: "t".into(),
            method: "GET".into(),
            url: "https://bad".into(),
        });
        cap.apply(NetEvent::Completed {
            tag: "t".into(),
            ok: false,
            status: 0,
            error: "dns failure".into(),
        });
        let e = cap.iter().next().unwrap();
        assert_eq!(e.error.as_deref(), Some("dns failure"));
        assert!(e.render().contains("ERR"));
    }

    #[test]
    fn ring_evicts_oldest_over_cap() {
        let mut cap = NetworkCapture::default();
        for i in 0..(NETWORK_RING_CAP + 10) {
            cap.apply(NetEvent::Started {
                tag: format!("t{i}"),
                method: "GET".into(),
                url: "u".into(),
            });
        }
        assert_eq!(cap.len(), NETWORK_RING_CAP);
    }
}
