//! Rendering a document for a request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::{IntoScheduleConfigs, ResMut, Resource};
use crossbeam_channel::{Sender, unbounded};
use lumen_core::nav::SEGMENT_SIGNAL;
use lumen_core::prelude::TickStage;
use lumen_core::property_store::PropertyStore;
use lumen_core::request;
use lumen_prerender::{Booted, Budget, Settled};
use lumen_script::ScriptSet;
use lumen_script::http::{HttpDispatch, ThreadDispatch};
use lumen_script::runtime::ScriptCommandEvent;

use crate::error::SsrError;
use crate::fetch::{CountingDispatch, FetchPolicy, Flight};
use crate::request::{HeaderPolicy, SsrRequest};
use crate::response::{
    DEFAULT_STATUS, NOT_FOUND_STATUS, REDIRECT_STATUS, ResponseState, SsrResponse,
    apply_response_commands,
};
use crate::site::SsrSite;

/// The content type every document is served as.
const HTML: &str = "text/html; charset=utf-8";

/// The header a render sets on a document that is missing state the app had
/// not finished producing.
const RENDER_HEADER: &str = "X-Lumen-Render";

/// The header naming the language a document is written in.
const CONTENT_LANGUAGE: &str = "Content-Language";

/// The header telling a shared cache that the document depends on what the
/// visitor asked for.
const VARY: &str = "Vary";

/// How a renderer answers requests.
///
/// The defaults are the careful ones: an app reaches no host until it is
/// given one, and reads only the headers that say what a browser is rather
/// than who is using it.
#[derive(Clone)]
pub struct RenderOptions {
    /// How long one render gets. An app still changing when it runs out is
    /// answered with what it had reached.
    pub budget: Budget,
    /// Which request headers the app may read.
    pub headers: HeaderPolicy,
    /// Which addresses a render may ask for, and how many times.
    pub fetch: FetchPolicy,
    /// The transport the app's own HTTP calls run on.
    ///
    /// Whatever goes here is wrapped, not replaced: the wrapper counts what
    /// is in flight and applies [`Self::fetch`], and this performs the
    /// request. A test double goes here, and so does a client with an
    /// embedder's own timeouts, proxy and certificates.
    pub dispatch: Arc<dyn HttpDispatch>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            budget: Budget::default(),
            headers: HeaderPolicy::default(),
            fetch: FetchPolicy::default(),
            dispatch: default_dispatch(),
        }
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("budget", &self.budget)
            .field("headers", &self.headers)
            .field("fetch", &self.fetch)
            .finish_non_exhaustive()
    }
}

/// The transport a render uses when the embedder names none: the client
/// Lumen ships, on a thread per request.
#[cfg(feature = "http-client")]
fn default_dispatch() -> Arc<dyn HttpDispatch> {
    Arc::new(ThreadDispatch::new(Arc::new(
        lumen_http_ureq::UreqHttpClient,
    )))
}

/// The transport a build without `http-client` gets: every request answers
/// with why there is none, so a page says what it is missing instead of
/// hanging.
#[cfg(not(feature = "http-client"))]
fn default_dispatch() -> Arc<dyn HttpDispatch> {
    Arc::new(ThreadDispatch::new(Arc::new(
        lumen_script::http::DisabledHttpClient,
    )))
}

/// Whether this process has a renderer. See [`SsrError::AlreadyRunning`].
static RENDERING: AtomicBool = AtomicBool::new(false);

/// One request, and where to put the answer.
struct Job {
    request: SsrRequest,
    reply: Sender<Result<SsrResponse, SsrError>>,
}

