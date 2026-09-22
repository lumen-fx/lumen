//! Answering requests by rendering the app for each one.
//!
//! This is the seam between the server, which reads HTTP and knows nothing
//! about apps, and [`lumen_ssr`], which renders apps and knows nothing about
//! HTTP. A request the server has read becomes a render, and the document
//! comes back as the response.
//!
//! Renders happen one at a time: the renderer owns a thread and every request
//! queues for it, because the buses an app reads its state through belong to
//! the process. Serving more requests at once means more processes.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lumen_ssr::{RenderOptions, Renderer, SsrError, SsrRequest, SsrSite};

use crate::http::{Request, Response};
use crate::log::Log;
use crate::server::{RequestHandler, Status};

/// How much a failed render says about why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorPages {
    /// The page says what went wrong, for the developer looking at it.
    Detailed,
    /// The page says only the status; the log says the rest.
    Plain,
}

/// How a [`RenderHandler`] runs.
#[derive(Debug, Clone)]
pub struct RenderSettings {
    /// How long a render gets once it starts. A render past it is answered
    /// with a 504 and the process retires, since a tick that never returns
    /// cannot be stopped from inside it. `None` waits as long as it takes.
    pub limit: Option<Duration>,
    /// Retire after about this many renders, so whatever supervises the
    /// process starts a fresh one. `None` never does.
    pub max_renders: Option<u64>,
    /// How much a failed render says.
    pub errors: ErrorPages,
    /// Where warnings and failures are written.
    pub log: Arc<Log>,
}

impl RenderSettings {
    /// What a developer's own server runs with: no limits, and every failure
    /// explained in the page.
    pub fn development(log: Arc<Log>) -> Self {
        Self {
            limit: None,
            max_renders: None,
            errors: ErrorPages::Detailed,
            log,
        }
    }
}

/// Answers every page by rendering the app for the request that asked for it.
pub struct RenderHandler {
    renderer: Renderer,
    settings: RenderSettings,
    /// How many renders to retire after, with its spread already added.
    retire_after: Option<u64>,
    renders: AtomicU64,
    /// What has already been said, so a page reloaded twenty times does not
    /// bury a new warning under twenty copies of an old one.
    said: Mutex<BTreeSet<String>>,
}

impl RenderHandler {
    /// Start rendering `site`.
    ///
    /// The site holds a tree per language it was emitted in, and a render
    /// answers every one of them, so nothing here is left to the directory.
    pub fn start(
        site: SsrSite,
        options: RenderOptions,
        settings: RenderSettings,
    ) -> Result<Self, String> {
        let renderer =
            Renderer::start(Arc::new(site), options).map_err(|error| error.to_string())?;
        let retire_after = settings.max_renders.map(|limit| limit + spread(limit));
        Ok(Self {
            renderer,
            settings,
            retire_after,
            renders: AtomicU64::new(0),
            said: Mutex::new(BTreeSet::new()),
        })
    }

    /// Write a warning the render came back with, unless it has been written.
    fn say(&self, warning: &str) {
        if let Ok(mut said) = self.said.lock()
            && said.insert(warning.to_string())
        {
            self.settings.log.warn(warning);
        }
    }
}

/// The page a render that could not happen is answered with.
fn failed_page(settings: &RenderSettings, request: &Request, error: &SsrError) -> Response {
    let (status, plain) = match error {
        SsrError::Busy => (503, "busy"),
        SsrError::TimedOut => (504, "the page took too long"),
        SsrError::Stopped => (503, "unavailable"),
        _ => (500, "internal server error"),
    };
    let mut message = format!("cannot render {}: {error}", request.path);
    if matches!(error, SsrError::Stopped) && settings.limit.is_none() {
        // A render that panicked takes the thread it ran on, and every
        // request after it lands here. What panicked was said on the way
        // past, which is where to look.
        message.push_str(
            ". A render ended in a panic and the ones after it cannot run; the panic is above, \
             and the server has to be started again.",
        );
    }
    // A full queue is load, not a fault; the access line records it.
    if !matches!(error, SsrError::Busy) {
        settings.log.error(&message);
    }
    let body = match settings.errors {
        ErrorPages::Detailed => message,
        ErrorPages::Plain => plain.to_string(),
    };
    let response = Response::text(status, &body);
    if status == 503 {
        // Whoever is asking is told when to come back rather than left to
        // guess, and a balancer reads it too.
        response.with_header("Retry-After", "1")
    } else {
        response
    }
}

/// A few percent on top of `limit`, different in every process, so workers
/// started together do not all retire together.
fn spread(limit: u64) -> u64 {
    let range = limit / 10;
    if range == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or_default();
    let seed = u64::from(nanos) ^ (u64::from(std::process::id()) << 17);
    // One round of a 64-bit mix, which is plenty for spreading restarts.
    let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (x ^ (x >> 31)) % (range + 1)
}

/// The request a render is asked for, from the request the server read.
fn render_request(request: &Request) -> SsrRequest {
    let target = if request.query.is_empty() {
        request.path.clone()
    } else {
        format!("{}?{}", request.path, request.query)
    };
    let mut render = SsrRequest::new(&request.method, &target).with_body(request.body.clone());
    // Whether the visitor's side arrived over TLS is what a trusted proxy in
    // front said; the server has already dropped what anyone else said.
    if request.secure {
        render = render.secure();
    }
    for (name, value) in &request.headers {
        render = render.with_header(name.clone(), value.clone());
    }
    render
}

