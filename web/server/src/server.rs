//! Serving a site directory over HTTP.
//!
//! It answers the way a static host answers: a file is served from disk, and
//! a path with no file behind it gets the app shell, with the status a static
//! host would send.
//!
//! A [`RequestHandler`] changes what a document is. With one installed, the
//! server reads the request whole and asks the handler for every page, and
//! serves the file on disk only when the handler has nothing to say. Files
//! that are not documents never reach it: a stylesheet, an artifact and the
//! wasm module are answered straight from disk on the connection's own
//! thread, so they never wait behind a page being rendered.
//!
//! Every connection has a thread, up to [`Limits::max_connections`] of them;
//! past that, a new connection waits in the listener's backlog until one
//! closes. Every read and write runs against a deadline from [`Limits`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::http::{
    self, FileBody, HTML, Head, Persist, Request, RequestHead, Response, TEXT, Timed, read_body,
    read_head, write_response,
};
use crate::log::{Access, Log};
use crate::proxy::Trust;

/// The address the server listens on when none is named.
pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Where the health endpoints live unless the server is told otherwise.
pub const HEALTH_PATH: &str = "/_lumen";

/// What a file whose name carries the hash of its bytes is served with. Its
/// bytes never change under that name, so a browser and a CDN keep it.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// What every other file is served with: kept, and checked before reuse.
const REVALIDATE: &str = "no-cache";

/// The content type each extension is served with.
///
/// `application/wasm` is the one that has to be right: a browser refuses to
/// instantiate a streamed module served as anything else, and the failure
/// reads like a fault in the app rather than in the server.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "html" => HTML,
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "xml" => "application/xml",
        "txt" => TEXT,
        _ => "application/octet-stream",
    }
}

/// Whether a file's name carries the hash of its contents, the way a build
/// names every file but the documents and the manifest:
/// `styles.<16 hex digits>.css`, or `LICENSE.<16 hex digits>`.
fn is_hashed(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.split('.')
        .skip(1)
        .any(|part| part.len() == 16 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// How long the server waits, and how much it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Connections served at once. Past this, a new one waits in the
    /// listener's backlog.
    pub max_connections: usize,
    /// How long a client has to send a request's line and headers, counted
    /// from when the connection opened or from the request's first byte.
    pub header_timeout: Duration,
    /// How long a client has to send a request's body.
    pub body_timeout: Duration,
    /// How long a client has to take a response.
    pub write_timeout: Duration,
    /// How long an open connection waits for its next request. Zero closes
    /// every connection after one response.
    pub keep_alive: Duration,
    /// How long a stopping server waits for the requests it is answering.
    pub shutdown_grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 256,
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            keep_alive: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(30),
        }
    }
}

/// Whether a handler can take the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// It can.
    Ready,
    /// It is answering as many requests as it holds, so a request now is
    /// turned away. The server reports itself not ready meanwhile.
    Saturated,
    /// It has done what this process is for; the process should finish what
    /// it is answering and exit, and whatever started it starts another.
    Retiring,
    /// It will not answer again, so the process should exit.
    Wedged,
}

/// Something that answers a request instead of the directory.
///
/// One render at a time is the contract a Lumen app is rendered under, so a
/// handler that renders is called from several connection threads and answers
/// them one after another. That is why files are answered before a handler is
/// asked: the page a visitor waits for is the slow part, and the stylesheet
/// beside it should not wait with it.
pub trait RequestHandler: Send + Sync {
    /// Answer `request`, or return `None` to let the directory answer it.
    fn handle(&self, request: &Request) -> Option<Response>;

    /// Whether the handler can take the next request.
    fn status(&self) -> Status {
        Status::Ready
    }
}

/// Why a server stopped answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It was told to, by a signal or through a [`Shutdown`].
    Stopped,
    /// Its handler retired. See [`Status::Retiring`].
    Recycled,
    /// Its handler will not answer again. See [`Status::Wedged`].
    Wedged,
}

/// What every connection thread and the accepting loop share.
struct Control {
    draining: AtomicBool,
    exit: Mutex<Option<Exit>>,
    active: Mutex<usize>,
    changed: Condvar,
    /// Connections waiting for their next request, which a drain closes
    /// rather than waiting out their keep-alive.
    idle: Mutex<HashMap<u64, TcpStream>>,
    next: AtomicU64,
    /// Where to connect to wake an accept that cannot be given a timeout.
    #[cfg(not(unix))]
    wake: Option<SocketAddr>,
}

