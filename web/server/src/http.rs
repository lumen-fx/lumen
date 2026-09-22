//! Reading a request off a connection and writing a response back.
//!
//! Just enough HTTP/1.1 for a site behind a browser or a reverse proxy: a
//! request line, headers, a body with a `Content-Length`, and persistent
//! connections. Every read and write runs against a deadline, so a client
//! that stops sending, or sends one byte at a time, holds its connection for
//! as long as the deadline allows and no longer.

use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::time::http_date;

/// Longest request line and header block accepted, which is well past any
/// URL a browser sends and short enough that a stuck client cannot grow the
/// buffer.
pub(crate) const MAX_HEAD: usize = 16 * 1024;

/// Longest request body accepted. A form a page posts fits many times over,
/// and a client that claims more is refused before a byte of it is read.
pub(crate) const MAX_BODY: usize = 1024 * 1024;

/// The content type a document is served as.
pub(crate) const HTML: &str = "text/html; charset=utf-8";

/// The content type the server's own messages are served as.
pub(crate) const TEXT: &str = "text/plain; charset=utf-8";

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
    /// The headers, in the order they arrived. `X-Forwarded-*` and
    /// `Forwarded` are here only when they came from a proxy the server
    /// trusts.
    pub headers: Vec<(String, String)>,
    /// The body, empty when there was none.
    pub body: String,
    /// Whether the visitor's side of the request arrived over TLS, as a
    /// trusted proxy reports it.
    pub secure: bool,
    /// The visitor's address: the peer, or what a trusted proxy says it
    /// forwarded for.
    pub client: Option<IpAddr>,
}

/// What the server sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Headers to send. `Content-Length` and `Connection` are the server's,
    /// a `Content-Type` is added when there is none, and a `Cache-Control`
    /// is added when there is none: `no-store`, since a response a handler
    /// wrote is written for the request that asked.
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

    /// Add a header.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The value of a header, by name.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// The value of a header, by name.
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// A connection's socket, read from and written to against a deadline.
///
/// The deadline is for the whole of what is being read or written, not for
/// each call: a timeout set on the socket alone lets a client that sends a
/// byte just inside it keep a connection forever.
pub(crate) struct Timed {
    stream: TcpStream,
    deadline: Option<Instant>,
}

impl Timed {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            deadline: None,
        }
    }

    /// Give whatever comes next until `deadline`.
    pub(crate) fn until(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }

    /// How long is left, or the error that says there is nothing left.
    fn left(&self) -> io::Result<Option<Duration>> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline passed"));
        }
        Ok(Some(deadline - now))
    }
}

/// Which way a socket timeout ran out reads differently per platform.
fn timed_out(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, "deadline passed")
        }
        _ => error,
    }
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.stream.set_read_timeout(left)?;
        self.stream.read(buf).map_err(timed_out)
    }
}

impl Write for Timed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.stream.set_write_timeout(left)?;
        self.stream.write(buf).map_err(timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// The request line and the headers.
#[derive(Debug)]
pub(crate) struct RequestHead {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) version: String,
    pub(crate) headers: Vec<(String, String)>,
}

impl RequestHead {
    /// Whether the client asked to keep the connection for another request.
    pub(crate) fn wants_keep_alive(&self) -> bool {
        let connection = header(&self.headers, "connection")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let says = |word: &str| connection.split(',').any(|part| part.trim() == word);
        if self.version == "HTTP/1.0" {
            says("keep-alive")
        } else {
            !says("close")
        }
    }
}

/// How reading a request head went.
#[derive(Debug)]
pub(crate) enum Head {
    /// A head the server can answer.
    Read(RequestHead),
    /// The client sent nothing, or went away part way through.
    Closed,
    /// The head is longer than [`MAX_HEAD`], so it was not read to the end.
    TooLarge,
    /// The head did not arrive whole before its deadline.
    TimedOut,
}

/// Read the request line and the headers, up to [`MAX_HEAD`] bytes of them.
///
/// The cap is what keeps a client from growing the buffer: the reader stops
/// at it, and a head that has not ended by then is refused rather than
/// answered from the part that arrived.
pub(crate) fn read_head<R: BufRead>(reader: &mut R) -> Head {
    match read_head_inner(reader) {
        Ok(head) => head,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Head::TimedOut,
        Err(_) => Head::Closed,
    }
}

fn read_head_inner<R: BufRead>(reader: &mut R) -> io::Result<Head> {
    let mut limited = reader.by_ref().take(MAX_HEAD as u64);
    let mut line = String::new();
    // A blank line before the request line is tolerated, as RFC 9112 asks:
    // some clients send one after a body.
    while line.trim().is_empty() {
        line.clear();
        if limited.read_line(&mut line)? == 0 {
            return Ok(Head::Closed);
        }
        if !line.ends_with('\n') {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();
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
        version,
        headers,
    }))
}

/// Read the body the headers say is coming, and refuse one this server will
/// not hold.
pub(crate) fn read_body<R: BufRead>(
    reader: &mut R,
    headers: &[(String, String)],
) -> Result<String, Response> {
    if header(headers, "transfer-encoding").is_some() {
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
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Err(Response::text(
            408,
            "the request body did not arrive in time",
        )),
        Err(_) => Err(Response::text(400, "the request body ended early")),
    }
}

/// The reason phrase that goes with a status.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        410 => "Gone",
        411 => "Length Required",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// How a response ends its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Persist {
    /// The connection stays open for the next request.
    Keep,
    /// The connection stays open, and an HTTP/1.0 client has to be told so.
    KeepOld,
    /// The connection closes after this response.
    Close,
}

