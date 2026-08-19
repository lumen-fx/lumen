//! Serving an emitted site over HTTP, so you can open it in a browser.
//!
//! A browser will not run a Lumen site off the filesystem: a module script
//! and a streamed WebAssembly instantiation both need a real origin and a
//! real content type. This is that origin, and nothing more. It serves one
//! developer's browser from one directory, on the loopback address unless it
//! is told otherwise.
//!
//! It answers the way a static host answers, because that is what the build
//! is aimed at: a path with no file behind it gets the app shell, with the
//! status a static host would send.
//!
//! A [`RequestHandler`] changes what a document is. With one installed, the
//! server reads the request whole and asks the handler for every page, and
//! serves the file on disk only when the handler has nothing to say. Files
//! that are not documents never reach it: a stylesheet, an artifact and the
//! wasm module are answered straight from disk on the connection's own thread,
//! so they never wait behind a page being rendered.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Longest request line and header block accepted, which is well past any
/// URL a browser sends and short enough that a stuck client cannot grow the
/// buffer.
const MAX_HEAD: usize = 16 * 1024;

/// Longest request body accepted. A form a page posts fits many times over,
/// and a client that claims more is refused before a byte of it is read.
const MAX_BODY: usize = 1024 * 1024;

/// The address the server listens on when none is named.
pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "xml" => "application/xml",
        "txt" => TEXT,
        _ => "application/octet-stream",
    }
}

/// The content type a document is served as.
const HTML: &str = "text/html; charset=utf-8";

/// The content type the server's own messages are served as.
const TEXT: &str = "text/plain; charset=utf-8";

/// A request the server has read, as a handler sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The method, as it arrived.
    pub method: String,
    /// The path asked for, decoded, relative to the site's base path and
    /// starting with a slash. A site served under `/docs` sees `/user/42`
    /// for a request for `/docs/user/42`.
    pub path: String,
    /// The query string, without the leading `?`.
    pub query: String,
    /// The headers, in the order they arrived.
    pub headers: Vec<(String, String)>,
    /// The body, empty when there was none.
    pub body: String,
}

/// What the server sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Headers to send. `Content-Length`, `Cache-Control` and `Connection`
    /// are the server's, and a `Content-Type` is added when there is none.
    pub headers: Vec<(String, String)>,
    /// The body, sent for every method but `HEAD`.
    pub body: Vec<u8>,
}

impl Response {
    /// A response carrying `body` as `content_type`.
    pub fn new(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_string(), content_type.to_string())],
            body,
        }
    }

    /// A plain-text response, which is what a server says things with.
    pub fn text(status: u16, message: &str) -> Self {
        Self::new(status, TEXT, message.as_bytes().to_vec())
    }
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
}

/// A running site server.
pub struct Server {
    listener: TcpListener,
    root: PathBuf,
    base: String,
    handler: Option<Arc<dyn RequestHandler>>,
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
                "cannot listen on {host} port {port}: {e}. Pass --port for another port, --port 0 \
                 for any free one, or --host for an address this machine has."
            )
        })?;
        Ok(Self {
            listener,
            root: root.to_path_buf(),
            base: normalize_base(base),
            handler: None,
        })
    }

    /// Answer pages with `handler` rather than with the documents on disk.
    pub fn with_handler(mut self, handler: Arc<dyn RequestHandler>) -> Self {
        self.handler = Some(handler);
        self
    }

    /// The address the server is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from((LOOPBACK, 0)))
    }

    /// The URL the site opens at, base path included.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr(), self.base)
    }

    /// Answer requests until the process is stopped.
    pub fn run(&self) {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            let root = self.root.clone();
            let base = self.base.clone();
            let handler = self.handler.clone();
            // A browser opens several connections at once; a thread each
            // keeps one slow fetch, or one page being rendered, from holding
            // up the rest of what the page needs.
            std::thread::spawn(move || {
                let _ = answer(stream, &root, &base, handler.as_deref());
            });
        }
    }
}