impl Control {
    fn begin(&self, reason: Exit) {
        if let Ok(mut exit) = self.exit.lock() {
            // A wedged handler outranks the reason a drain started for,
            // because it decides whether the process can still answer.
            if exit.is_none() || reason == Exit::Wedged {
                *exit = Some(reason);
            }
        }
        if self.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(idle) = self.idle.lock() {
            for stream in idle.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        self.changed.notify_all();
        #[cfg(not(unix))]
        if let Some(wake) = self.wake {
            let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(500));
        }
    }

    fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
}

/// Stops a running server: it stops accepting, finishes what it is
/// answering, and [`Server::run`] returns.
#[derive(Clone)]
pub struct Shutdown(Arc<Control>);

impl Shutdown {
    /// Start stopping.
    pub fn shutdown(&self) {
        self.0.begin(Exit::Stopped);
    }
}

/// A site server.
pub struct Server {
    listener: TcpListener,
    site: Arc<Site>,
    control: Arc<Control>,
}

/// What a connection thread needs to answer requests.
struct Site {
    root: PathBuf,
    base: String,
    handler: Option<Arc<dyn RequestHandler>>,
    limits: Limits,
    health: Option<String>,
    trust: Trust,
    log: Option<Arc<Log>>,
    /// Whether other processes accept from the same listening socket.
    siblings: bool,
}

impl Server {
    /// Take a port on `host` and hold it.
    ///
    /// `port` 0 asks the system for a free one, which is what [`Self::addr`]
    /// then reports. `host` is [`LOOPBACK`] unless the caller has been asked
    /// for something reachable from elsewhere.
    pub fn bind(root: &Path, base: &str, host: IpAddr, port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind((host, port)).map_err(|e| {
            format!(
                "cannot listen on {host} port {port}: {e}. Name another port, port 0 for any \
                 free one, or an address this machine has."
            )
        })?;
        Ok(Self::on(listener, root, base))
    }

    /// Serve on a listener that is already bound, such as one a supervisor
    /// hands to its workers.
    pub fn on(listener: TcpListener, root: &Path, base: &str) -> Self {
        #[cfg(not(unix))]
        let wake = listener.local_addr().ok().map(|mut addr| {
            if addr.ip().is_unspecified() {
                addr.set_ip(match addr {
                    SocketAddr::V4(_) => LOOPBACK,
                    SocketAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                });
            }
            addr
        });
        Self {
            listener,
            site: Arc::new(Site {
                root: root.to_path_buf(),
                base: normalize_base(base),
                handler: None,
                limits: Limits::default(),
                health: Some(HEALTH_PATH.to_string()),
                trust: Trust::Nobody,
                log: None,
                siblings: false,
            }),
            control: Arc::new(Control {
                draining: AtomicBool::new(false),
                exit: Mutex::new(None),
                active: Mutex::new(0),
                changed: Condvar::new(),
                idle: Mutex::new(HashMap::new()),
                next: AtomicU64::new(0),
                #[cfg(not(unix))]
                wake,
            }),
        }
    }

    fn site_mut(&mut self) -> &mut Site {
        Arc::get_mut(&mut self.site).expect("a server is configured before it runs")
    }

    /// Answer pages with `handler` rather than with the documents on disk.
    pub fn with_handler(mut self, handler: Arc<dyn RequestHandler>) -> Self {
        self.site_mut().handler = Some(handler);
        self
    }

