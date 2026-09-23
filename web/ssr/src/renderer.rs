//! Rendering a document for a request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::{IntoScheduleConfigs, ResMut, Resource};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded, unbounded};
use lumen_core::prelude::TickStage;
use lumen_core::request;
use lumen_prerender::{Booted, Budget, Location, Settled};
use lumen_script::ScriptSet;
use lumen_script::http::{HttpDispatch, ThreadDispatch};
use lumen_script::runtime::ScriptCommandEvent;
use lumen_web::ServerPolicy;

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
    /// How many requests may wait while one is rendered. `None` lets the
    /// queue grow as long as callers keep asking; a server facing the public
    /// names a number, so [`Renderer::try_render`] can turn a request away
    /// with [`SsrError::Busy`] instead of making it wait behind all the
    /// others.
    pub queue: Option<usize>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            budget: Budget::default(),
            headers: HeaderPolicy::default(),
            fetch: FetchPolicy::default(),
            dispatch: default_dispatch(),
            queue: None,
        }
    }
}

impl RenderOptions {
    /// Also allow what `policy` allows: its hosts, its request cap and its
    /// headers, on top of what these options already allow.
    ///
    /// The policy is the app's, written down by the build from `lumen.toml`
    /// `[web.ssr]` and read back with [`SsrSite::policy`]; the rest of the
    /// options are the server's.
    pub fn with_policy(mut self, policy: &ServerPolicy) -> Self {
        for host in &policy.allow_hosts {
            self.fetch = self.fetch.allow_host(host);
        }
        if let Some(max) = policy.max_requests {
            self.fetch.max_requests = max;
        }
        for name in &policy.headers {
            self.headers = self.headers.allow(name);
        }
        self
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("budget", &self.budget)
            .field("headers", &self.headers)
            .field("fetch", &self.fetch)
            .field("queue", &self.queue)
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
    reply: Sender<Reply>,
}

/// What a caller hears back about its request.
enum Reply {
    /// The render has started, so the wait from here on is the render's own.
    Started,
    /// The render is over.
    Done(Result<SsrResponse, SsrError>),
}

/// What the callers and the renderer's thread both read.
#[derive(Default)]
struct Shared {
    /// A render is in progress.
    busy: AtomicBool,
    /// A render ran past its limit and has not come back, so nothing queued
    /// behind it will run.
    wedged: AtomicBool,
    /// The thread has ended, by a panic or by being told to.
    stopped: AtomicBool,
}

/// Lets the process start another renderer once the thread is gone, however
/// it went. It runs on a panic as well as on the way out of the loop, which is
/// why it is a guard rather than a line at the end.
struct Finished {
    shared: Arc<Shared>,
    requests: Receiver<Job>,
}

impl Drop for Finished {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        // A request that arrived while the thread was going would otherwise
        // wait for an answer nobody is left to give.
        answer_waiting(&self.requests, &SsrError::Stopped);
        RENDERING.store(false, Ordering::SeqCst);
    }
}

/// Answer every request still in the queue with `error`.
fn answer_waiting(requests: &Receiver<Job>, error: &SsrError) {
    while let Ok(job) = requests.try_recv() {
        let _ = job.reply.send(Reply::Done(Err(error.clone())));
    }
}

