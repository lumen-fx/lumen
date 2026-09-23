use std::io::{Read, Write};
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
fn a_name_carrying_its_hash_is_told_apart_from_one_that_does_not() {
    assert!(is_hashed(Path::new("styles.0123456789abcdef.css")));
    assert!(is_hashed(Path::new("assets/LICENSE.0123456789abcdef")));
    assert!(is_hashed(Path::new("app.fedcba9876543210.lmna")));
    assert!(!is_hashed(Path::new("lumen.web.json")));
    assert!(!is_hashed(Path::new("index.html")));
    assert!(!is_hashed(Path::new("0123456789abcdef")));
    assert!(!is_hashed(Path::new("styles.0123456789abcdeg.css")));
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
fn a_path_inside_a_language_tree_falls_back_to_that_trees_shell() {
    let root = site("locale-shell");
    std::fs::write(root.join("404.html"), "<!doctype html>root").expect("the root shell");
    std::fs::create_dir_all(root.join("de-DE")).expect("the German tree");
    std::fs::write(root.join("de-DE/404.html"), "<!doctype html>de").expect("its shell");

    assert_eq!(
        shell_for(&root, "de-DE/user/42"),
        root.join("de-DE/404.html")
    );
    // A first segment that is a page rather than a tree, and a path at the
    // site root, are both the root's to answer.
    assert_eq!(shell_for(&root, "user/42"), root.join("404.html"));
    assert_eq!(shell_for(&root, ""), root.join("404.html"));
}

#[test]
fn an_access_line_records_the_path_and_not_the_query() {
    assert_eq!(access_path("/reset?token=s3cret"), "/reset");
    assert_eq!(access_path("/a#b?c"), "/a");
    assert_eq!(access_path("/plain"), "/plain");
    assert_eq!(access_path("?only=query"), "");
}

#[test]
fn a_percent_encoded_path_is_read_back() {
    assert_eq!(decode("/a%20b/c"), "/a b/c");
    assert_eq!(decode("/plain"), "/plain");
}

/// A handler that answers with what it was asked, and that a test can hold
/// part way through to see what happens meanwhile.
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

/// A directory of this case's own, with the site inside it: what sits beside
/// the site is what a traversal would be reaching for.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-server-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("site")).expect("creating the site directory");
    dir
}

/// An emitted site: one document and one stylesheet.
fn site(name: &str) -> PathBuf {
    let dir = scratch(name).join("site");
    std::fs::write(dir.join("index.html"), "<!doctype html>built").expect("writing the document");
    std::fs::write(dir.join("styles.css"), "body{}").expect("writing the stylesheet");
    dir
}

/// Start a server on a free port and answer on it until the test ends.
fn serve(root: &Path, handler: Option<Arc<dyn RequestHandler>>) -> SocketAddr {
    serve_with(root, handler, Limits::default()).0
}

fn serve_with(
    root: &Path,
    handler: Option<Arc<dyn RequestHandler>>,
    limits: Limits,
) -> (SocketAddr, Shutdown, std::thread::JoinHandle<Exit>) {
    let mut server = Server::bind(root, "/", LOOPBACK, 0)
        .expect("a free port")
        .with_limits(limits);
    if let Some(handler) = handler {
        server = server.with_handler(handler);
    }
    let addr = server.addr();
    let stop = server.shutdown_handle();
    let running = std::thread::spawn(move || server.run());
    (addr, stop, running)
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connecting to the server");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    stream
}

/// Send `request` verbatim and read the whole answer back. The request is
/// expected to close the connection.
fn ask(addr: SocketAddr, request: &str) -> String {
    let mut stream = connect(addr);
    stream
        .write_all(request.as_bytes())
        .expect("sending the request");
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    String::from_utf8_lossy(&answer).into_owned()
}

/// A plain GET of `path`, closing the connection after it.
fn get(addr: SocketAddr, path: &str) -> String {
    ask(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"),
    )
}

