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

use lumen_core::warn_line;
use lumen_ssr::{RenderOptions, Renderer, SsrError, SsrRequest, SsrSite};

use crate::web_serve::{Request, RequestHandler, Response};

/// The header a proxy says the visitor arrived over TLS with.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// Answers every page by rendering the app for the request that asked for it.
pub struct RenderHandler {
    renderer: Renderer,
    /// What has already been said, so a page reloaded twenty times does not
    /// bury a new warning under twenty copies of an old one.
    said: Mutex<BTreeSet<String>>,
}

impl RenderHandler {
    /// Start rendering `site`.
    ///
    /// The site holds a tree per language it was emitted in, and a render
    /// answers every one of them, so nothing here is left to the directory.
    pub fn start(site: SsrSite, options: RenderOptions) -> Result<Self, String> {
        let renderer =
            Renderer::start(Arc::new(site), options).map_err(|error| error.to_string())?;
        Ok(Self {
            renderer,
            said: Mutex::new(BTreeSet::new()),
        })
    }

    /// Print a warning the render came back with, unless it has been printed.
    fn say(&self, warning: &str) {
        if let Ok(mut said) = self.said.lock()
            && said.insert(warning.to_string())
        {
            warn_line!("lumenc web: warning: {warning}");
        }
    }
}

/// The request a render is asked for, from the request the server read.
fn render_request(request: &Request) -> SsrRequest {
    let target = if request.query.is_empty() {
        request.path.clone()
    } else {
        format!("{}?{}", request.path, request.query)
    };
    let mut render = SsrRequest::new(&request.method, &target).with_body(request.body.clone());
    // A proxy in front of this one is where TLS ends, so what it says about
    // the visitor's side of it is what the app reads.
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
    render
}

impl RequestHandler for RenderHandler {
    fn handle(&self, request: &Request) -> Option<Response> {
        match self.renderer.render(render_request(request)) {
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
                let mut message = format!("cannot render {}: {error}", request.path);
                if matches!(error, SsrError::Stopped) {
                    // A render that panicked takes the thread it ran on, and
                    // every request after it lands here. What panicked was
                    // said on the way past, which is where to look.
                    message.push_str(
                        ". A render ended in a panic and the ones after it cannot run; the panic \
                         is above, and the server has to be started again.",
                    );
                }
                warn_line!("lumenc web: {message}");
                Some(Response::text(500, &message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lumen_ir::artifact::CompiledApp;
    use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
    use lumen_web::WebSpec;

    use super::*;

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
    fn what_a_proxy_says_about_tls_is_what_the_app_reads() {
        let mut asked = asking("/");
        asked.headers = vec![
            ("X-Forwarded-Proto".to_string(), "https".to_string()),
            ("Accept-Language".to_string(), "en-GB".to_string()),
        ];
        let render = render_request(&asked);
        assert!(render.secure);
        // Every header goes through; which of them the app may read is the
        // renderer's policy, not this one's.
        assert_eq!(render.headers.len(), 2);

        let mut plain = asking("/");
        plain.headers = vec![("X-Forwarded-Proto".to_string(), "http".to_string())];
        assert!(!render_request(&plain).secure);
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
    fn a_request_comes_back_as_the_document_it_was_rendered_into() {
        let site = SsrSite::new(one_page(), WebSpec::default()).expect("the entry is the page");
        let handler = RenderHandler::start(site, RenderOptions::default())
            .expect("nothing else in this process is rendering");

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
    }
}