    /// Wait and hold as `limits` says.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.site_mut().limits = limits;
        self
    }

    /// Answer the health endpoints under `prefix` rather than under
    /// [`HEALTH_PATH`]: `<prefix>/healthz` and `<prefix>/readyz`.
    pub fn with_health_path(mut self, prefix: &str) -> Self {
        let prefix = prefix.trim().trim_end_matches('/');
        let prefix = if prefix.starts_with('/') || prefix.is_empty() {
            prefix.to_string()
        } else {
            format!("/{prefix}")
        };
        self.site_mut().health = Some(prefix);
        self
    }

    /// Believe the forwarding headers from the peers `trust` names.
    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.site_mut().trust = trust;
        self
    }

    /// Say that other processes accept from this server's listening socket,
    /// as the workers a supervisor starts do.
    ///
    /// A server with siblings takes no connection while its handler is
    /// saturated, and leaves it in the socket's queue for a sibling that can
    /// answer it. A server alone takes it and answers a page with a 503, so a
    /// balancer in front hears at once that it is full.
    pub fn with_siblings(mut self, siblings: bool) -> Self {
        self.site_mut().siblings = siblings;
        self
    }

    /// Write an access line for every request to `log`.
    pub fn with_access_log(mut self, log: Arc<Log>) -> Self {
        self.site_mut().log = Some(log);
        self
    }

    /// What stops this server from another thread.
    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown(Arc::clone(&self.control))
    }

    /// The address the server is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from((LOOPBACK, 0)))
    }

    /// The URL the site opens at, base path included.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr(), self.site.base)
    }

    /// Answer requests until the server is stopped, then finish what it is
    /// answering, for up to [`Limits::shutdown_grace`], and say why it
    /// stopped.
    pub fn run(&self) -> Exit {
        #[cfg(unix)]
        let _ = self.listener.set_nonblocking(true);
        loop {
            if !self.wait_for_room() {
                break;
            }
            let Some((stream, peer)) = self.next_connection() else {
                if self.control.draining() {
                    break;
                }
                continue;
            };
            let site = Arc::clone(&self.site);
            let control = Arc::clone(&self.control);
            if let Ok(mut active) = self.control.active.lock() {
                *active += 1;
            }
            let spawned = std::thread::Builder::new()
                .name("lumen-server-conn".to_string())
                .spawn(move || {
                    let _slot = Slot(&control);
                    connection(stream, peer, &site, &control);
                });
            if spawned.is_err()
                && let Ok(mut active) = self.control.active.lock()
            {
                *active -= 1;
            }
        }
        self.drain();
        self.control
            .exit
            .lock()
            .ok()
            .and_then(|exit| *exit)
            .unwrap_or(Exit::Stopped)
    }

    /// Wait until a connection may be taken. `false` once the server is
    /// stopping.
    fn wait_for_room(&self) -> bool {
        let Ok(mut active) = self.control.active.lock() else {
            return false;
        };
        loop {
            if self.control.draining() {
                return false;
            }
            let saturated = self.site.siblings
                && self
                    .site
                    .handler
                    .as_ref()
                    .is_some_and(|handler| handler.status() == Status::Saturated);
            if !saturated && *active < self.site.limits.max_connections.max(1) {
                return true;
            }
            // Saturation clears when a render finishes, which nothing here is
            // told about, so it is looked at again soon.
            let wait = if saturated {
                Duration::from_millis(20)
            } else {
                Duration::from_millis(200)
            };
            match self.control.changed.wait_timeout(active, wait) {
                Ok((next, _)) => active = next,
                Err(_) => return false,
            }
        }
    }

    /// The next connection, or `None` when there is nothing yet or the wait
    /// was cut short to look at whether the server is stopping.
    #[cfg(unix)]
    fn next_connection(&self) -> Option<(TcpStream, SocketAddr)> {
        use std::os::fd::AsRawFd;
        let mut ready = libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is one valid pollfd for the length passed, and the
        // descriptor it names is the listener this server owns.
        let found = unsafe { libc::poll(&mut ready, 1, 200) };
        // A server that started stopping while it waited leaves the
        // connection in the backlog, where a worker still running takes it.
        if found <= 0 || self.control.draining() {
            return None;
        }
        // Another worker sharing the listener may have taken it first, which
        // a non-blocking accept reports as would-block.
        let (stream, peer) = self.listener.accept().ok()?;
        // Some systems hand the accepted socket the listener's mode.
        let _ = stream.set_nonblocking(false);
        Some((stream, peer))
    }

    /// The next connection. The accept blocks, and a stopping server wakes it
    /// by connecting to itself.
    #[cfg(not(unix))]
    fn next_connection(&self) -> Option<(TcpStream, SocketAddr)> {
        // The connection that woke a stopping server is answered like any
        // other; it sends nothing, so it closes at once.
        self.listener.accept().ok()
    }

    /// Wait for every connection to finish, for as long as the grace allows.
    fn drain(&self) {
        let deadline = Instant::now() + self.site.limits.shutdown_grace;
        let Ok(mut active) = self.control.active.lock() else {
            return;
        };
        while *active > 0 {
            let now = Instant::now();
            if now >= deadline {
                if let Some(log) = &self.site.log {
                    log.warn(&format!(
                        "stopping with {} connection(s) still open after the shutdown grace",
                        *active
                    ));
                }
                return;
            }
            match self.control.changed.wait_timeout(active, deadline - now) {
                Ok((next, _)) => active = next,
                Err(_) => return,
            }
        }
    }
}

