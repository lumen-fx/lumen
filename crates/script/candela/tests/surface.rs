//! The builtins that close the candela host's gap against the Rhai and Lua
//! surfaces: `http`, `parse_json`, `parse_markdown`, `local_id`, `is_valid`,
//! the color-signal pair, `print`, and `window::size`.

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// Load `src` with the prelude available.
fn load(src: &str) -> CandelaHost {
    let mut host = CandelaHost::new();
    host.load(src, "surface.cdl").expect("compiles");
    host
}

/// `http(request)` queues one general request from a flat string map, with each
/// header on a `header:<Name>` key. Only `url` and `tag` are required; `method`
/// defaults to `GET`.
#[test]
fn http_queues_a_request_from_a_map() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn go() {
    lumen::http({
        "method": "POST",
        "url": "https://example.test/items",
        "header:Accept": "application/json",
        "body": "{\"n\":1}",
        "timeout_ms": "2500",
        "tag": "items"
    });
}
fn minimal() {
    lumen::http({"url": "https://example.test/ping", "tag": "ping"});
}
fn from_json() {
    let req = as_map(lumen::parse_json("{\"url\":\"https://example.test/j\",\"tag\":\"j\",\"timeout_ms\":900,\"headers\":{\"Accept\":\"text/plain\"}}"));
    lumen::http(req);
}
fn main() {}
"#,
    );

    let out = host.call("go", &[]).expect("go ok");
    let cmd = out
        .commands
        .iter()
        .find(|c| matches!(c, ScriptCommand::Http { .. }))
        .expect("an Http command");
    let ScriptCommand::Http {
        method,
        url,
        headers,
        body,
        timeout_ms,
        tag,
    } = cmd
    else {
        unreachable!()
    };
    assert_eq!(method, "POST");
    assert_eq!(url, "https://example.test/items");
    assert_eq!(
        headers.as_slice(),
        [("Accept".to_owned(), "application/json".to_owned())]
    );
    assert_eq!(body.as_deref(), Some("{\"n\":1}"));
    assert_eq!(*timeout_ms, Some(2500));
    assert_eq!(tag, "items");

    let out = host.call("minimal", &[]).expect("minimal ok");
    let Some(ScriptCommand::Http {
        method,
        headers,
        body,
        timeout_ms,
        ..
    }) = out
        .commands
        .iter()
        .find(|c| matches!(c, ScriptCommand::Http { .. }))
    else {
        panic!("an Http command")
    };
    assert_eq!(method, "GET", "method defaults to GET");
    assert!(headers.is_empty());
    assert_eq!(body.as_deref(), None);
    assert_eq!(*timeout_ms, None);

    // A request that did not come from a literal may carry the nested
    // `headers` map and an int `timeout_ms` the other hosts take.
    let out = host.call("from_json", &[]).expect("from_json ok");
    let Some(ScriptCommand::Http {
        headers,
        timeout_ms,
        tag,
        ..
    }) = out
        .commands
        .iter()
        .find(|c| matches!(c, ScriptCommand::Http { .. }))
    else {
        panic!("an Http command")
    };
    assert_eq!(
        headers.as_slice(),
        [("Accept".to_owned(), "text/plain".to_owned())]
    );
    assert_eq!(*timeout_ms, Some(900));
    assert_eq!(tag, "j");
}

/// `parse_json` hands back candela values, not text: scalars keep their type,
/// nesting survives, and a key longer than candela's inline-string limit still
/// resolves because host-returned map keys are interned.
#[test]
fn parse_json_returns_typed_candela_values() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn status(body) {
    let root = as_map(lumen::parse_json(body));
    return as_int(root.get("status_code"));
}
fn city(body) {
    let root = as_map(lumen::parse_json(body));
    let geo = as_map(root.get("geo"));
    return as_str(geo.get("city_name"));
}
fn first_tag(body) {
    let root = as_map(lumen::parse_json(body));
    let tags = as_list(root.get("tags"));
    return as_str(tags[0]);
}
fn broken() { return is_null(lumen::parse_json("{oops")); }
fn main() {}
"#,
    );

    let body = ScriptValue::Str(
        r#"{"status_code": 200, "geo": {"city_name": "Paris"}, "tags": ["a", "b"]}"#.to_owned(),
    );
    assert_eq!(
        host.call("status", std::slice::from_ref(&body))
            .unwrap()
            .ret,
        Some(ScriptValue::I64(200))
    );
    assert_eq!(
        host.call("city", std::slice::from_ref(&body)).unwrap().ret,
        Some(ScriptValue::Str("Paris".to_owned()))
    );
    assert_eq!(
        host.call("first_tag", std::slice::from_ref(&body))
            .unwrap()
            .ret,
        Some(ScriptValue::Str("a".to_owned()))
    );
    assert_eq!(
        host.call("broken", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
}

/// `parse_markdown` returns block records a `<for>` can render, each carrying
/// its kind, heading level, text, and code-fence language.
#[test]
fn parse_markdown_returns_block_records() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn blocks(src) { return as_list(lumen::parse_markdown(src)).len(); }
fn first_kind(src) {
    let b = as_map(as_list(lumen::parse_markdown(src))[0]);
    return as_str(b.get("kind"));
}
fn first_level(src) {
    let b = as_map(as_list(lumen::parse_markdown(src))[0]);
    return as_int(b.get("level"));
}
fn main() {}
"#,
    );

    let src = ScriptValue::Str("## Heading\n\nA paragraph.\n".to_owned());
    assert_eq!(
        host.call("blocks", std::slice::from_ref(&src)).unwrap().ret,
        Some(ScriptValue::I64(2))
    );
    assert_eq!(
        host.call("first_kind", std::slice::from_ref(&src))
            .unwrap()
            .ret,
        Some(ScriptValue::Str("h".to_owned()))
    );
    assert_eq!(
        host.call("first_level", std::slice::from_ref(&src))
            .unwrap()
            .ret,
        Some(ScriptValue::I64(2))
    );
}