/// Read one request and write one response.
fn answer(
    mut stream: TcpStream,
    root: &Path,
    base: &str,
    handler: Option<&dyn RequestHandler>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let head = match read_head(&mut reader)? {
        Head::Read(head) => head,
        Head::Closed => return Ok(()),
        Head::TooLarge => {
            return write_response(
                &mut stream,
                Response::text(431, "the request headers are longer than this server reads"),
                false,
            );
        }
    };
    let head_only = head.method == "HEAD";
    let body = match read_body(&mut reader, &head.headers) {
        Ok(body) => body,
        Err(refusal) => return write_response(&mut stream, refusal, head_only),
    };

    // A method the directory has no answer for reaches a handler, and stops
    // here when there is none: a form posts to something that renders it,
    // and a directory of files renders nothing.
    let reading = head.method == "GET" || head_only;
    if !reading && handler.is_none() {
        return write_response(&mut stream, method_not_allowed(), head_only);
    }

    // The fragment comes off first, so a target carrying both does not leave
    // `#top` on the end of the query. A browser keeps the fragment to itself,
    // but a link followed by hand or by a tool sends it.
    let (target, _fragment) = head.target.split_once('#').unwrap_or((&head.target, ""));
    let (target, query) = target.split_once('?').unwrap_or((target, ""));
    // The traversal guard comes first and a handler never sees past it: a
    // path outside the site is refused whatever would have answered it.
    let Some(relative) = site_path(base, target) else {
        return write_response(&mut stream, Response::text(404, "not found"), head_only);
    };
    let found = find_file(root, &relative);

    if let Some(handler) = handler
        && (!reading || matches!(found, Found::Document(_) | Found::Nothing))
    {
        let request = Request {
            method: head.method.clone(),
            path: format!("/{relative}"),
            query: query.to_string(),
            headers: head.headers.clone(),
            body,
        };
        if let Some(response) = handler.handle(&request) {
            return write_response(&mut stream, response, head_only);
        }
        // The handler is the only thing that answers a method the directory
        // does not, so a request it passed on has nowhere else to go.
        if !reading {
            return write_response(&mut stream, method_not_allowed(), head_only);
        }
    }

    let response = match found {
        Found::File(file) | Found::Document(file) => match std::fs::read(&file) {
            Ok(bytes) => Response::new(200, content_type(&file), bytes),
            Err(_) => Response::text(404, "not found"),
        },
        // The status a static host sends for a path it has no file for. The
        // document is the app shell, so the page still loads and resolves
        // the path itself; sending 200 here would hide from a browser what
        // it will be told in production.
        Found::Nothing => {
            let shell = root.join(lumen_web::NOT_FOUND_FILE);
            match std::fs::read(&shell) {
                Ok(bytes) => Response::new(404, content_type(&shell), bytes),
                Err(_) => Response::text(404, "not found"),
            }
        }
    };
    write_response(&mut stream, response, head_only)
}

