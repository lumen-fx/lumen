//! `lumen-server` end to end: the built binary, serving a site `lumenc web
//! --render ssr` built from an app in this repository.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// The repository this test is built from.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("web/server sits two levels under the repository")
        .to_path_buf()
}

/// A fresh directory of its own for one case.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-server-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Build `app` into a site the way a user does, with `lumenc web`. The
/// browser runtime stands in as two files, because what a build copies is
/// not this suite's subject.
fn build(app: &Path, name: &str) -> PathBuf {
    let scratch = scratch(name);
    let lib = scratch.join("lib");
    std::fs::create_dir_all(&lib).expect("create the runtime directory");
    std::fs::write(lib.join("lumen-web.wasm"), b"\0asm\x01\0\0\0").expect("write the wasm stub");
    std::fs::write(lib.join("lumen-web.js"), b"export function boot() {}\n")
        .expect("write the module stub");
    let out = scratch.join("site");
    let args = [
        app.to_string_lossy().into_owned(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
        "--lib-dir".to_string(),
        lib.to_string_lossy().into_owned(),
        "--render".to_string(),
        "ssr".to_string(),
    ];
    let _ = lumenc::web::cli::cmd_web(args.into_iter());
    assert!(
        out.join(lumen_ssr::SERVER_SPEC_FILE).is_file(),
        "lumenc web did not build {}",
        app.display()
    );
    out
}

/// The sites, built once for the whole suite. A build runs parts of the app
/// in this process, so the two are built one after the other.
fn sites() -> &'static (PathBuf, PathBuf) {
    static SITES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    SITES.get_or_init(|| {
        (
            build(&repo().join("fixtures/ssr-site"), "ssr-site"),
            build(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slow-upstream"),
                "slow-upstream",
            ),
        )
    })
}

/// A running `lumen-server`, and everything it has written.
struct Running {
    child: Option<Child>,
    addr: SocketAddr,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
}

impl Running {
    fn start(site: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-server"))
            .arg(site)
            .args(["--port", "0"])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start lumen-server");
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));
        collect(child.stdout.take().expect("stdout"), Arc::clone(&out), None);
        let (found, address) = channel();
        collect(
            child.stderr.take().expect("stderr"),
            Arc::clone(&err),
            Some(found),
        );
        let addr = match address.recv_timeout(Duration::from_secs(120)) {
            Ok(addr) => addr,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "lumen-server never said where it listens:\n{}",
                    err.lock().expect("the log")
                );
            }
        };
        let running = Self {
            child: Some(child),
            addr,
            out,
            err,
        };
        running.until("a worker answering", || {
            status(&running.get("/_lumen/healthz")) == 200
        });
        running
    }

    fn get(&self, path: &str) -> String {
        ask(
            self.addr,
            &format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"),
        )
    }

    fn stdout(&self) -> String {
        self.out.lock().expect("the log").clone()
    }

    fn stderr(&self) -> String {
        self.err.lock().expect("the log").clone()
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("still running").id()
    }

    /// Send SIGTERM, which is what a container runtime stops it with.
    #[cfg(unix)]
    fn terminate(&self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.pid().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success());
    }

    /// Wait for the process to exit, for up to `within`.
    fn wait(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        let child = self.child.as_mut().expect("still running");
        loop {
            if let Some(status) = child.try_wait().expect("waiting on lumen-server") {
                self.child = None;
                // Let the readers take the last lines.
                thread::sleep(Duration::from_millis(100));
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "lumen-server still running after {within:?}:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn until(&self, what: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "never saw {what}:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read a pipe line by line into `into`, and send the address the first line
/// naming one gives.
fn collect(
    pipe: impl Read + Send + 'static,
    into: Arc<Mutex<String>>,
    found: Option<std::sync::mpsc::Sender<SocketAddr>>,
) {
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if let Some(found) = &found
                && let Some(rest) = line.split("http://").nth(1)
            {
                let addr: String = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '"')
                    .collect();
                if let Ok(addr) = addr.parse() {
                    let _ = found.send(addr);
                }
            }
            if let Ok(mut into) = into.lock() {
                into.push_str(&line);
                into.push('\n');
            }
        }
    });
}