/// `local_id` targets a sibling inside the same template instance; a source
/// with no instance prefix returns the suffix unchanged.
#[test]
fn local_id_resolves_template_siblings() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn nested() { return lumen::local_id("card:a:btn", "label"); }
fn bare() { return lumen::local_id("btn", "label"); }
fn main() {}
"#,
    );
    assert_eq!(
        host.call("nested", &[]).unwrap().ret,
        Some(ScriptValue::Str("card:a:label".to_owned()))
    );
    assert_eq!(
        host.call("bare", &[]).unwrap().ret,
        Some(ScriptValue::Str("label".to_owned()))
    );
}

/// `is_valid` reads the per-tick `valid:<id>` signal. An element that never
/// wrote one reads as valid.
#[test]
fn is_valid_reads_the_validation_signal() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn fail_it() { lumen::signal_set("valid:email", "false"); }
fn pass_it() { lumen::signal_set("valid:email", "true"); }
fn check() { return lumen::is_valid("email"); }
fn unknown() { return lumen::is_valid("never-validated"); }
fn main() {}
"#,
    );
    assert_eq!(
        host.call("unknown", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
    host.call("fail_it", &[]).expect("fail_it ok");
    assert_eq!(
        host.call("check", &[]).unwrap().ret,
        Some(ScriptValue::Bool(false))
    );
    host.call("pass_it", &[]).expect("pass_it ok");
    assert_eq!(
        host.call("check", &[]).unwrap().ret,
        Some(ScriptValue::Bool(true))
    );
}

/// The color pair round-trips a hex string as `{ r, g, b, a }` channels, fills
/// in an opaque alpha for the six-digit form, and ignores malformed input.
#[test]
fn color_signals_round_trip_channels() {
    lumen_core::property_store::init_external_properties();
    let mut host = load(
        r##"
import "lumen.cdl";
fn write_it() { lumen::signal_set_color("accent", "#ff8800"); }
fn write_alpha() { lumen::signal_set_color("accent", "#ff880040"); }
fn write_junk() { lumen::signal_set_color("junk", "not-a-color"); }
fn red() { return lumen::signal_get_color("accent").get("r"); }
fn alpha() { return lumen::signal_get_color("accent").get("a"); }
fn junk_size() { return lumen::signal_get_color("junk").len(); }
fn main() {}
"##,
    );

    host.call("write_it", &[]).expect("write ok");
    assert_eq!(
        host.call("red", &[]).unwrap().ret,
        Some(ScriptValue::I64(255))
    );
    assert_eq!(
        host.call("alpha", &[]).unwrap().ret,
        Some(ScriptValue::I64(255)),
        "the six-digit form is opaque"
    );

    host.call("write_alpha", &[]).expect("write ok");
    assert_eq!(
        host.call("alpha", &[]).unwrap().ret,
        Some(ScriptValue::I64(64))
    );

    host.call("write_junk", &[]).expect("write ok");
    assert_eq!(
        host.call("junk_size", &[]).unwrap().ret,
        Some(ScriptValue::I64(0)),
        "an unparseable color writes nothing, so the read is empty"
    );
}

/// `lumen::print` routes through the command sink rather than process stdout,
/// joining its arguments with a space.
#[test]
fn print_reaches_the_command_sink() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn log_it() { lumen::print("count", 7, true); }
fn main() {}
"#,
    );
    let out = host.call("log_it", &[]).expect("log_it ok");
    let printed: Vec<&String> = out
        .commands
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(printed, [&"count 7 true".to_owned()]);
}

/// `window::size()` reads the current window extent as `[width, height]`.
#[test]
fn window_size_reads_two_floats() {
    lumen_core::window_state::set_size(1024.0, 768.0);
    let mut host = load(
        r#"
import "lumen.cdl";
fn w() { return window::size()[0]; }
fn h() { return window::size()[1]; }
fn main() {}
"#,
    );
    assert_eq!(
        host.call("w", &[]).unwrap().ret,
        Some(ScriptValue::F64(1024.0))
    );
    assert_eq!(
        host.call("h", &[]).unwrap().ret,
        Some(ScriptValue::F64(768.0))
    );
}

/// `matched_rules` returns a list even with no live document, so a script can
/// walk the result unconditionally.
#[test]
fn matched_rules_is_a_list_for_an_unknown_node() {
    let mut host = load(
        r#"
import "lumen.cdl";
fn count() { return as_list(lumen::matched_rules(0)).len(); }
fn main() {}
"#,
    );
    assert_eq!(
        host.call("count", &[]).unwrap().ret,
        Some(ScriptValue::I64(0))
    );
}