/// A running renderer.
///
/// It owns a thread, and every render happens on it: an app is built,
/// ticked and dropped there, and never touches the thread that asked. Calls
/// from several threads queue, because the process renders one request at a
/// time.
pub struct Renderer {
    jobs: Option<Sender<Job>>,
    /// The queue's other end, which a caller that finds the renderer wedged
    /// empties so nobody waits behind a render that is not coming back.
    waiting: Receiver<Job>,
    shared: Arc<Shared>,
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
        let (jobs, requests) = match options.queue {
            Some(depth) => bounded::<Job>(depth),
            None => unbounded::<Job>(),
        };
        let shared = Arc::new(Shared::default());
        let finished = Finished {
            shared: Arc::clone(&shared),
            requests: requests.clone(),
        };
        let waiting = requests.clone();
        let busy = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("lumen-ssr".to_string())
            .spawn(move || {
                let _finished = finished;
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
                    busy.busy.store(true, Ordering::SeqCst);
                    let _ = job.reply.send(Reply::Started);
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
                    let _ = job.reply.send(Reply::Done(answer));
                    busy.busy.store(false, Ordering::SeqCst);
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            // The closure never ran, so the guard inside it went with it and
            // has already let go of the process's renderer.
            Err(_) => return Err(SsrError::Stopped),
        };
        Ok(Self {
            jobs: Some(jobs),
            waiting,
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Render the document for `request`, blocking until it is written.
    ///
    /// With a bounded [`RenderOptions::queue`] this also waits for a place in
    /// the queue. A render that panics takes the renderer with it: every
    /// later call answers [`SsrError::Stopped`], and a server that has to
    /// carry on drops this one and starts another.
    pub fn render(&self, request: SsrRequest) -> Result<SsrResponse, SsrError> {
        let (reply, answer) = unbounded();
        self.usable()?
            .send(Job { request, reply })
            .map_err(|_| SsrError::Stopped)?;
        self.after_sending();
        loop {
            match answer.recv() {
                Ok(Reply::Started) => continue,
                Ok(Reply::Done(result)) => return result,
                Err(_) => return Err(SsrError::Stopped),
            }
        }
    }

    /// Render the document for `request`, or say at once why not.
    ///
    /// A full queue answers [`SsrError::Busy`] without waiting, which is the
    /// moment to tell the visitor to come back rather than to hold their
    /// connection. Once the render has started it gets `limit` to finish; a
    /// render still running after that answers [`SsrError::TimedOut`].
    ///
    /// A render past its limit cannot be stopped from here. A tick that never
    /// returns holds the renderer's thread for good, so the renderer is
    /// wedged from then on: whatever was queued behind it and every later
    /// call answers [`SsrError::Stopped`], and [`Self::is_stopped`] says so.
    /// The process that owns it is the thing to restart. The limit covers
    /// only renders asked for through this method; one asked for through
    /// [`Self::render`] has none.
    pub fn try_render(
        &self,
        request: SsrRequest,
        limit: Duration,
    ) -> Result<SsrResponse, SsrError> {
        let (reply, answer) = unbounded();
        match self.usable()?.try_send(Job { request, reply }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(SsrError::Busy),
            Err(TrySendError::Disconnected(_)) => return Err(SsrError::Stopped),
        }
        self.after_sending();
        // The wait for a turn has no limit of its own. The render ahead is
        // bounded by its caller's limit, and the caller that finds it past
        // that answers everything still waiting.
        match answer.recv() {
            Ok(Reply::Started) => {}
            Ok(Reply::Done(result)) => return result,
            Err(_) => return Err(SsrError::Stopped),
        }
        match answer.recv_timeout(limit) {
            Ok(Reply::Done(result)) => result,
            Ok(Reply::Started) | Err(RecvTimeoutError::Disconnected) => Err(SsrError::Stopped),
            Err(RecvTimeoutError::Timeout) => {
                self.shared.wedged.store(true, Ordering::SeqCst);
                answer_waiting(&self.waiting, &SsrError::Stopped);
                Err(SsrError::TimedOut)
            }
        }
    }

    /// Whether a render is in progress and every place in the queue behind
    /// it is taken, so the next [`Self::try_render`] answers
    /// [`SsrError::Busy`]. An unbounded queue is never full.
    pub fn is_saturated(&self) -> bool {
        let Some(jobs) = &self.jobs else {
            return true;
        };
        self.shared.busy.load(Ordering::SeqCst)
            && jobs.capacity().is_some_and(|depth| jobs.len() >= depth)
    }

    /// Whether this renderer will not answer again: a render ran past its
    /// limit (see [`Self::try_render`]), or one panicked and took the thread
    /// with it.
    pub fn is_stopped(&self) -> bool {
        self.shared.wedged.load(Ordering::SeqCst) || self.shared.stopped.load(Ordering::SeqCst)
    }

    /// The queue, unless the renderer can no longer answer.
    fn usable(&self) -> Result<&Sender<Job>, SsrError> {
        if self.shared.wedged.load(Ordering::SeqCst) || self.shared.stopped.load(Ordering::SeqCst) {
            return Err(SsrError::Stopped);
        }
        self.jobs.as_ref().ok_or(SsrError::Stopped)
    }

    /// Close the gap between checking the thread is alive and handing it a
    /// request: a thread that ended in between has already emptied the
    /// queue, so what arrived after that is answered here.
    fn after_sending(&self) {
        if self.shared.stopped.load(Ordering::SeqCst) {
            answer_waiting(&self.waiting, &SsrError::Stopped);
        }
    }

    /// Stop rendering, and wait for the render in progress to finish.
    ///
    /// The process can start another renderer afterwards. Dropping a
    /// [`Renderer`] does the same thing. A wedged renderer is not waited for:
    /// its thread is left to finish on its own, and the process cannot start
    /// another renderer until it does.
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
            && !self.shared.wedged.load(Ordering::SeqCst)
        {
            let _ = worker.join();
        }
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
        browser_only,
        unsupported_engines,
        language_error,
    } = lumen_prerender::boot(
        site.compiled(),
        &Location {
            path: key.clone(),
            segment,
        },
        site.language(tree),
        site.seed(),
        dispatch,
    );
    if let Some(error) = language_error {
        warnings.push(format!(
            "the app could not start in this tree's locale, so it ran untranslated: {error}"
        ));
    }
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
    // What the components inside this render's `<for>` rows built, read off
    // the world that built them. The rows and the bodies come from one app,
    // so a document written from them shows a card for every row it shows.
    let fills = lumen_prerender::row_fills(&mut app);
    // From here a reply belongs to nobody: the app that asked for it is
    // about to go, and the next request gets its own.
    flight.close();
    drop(app);

    warnings.extend(response.refused.iter().cloned());
    for name in browser_only.take() {
        warnings.push(format!(
            "the app called `{name}`, which runs only in a browser; the call raised here and the \
             document shows what the markup gives in its place"
        ));
    }
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
    page.nodes = state.nodes;
    page.fills = fills;
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
        if event.0.builds_nodes() {
            built.0 = true;
        }
    }
}
