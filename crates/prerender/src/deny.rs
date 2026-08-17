//! Answering an app's HTTP requests without leaving the machine.

use std::sync::{Arc, Mutex};

use lumen_script::{HttpDispatch, HttpDone, HttpRequest};

/// Refuses every request, and remembers what was asked for.
///
/// A page written at build time has to come out the same on every machine, and
/// an answer fetched from a server is the one thing that cannot be promised
/// to. So a build answers the request itself, immediately and with a refusal:
/// the app carries on with the state it has, nothing is ever in flight, and
/// the addresses it wanted are reported to whoever ran the build. In a browser
/// the same call reaches the network, which is where the part of a page that
/// nobody can know at build time comes from.
#[derive(Debug, Default, Clone)]
pub struct DenyDispatch {
    asked: Arc<Mutex<Vec<String>>>,
}

impl DenyDispatch {
    /// Every address that was asked for, once each, in the order it was first
    /// asked.
    pub fn take(&self) -> Vec<String> {
        let Ok(mut asked) = self.asked.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut asked)
    }
}

impl HttpDispatch for DenyDispatch {
    fn dispatch(&self, _label: &str, request: HttpRequest, _body_limit: u64, done: HttpDone) {
        if let Ok(mut asked) = self.asked.lock()
            && !asked.contains(&request.url)
        {
            asked.push(request.url.clone());
        }
        done(Err(format!(
            "the network is answered by the build, so `{}` was not requested",
            request.url
        )));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn a_request_is_refused_where_it_was_made() {
        let deny = DenyDispatch::default();
        let (tx, rx) = mpsc::channel();
        deny.dispatch(
            "items",
            HttpRequest {
                url: "https://example.invalid/items.json".to_string(),
                ..HttpRequest::default()
            },
            0,
            Box::new(move |outcome| tx.send(outcome).expect("the test holds the receiver")),
        );
        // Answered before `dispatch` returned, so nothing is ever in flight
        // and a run has nothing to wait for.
        let outcome = rx.try_recv().expect("the refusal is already queued");
        assert!(outcome.is_err());
        assert_eq!(deny.take(), vec!["https://example.invalid/items.json"]);
    }

    #[test]
    fn one_address_is_reported_once() {
        let deny = DenyDispatch::default();
        for _ in 0..3 {
            deny.dispatch(
                "poll",
                HttpRequest {
                    url: "https://example.invalid/poll".to_string(),
                    ..HttpRequest::default()
                },
                0,
                Box::new(|_| {}),
            );
        }
        assert_eq!(deny.take().len(), 1);
    }
}
