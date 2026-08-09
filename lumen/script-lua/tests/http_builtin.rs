//! The `http({...})` builtin (parity with the Rhai host's `http_builtin`
//! test): the Lua table parses into a `ScriptCommand::Http` with the
//! right method / headers / body / timeout, omitted fields take
//! web-`fetch`-like defaults, `fetch(url, tag)` still lowers to
//! `ScriptCommand::Fetch`, and `on_http` receives a structured response.

use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_lua::LuaHost;

fn only_http(cmds: &[ScriptCommand]) -> &ScriptCommand {
    cmds.iter()
        .find(|c| matches!(c, ScriptCommand::Http { .. }))
        .expect("expected a ScriptCommand::Http")
}

#[test]
fn http_full_request_parses() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_start()
            http({
                method = "POST",
                url = "http://127.0.0.1:9/things",
                headers = { ["Content-Type"] = "application/json", ["X-Token"] = "abc" },
                body = "{\"a\":1}",
                timeout_ms = 1500,
                tag = "create",
            })
        end
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
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_start()
            http({ url = "http://127.0.0.1:9/x", tag = "g" })
        end
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
    assert_eq!(method, "GET");
    assert_eq!(url, "http://127.0.0.1:9/x");
    assert!(headers.is_empty());
    assert_eq!(*body, None);
    assert_eq!(*timeout_ms, None);
    assert_eq!(tag, "g");
}

#[test]
fn fetch_still_lowers_to_fetch_command() {
    let mut host = LuaHost::new();
    host.load(r#"function on_start() fetch("http://127.0.0.1:9/y", "simple") end"#)
        .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    assert!(cmds.iter().any(|c| matches!(
        c,
        ScriptCommand::Fetch { url, tag } if url == "http://127.0.0.1:9/y" && tag == "simple"
    )));
}

#[test]
fn on_http_receives_structured_response_map() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_http(tag, resp)
            if resp.ok and resp.status == 200 then
                print(tag .. ":" .. resp.body .. ":" .. resp.headers["x-kind"])
            end
        end
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