/// Holds one of the server's connection places, and gives it back on drop.
struct Slot<'a>(&'a Control);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active = active.saturating_sub(1);
        }
        self.0.changed.notify_all();
    }
}

/// Registers a connection as waiting for its next request while it lives.
struct Idle<'a> {
    control: &'a Control,
    id: u64,
}

impl<'a> Idle<'a> {
    fn new(control: &'a Control, stream: &TcpStream) -> Option<Self> {
        let id = control.next.fetch_add(1, Ordering::Relaxed);
        let clone = stream.try_clone().ok()?;
        control.idle.lock().ok()?.insert(id, clone);
        // A drain that began before this connection was listed would not
        // have closed it.
        if control.draining() {
            return None;
        }
        Some(Self { control, id })
    }
}

impl Drop for Idle<'_> {
    fn drop(&mut self) {
        if let Ok(mut idle) = self.control.idle.lock() {
            idle.remove(&self.id);
        }
    }
}

/// Answer requests on one connection until it closes.
fn connection(stream: TcpStream, peer: SocketAddr, site: &Site, control: &Control) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(Timed::new(read_half));
    let mut writer = Timed::new(write_half);
    let opened = Instant::now();
    let mut first = true;
    loop {
        // Wait for the request to begin. The first one has the header
        // timeout from the moment the connection opened; a later one has
        // the keep-alive to begin and the header timeout from its first byte.
        let began = if first {
            reader.get_mut().until(opened + site.limits.header_timeout);
            // A connection that never sends a byte, such as one a browser
            // opened ahead of need, closes without an answer.
            match reader.fill_buf() {
                Ok(bytes) if !bytes.is_empty() => {}
                _ => break,
            }
            opened
        } else {
            let idle = Idle::new(control, &stream);
            if idle.is_none() {
                break;
            }
            reader
                .get_mut()
                .until(Instant::now() + site.limits.keep_alive);
            match reader.fill_buf() {
                Ok(bytes) if !bytes.is_empty() => {}
                _ => break,
            }
            drop(idle);
            let now = Instant::now();
            reader.get_mut().until(now + site.limits.header_timeout);
            now
        };
        let head = match read_head(&mut reader) {
            Head::Read(head) => head,
            Head::Closed => break,
            Head::TooLarge => {
                let refusal =
                    Response::text(431, "the request headers are longer than this server reads");
                respond(&mut writer, site, &refusal, None, false, Persist::Close);
                break;
            }
            Head::TimedOut => {
                let refusal = Response::text(408, "the request did not arrive in time");
                respond(&mut writer, site, &refusal, None, false, Persist::Close);
                break;
            }
            // Where a malformed request ends is unknown, so nothing after it
            // on this connection can be read as a request either.
            Head::Malformed(why) => {
                respond(
                    &mut writer,
                    site,
                    &Response::text(400, why),
                    None,
                    false,
                    Persist::Close,
                );
                break;
            }
        };
        let persist = if site.limits.keep_alive.is_zero() || !head.wants_keep_alive() {
            Persist::Close
        } else if head.version == "HTTP/1.0" {
            Persist::KeepOld
        } else {
            Persist::Keep
        };
        reader
            .get_mut()
            .until(Instant::now() + site.limits.body_timeout);
        let (response, mut file, persist, request) = match read_body(&mut reader, &head.headers) {
            Ok(body) => {
                let (response, file, request) = answer(&head, body, peer.ip(), site, control);
                (response, file, persist, Some(request))
            }
            // A body that was not read leaves the connection mid-request, so
            // it cannot carry another.
            Err(refusal) => (refusal, None, Persist::Close, None),
        };
        // A handler that has just retired or wedged stops this process from
        // taking more before its answer goes out, so the client's next
        // request lands on a process that will answer it.
        if let Some(handler) = &site.handler {
            match handler.status() {
                Status::Retiring => control.begin(Exit::Recycled),
                Status::Wedged => control.begin(Exit::Wedged),
                Status::Ready | Status::Saturated => {}
            }
        }
        let persist = if control.draining() {
            Persist::Close
        } else {
            persist
        };
        let head_only = head.method == "HEAD";
        let bytes = match &file {
            Some(file) => file.len,
            None => response.body.len() as u64,
        };
        let written = respond(
            &mut writer,
            site,
            &response,
            file.as_mut(),
            head_only,
            persist,
        );
        if let Some(log) = &site.log {
            let path = access_path(&head.target);
            log.access(&Access {
                client: request
                    .as_ref()
                    .and_then(|request| request.client)
                    .or(Some(peer.ip())),
                method: &head.method,
                path,
                status: response.status,
                bytes: if head_only { 0 } else { bytes },
                took: began.elapsed(),
            });
        }
        if !written || persist == Persist::Close {
            break;
        }
        first = false;
    }
    http::close(&stream);
}