/// Send `request` verbatim and read the whole answer back.
fn ask(addr: SocketAddr, request: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(addr) else {
        return String::new();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    if stream.write_all(request.as_bytes()).is_err() {
        return String::new();
    }
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    String::from_utf8_lossy(&answer).into_owned()
}

fn status(answer: &str) -> u16 {
    answer
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

fn header<'a>(answer: &'a str, name: &str) -> Option<&'a str> {
    answer.split("\r\n\r\n").next()?.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// Every path on this site a document points at.
fn local_references(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    for attr in ["href=\"", "src=\""] {
        let mut rest = html;
        while let Some(at) = rest.find(attr) {
            rest = &rest[at + attr.len()..];
            let end = rest.find('"').unwrap_or(rest.len());
            let value = &rest[..end];
            if value.starts_with('/') && !value.starts_with("//") {
                found.push(value.split('?').next().unwrap_or(value).to_string());
            }
        }
    }
    found
}

#[test]
fn a_served_site_answers_health_and_renders_its_pages_with_their_files() {
    let (site, _) = sites();
    let server = Running::start(site, &[]);

    let live = server.get("/_lumen/healthz");
    assert_eq!(status(&live), 200, "{live}");
    let ready = server.get("/_lumen/readyz");
    assert_eq!(status(&ready), 200, "{ready}");

    let page = server.get("/");
    assert_eq!(status(&page), 200, "{page}");
    assert!(
        page.contains("Hello") && page.contains("Ready to go"),
        "{page}"
    );
    assert_eq!(header(&page, "Cache-Control"), Some("no-store"), "{page}");
    let german = server.get("/de-DE/");
    assert!(german.contains("Startklar"), "{german}");

    // Every file the page points at is served, and a file named after its
    // own bytes is kept for good.
    let references = local_references(&page);
    let hashed: Vec<&String> = references
        .iter()
        .filter(|path| path.contains("styles."))
        .collect();
    assert!(!hashed.is_empty(), "{references:?}");
    for path in &references {
        let file = server.get(path);
        assert_eq!(status(&file), 200, "{path}: {file}");
        assert!(
            site.join(path.trim_start_matches('/')).is_file(),
            "{path} is not a file the build wrote"
        );
    }
    for path in hashed {
        let file = server.get(path);
        assert_eq!(
            header(&file, "Cache-Control"),
            Some("public, max-age=31536000, immutable"),
            "{path}: {file}"
        );
        assert_eq!(
            header(&file, "Content-Type"),
            Some("text/css; charset=utf-8")
        );
    }

    // The spec file is the server's input, not a file the site serves.
    let spec = server.get(&format!("/{}", lumen_ssr::SERVER_SPEC_FILE));
    assert_eq!(status(&spec), 404, "{spec}");
    assert!(!spec.contains("api.example.com"), "{spec}");
}

#[test]
fn an_access_line_is_written_and_carries_no_credentials() {
    let (site, _) = sites();
    let mut server = Running::start(site, &["--log-format", "json"]);
    let answer = ask(
        server.addr,
        "GET /?from=log HTTP/1.1\r\nHost: test\r\nCookie: session=cookie-secret-value\r\n\
         Authorization: Bearer auth-secret-value\r\nProxy-Authorization: Basic \
         proxy-secret-value\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&answer), 200, "{answer}");
    #[cfg(unix)]
    server.terminate();
    #[cfg(not(unix))]
    let _ = &mut server;
    #[cfg(unix)]
    let _ = server.wait(Duration::from_secs(30));

    let stdout = server.stdout();
    let line = stdout
        .lines()
        .find(|line| line.contains("from=log"))
        .unwrap_or_else(|| panic!("no access line for the request:\n{stdout}"));
    let entry: serde_json::Value = serde_json::from_str(line).expect("a JSON access line");
    assert_eq!(entry["status"], 200);
    assert_eq!(entry["method"], "GET");
    for secret in [
        "cookie-secret-value",
        "auth-secret-value",
        "proxy-secret-value",
    ] {
        assert!(
            !stdout.contains(secret),
            "{secret} in the access log:\n{stdout}"
        );
        assert!(
            !server.stderr().contains(secret),
            "{secret} in the server log:\n{}",
            server.stderr()
        );
    }
}

#[test]
fn a_client_that_trickles_its_headers_is_cut_off() {
    let (site, _) = sites();
    let server = Running::start(site, &["--header-timeout", "1s"]);
    let mut stream = TcpStream::connect(server.addr).expect("connect");
    let started = Instant::now();
    stream
        .write_all(b"GET / HTTP/1.1\r\n")
        .expect("send the start of a request");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("a short read timeout");
    let mut answer = Vec::new();
    let mut chunk = [0u8; 1024];
    // A header every 200ms, each far inside a timeout that restarted on every
    // read, and all of them together far past the header timeout.
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
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    assert_eq!(status(&answer), 408, "{answer}");
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "cut off only after {:?}",
        started.elapsed()
    );
    // And the server is still answering everyone else.
    assert_eq!(status(&server.get("/_lumen/healthz")), 200);
}

/// An upstream that takes a request and answers it only when told to.
struct Upstream {
    addr: SocketAddr,
    asked: Receiver<TcpStream>,
}

impl Upstream {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let addr = listener.local_addr().expect("its address");
        let (found, asked) = channel();
        thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                if found.send(stream).is_err() {
                    return;
                }
            }
        });
        Self { addr, asked }
    }

    /// Wait for the render's request, and hand back its connection.
    fn held(&self) -> TcpStream {
        let mut stream = self
            .asked
            .recv_timeout(Duration::from_secs(60))
            .expect("the render asked the upstream");
        // Read the request head, so answering does not race it.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n > 0) {
            head.push(byte[0]);
        }
        stream
    }
}

