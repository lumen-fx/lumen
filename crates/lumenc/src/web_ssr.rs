//! Answering the dev server's requests by rendering the app for each one.
//!
//! This is the seam between the server in [`crate::web_serve`], which reads
//! HTTP and knows nothing about apps, and [`lumen_ssr`], which renders apps
//! and knows nothing about HTTP. A request the server has read becomes a
//! render, and the document comes back as the response.
//!
//! Renders happen one at a time: the renderer owns a thread and every request
//! queues for it, because the buses an app reads its state through belong to
//! the process. Serving more requests at once means more processes, which is
//! what a reverse proxy in front of this is for.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use lumen_ssr::{RenderOptions, Renderer, SsrRequest, SsrSite};

use crate::web_serve::{Request, RequestHandler, Response};

/// The header a proxy says the visitor arrived over TLS with.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// Answers every page by rendering the app for the request that asked for it.
pub struct RenderHandler {
    renderer: Renderer,
    /// Path prefixes this leaves to the documents on disk.
    leave: Vec<String>,
    /// What has already been said, so a page reloaded twenty times does not
    /// bury a new warning under twenty copies of an old one.
    said: Mutex<BTreeSet<String>>,
}

impl RenderHandler {
    /// Start rendering `site`.
    ///
    /// A renderer holds one site, and a site is in one language, so the trees
    /// of the other languages are answered by the documents the build wrote.
    /// `leave` names their prefixes.
    pub fn start(
        site: SsrSite,
        options: RenderOptions,
        leave: Vec<String>,
    ) -> Result<Self, String> {
        let renderer =
            Renderer::start(Arc::new(site), options).map_err(|error| error.to_string())?;
        Ok(Self {
            renderer,
            leave,
            said: Mutex::new(BTreeSet::new()),
        })
    }

    /// Whether a path belongs to a tree this handler leaves alone.
    fn left_alone(&self, path: &str) -> bool {
        let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
        self.leave.iter().any(|prefix| prefix == first)
    }

    /// Print a warning the render came back with, unless it has been printed.
    fn say(&self, warning: &str) {
        if let Ok(mut said) = self.said.lock()
            && said.insert(warning.to_string())
        {
            eprintln!("lumenc web: warning: {warning}");
        }
    }
}

impl RequestHandler for RenderHandler {
    fn handle(&self, request: &Request) -> Option<Response> {
        if self.left_alone(&request.path) {
            return None;
        }
        let target = if request.query.is_empty() {
            request.path.clone()
        } else {
            format!("{}?{}", request.path, request.query)
        };
        let mut render = SsrRequest::new(&request.method, &target).with_body(request.body.clone());
        // A proxy in front of this one is where TLS ends, so what it says
        // about the visitor's side of it is what the app reads.
        if request
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case(FORWARDED_PROTO) && value == "https")
        {
            render = render.secure();
        }
        for (name, value) in &request.headers {
            render = render.with_header(name.clone(), value.clone());
        }

        match self.renderer.render(render) {
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
            // A render that could not happen is the developer's to see, so it
            // is answered in the page rather than logged and hidden.
            Err(error) => {
                let message = format!("cannot render {}: {error}", request.path);
                eprintln!("lumenc web: {message}");
                Some(Response::text(500, &message))
            }
        }
    }
}