/// A running renderer.
///
/// It owns a thread, and every render happens on it: an app is built,
/// ticked and dropped there, and never touches the thread that asked. Calls
/// from several threads queue, because the process renders one request at a
/// time.
pub struct Renderer {
    jobs: Option<Sender<Job>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Renderer {
    /// Start rendering `site`.
    ///
    /// Fails with [`SsrError::AlreadyRunning`] when this process already has
    /// a renderer. That is not a queue depth to tune: the buses an app reads
    /// its state through belong to the process, so a second app ticking
    /// alongside the first would take writes meant for it. Scaling is more
    /// processes behind whatever balances them.
    pub fn start(site: Arc<SsrSite>, options: RenderOptions) -> Result<Self, SsrError> {
        if RENDERING.swap(true, Ordering::SeqCst) {
            return Err(SsrError::AlreadyRunning);
        }
        let (jobs, requests) = unbounded::<Job>();
        let worker = std::thread::Builder::new()
            .name("lumen-ssr".to_string())
            .spawn(move || {
                let flight = Arc::new(Flight::default());
                // The answer to an address no page answers for is the shell,
                // which holds no state and so is the same document every
                // time. One per language, written on the first request that
                // needs it, because the shell is the app in one of them.
                let mut missing: Vec<Option<Result<SsrResponse, SsrError>>> =
                    vec![None; site.locales().len()];
                // Every app this thread builds is also dropped here, before
                // the next request is taken.
                for job in requests {
                    let route = site.route(&job.request);
                    let mut answer = match route.page {
                        Some(page) => {
                            render_one(&site, &options, &flight, &job.request, route.tree, page)
                        }
                        None => missing[route.tree]
                            .get_or_insert_with(|| not_found(&site, route.tree))
                            .clone(),
                    };
                    if let Ok(response) = &mut answer {
                        response.warnings.extend(route.warnings);
                    }
                    let _ = job.reply.send(answer);
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(_) => {
                RENDERING.store(false, Ordering::SeqCst);
                return Err(SsrError::Stopped);
            }
        };
        Ok(Self {
            jobs: Some(jobs),
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Render the document for `request`, blocking until it is written.
    ///
    /// A render that panics takes the renderer with it: every later call
    /// answers [`SsrError::Stopped`], and a server that has to carry on drops
    /// this one and starts another.
    pub fn render(&self, request: SsrRequest) -> Result<SsrResponse, SsrError> {
        let (reply, answer) = unbounded();
        self.jobs
            .as_ref()
            .ok_or(SsrError::Stopped)?
            .send(Job { request, reply })
            .map_err(|_| SsrError::Stopped)?;
        answer.recv().map_err(|_| SsrError::Stopped)?
    }

    /// Stop rendering, and wait for the render in progress to finish.
    ///
    /// The process can start another renderer afterwards. Dropping a
    /// [`Renderer`] does the same thing.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The worker's loop ends when the last sender goes.
        self.jobs.take();
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
        RENDERING.store(false, Ordering::SeqCst);
    }
}

/// Build the app, let it settle, and write the document it settled into.
fn render_one(
    site: &SsrSite,
    options: &RenderOptions,
    flight: &Arc<Flight>,
    request: &SsrRequest,
    tree: usize,
    page: (String, String),
) -> Result<SsrResponse, SsrError> {
    let mut warnings = Vec::new();
    let (key, segment) = page;

    // On this thread for as long as the render is, which is what the request
    // builtins read through.
    let _scope = request::enter(request.context(&options.headers));

    let epoch = flight.open();
    let dispatch = Arc::new(CountingDispatch::new(
        Arc::clone(&options.dispatch),
        Arc::clone(flight),
        epoch,
        options.fetch.clone(),
    ));

    let Booted {
        mut app,
        unsupported_engines,
    } = lumen_prerender::boot(site.compiled(), &key, site.seed(), dispatch);
    for engine in unsupported_engines {
        warnings.push(format!(
            "the app carries a `{engine}` program, which this renderer has no host for; what it \
             publishes is missing from the document"
        ));
    }
    // A render runs compiled programs, so a language the build had no compiler
    // for reaches it as source it cannot read. Said out loud, because the page
    // is otherwise missing everything that program would have published and
    // nothing says why.
    for script in &site.compiled().scripts {
        if script.bytecode.is_none() {
            warnings.push(format!(
                "the app's `{}` program was not compiled into the artifact, so this render runs \
                 none of it",
                script.engine
            ));
        }
    }

    // The request cells went in with the app, ahead of its scripts. What is
    // left is the part of the address the page set answers for: the tail of a
    // path whose page is `/user`, which the router works out and the page
    // reads.
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str(SEGMENT_SIGNAL, segment.as_str());
    app.world.init_resource::<ResponseState>();
    app.world.init_resource::<RenderedDom>();
    app.add_systems(
        TickStage::Systems,
        (apply_response_commands, note_dom_commands)
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::Fetch),
    );

    let (state, settled) =
        lumen_prerender::settle_while(&mut app, options.budget, || flight.outstanding());

    let response = app
        .world
        .remove_resource::<ResponseState>()
        .unwrap_or_default();
    let built_dom = app
        .world
        .remove_resource::<RenderedDom>()
        .is_some_and(|dom| dom.0);
    // From here a reply belongs to nobody: the app that asked for it is
    // about to go, and the next request gets its own.
    flight.close();
    drop(app);

    warnings.extend(response.refused.iter().cloned());
    for skipped in &state.skipped {
        warnings.push(format!("the document is written without {skipped}"));
    }
    if built_dom {
        warnings.push(
            "the app's scripts built nodes of their own, which a document carries only once the \
             browser has run them"
                .to_string(),
        );
    }
    let partial = match settled {
        Settled::At(_) => false,
        Settled::Capped(ticks) => {
            warnings.push(format!(
                "the app had not finished after {ticks} ticks, so the document holds the state it \
                 had reached"
            ));
            true
        }
    };

    let spec = site.tree(tree);
    if let Some(location) = &response.redirect {
        return Ok(redirect(location, site, tree, &response, partial, warnings));
    }

    let mut page = site.page(&key, spec);
    page.signals = state.signals;
    page.seed = state.seed;
    let body = lumen_web::document(&page, spec, &mut warnings)?;

    let mut headers = vec![("Content-Type".to_string(), HTML.to_string())];
    language_headers(&mut headers, site, tree);
    // The app's own headers go on last, so a page that sets one of these
    // itself is the one that is sent.
    for (name, value) in &response.headers {
        set_header(&mut headers, name, value);
    }
    if partial {
        set_header(&mut headers, RENDER_HEADER, "partial");
    }
    Ok(SsrResponse {
        status: response.status.unwrap_or(DEFAULT_STATUS),
        headers,
        body,
        warnings,
    })
}

/// The answer to an address no page answers for: the app shell, with the
/// status a static host sends for a path it has no file for.
///
/// The app is not built for it. The shell is the app with no page selected
/// and no state, so running the app would spend a whole boot to arrive at a
/// document that is the same every time, for an address anyone can guess.
fn not_found(site: &SsrSite, tree: usize) -> Result<SsrResponse, SsrError> {
    let (body, warnings) = site.not_found_body(tree)?;
    let mut headers = vec![("Content-Type".to_string(), HTML.to_string())];
    language_headers(&mut headers, site, tree);
    Ok(SsrResponse {
        status: NOT_FOUND_STATUS,
        headers,
        body,
        warnings,
    })
}

/// Say which language the response is in, and, for a site that holds more
/// than one, that the answer depends on which one was asked for.
///
/// A single-language site sends no `Vary`: its documents are the same for
/// every visitor, and saying otherwise would split a shared cache for
/// nothing.
fn language_headers(headers: &mut Vec<(String, String)>, site: &SsrSite, tree: usize) {
    let locales = site.locales();
    set_header(headers, CONTENT_LANGUAGE, locales[tree]);
    if locales.len() > 1 {
        set_header(headers, VARY, "Accept-Language");
    }
}

/// The answer to a request the app sent somewhere else.
fn redirect(
    location: &str,
    site: &SsrSite,
    tree: usize,
    response: &ResponseState,
    partial: bool,
    warnings: Vec<String>,
) -> SsrResponse {
    let mut headers = vec![("Location".to_string(), location.to_string())];
    language_headers(&mut headers, site, tree);
    for (name, value) in &response.headers {
        set_header(&mut headers, name, value);
    }
    if partial {
        set_header(&mut headers, RENDER_HEADER, "partial");
    }
    SsrResponse {
        status: response.status.unwrap_or(REDIRECT_STATUS),
        headers,
        body: String::new(),
        warnings,
    }
}

/// Set a header, replacing whatever was under that name.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    match headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        Some(existing) => existing.1 = value.to_string(),
        None => headers.push((name.to_string(), value.to_string())),
    }
}

/// Whether the app's scripts built any nodes of their own.
#[derive(Resource, Default)]
struct RenderedDom(bool);

/// Watch the command stream for a script building its own nodes, so the
/// render can say the document is missing them.
fn note_dom_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut built: ResMut<RenderedDom>,
) {
    for event in events.read() {
        if event.0.mutates_dom() {
            built.0 = true;
        }
    }
}
