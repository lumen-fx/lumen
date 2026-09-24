//! The compiled-in shape: [`CookiePlugin`] installed on a headless app with
//! the engine's own HTTP client, driven from candela against a server this
//! test runs on loopback.
//!
//! What this proves: a `Set-Cookie` a reply carries goes into the jar, the
//! next `http()` to the same server carries it back, the script reads it with
//! `cookie::get`, and the lasting one is in the jar's file for the next run.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lumen_cookie::CookiePlugin;
use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// Answer two requests on a loopback port: the first sets a cookie, the
/// second reports back the `Cookie` header it arrived with. Answers with the
/// port, and hands every request's `Cookie` header down `seen`.
fn server(seen: mpsc::Sender<String>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("a local address").port();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().take(2).enumerate() {
            let mut stream = stream.expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut cookie = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("cookie")
                {
                    cookie = value.trim().to_string();
                }
            }
            let _ = seen.send(cookie.clone());
            let (extra, body) = if n == 0 {
                (
                    "Set-Cookie: sid=abc123; Path=/; Max-Age=3600\r\nSet-Cookie: tmp=1; Path=/\r\n",
                    "logged in".to_string(),
                )
            } else {
                ("", cookie)
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
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

#[test]
fn the_jar_keeps_what_a_reply_sets_and_the_next_request_carries_it() {
    let dir = std::env::temp_dir().join(format!("lumen-cookie-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("lumen.toml");
    let file = dir.join("data").join("cookies.json");

    let (tx, seen) = mpsc::channel();
    let port = server(tx);
    let source = format!(
        r#"import "lumen.cdl";

fn on_start() {{
    lumen::http({{"url": "http://127.0.0.1:{port}/login", "tag": "login"}});
}}

fn on_http(tag: string, response: any) {{
    let r = as_map(response);
    if tag == "login" {{
        lumen::signal_set("read", str(cookie::get("sid")));
        lumen::http({{"url": "http://127.0.0.1:{port}/me", "tag": "me"}});
    }} else {{
        lumen::signal_set("echoed", as_str(r.get("body")));
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
        script_source: source.clone(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source,
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_plugin(CookiePlugin::at(&file));
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");

    let deadline = Instant::now() + Duration::from_secs(10);
    while signal(&app, "echoed").is_none() && Instant::now() < deadline {
        app.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(1)).as_deref(),
        Ok(""),
        "the first request carries nothing"
    );
    assert_eq!(signal(&app, "read").as_deref(), Some("abc123"));
    assert_eq!(signal(&app, "echoed").as_deref(), Some("sid=abc123; tmp=1"));

    let kept = std::fs::read_to_string(&file).expect("the jar's file");
    assert!(kept.contains("abc123"), "{kept}");
    assert!(
        !kept.contains("\"tmp\""),
        "a cookie with no expiry lasts for the process only: {kept}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