/// The answer to a method nothing here has an answer for.
fn method_not_allowed() -> Response {
    Response::text(
        405,
        "this server serves a directory, which answers GET and HEAD",
    )
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
    if !file.is_file() {
        return Found::Nothing;
    }
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => Found::Document(file),
        _ => Found::File(file),
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

/// The request line and the headers.
struct RequestHead {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

/// How reading a request head went.
enum Head {
    /// A head the server can answer.
    Read(RequestHead),
    /// The client sent nothing, or went away part way through.
    Closed,
    /// The head is longer than [`MAX_HEAD`], so it was not read to the end.
    TooLarge,
}

/// Read the request line and the headers, up to [`MAX_HEAD`] bytes of them.
///
/// The cap is what keeps a client from growing the buffer: the reader stops
/// at it, and a head that has not ended by then is refused rather than
/// answered from the part that arrived.
fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Head> {
    let mut limited = reader.by_ref().take(MAX_HEAD as u64);
    let mut line = String::new();
    if limited.read_line(&mut line)? == 0 {
        return Ok(Head::Closed);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    let ended = loop {
        let mut header = String::new();
        if limited.read_line(&mut header)? == 0 {
            break false;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break true;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    };
    if !ended {
        return Ok(if limited.limit() == 0 {
            Head::TooLarge
        } else {
            Head::Closed
        });
    }
    Ok(Head::Read(RequestHead {
        method,
        target,
        headers,
    }))
}

/// Read the body the headers say is coming, and refuse one this server will
/// not hold.
fn read_body(
    reader: &mut BufReader<TcpStream>,
    headers: &[(String, String)],
) -> Result<String, Response> {
    if header(headers, "transfer-encoding").is_some() {
        // Reassembling a chunked body is a production server's job, and this
        // one is in front of one developer's browser.
        return Err(Response::text(
            411,
            "send a body with a Content-Length; this server does not read a chunked one",
        ));
    }
    let Some(length) = header(headers, "content-length") else {
        return Ok(String::new());
    };
    let Ok(length) = length.trim().parse::<usize>() else {
        return Err(Response::text(400, "Content-Length is not a length"));
    };
    if length > MAX_BODY {
        return Err(Response::text(
            413,
            "the request body is larger than this server reads",
        ));
    }
    let mut body = vec![0u8; length];
    match reader.read_exact(&mut body) {
        Ok(()) => Ok(String::from_utf8_lossy(&body).into_owned()),
        Err(_) => Err(Response::text(400, "the request body ended early")),
    }
}

/// The value of a header, by name.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn write_response(
    stream: &mut TcpStream,
    response: Response,
    head_only: bool,
) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    if header(&response.headers, "content-type").is_none() {
        head.push_str(&format!("Content-Type: {TEXT}\r\n"));
    }
    for (name, value) in &response.headers {
        // The three below are this server's to set, so a handler's copy of
        // them is left out rather than sent twice.
        if ["content-length", "connection", "cache-control"]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.body.len()
    ));
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(&response.body)?;
    }
    stream.flush()?;
    // Say the response is over by closing this side, rather than by dropping
    // the socket. Windows answers a close that still has unread bytes in its
    // receive queue with a reset, and a peer reading the response then sees
    // the connection fail instead of ending.
    let _ = stream.shutdown(Shutdown::Write);
    let mut rest = [0u8; 1024];
    while matches!(stream.read(&mut rest), Ok(n) if n > 0) {}
    Ok(())
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
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_wasm_runtime_is_served_as_wasm() {
        assert_eq!(
            content_type(Path::new("a/lumen-web.wasm")),
            "application/wasm"
        );
        assert_eq!(
            content_type(Path::new("lumen-web.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("lumen.web.json")),
            "application/json"
        );
        assert_eq!(content_type(Path::new("index.html")), HTML);
        // The compiled app and its bytecode are bytes, not text.
        assert_eq!(
            content_type(Path::new("app.lmna")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type(Path::new("app.cdlb")),
            "application/octet-stream"
        );
    }

    #[test]
    fn a_request_cannot_climb_out_of_the_site() {
        assert_eq!(site_path("/", "/../../etc/passwd"), None);
        assert_eq!(site_path("/docs/", "/other"), None);
        assert_eq!(site_path("/docs/", "/docs"), Some(String::new()));
        assert_eq!(
            site_path("/docs/", "/docs/user/42"),
            Some("user/42".to_string())
        );
    }

    #[test]
    fn a_percent_encoded_path_is_read_back() {
        assert_eq!(decode("/a%20b/c"), "/a b/c");
        assert_eq!(decode("/plain"), "/plain");
    }

    /// A handler that answers with what it was asked, and that a test can
    /// hold part way through to see what happens meanwhile.
    struct Probe {
        /// Told the request as it arrives.
        seen: Sender<Request>,
        /// Held here until the test lets go, when there is one.
        hold: Option<Mutex<Receiver<()>>>,
        /// Answers nothing, so the directory answers instead.
        declines: bool,
    }

    impl RequestHandler for Probe {
        fn handle(&self, request: &Request) -> Option<Response> {
            let _ = self.seen.send(request.clone());
            if let Some(hold) = &self.hold
                && let Ok(hold) = hold.lock()
            {
                let _ = hold.recv();
            }
            if self.declines {
                return None;
            }
            Some(Response::new(
                200,
                HTML,
                format!(
                    "<!doctype html><title>{} {}</title>",
                    request.method, request.path
                )
                .into_bytes(),
            ))
        }
    }

    fn probe() -> (Arc<Probe>, Receiver<Request>) {
        let (seen, requests) = channel();
        (
            Arc::new(Probe {
                seen,
                hold: None,
                declines: false,
            }),
            requests,
        )
    }

    /// A directory of this case's own, with the site inside it: what sits
    /// beside the site is what a traversal would be reaching for.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-serve-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("site")).expect("creating the site directory");
        dir
    }

    /// An emitted site: one document and one stylesheet.
    fn site(name: &str) -> PathBuf {
        let dir = scratch(name).join("site");
        std::fs::write(dir.join("index.html"), "<!doctype html>built")
            .expect("writing the document");
        std::fs::write(dir.join("styles.css"), "body{}").expect("writing the stylesheet");
        dir
    }

    /// Start a server on a free port and answer on it until the test ends.
    fn serve(root: &Path, handler: Option<Arc<dyn RequestHandler>>) -> SocketAddr {
        let mut server = Server::bind(root, "/", LOOPBACK, 0).expect("a free port");
        if let Some(handler) = handler {
            server = server.with_handler(handler);
        }
        let addr = server.addr();
        std::thread::spawn(move || server.run());
        addr
    }

    /// Send `request` verbatim and read the whole answer back.
    fn ask(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connecting to the server");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a read timeout");
        stream
            .write_all(request.as_bytes())
            .expect("sending the request");
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).expect("reading the answer");
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// A plain GET of `path`.
    fn get(addr: SocketAddr, path: &str) -> String {
        ask(addr, &format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n"))
    }

    fn status(answer: &str) -> u16 {
        answer
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn a_traversal_is_refused_even_with_a_handler_installed() {
        let root = site("traversal");
        let secret = root
            .parent()
            .expect("the site sits inside the case directory")
            .join("secret.txt");
        std::fs::write(&secret, "not yours").expect("writing the file beside the site");
        let (handler, requests) = probe();
        let addr = serve(&root, Some(handler));

        let answer = get(addr, "/../secret.txt");
        assert_eq!(status(&answer), 404, "{answer}");
        assert!(!answer.contains("not yours"), "{answer}");
        assert!(
            requests.try_recv().is_err(),
            "a path outside the site reached the handler"
        );
    }

    #[test]
    fn a_head_longer_than_the_cap_is_refused() {
        let addr = serve(&site("long-head"), None);
        let padding = "x".repeat(MAX_HEAD);
        let answer = ask(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: test\r\nX-Long: {padding}\r\n\r\n"),
        );
        assert_eq!(status(&answer), 431, "{answer}");
    }

    #[test]
    fn a_body_past_the_bound_is_refused_before_it_is_read() {
        let (handler, requests) = probe();
        let addr = serve(&site("long-body"), Some(handler));
        let answer = ask(
            addr,
            &format!(
                "POST / HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            ),
        );
        assert_eq!(status(&answer), 413, "{answer}");
        assert!(
            requests.try_recv().is_err(),
            "a body past the bound reached the handler"
        );
    }

    #[test]
    fn a_post_reaches_a_handler_and_stops_at_a_directory() {
        let root = site("post");
        let (handler, requests) = probe();
        let addr = serve(&root, Some(handler));
        let answer = ask(
            addr,
            "POST /submit HTTP/1.1\r\nHost: test\r\nContent-Length: 7\r\n\r\nname=ok",
        );
        assert_eq!(status(&answer), 200, "{answer}");
        let seen = requests.recv().expect("the handler was asked");
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.path, "/submit");
        assert_eq!(seen.body, "name=ok");

        let alone = serve(&root, None);
        let refused = ask(
            alone,
            "POST /submit HTTP/1.1\r\nHost: test\r\nContent-Length: 7\r\n\r\nname=ok",
        );
        assert_eq!(status(&refused), 405, "{refused}");
    }

    #[test]
    fn the_headers_a_handler_needs_arrive_intact() {
        let (handler, requests) = probe();
        let addr = serve(&site("headers"), Some(handler));
        let answer = ask(
            addr,
            "GET /user/42?tab=posts HTTP/1.1\r\nHost: test\r\nAccept-Language: en-GB\r\nCookie: \
             session=abc\r\nX-Forwarded-Proto: https\r\n\r\n",
        );
        assert_eq!(status(&answer), 200, "{answer}");
        let seen = requests.recv().expect("the handler was asked");
        assert_eq!(seen.path, "/user/42");
        assert_eq!(seen.query, "tab=posts");
        assert_eq!(header(&seen.headers, "accept-language"), Some("en-GB"));
        assert_eq!(header(&seen.headers, "cookie"), Some("session=abc"));
        assert_eq!(header(&seen.headers, "x-forwarded-proto"), Some("https"));
    }

    #[test]
    fn a_document_comes_from_the_handler_and_a_file_comes_from_disk() {
        let (handler, _requests) = probe();
        let addr = serve(&site("split"), Some(handler));

        let page = get(addr, "/");
        assert!(page.contains("GET /"), "{page}");
        assert!(!page.contains("built"), "the file answered instead: {page}");

        let sheet = get(addr, "/styles.css");
        assert!(sheet.contains("body{}"), "{sheet}");
        assert!(sheet.contains("text/css"), "{sheet}");
    }

    #[test]
    fn a_handler_that_declines_leaves_the_document_to_the_directory() {
        let (seen, _requests) = channel();
        let handler = Arc::new(Probe {
            seen,
            hold: None,
            declines: true,
        });
        let addr = serve(&site("declines"), Some(handler));
        let page = get(addr, "/");
        assert!(page.contains("built"), "{page}");

        // A directory has no answer to a post, and the handler passed on it.
        let posted = ask(addr, "POST / HTTP/1.1\r\nHost: test\r\n\r\n");
        assert_eq!(status(&posted), 405, "{posted}");
    }

    #[test]
    fn a_file_is_served_while_a_render_is_in_flight() {
        let (seen, requests) = channel();
        let (release, held) = channel();
        let handler = Arc::new(Probe {
            seen,
            hold: Some(Mutex::new(held)),
            declines: false,
        });
        let addr = serve(&site("in-flight"), Some(handler));

        // A page that is still being rendered, and stays that way until this
        // test lets go of it.
        let page = std::thread::spawn(move || get(addr, "/"));
        requests
            .recv_timeout(Duration::from_secs(30))
            .expect("the handler was asked for the page");

        // The stylesheet answers meanwhile. It would not if a file and a
        // render shared one queue.
        let sheet = get(addr, "/styles.css");
        assert!(sheet.contains("body{}"), "{sheet}");

        let _ = release.send(());
        let page = page.join().expect("the page thread");
        assert!(page.contains("GET /"), "{page}");
    }

    #[test]
    fn a_path_with_no_file_reaches_the_handler_before_the_shell() {
        let root = site("deep");
        std::fs::write(root.join("404.html"), "<!doctype html>shell").expect("writing the shell");
        let (handler, requests) = probe();
        let addr = serve(&root, Some(handler));

        let deep = get(addr, "/user/42");
        assert_eq!(status(&deep), 200, "{deep}");
        assert!(deep.contains("/user/42"), "{deep}");
        assert_eq!(
            requests.recv().expect("the handler was asked").path,
            "/user/42"
        );

        // Without a handler the shell answers it, with the status a static
        // host sends.
        let alone = serve(&root, None);
        let shell = get(alone, "/user/42");
        assert_eq!(status(&shell), 404, "{shell}");
        assert!(shell.contains("shell"), "{shell}");
    }

    #[test]
    fn a_head_request_carries_the_length_and_no_body() {
        let addr = serve(&site("head"), None);
        let answer = ask(addr, "HEAD /styles.css HTTP/1.1\r\nHost: test\r\n\r\n");
        assert_eq!(status(&answer), 200, "{answer}");
        assert!(answer.contains("Content-Length: 6"), "{answer}");
        assert!(!answer.contains("body{}"), "{answer}");
    }

    #[test]
    fn a_handler_sets_its_own_headers_and_the_server_frames_the_body() {
        struct Redirects;
        impl RequestHandler for Redirects {
            fn handle(&self, _request: &Request) -> Option<Response> {
                Some(Response {
                    status: 302,
                    headers: vec![
                        ("Location".to_string(), "/elsewhere".to_string()),
                        // Framing is the server's, so this one is dropped.
                        ("Content-Length".to_string(), "99".to_string()),
                    ],
                    body: Vec::new(),
                })
            }
        }
        let addr = serve(&site("redirect"), Some(Arc::new(Redirects)));
        let answer = get(addr, "/");
        assert_eq!(status(&answer), 302, "{answer}");
        assert!(answer.contains("Location: /elsewhere"), "{answer}");
        assert_eq!(
            answer
                .lines()
                .filter(|line| line.starts_with("Content-Length"))
                .count(),
            1,
            "{answer}"
        );
        assert!(answer.contains("Content-Length: 0"), "{answer}");
    }

    /// The head is read through a reader that stops at the cap, and the body
    /// is read from that same reader afterwards.
    #[test]
    fn a_body_after_a_short_head_is_still_readable() {
        let (handler, requests) = probe();
        let addr = serve(&site("body"), Some(handler));
        let _ = ask(
            addr,
            "POST /note HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\n\r\nhello",
        );
        assert_eq!(
            requests.recv().expect("the handler was asked").body,
            "hello"
        );
    }

    #[test]
    fn a_chunked_body_is_refused_rather_than_half_read() {
        let (handler, _requests) = probe();
        let addr = serve(&site("chunked"), Some(handler));
        let answer = ask(
            addr,
            "POST /submit HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: \
             chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        );
        assert_eq!(status(&answer), 411, "{answer}");
    }
}