impl RequestHandler for RenderHandler {
    fn handle(&self, request: &Request) -> Option<Response> {
        let asked = render_request(request);
        let result = match self.settings.limit {
            Some(limit) => self.renderer.try_render(asked, limit),
            None => self.renderer.render(asked),
        };
        if !matches!(result, Err(SsrError::Busy)) {
            self.renders.fetch_add(1, Ordering::SeqCst);
        }
        match result {
            Ok(response) => {
                for warning in &response.warnings {
                    self.say(warning);
                }
                Some(Response {
                    status: response.status,
                    headers: response.headers,
                    body: response.body.into_bytes(),
                })
            }
            Err(error) => Some(failed_page(&self.settings, request, &error)),
        }
    }

    fn status(&self) -> Status {
        if self.renderer.is_stopped() {
            return Status::Wedged;
        }
        if self
            .retire_after
            .is_some_and(|limit| self.renders.load(Ordering::SeqCst) >= limit)
        {
            return Status::Retiring;
        }
        if self.renderer.is_saturated() {
            return Status::Saturated;
        }
        Status::Ready
    }
}

#[cfg(test)]
mod tests {
    use lumen_ir::artifact::CompiledApp;
    use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
    use lumen_web::WebSpec;

    use super::*;
    use crate::log::LogFormat;

    fn asking(path: &str) -> Request {
        Request {
            method: "GET".to_string(),
            path: path.to_string(),
            ..Request::default()
        }
    }

    #[test]
    fn the_address_a_render_is_asked_for_is_the_one_that_arrived() {
        let mut asked = asking("/user/42");
        asked.query = "tab=posts".to_string();
        asked.method = "POST".to_string();
        asked.body = "name=ada".to_string();
        let render = render_request(&asked);
        assert_eq!(render.method, "POST");
        assert_eq!(render.path, "/user/42");
        assert_eq!(render.query, "tab=posts");
        assert_eq!(render.body, "name=ada");
        assert!(!render.secure);

        // A path with nothing after it keeps no empty query behind it.
        assert_eq!(render_request(&asking("/")).query, "");
    }

    #[test]
    fn what_the_server_says_about_tls_is_what_the_app_reads() {
        let mut asked = asking("/");
        asked.secure = true;
        asked.headers = vec![
            ("X-Forwarded-Proto".to_string(), "https".to_string()),
            ("Accept-Language".to_string(), "en-GB".to_string()),
        ];
        let render = render_request(&asked);
        assert!(render.secure);
        // Every header goes through; which of them the app may read is the
        // renderer's policy, not this one's.
        assert_eq!(render.headers.len(), 2);
    }

    #[test]
    fn restarts_are_spread_by_a_tenth_at_most() {
        assert_eq!(spread(5), 0);
        for _ in 0..100 {
            assert!(spread(1000) <= 100);
        }
    }

    /// An app of one label, which is enough to tell a document apart from
    /// nothing having been rendered.
    fn one_page() -> CompiledApp {
        let label = Element {
            tag: "label".to_string(),
            attrs: Attributes {
                text: Some("rendered here".to_string()),
                ..Attributes::default()
            },
            ..Element::default()
        };
        CompiledApp {
            ir: LayoutIR {
                root: Element {
                    tag: "root".to_string(),
                    children: vec![label],
                    ..Element::default()
                },
                ..LayoutIR::default()
            },
            ..CompiledApp::default()
        }
    }

    /// The process renders one request at a time, so this is the only case
    /// here that starts a renderer, and it lets go of it before it ends.
    #[test]
    fn a_request_comes_back_as_the_document_it_was_rendered_into_until_it_retires() {
        let site = SsrSite::new(one_page(), WebSpec::default()).expect("the entry is the page");
        let settings = RenderSettings {
            limit: Some(Duration::from_secs(60)),
            max_renders: Some(2),
            errors: ErrorPages::Plain,
            log: Arc::new(Log::new(LogFormat::Text, "test")),
        };
        let handler = RenderHandler::start(
            site,
            RenderOptions {
                queue: Some(2),
                ..RenderOptions::default()
            },
            settings,
        )
        .expect("nothing else in this process is rendering");
        assert_eq!(handler.status(), Status::Ready);

        let page = handler.handle(&asking("/")).expect("a page was rendered");
        assert_eq!(page.status, 200);
        assert!(
            page.headers
                .iter()
                .any(|(name, value)| name == "Content-Type" && value.contains("text/html")),
            "{:?}",
            page.headers
        );
        let body = String::from_utf8(page.body).expect("a document is text");
        assert!(body.contains("rendered here"), "{body}");
        assert_eq!(handler.status(), Status::Ready);

        let _ = handler.handle(&asking("/"));
        assert_eq!(handler.status(), Status::Retiring);
    }

    #[test]
    fn a_plain_error_page_says_nothing_the_log_does_not() {
        let settings = RenderSettings {
            limit: None,
            max_renders: None,
            errors: ErrorPages::Plain,
            log: Arc::new(Log::new(LogFormat::Text, "test")),
        };
        let error = SsrError::Artifact("secret detail".to_string());
        let (status, body) = {
            let plain = page_for(&settings, &error);
            (plain.status, String::from_utf8(plain.body).expect("text"))
        };
        assert_eq!(status, 500);
        assert!(!body.contains("secret"), "{body}");

        let detailed = RenderSettings {
            errors: ErrorPages::Detailed,
            ..settings
        };
        let body = String::from_utf8(page_for(&detailed, &error).body).expect("text");
        assert!(body.contains("secret detail"), "{body}");

        let busy = page_for(&detailed, &SsrError::Busy);
        assert_eq!(busy.status, 503);
        assert_eq!(busy.header("Retry-After"), Some("1"));
        assert_eq!(page_for(&detailed, &SsrError::TimedOut).status, 504);
    }

    /// The error page `settings` gives `error`.
    fn page_for(settings: &RenderSettings, error: &SsrError) -> Response {
        failed_page(settings, &asking("/"), error)
    }
}
