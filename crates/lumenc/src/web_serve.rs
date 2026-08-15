//! Serving an emitted site over HTTP, so you can open it in a browser.
//!
//! A browser will not run a Lumen site off the filesystem: a module script
//! and a streamed WebAssembly instantiation both need a real origin and a
//! real content type. This is that origin, and nothing more. It serves one
//! developer's browser from one directory, on the loopback address.
//!
//! It answers the way a static host answers, because that is what the build
//! is aimed at: a path with no file behind it gets the app shell, with the
//! status a static host would send.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

/// Longest request line and header block accepted, which is well past any
/// URL a browser sends and short enough that a stuck client cannot grow the
/// buffer.
const MAX_HEAD: usize = 16 * 1024;

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
        "html" => "text/html; charset=utf-8",
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
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// A running site server.
pub struct Server {
    listener: TcpListener,
    root: PathBuf,
    base: String,
}

impl Server {
    /// Take a port and hold it. `port` 0 asks the system for a free one,
    /// which is what [`Self::addr`] then reports.
    pub fn bind(root: &Path, base: &str, port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            format!(
                "cannot listen on port {port}: {e}. Pass --port to pick another, or --port 0 for \
                 any free one."
            )
        })?;
        Ok(Self {
            listener,
            root: root.to_path_buf(),
            base: normalize_base(base),
        })
    }

    /// The address the server is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)))
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
            // A browser opens several connections at once; a thread each
            // keeps one slow fetch from holding up the page.
            std::thread::spawn(move || {
                let _ = answer(stream, &root, &base);
            });
        }
    }
}

/// Read one request and write one response.
fn answer(mut stream: TcpStream, root: &Path, base: &str) -> std::io::Result<()> {
    let Some(request) = read_request_line(&stream)? else {
        return Ok(());
    };
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method != "GET" && method != "HEAD" {
        return write_response(&mut stream, 405, "text/plain; charset=utf-8", b"", false);
    }
    let head_only = method == "HEAD";
    let path = target.split(['?', '#']).next().unwrap_or("/");

    match resolve(root, base, path) {
        Resolved::File(file) => match std::fs::read(&file) {
            Ok(bytes) => write_response(&mut stream, 200, content_type(&file), &bytes, head_only),
            Err(_) => write_response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"",
                head_only,
            ),
        },
        // The status a static host sends for a path it has no file for. The
        // document is the app shell, so the page still loads and resolves
        // the path itself; sending 200 here would hide from a browser what
        // it will be told in production.
        Resolved::NotFound(shell) => {
            let body = std::fs::read(&shell).unwrap_or_else(|_| b"not found".to_vec());
            let kind = if shell.exists() {
                content_type(&shell)
            } else {
                "text/plain; charset=utf-8"
            };
            write_response(&mut stream, 404, kind, &body, head_only)
        }
        Resolved::OffSite => write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"",
            head_only,
        ),
    }
}

/// What a request path names.
enum Resolved {
    /// A file inside the site.
    File(PathBuf),
    /// Nothing, so the app shell answers.
    NotFound(PathBuf),
    /// A path outside the site's base path or outside its directory.
    OffSite,
}

fn resolve(root: &Path, base: &str, path: &str) -> Resolved {
    let Some(relative) = path.strip_prefix(base) else {
        // The base path itself, without its trailing slash, is the site.
        if format!("{path}/") == base {
            return Resolved::File(root.join("index.html"));
        }
        return Resolved::OffSite;
    };
    let relative = decode(relative);
    let mut file = root.to_path_buf();
    for component in Path::new(&relative).components() {
        match component {
            Component::Normal(part) => file.push(part),
            // A request may not climb out of the site.
            Component::ParentDir => return Resolved::OffSite,
            _ => {}
        }
    }
    // A directory, the site root included, is served by its own document.
    if file.is_dir() {
        file.push("index.html");
    }
    if file.is_file() {
        Resolved::File(file)
    } else {
        Resolved::NotFound(root.join(lumen_web::NOT_FOUND_FILE))
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

/// Read the request line and drop the headers.
fn read_request_line(stream: &TcpStream) -> std::io::Result<Option<String>> {
    let mut reader = BufReader::new(stream.try_clone()?).take(MAX_HEAD as u64);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let request = line.trim_end().to_string();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
    }
    Ok(Some(request))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
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
        assert_eq!(
            content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
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
        let root = Path::new("/tmp/site");
        assert!(matches!(
            resolve(root, "/", "/../../etc/passwd"),
            Resolved::OffSite
        ));
        assert!(matches!(
            resolve(root, "/docs/", "/other"),
            Resolved::OffSite
        ));
    }

    #[test]
    fn a_percent_encoded_path_is_read_back() {
        assert_eq!(decode("/a%20b/c"), "/a b/c");
        assert_eq!(decode("/plain"), "/plain");
    }
}