/// Write `response`, framed, and say whether the connection stays open.
pub(crate) fn write_response<W: Write>(
    out: &mut W,
    response: &Response,
    head_only: bool,
    persist: Persist,
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    if header(&response.headers, "content-type").is_none() {
        head.push_str(&format!("Content-Type: {TEXT}\r\n"));
    }
    if header(&response.headers, "cache-control").is_none() {
        head.push_str("Cache-Control: no-store\r\n");
    }
    for (name, value) in &response.headers {
        // Framing is this server's to set, so a handler's copy of it is left
        // out rather than sent twice. A value that would end the header and
        // start another is dropped the same way.
        if ["content-length", "connection", "transfer-encoding", "date"]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
            || value.contains(['\r', '\n'])
            || name.contains(['\r', '\n', ':'])
        {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("X-Content-Type-Options: nosniff\r\n");
    head.push_str(&format!(
        "Date: {}\r\n",
        http_date(std::time::SystemTime::now())
    ));
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    match persist {
        Persist::Keep => {}
        Persist::KeepOld => head.push_str("Connection: keep-alive\r\n"),
        Persist::Close => head.push_str("Connection: close\r\n"),
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    if !head_only {
        out.write_all(&response.body)?;
    }
    out.flush()
}

/// Close a connection without throwing away the response just written.
///
/// Windows, and Linux with unread bytes in the receive queue, answer a close
/// with a reset, and a peer reading the response then sees the connection
/// fail instead of ending. Closing the writing side first and reading what
/// the client still sends avoids that, for a short while and a few bytes at
/// most, so a client that keeps sending cannot hold the connection open.
pub(crate) fn close(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let Ok(mut reader) = stream.try_clone() else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut left = 256 * 1024usize;
    let mut rest = [0u8; 4096];
    while left > 0 {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let _ = reader.set_read_timeout(Some(deadline - now));
        match reader.read(&mut rest) {
            Ok(n) if n > 0 => left = left.saturating_sub(n),
            _ => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    fn head(text: &str) -> Head {
        read_head(&mut BufReader::new(text.as_bytes()))
    }

    #[test]
    fn a_head_is_read_to_its_blank_line() {
        let Head::Read(read) = head("GET /a?b HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\nrest")
        else {
            panic!("the head was not read");
        };
        assert_eq!(read.method, "GET");
        assert_eq!(read.target, "/a?b");
        assert_eq!(read.version, "HTTP/1.1");
        assert_eq!(header(&read.headers, "accept"), Some("*/*"));
    }

    #[test]
    fn a_head_with_no_end_is_closed_or_too_large() {
        assert!(matches!(
            head("GET / HTTP/1.1\r\nHost: x\r\n"),
            Head::Closed
        ));
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "y".repeat(MAX_HEAD));
        assert!(matches!(head(&long), Head::TooLarge));
        assert!(matches!(head(""), Head::Closed));
    }

    #[test]
    fn keep_alive_follows_the_version_and_the_connection_header() {
        let asked = |version: &str, connection: Option<&str>| RequestHead {
            method: "GET".to_string(),
            target: "/".to_string(),
            version: version.to_string(),
            headers: connection
                .map(|value| vec![("Connection".to_string(), value.to_string())])
                .unwrap_or_default(),
        };
        assert!(asked("HTTP/1.1", None).wants_keep_alive());
        assert!(!asked("HTTP/1.1", Some("close")).wants_keep_alive());
        assert!(!asked("HTTP/1.0", None).wants_keep_alive());
        assert!(asked("HTTP/1.0", Some("Keep-Alive")).wants_keep_alive());
    }

    #[test]
    fn a_response_is_framed_by_the_server_whatever_the_handler_says() {
        let response = Response::text(302, "")
            .with_header("Location", "/elsewhere")
            .with_header("Content-Length", "99")
            .with_header("X-Split", "a\r\nSet-Cookie: stolen=1");
        let mut out = Vec::new();
        write_response(&mut out, &response, false, Persist::Close).expect("writing to memory");
        let text = String::from_utf8(out).expect("a head is text");
        assert!(text.starts_with("HTTP/1.1 302 Found\r\n"), "{text}");
        assert!(text.contains("Location: /elsewhere\r\n"), "{text}");
        assert!(text.contains("Content-Length: 0\r\n"), "{text}");
        assert!(!text.contains("99"), "{text}");
        assert!(!text.contains("stolen"), "{text}");
        assert!(text.contains("Cache-Control: no-store\r\n"), "{text}");
        assert!(text.contains("Connection: close\r\n"), "{text}");
    }

    #[test]
    fn a_handlers_own_cache_control_is_the_one_sent() {
        let response = Response::text(200, "ok").with_header("Cache-Control", "max-age=60");
        let mut out = Vec::new();
        write_response(&mut out, &response, true, Persist::Keep).expect("writing to memory");
        let text = String::from_utf8(out).expect("a head is text");
        assert!(text.contains("Cache-Control: max-age=60\r\n"), "{text}");
        assert!(!text.contains("no-store"), "{text}");
        assert!(!text.contains("Connection:"), "{text}");
        // A HEAD answer carries the length of the body it leaves out.
        assert!(text.contains("Content-Length: 2\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\n"), "{text}");
    }

    #[test]
    fn a_body_past_the_bound_is_refused() {
        let headers = vec![("Content-Length".to_string(), (MAX_BODY + 1).to_string())];
        let refused =
            read_body(&mut BufReader::new(&b""[..]), &headers).expect_err("a body past the bound");
        assert_eq!(refused.status, 413);
        let chunked = vec![("Transfer-Encoding".to_string(), "chunked".to_string())];
        let refused =
            read_body(&mut BufReader::new(&b""[..]), &chunked).expect_err("a chunked body");
        assert_eq!(refused.status, 411);
    }
}
