//! Roadmap item I: the `http(#{...})` builtin. Verifies the Rhai map is
//! parsed into a `ScriptCommand::Http` with the right method / headers /
//! body / timeout, that omitted fields take web-`fetch`-like defaults,
//! and that `fetch(url, tag)` still lowers to `ScriptCommand::Fetch`
//! (the sugar path is not broken).

use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_rhai::RhaiHost;

/// Return the single `ScriptCommand::Http` emitted, or panic.
fn only_http(cmds: &[ScriptCommand]) -> &ScriptCommand {
    cmds.iter()
        .find(|c| matches!(c, ScriptCommand::Http { .. }))
        .expect("expected a ScriptCommand::Http")
}

#[test]
fn http_full_request_parses() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_start() {
            http(#{
                method: "POST",
                url: "http://127.0.0.1:9/things",
                headers: #{ "Content-Type": "application/json", "X-Token": "abc" },
                body: "{\"a\":1}",
                timeout_ms: 1500,
                tag: "create",
            });
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    let ScriptCommand::Http {
        method,
        url,
        headers,
        body,
        timeout_ms,
        tag,
    } = only_http(&cmds)
    else {
        unreachable!()
    };
    assert_eq!(method, "POST");
    assert_eq!(url, "http://127.0.0.1:9/things");
    assert_eq!(body.as_deref(), Some("{\"a\":1}"));
    assert_eq!(*timeout_ms, Some(1500));
    assert_eq!(tag, "create");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json")
    );
    assert!(headers.iter().any(|(k, v)| k == "X-Token" && v == "abc"));
}

#[test]
fn http_defaults_when_fields_omitted() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_start() {
            http(#{ url: "http://127.0.0.1:9/x", tag: "g" });
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    let ScriptCommand::Http {
        method,
        url,
        headers,
        body,
        timeout_ms,
        tag,
    } = only_http(&cmds)
    else {
        unreachable!()
    };
    assert_eq!(method, "GET"); // web-fetch default
    assert_eq!(url, "http://127.0.0.1:9/x");
    assert!(headers.is_empty());
    assert_eq!(*body, None);
    assert_eq!(*timeout_ms, None);
    assert_eq!(tag, "g");
}

#[test]
fn fetch_still_lowers_to_fetch_command() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_start() { fetch("http://127.0.0.1:9/y", "simple"); }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    assert!(cmds.iter().any(|c| matches!(
        c,
        ScriptCommand::Fetch { url, tag } if url == "http://127.0.0.1:9/y" && tag == "simple"
    )));
}

#[test]
fn on_http_receives_structured_response_map() {
    // The completion handler is called with a `#{ ok, status, headers,
    // body, error }` map (mirrors how script-runtime marshals the reply).
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_http(tag, resp) {
            if resp.ok && resp.status == 200 {
                print(tag + ":" + resp.body + ":" + resp.headers["x-kind"]);
            }
        }
        "#,
    )
    .expect("load");

    let mut headers = std::collections::HashMap::new();
    headers.insert("x-kind".to_string(), ScriptValue::Str("json".to_string()));
    let mut resp = std::collections::HashMap::new();
    resp.insert("ok".to_string(), ScriptValue::Bool(true));
    resp.insert("status".to_string(), ScriptValue::I64(200));
    resp.insert("headers".to_string(), ScriptValue::Map(headers));
    resp.insert("body".to_string(), ScriptValue::Str("hi".to_string()));
    resp.insert("error".to_string(), ScriptValue::Str(String::new()));

    let cmds = host
        .call_event(
            "on_http",
            &[ScriptValue::Str("t".into()), ScriptValue::Map(resp)],
        )
        .expect("call");
    let prints: Vec<String> = cmds
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(prints, vec!["t:hi:json"]);
}