/// The part of a request target an access line records: the path, with the
/// query and the fragment left off.
fn access_path(target: &str) -> &str {
    target.split(['?', '#']).next().unwrap_or_default()
}

/// Write a response against the write deadline. `false` when it could not
/// be written whole.
fn respond(
    writer: &mut Timed,
    site: &Site,
    response: &Response,
    file: Option<&mut FileBody>,
    head_only: bool,
    persist: Persist,
) -> bool {
    writer.until(Instant::now() + site.limits.write_timeout);
    write_response(writer, response, file, head_only, persist).is_ok()
}

/// The response to one request, the file that is its body when a file is,
/// and the request as a handler saw it.
fn answer(
    head: &RequestHead,
    body: String,
    peer: IpAddr,
    site: &Site,
    control: &Control,
) -> (Response, Option<FileBody>, Request) {
    let mut headers = head.headers.clone();
    let (client, secure) = site.trust.resolve(Some(peer), &mut headers);

    // The fragment comes off first, so a target carrying both does not leave
    // `#top` on the end of the query. A browser keeps the fragment to itself,
    // but a link followed by hand or by a tool sends it.
    let (target, _fragment) = head.target.split_once('#').unwrap_or((&head.target, ""));
    let (target, query) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method: head.method.clone(),
        path: String::new(),
        query: query.to_string(),
        headers,
        body,
        secure,
        client,
    };
    let head_only = head.method == "HEAD";
    let reading = head.method == "GET" || head_only;

    if let Some(prefix) = &site.health
        && let Some(response) = health(prefix, target, reading, site, control.draining())
    {
        return (response, None, request);
    }

    // A method the directory has no answer for reaches a handler, and stops
    // here when there is none: a form posts to something that renders it,
    // and a directory of files renders nothing.
    if !reading && site.handler.is_none() {
        return (method_not_allowed(), None, request);
    }

    // The traversal guard comes first and a handler never sees past it: a
    // path outside the site is refused whatever would have answered it.
    let Some(relative) = site_path(&site.base, target) else {
        return (Response::text(404, "not found"), None, request);
    };
    request.path = format!("/{relative}");
    let found = find_file(&site.root, &relative);

    if let Some(handler) = &site.handler
        && (!reading || matches!(found, Found::Document(_) | Found::Nothing))
    {
        if let Some(response) = handler.handle(&request) {
            return (response, None, request);
        }
        // The handler is the only thing that answers a method the directory
        // does not, so a request it passed on has nowhere else to go.
        if !reading {
            return (method_not_allowed(), None, request);
        }
    }

    // A file is sent from disk as it is written, never read whole first: the
    // wasm module alone is megabytes, and a server holding one copy per
    // connection is a server a few hundred visitors can run out of memory.
    let (status, path) = match found {
        Found::File(file) | Found::Document(file) => (200, file),
        // The status a static host sends for a path it has no file for. The
        // document is the app shell, so the page still loads and resolves
        // the path itself; sending 200 here would hide from a browser what
        // it will be told in production.
        Found::Nothing => (404, shell_for(&site.root, &relative)),
    };
    let Ok(file) = FileBody::open(&path) else {
        return (Response::text(404, "not found"), None, request);
    };
    let mut response = Response::new(status, content_type(&path), Vec::new());
    if status == 200 {
        let cache = if is_hashed(&path) {
            IMMUTABLE
        } else {
            REVALIDATE
        };
        response = response.with_header("Cache-Control", cache);
    }
    (response, Some(file), request)
}