fn answer_upstream(mut stream: TcpStream, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    );
}

#[test]
fn a_full_queue_is_answered_503_while_files_are_still_served() {
    let (_, site) = sites();
    let upstream = Upstream::start();
    let server = Running::start(site, &["--queue-depth", "0"]);
    let addr = server.addr;
    let target = format!("/?http://{}/data", upstream.addr);
    let page = thread::spawn(move || {
        ask(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"),
        )
    });
    // The render is in flight until the upstream answers.
    let held = upstream.held();

    let started = Instant::now();
    let styles = std::fs::read_dir(site)
        .expect("the site")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("styles.") && name.ends_with(".css"))
        .expect("the build wrote a stylesheet");
    let sheet = server.get(&format!("/{styles}"));
    assert_eq!(status(&sheet), 200, "{sheet}");

    let ready = server.get("/_lumen/readyz");
    assert_eq!(status(&ready), 503, "{ready}");
    let turned_away = server.get("/");
    assert_eq!(status(&turned_away), 503, "{turned_away}");
    assert_eq!(header(&turned_away, "Retry-After"), Some("1"));
    // A production error page says nothing about why.
    assert!(turned_away.ends_with("busy"), "{turned_away}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the answers took {:?}; the render's budget may have run out under them",
        started.elapsed()
    );

    answer_upstream(held, "upstream-answered");
    let page = page.join().expect("the page thread");
    assert_eq!(status(&page), 200, "{page}");
    assert!(page.contains("upstream-answered"), "{page}");
    server.until("ready again", || {
        status(&server.get("/_lumen/readyz")) == 200
    });
}

#[cfg(unix)]
#[test]
fn a_sigterm_finishes_the_request_in_flight_then_exits() {
    let (site, _) = sites();
    let mut server = Running::start(site, &[]);
    let mut stream = TcpStream::connect(server.addr).expect("connect");
    // The head arrives; the body is still on its way when the signal lands.
    stream
        .write_all(b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 7\r\n\r\n")
        .expect("send the head");
    thread::sleep(Duration::from_millis(300));
    server.terminate();
    thread::sleep(Duration::from_millis(300));
    stream.write_all(b"name=ok").expect("send the body");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    assert_eq!(status(&answer), 200, "{answer}");
    assert_eq!(header(&answer, "Connection"), Some("close"), "{answer}");

    let exit = server.wait(Duration::from_secs(20));
    assert!(exit.success(), "{exit:?}:\n{}", server.stderr());
    // Nothing answers once it has gone.
    assert!(TcpStream::connect(server.addr).is_err());
}

#[cfg(unix)]
#[test]
fn a_worker_recycles_after_its_renders_and_the_port_keeps_answering() {
    let (site, _) = sites();
    let mut server = Running::start(site, &["--max-renders", "2", "--log-format", "json"]);
    let supervisor = u64::from(server.pid());
    for round in 0..6 {
        let page = server.get(&format!("/?round={round}"));
        assert_eq!(status(&page), 200, "round {round}: {page}");
    }
    server.terminate();
    let exit = server.wait(Duration::from_secs(30));
    assert!(exit.success(), "{exit:?}");

    let stderr = server.stderr();
    let restarts = stderr.matches("starting another").count();
    assert!(restarts >= 2, "{stderr}");
    let pids: std::collections::BTreeSet<u64> = server
        .stdout()
        .lines()
        .filter(|line| line.contains("round="))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| entry["pid"].as_u64())
        .collect();
    assert!(pids.len() >= 3, "{pids:?}");
    // Every page came from a worker, never from the supervisor.
    assert!(!pids.contains(&supervisor), "{pids:?}");
}

#[cfg(unix)]
#[test]
fn a_render_past_its_limit_is_answered_504_and_its_worker_replaced() {
    let (_, site) = sites();
    let upstream = Upstream::start();
    let server = Running::start(site, &["--render-timeout", "500ms"]);
    let addr = server.addr;
    let target = format!("/?http://{}/data", upstream.addr);
    let started = Instant::now();
    let page = thread::spawn(move || {
        ask(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"),
        )
    });
    // Held, and never answered.
    let _held = upstream.held();
    let page = page.join().expect("the page thread");
    assert_eq!(status(&page), 504, "{page}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "answered only after {:?}",
        started.elapsed()
    );

    server.until("the worker replaced", || {
        server
            .stderr()
            .contains("ran past its time limit; starting another")
    });
    let fresh = server.get("/");
    assert_eq!(status(&fresh), 200, "{fresh}");
    assert!(fresh.contains("asking"), "{fresh}");
}
