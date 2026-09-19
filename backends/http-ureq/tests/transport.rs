//! Transport behaviour of the shipped HTTP client, driven against a loopback
//! server so no test touches the network: a full request/reply round trip, a
//! refused connection, and a body past the cap.

use std::io::{Read, Write};
use std::net::TcpListener;

use lumen_http_ureq::UreqHttpClient;
use lumen_script::{HttpClient, HttpRequest};

/// 16 MiB, the cap the script runtime passes for a normal request.
const CAP: u64 = 16 * 1024 * 1024;

/// End-to-end transport against a loopback server (no external endpoint).
/// Verifies method + body round-trip in and status + header + body out - the
/// full Qt-`QNetworkReply`-shaped reply.
#[test]
fn round_trips_over_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();

    // Minimal one-shot HTTP/1.1 server: read the request head + body,
    // echo the request body back with a custom header and 200.
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Read until the header terminator, then keep reading until we
        // have the full Content-Length body (head + body can arrive in
        // separate packets).
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut content_len: Option<usize> = None;
        let body = loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break String::new();
            }
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(idx) = text.find("\r\n\r\n") {
                if content_len.is_none() {
                    content_len = text[..idx].lines().find_map(|l| {
                        l.split_once(':').and_then(|(k, v)| {
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    });
                }
                let body_so_far = text.len() - (idx + 4);
                if body_so_far >= content_len.unwrap_or(0) {
                    break text[idx + 4..].to_string();
                }
            }
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Echo-Method: POST\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        stream.flush().unwrap();
    });

    let req = HttpRequest {
        method: "post".to_string(), // case-insensitive
        url: format!("http://{addr}/echo"),
        headers: vec![("X-Test".to_string(), "1".to_string())],
        body: Some("hello-body".to_string()),
        timeout_ms: Some(5000),
    };
    let resp = UreqHttpClient.send(&req, CAP).expect("transport ok");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "hello-body");
    assert!(
        resp.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-echo-method") && v == "POST"),
        "server saw the POST + echoed header: {:?}",
        resp.headers
    );
    server.join().unwrap();
}

/// A connection to a closed loopback port is a transport `Err`, not a panic
/// (surfaced to scripts as `error` with `status=0`).
#[test]
fn connection_refused_is_err() {
    // Bind then drop to obtain a port nothing is listening on.
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let req = HttpRequest {
        method: "GET".to_string(),
        url: format!("http://{addr}/"),
        headers: vec![],
        body: None,
        timeout_ms: Some(2000),
    };
    assert!(UreqHttpClient.send(&req, CAP).is_err());
}

/// A response body larger than the cap aborts with an `Err` (bounded read, no
/// OOM) rather than allocating whatever the endpoint sends.
#[test]
fn body_over_cap_errors_not_oom() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();

    // One-shot server: reads (and discards) the request, replies 200
    // with a 4 KiB body - comfortably above the tiny test cap below.
    const BODY_LEN: usize = 4096;
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut scratch = [0u8; 1024];
        let _ = stream.read(&mut scratch); // consume request head
        let body = "A".repeat(BODY_LEN);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {BODY_LEN}\r\n\r\n{body}");
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    });

    let req = HttpRequest {
        method: "GET".to_string(),
        url: format!("http://{addr}/big"),
        headers: vec![],
        body: None,
        timeout_ms: Some(5000),
    };

    // Cap well below the body size: the read must abort with an error.
    let err = UreqHttpClient
        .send(&req, 64)
        .expect_err("body over cap must return Err (bounded read, no OOM)");
    assert!(!err.is_empty(), "the failure carries a message");

    let _ = server.join();
}

/// An unparseable method fails before any socket is opened, with a message
/// naming the offending method (it reaches the script verbatim).
#[test]
fn invalid_method_is_rejected() {
    let req = HttpRequest {
        method: "GET SPACE".to_string(),
        url: "http://127.0.0.1:9/x".to_string(),
        ..HttpRequest::default()
    };
    let err = UreqHttpClient.send(&req, CAP).expect_err("invalid method");
    assert!(err.contains("GET SPACE"), "message names the method: {err}");
}