/// The answer to a health endpoint, when `target` names one.
fn health(
    prefix: &str,
    target: &str,
    reading: bool,
    site: &Site,
    draining: bool,
) -> Option<Response> {
    let rest = target.strip_prefix(prefix)?;
    let live = match rest {
        "/healthz" => true,
        "/readyz" => false,
        _ => return None,
    };
    if !reading {
        return Some(method_not_allowed());
    }
    let response = if live {
        Response::text(200, "ok")
    } else {
        // Not ready while stopping, and while a request now would be
        // turned away, so a balancer sends it elsewhere.
        let status = site
            .handler
            .as_ref()
            .map(|handler| handler.status())
            .unwrap_or(Status::Ready);
        match status {
            Status::Ready if !draining => Response::text(200, "ready"),
            _ => Response::text(503, "not ready"),
        }
    };
    Some(response)
}

/// The shell that answers for `relative`, which has no file of its own.
///
/// A site emitted in several languages has one shell per language, under the
/// tree it belongs to. A path inside such a tree is answered by that tree's
/// shell, so a visitor asking in German is not handed the English app.
fn shell_for(root: &Path, relative: &str) -> PathBuf {
    let first = relative.split('/').next().unwrap_or_default();
    let in_tree = root.join(first).join(lumen_web::NOT_FOUND_FILE);
    if !first.is_empty() && in_tree.is_file() {
        return in_tree;
    }
    root.join(lumen_web::NOT_FOUND_FILE)
}

/// The answer to a method nothing here has an answer for.
fn method_not_allowed() -> Response {
    Response::text(405, "this address answers GET and HEAD").with_header("Allow", "GET, HEAD")
}

/// What a request path names inside the site.
enum Found {
    /// A file that is not a document, which the directory always answers.
    File(PathBuf),
    /// A document, which a handler answers first when there is one.
    Document(PathBuf),
    /// Nothing, so the app shell answers.
    Nothing,
}

/// The part of a request path that names something inside the site: decoded,
/// with the base path taken off and no leading slash.
///
/// `None` means the path is outside the site, either because it does not start
/// with the base path or because it climbs out of the directory.
fn site_path(base: &str, path: &str) -> Option<String> {
    let Some(relative) = path.strip_prefix(base) else {
        // The base path itself, without its trailing slash, is the site.
        return (format!("{path}/") == base).then(String::new);
    };
    let relative = decode(relative);
    let mut parts: Vec<String> = Vec::new();
    for component in Path::new(&relative).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            // A request may not climb out of the site.
            Component::ParentDir => return None,
            _ => {}
        }
    }
    Some(parts.join("/"))
}

/// The file a site-relative path names.
fn find_file(root: &Path, relative: &str) -> Found {
    let mut file = root.to_path_buf();
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        file.push(part);
    }
    // A directory, the site root included, is served by its own document.
    if file.is_dir() {
        file.push("index.html");
    }
    // The spec file is the server's input, not the browser's: it names the
    // app's policy, which is nobody else's business.
    if !file.is_file() || is_spec_file(root, &file) {
        return Found::Nothing;
    }
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => Found::Document(file),
        _ => Found::File(file),
    }
}

/// Whether `file` is the site's spec file, under whatever name reached it.
///
/// A case-insensitive file system opens `LUMEN.SITE.JSON` as the spec file,
/// and Windows opens it by its short name or with a trailing dot too, so the
/// name a request used is not enough to tell. The file is compared with the
/// spec file itself.
fn is_spec_file(root: &Path, file: &Path) -> bool {
    let named = file.parent() == Some(root)
        && file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(lumen_ssr::SERVER_SPEC_FILE));
    named || same_file(file, &root.join(lumen_ssr::SERVER_SPEC_FILE))
}

/// Whether two paths name one file.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether two paths name one file. Windows resolves a path to the file's
/// own full name, in the case it was created with, which is what makes two
/// names of one file compare equal.
#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Percent-decoding, enough for a path a browser sends.
fn decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A base path with the slashes it needs: one at each end.
fn normalize_base(base: &str) -> String {
    let trimmed = base.trim().trim_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("/{trimmed}/")
    }
}

#[cfg(test)]
mod tests;