fn status(answer: &str) -> u16 {
    answer
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// Read one response off an open connection, by its `Content-Length`.
fn read_one(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let n = stream.read(&mut byte).expect("reading the head");
        assert!(n > 0, "the connection closed mid-head: {:?}", head);
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.trim().parse().ok())
        .expect("a length");
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).expect("reading the body");
    format!("{head}{}", String::from_utf8_lossy(&body))
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
fn the_spec_file_stays_on_the_server() {
    let root = site("spec");
    std::fs::write(root.join(lumen_ssr::SERVER_SPEC_FILE), "{\"policy\":{}}")
        .expect("writing the spec");
    let addr = serve(&root, None);
    let answer = get(addr, &format!("/{}", lumen_ssr::SERVER_SPEC_FILE));
    assert_eq!(status(&answer), 404, "{answer}");
    assert!(!answer.contains("policy"), "{answer}");
}

#[test]
fn the_spec_file_stays_on_the_server_under_any_name_that_opens_it() {
    let root = site("spec-names");
    std::fs::write(root.join(lumen_ssr::SERVER_SPEC_FILE), "{\"policy\":{}}")
        .expect("writing the spec");
    // What a case-insensitive file system opens the spec file by. This one
    // may be case-sensitive, so the name is made to exist.
    std::fs::write(root.join("LUMEN.SITE.JSON"), "{\"policy\":{}}").expect("writing the variant");
    // Another name for the same file, which only its identity gives away.
    #[cfg(unix)]
    std::fs::hard_link(
        root.join(lumen_ssr::SERVER_SPEC_FILE),
        root.join("renamed.json"),
    )
    .expect("linking the spec");
    let addr = serve(&root, None);
    let mut names = vec!["LUMEN.SITE.JSON", "Lumen.Site.Json"];
    if cfg!(unix) {
        names.push("renamed.json");
    }
    for name in names {
        let answer = get(addr, &format!("/{name}"));
        assert_eq!(status(&answer), 404, "{name}: {answer}");
        assert!(!answer.contains("policy"), "{name}: {answer}");
    }
}

