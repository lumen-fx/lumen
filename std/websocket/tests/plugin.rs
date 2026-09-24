//! The compiled-in shape: [`WebSocketPlugin`] installed on a headless app,
//! driven from candela against an echo server this test runs on loopback.
//!
//! What this proves: a connection opens off the tick, a text frame goes out
//! and comes back through `on_ws_message`, a close the script asks for comes
//! back through `on_ws_close` with the code, and a refused connection reports
//! an error and an unclean close, all as the events a page gets.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_websocket::WebSocketPlugin;
use tungstenite::Message;

/// The app directory, the DOM snapshot, and the property store are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Accept one connection on a loopback port and echo its text frames until
/// it closes. Answers with the port.
fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("a local address").port();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut socket = tungstenite::accept(stream).expect("handshake");
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let _ = socket.send(Message::text(format!("echo:{text}")));
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        // Finish the closing handshake the client started.
        let _ = socket.flush();
    });
    port
}

fn app(source: &str) -> EcsApp {
    let dir = std::env::temp_dir().join(format!("lumen-websocket-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("lumen.toml");
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        script_source: source.to_string(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_plugin(WebSocketPlugin);
    opts.bounded = true;
    let (app, _window) = build_headless_app(opts).expect("build headless app");
    app
}

fn signal(app: &EcsApp, name: &str) -> Option<String> {
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

/// Tick until `name` holds a value, or give up after a few seconds.
fn wait_for(app: &mut EcsApp, name: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        app.tick();
        if let Some(value) = signal(app, name) {
            return Some(value);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn a_frame_goes_out_and_comes_back_and_the_close_reports_its_code() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let port = echo_server();
    let mut app = app(&format!(
        r#"import "lumen.cdl";

fn on_start() {{
    lumen::signal_set_bool("opened", ws::open("echo", "ws://127.0.0.1:{port}/"));
    lumen::signal_set_bool("again", ws::open("echo", "ws://127.0.0.1:{port}/"));
    lumen::signal_set("state_at_start", ws::state("echo"));
    lumen::signal_set("unknown", ws::state("nobody"));
}}

fn on_ws_open(key) {{
    lumen::signal_set("state_on_open", ws::state(key));
    ws::send(key, "hello");
}}

fn on_ws_message(key, text) {{
    lumen::signal_set("message", text);
    ws::close(key, 1000, "done");
}}

fn on_ws_close(key, code, reason, clean) {{
    lumen::signal_set("closed", str(code) + " " + reason + " " + str(clean));
    lumen::signal_set("state_after", ws::state(key));
}}

fn main() {{}}
"#
    ));
    app.tick();
    assert_eq!(signal(&app, "opened").as_deref(), Some("true"));
    assert_eq!(
        signal(&app, "again").as_deref(),
        Some("false"),
        "a key whose connection is still opening is taken"
    );
    assert_eq!(
        signal(&app, "state_at_start").as_deref(),
        Some("connecting")
    );
    assert_eq!(signal(&app, "unknown").as_deref(), Some("closed"));
    assert_eq!(wait_for(&mut app, "message").as_deref(), Some("echo:hello"));
    assert_eq!(signal(&app, "state_on_open").as_deref(), Some("open"));
    assert_eq!(
        wait_for(&mut app, "closed").as_deref(),
        Some("1000 done true")
    );
    assert_eq!(signal(&app, "state_after").as_deref(), Some("closed"));
}

#[test]
fn a_refused_connection_reports_an_error_and_an_unclean_close() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // A port nothing listens on: bound, then let go.
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let mut app = app(&format!(
        r#"import "lumen.cdl";

fn on_start() {{
    ws::open("gone", "http://127.0.0.1:{port}/");
}}

fn on_ws_error(key, message) {{
    lumen::signal_set("error", message);
}}

fn on_ws_close(key, code, reason, clean) {{
    lumen::signal_set("closed", str(code) + " " + str(clean));
}}

fn main() {{}}
"#
    ));
    let error = wait_for(&mut app, "error").expect("an error arrives");
    assert!(error.starts_with("the connection failed"), "{error}");
    assert_eq!(wait_for(&mut app, "closed").as_deref(), Some("1006 false"));
}
