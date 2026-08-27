//! A loopback HTTP server the module's own tests download from.
//!
//! It ships in the crate rather than in a dev-dependency because three test
//! binaries need it, one of them across a subprocess boundary. Nothing in the
//! shipping surface refers to it, and no test in this crate reaches the
//! network.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The body `/fixed` serves, and the one `/drip` dribbles out.
pub const BODY: &[u8] = b"lumen download module fixture body";

/// The body `/mismatch` serves: a different one, so a request carrying
/// `BODY`'s digest fails verification.
pub const OTHER_BODY: &[u8] = b"a different body entirely, same length ok";

/// A running server. Dropping it stops the accept loop.
pub struct TestServer {
    addr: SocketAddr,
    stopping: Arc<AtomicBool>,
}

impl TestServer {
    /// Bind an ephemeral loopback port and start serving.
    ///
    /// Routes: `/fixed` (a body with `Content-Length`), `/nolength` (a body
    /// the closed connection delimits), `/drip` (the same body in pieces, with
    /// pauses), `/mismatch` (a different body), and `/missing` (404). Anything
    /// else is a 404 too.
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let stopping = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopping);
        std::thread::Builder::new()
            .name("lumen-download-testkit".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    std::thread::spawn(move || serve(stream));
                }
            })
            .expect("testkit thread");
        Self { addr, stopping }
    }

    /// The URL of one route on this server, loopback address and ephemeral
    /// port included, so a test hands the whole string to whatever downloads
    /// it, another process included.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        // One connection to wake the blocked accept so the thread sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Answer one request.
fn serve(mut stream: TcpStream) {
    // The whole request head is read, not just its first line: closing a
    // socket with bytes still unread makes the kernel answer RST, and the
    // client loses the reply it was in the middle of reading.
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let _ = match path.as_str() {
        "/fixed" => sized(&mut stream, BODY),
        "/mismatch" => sized(&mut stream, OTHER_BODY),
        "/nolength" => {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
            stream.write_all(BODY)
        }
        "/drip" => {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
            let mut sent = Ok(());
            for piece in BODY.chunks(4) {
                sent = stream.write_all(piece).and_then(|()| stream.flush());
                if sent.is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            sent
        }
        _ => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found"),
    };
    let _ = stream.flush();
}

/// A 200 with a declared length.
fn sized(stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)
}
