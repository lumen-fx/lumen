//! An HTTP reply that lands while the app is idle wakes the event loop, so the
//! script's handler runs without any other tick source. Both run loops park
//! between ticks and tick again only when woken; this drives the app the same
//! way, against a local server whose reply the test releases on cue.

#![cfg(feature = "http-fetch")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use lumen_core::app::EventLoopWaker;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

// The candela host, compiled in: the artifact names the module that runs its
// program, and a test binary has no shared engine to open it from.
use lumen_candela_dev as _;

/// The wake flag a parked loop waits on.
#[derive(Default)]
struct Woken {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Woken {
    fn wake(&self) {
        *self.flag.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn clear(&self) {
        *self.flag.lock().unwrap() = false;
    }

    /// Park until woken or `timeout` passes; `true` when woken.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.flag.lock().unwrap();
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |woken| !*woken)
            .unwrap();
        *guard
    }
}

#[test]
fn a_reply_that_lands_while_idle_wakes_the_loop() {
    // One-shot server: report the request, then hold the reply until the
    // test says the app has gone idle.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let port = listener.local_addr().expect("local addr").port();
    let (received_tx, received_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).expect("read request");
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        received_tx.send(()).expect("report request");
        release_rx.recv().expect("wait for release");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi")
            .expect("write reply");
    });

    let dir = std::env::temp_dir().join(format!("lumen_http_wake_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    let source = format!(
        r#"import "lumen.cdl";

fn on_start() {{
    lumen::http({{"url": "http://127.0.0.1:{port}/x", "tag": "t"}});
}}

fn on_http(tag: string, response: any) {{
    let r = response as {{string: any}};
    lumen::signal_set("got", tag + ":" + (r["body"] as string));
}}

fn main() {{}}
"#
    );
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            module: "lumen-candela-dev".to_string(),
            source,
            bytecode: None,
            source_map: Default::default(),
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");

    let woken = Arc::new(Woken::default());
    let hook = Arc::clone(&woken);
    app.world
        .insert_resource(EventLoopWaker(Arc::new(move || hook.wake())));

    // The startup tick sends the request.
    app.tick();
    received_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the request reached the server");

    // Nothing is left to do, so the loop parks. Only the reply may wake it.
    woken.clear();
    release_tx.send(()).expect("release the reply");
    assert!(
        woken.wait(Duration::from_secs(10)),
        "the reply landed but nothing woke the parked loop"
    );

    // The tick the wake runs delivers the reply.
    app.tick();
    let got = match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global("got"))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    };
    assert_eq!(got.as_deref(), Some("t:hi"));

    server.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A local server that answers every request at once and reports each path
/// it was asked for.
fn answering_server() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let line = String::from_utf8_lossy(&request);
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let _ = tx.send(path);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
        }
    });
    (port, rx)
}

/// A request a timer handler sends, or a reply's handler sends on, goes out
/// on the tick that ran the handler. An app that parks after that tick has
/// nothing else to wake it, so a request left for the next tick would wait
/// until something unrelated ticked the app.
#[test]
fn a_request_from_a_handler_goes_out_on_the_tick_that_sent_it() {
    let (port, asked) = answering_server();
    let dir = std::env::temp_dir().join(format!("lumen_http_send_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    let source = format!(
        r#"import "lumen.cdl";

fn on_start() {{
    lumen::set_timeout("go", 0);
}}

fn on_timer(name: string) {{
    lumen::http({{"url": "http://127.0.0.1:{port}/first", "tag": "first"}});
    lumen::signal_set("sent", "first");
}}

fn on_http(tag: string, response: any) {{
    if tag == "first" {{
        lumen::http({{"url": "http://127.0.0.1:{port}/second", "tag": "second"}});
        lumen::signal_set("sent", "second");
    }}
}}

fn main() {{}}
"#
    );
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            module: "lumen-candela-dev".to_string(),
            source,
            bytecode: None,
            source_map: Default::default(),
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    let woken = Arc::new(Woken::default());
    let hook = Arc::clone(&woken);
    app.world
        .insert_resource(EventLoopWaker(Arc::new(move || hook.wake())));
    let sent = |app: &lumen_core::app::App| match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global("sent"))
    {
        Some(PropertyValue::Str(s)) => s.to_string(),
        _ => String::new(),
    };

    // Tick until the timer's handler has run, then no more.
    for _ in 0..20 {
        if sent(&app) == "first" {
            break;
        }
        app.tick();
    }
    assert_eq!(sent(&app), "first", "the timer fired");
    assert_eq!(
        asked.recv_timeout(Duration::from_secs(10)).as_deref(),
        Ok("/first"),
        "the timer's request went out without another tick"
    );

    // The reply wakes the loop; the tick it runs delivers it, and the
    // handler's own request goes out on that tick too.
    assert!(
        woken.wait(Duration::from_secs(10)),
        "the reply woke the loop"
    );
    for _ in 0..20 {
        if sent(&app) == "second" {
            break;
        }
        app.tick();
    }
    assert_eq!(sent(&app), "second", "the reply reached its handler");
    assert_eq!(
        asked.recv_timeout(Duration::from_secs(10)).as_deref(),
        Ok("/second"),
        "the reply handler's request went out without another tick"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
