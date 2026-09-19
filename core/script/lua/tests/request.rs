//! The request surface: `request_header` / `request_cookie` / `request_body`
//! read the request the document is being rendered for, and
//! `response_status` / `response_header` / `redirect` answer it. The readers
//! give back an empty string when no request is installed on the thread,
//! which is every desktop app.

use lumen_core::request::{RequestContext, enter};
use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_lua::LuaHost;

fn context() -> RequestContext {
    RequestContext {
        method: "POST".to_string(),
        path: "/user/42".to_string(),
        query: "tab=posts".to_string(),
        hash: "top".to_string(),
        secure: true,
        headers: vec![("accept-language".to_string(), "en-GB".to_string())],
        cookies: vec![("session".to_string(), "abc".to_string())],
        body: "{\"name\":\"ada\"}".to_string(),
    }
}

fn returns(body: &str) -> Option<ScriptValue> {
    let mut host = LuaHost::new();
    host.load(&format!("function go() return {body} end"))
        .expect("load");
    host.call("go", &[]).expect("call").ret
}

fn commands(body: &str) -> Vec<ScriptCommand> {
    let mut host = LuaHost::new();
    host.load(&format!("function on_start() {body} end"))
        .expect("load");
    host.call_event("on_start", &[]).expect("call")
}

#[test]
fn the_readers_reach_the_installed_request() {
    let _scope = enter(context());
    assert_eq!(
        returns("request_header(\"accept-language\")"),
        Some(ScriptValue::Str("en-GB".into()))
    );
    assert_eq!(
        returns("request_cookie(\"session\")"),
        Some(ScriptValue::Str("abc".into()))
    );
    assert_eq!(
        returns("request_body()"),
        Some(ScriptValue::Str("{\"name\":\"ada\"}".into()))
    );
    assert_eq!(
        returns("window.location.query()"),
        Some(ScriptValue::Str("tab=posts".into()))
    );
    assert_eq!(
        returns("window.location.hash()"),
        Some(ScriptValue::Str("top".into()))
    );
}

#[test]
fn the_readers_are_empty_with_no_request() {
    assert_eq!(
        returns("request_header(\"accept-language\")"),
        Some(ScriptValue::Str(String::new()))
    );
    assert_eq!(
        returns("request_cookie(\"session\")"),
        Some(ScriptValue::Str(String::new()))
    );
    assert_eq!(
        returns("request_body()"),
        Some(ScriptValue::Str(String::new()))
    );
    assert_eq!(
        returns("window.location.query()"),
        Some(ScriptValue::Str(String::new()))
    );
}

#[test]
fn the_answer_builtins_queue_their_commands() {
    let cmds = commands(
        "response_status(404) response_header(\"x-cache\", \"miss\") redirect(\"/login\")",
    );
    assert!(
        cmds.iter()
            .any(|c| matches!(c, ScriptCommand::SetResponseStatus { status } if *status == 404))
    );
    assert!(cmds.iter().any(|c| matches!(
        c,
        ScriptCommand::SetResponseHeader { name, value } if name == "x-cache" && value == "miss"
    )));
    assert!(
        cmds.iter()
            .any(|c| matches!(c, ScriptCommand::Redirect { location } if location == "/login"))
    );
}

#[test]
fn a_status_outside_the_http_range_is_clamped() {
    let statuses = |body: &str| -> Vec<u16> {
        commands(body)
            .iter()
            .filter_map(|c| match c {
                ScriptCommand::SetResponseStatus { status } => Some(*status),
                _ => None,
            })
            .collect()
    };
    assert_eq!(statuses("response_status(7)"), vec![100]);
    assert_eq!(statuses("response_status(9000)"), vec![599]);
}