#[test]
fn a_large_file_is_sent_whole_and_a_head_request_sends_none_of_it() {
    let root = site("large");
    let bytes: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("lumen-web.wasm"), &bytes).expect("writing the module");
    let addr = serve(&root, None);

    let mut stream = connect(addr);
    stream
        .write_all(b"GET /lumen-web.wasm HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .expect("sending the request");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).expect("reading the answer");
    let split = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a head")
        + 4;
    let head = String::from_utf8_lossy(&answer[..split]);
    assert!(
        head.contains(&format!("Content-Length: {}\r\n", bytes.len())),
        "{head}"
    );
    assert!(
        head.contains("Content-Type: application/wasm\r\n"),
        "{head}"
    );
    assert!(answer[split..] == bytes[..], "the body is not the file");

    let headed = ask(
        addr,
        "HEAD /lumen-web.wasm HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    );
    assert!(
        headed.contains(&format!("Content-Length: {}\r\n", bytes.len())),
        "{headed}"
    );
    assert!(headed.ends_with("\r\n\r\n"), "a HEAD answer carried a body");
}

#[test]
fn a_hashed_file_is_kept_and_any_other_is_checked() {
    let root = site("cache");
    std::fs::write(root.join("app.0123456789abcdef.lmna"), "app").expect("writing the artifact");
    let addr = serve(&root, None);
    let hashed = get(addr, "/app.0123456789abcdef.lmna");
    assert!(
        hashed.contains("Cache-Control: public, max-age=31536000, immutable\r\n"),
        "{hashed}"
    );
    let plain = get(addr, "/styles.css");
    assert!(plain.contains("Cache-Control: no-cache\r\n"), "{plain}");
}

#[test]
fn the_health_endpoints_answer_under_their_prefix() {
    let addr = serve(&site("health"), None);
    let live = get(addr, "/_lumen/healthz");
    assert_eq!(status(&live), 200, "{live}");
    let ready = get(addr, "/_lumen/readyz");
    assert_eq!(status(&ready), 200, "{ready}");
    assert!(ready.ends_with("ready"), "{ready}");

    let root = site("health-moved");
    let server = Server::bind(&root, "/", LOOPBACK, 0)
        .expect("a free port")
        .with_health_path("/");
    let moved = server.addr();
    std::thread::spawn(move || server.run());
    assert_eq!(status(&get(moved, "/healthz")), 200);
    assert_eq!(status(&get(moved, "/_lumen/healthz")), 404);
}

/// A handler that says it cannot take another request.
struct Full;

impl RequestHandler for Full {
    fn handle(&self, _request: &Request) -> Option<Response> {
        None
    }

    fn status(&self) -> Status {
        Status::Saturated
    }
}

#[test]
fn a_saturated_handler_makes_the_server_not_ready_and_still_live() {
    let addr = serve(&site("saturated"), Some(Arc::new(Full)));
    assert_eq!(status(&get(addr, "/_lumen/readyz")), 503);
    assert_eq!(status(&get(addr, "/_lumen/healthz")), 200);
}

#[test]
fn a_stopping_server_is_not_ready() {
    let root = site("stopping");
    let listener = TcpListener::bind((LOOPBACK, 0)).expect("a free port");
    let server = Server::on(listener, &root, "/");
    let ready = |draining| {
        health(HEALTH_PATH, "/_lumen/readyz", true, &server.site, draining)
            .expect("the readiness endpoint")
            .status
    };
    assert_eq!(ready(false), 200);
    assert_eq!(ready(true), 503);
    // Liveness is not readiness: a stopping server is still alive.
    let live = health(HEALTH_PATH, "/_lumen/healthz", true, &server.site, true)
        .expect("the liveness endpoint");
    assert_eq!(live.status, 200);
}

/// A handler that answers the way a full render queue does whenever it is
/// asked, and says it is full whenever it is looked at.
struct Busy;

impl RequestHandler for Busy {
    fn handle(&self, _request: &Request) -> Option<Response> {
        Some(Response::text(503, "busy"))
    }

    fn status(&self) -> Status {
        Status::Saturated
    }
}

#[test]
fn a_saturated_worker_leaves_new_connections_to_an_idle_sibling() {
    // Two workers on one listening socket, the way a supervisor runs them:
    // one whose render queue is full, and one with room.
    let root = site("siblings");
    let listener = TcpListener::bind((LOOPBACK, 0)).expect("a free port");
    let addr = listener.local_addr().expect("its address");
    let full = Server::on(
        listener.try_clone().expect("a second handle on the socket"),
        &root,
        "/",
    )
    .with_handler(Arc::new(Busy))
    .with_siblings(true);
    let (handler, _requests) = probe();
    let idle = Server::on(listener, &root, "/")
        .with_handler(handler)
        .with_siblings(true);
    std::thread::spawn(move || full.run());
    std::thread::spawn(move || idle.run());

    // Every page is answered by the worker with room, however the kernel
    // would have spread the connections between the two.
    let pages: Vec<_> = (0..24)
        .map(|_| std::thread::spawn(move || get(addr, "/")))
        .collect();
    for page in pages {
        let page = page.join().expect("the page thread");
        assert_eq!(status(&page), 200, "{page}");
    }
}

#[test]
fn a_saturated_server_alone_still_answers_503_at_once() {
    // Without a sibling there is nobody to leave the connection to, and a
    // 503 now tells a balancer more than a connection that hangs.
    let addr = serve(&site("alone"), Some(Arc::new(Busy)));
    let page = get(addr, "/");
    assert_eq!(status(&page), 503, "{page}");
    let sheet = get(addr, "/styles.css");
    assert_eq!(status(&sheet), 200, "{sheet}");
}

#[test]
fn a_head_longer_than_the_cap_is_refused() {
    let addr = serve(&site("long-head"), None);
    let padding = "x".repeat(http::MAX_HEAD);
    let answer = ask(
        addr,
        &format!("GET / HTTP/1.1\r\nHost: test\r\nX-Long: {padding}\r\n\r\n"),
    );
    assert_eq!(status(&answer), 431, "{answer}");
}

#[test]
fn a_malformed_head_is_answered_400_and_the_connection_closed() {
    let (handler, requests) = probe();
    let addr = serve(&site("malformed"), Some(handler));
    // The body a lenient reader would frame, and a second request after it
    // that a smuggler hopes the server reads as its own.
    let answer = ask(
        addr,
        "POST / HTTP/1.1\r\nHost: test\r\nContent-Length : 5\r\n\r\nhelloGET /styles.css \
         HTTP/1.1\r\nHost: test\r\n\r\n",
    );
    assert_eq!(status(&answer), 400, "{answer}");
    assert!(answer.contains("Connection: close\r\n"), "{answer}");
    assert_eq!(answer.matches("HTTP/1.1 ").count(), 1, "{answer}");
    assert!(
        requests.try_recv().is_err(),
        "a malformed request reached the handler"
    );
}

#[test]
fn a_body_past_the_bound_is_refused_before_it_is_read() {
    let (handler, requests) = probe();
    let addr = serve(&site("long-body"), Some(handler));
    let answer = ask(
        addr,
        &format!(
            "POST / HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n\r\n",
            http::MAX_BODY + 1
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
    let post = "POST /submit HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: \
                7\r\n\r\nname=ok";
    let answer = ask(addr, post);
    assert_eq!(status(&answer), 200, "{answer}");
    let seen = requests.recv().expect("the handler was asked");
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path, "/submit");
    assert_eq!(seen.body, "name=ok");

    let alone = serve(&root, None);
    let refused = ask(alone, post);
    assert_eq!(status(&refused), 405, "{refused}");
}

#[test]
fn the_headers_a_handler_needs_arrive_intact() {
    let (handler, requests) = probe();
    let root = site("headers");
    let server = Server::bind(&root, "/", LOOPBACK, 0)
        .expect("a free port")
        .with_handler(handler)
        .with_trust(Trust::Everybody);
    let addr = server.addr();
    std::thread::spawn(move || server.run());
    let answer = ask(
        addr,
        "GET /user/42?tab=posts HTTP/1.1\r\nHost: test\r\nAccept-Language: en-GB\r\nCookie: \
         session=abc\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&answer), 200, "{answer}");
    let seen = requests.recv().expect("the handler was asked");
    assert_eq!(seen.path, "/user/42");
    assert_eq!(seen.query, "tab=posts");
    assert_eq!(
        http::header(&seen.headers, "accept-language"),
        Some("en-GB")
    );
    assert_eq!(http::header(&seen.headers, "cookie"), Some("session=abc"));
    assert_eq!(
        http::header(&seen.headers, "x-forwarded-proto"),
        Some("https")
    );
    assert!(seen.secure);
}

#[test]
fn what_an_untrusted_peer_says_about_the_origin_is_dropped() {
    let (handler, requests) = probe();
    let addr = serve(&site("untrusted"), Some(handler));
    let _ = ask(
        addr,
        "GET / HTTP/1.1\r\nHost: test\r\nX-Forwarded-Proto: https\r\nX-Forwarded-For: \
         1.2.3.4\r\nConnection: close\r\n\r\n",
    );
    let seen = requests.recv().expect("the handler was asked");
    assert!(!seen.secure);
    assert_eq!(http::header(&seen.headers, "x-forwarded-proto"), None);
    assert_eq!(seen.client, Some(LOOPBACK));
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
    let posted = ask(
        addr,
        "POST / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    );
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

    // The stylesheet answers meanwhile. It would not if a file and a render
    // shared one queue.
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

    // Without a handler the shell answers it, with the status a static host
    // sends.
    let alone = serve(&root, None);
    let shell = get(alone, "/user/42");
    assert_eq!(status(&shell), 404, "{shell}");
    assert!(shell.contains("shell"), "{shell}");
}

#[test]
fn a_head_request_carries_the_length_and_no_body() {
    let addr = serve(&site("head"), None);
    let answer = ask(
        addr,
        "HEAD /styles.css HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&answer), 200, "{answer}");
    assert!(answer.contains("Content-Length: 6"), "{answer}");
    assert!(!answer.contains("body{}"), "{answer}");
}

#[test]
fn a_body_after_a_short_head_is_still_readable() {
    let (handler, requests) = probe();
    let addr = serve(&site("body"), Some(handler));
    let _ = ask(
        addr,
        "POST /note HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: \
         5\r\n\r\nhello",
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

#[test]
fn one_connection_carries_several_requests() {
    let addr = serve(&site("keep-alive"), None);
    let mut stream = connect(addr);
    for _ in 0..3 {
        stream
            .write_all(b"GET /styles.css HTTP/1.1\r\nHost: test\r\n\r\n")
            .expect("sending a request");
        let answer = read_one(&mut stream);
        assert_eq!(status(&answer), 200, "{answer}");
        assert!(answer.ends_with("body{}"), "{answer}");
        assert!(!answer.contains("Connection: close"), "{answer}");
    }
    // An HTTP/1.0 client keeps the connection only when it asks.
    stream
        .write_all(b"GET /styles.css HTTP/1.0\r\n\r\n")
        .expect("sending a request");
    let last = read_one(&mut stream);
    assert!(last.contains("Connection: close"), "{last}");
}

#[test]
fn a_client_that_trickles_its_head_is_cut_off() {
    let limits = Limits {
        header_timeout: Duration::from_millis(500),
        ..Limits::default()
    };
    let (addr, _stop, _running) = serve_with(&site("slowloris"), None, limits);
    let mut stream = connect(addr);
    let started = Instant::now();
    stream
        .write_all(b"GET / HTTP/1.1\r\n")
        .expect("sending the start of a request");
    // One header line at a time, each well inside a per-read timeout of the
    // same length, which is what a deadline for the whole head is for. The
    // client stops sending once the server has answered, so the answer is not
    // lost to a reset.
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("a short read timeout");
    let mut answer = Vec::new();
    let mut chunk = [0u8; 1024];
    for i in 0..50 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                answer.extend_from_slice(&chunk[..n]);
                break;
            }
            Err(_) => {}
        }
        if stream
            .write_all(format!("X-Slow-{i}: x\r\n").as_bytes())
            .is_err()
        {
            break;
        }
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout");
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    assert_eq!(status(&answer), 408, "{answer}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cut off only after {:?}",
        started.elapsed()
    );
}

#[test]
fn a_stop_finishes_the_request_in_flight_and_closes_the_idle_ones() {
    let (seen, requests) = channel();
    let (release, held) = channel();
    let handler = Arc::new(Probe {
        seen,
        hold: Some(Mutex::new(held)),
        declines: false,
    });
    let (addr, stop, running) = serve_with(&site("drain"), Some(handler), Limits::default());

    // A connection that has finished one request and waits for the next.
    let mut idle = connect(addr);
    idle.write_all(b"GET /styles.css HTTP/1.1\r\nHost: test\r\n\r\n")
        .expect("sending a request");
    let _ = read_one(&mut idle);

    // A request the handler is still answering.
    let page = std::thread::spawn(move || get(addr, "/"));
    requests
        .recv_timeout(Duration::from_secs(30))
        .expect("the handler was asked for the page");

    let stopped = Instant::now();
    stop.shutdown();
    // The idle connection is closed rather than waited for.
    let mut rest = Vec::new();
    let _ = idle.read_to_end(&mut rest);
    assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));

    let _ = release.send(());
    let page = page.join().expect("the page thread");
    assert_eq!(status(&page), 200, "{page}");
    assert!(page.contains("Connection: close"), "{page}");
    assert_eq!(running.join().expect("the server thread"), Exit::Stopped);
    assert!(stopped.elapsed() < Duration::from_secs(10));
}
