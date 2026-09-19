//! `parse_json` / `parse_markdown`: the two builtins whose result has no
//! shape narrower than `any`. The walk lives once in `lumen-script`'s
//! `text_parse` module (shared with Lua and candela); this exercises the
//! Rhai binding end to end.

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_rhai::RhaiHost;

fn load(src: &str) -> RhaiHost {
    let mut host = RhaiHost::new();
    host.load(src).expect("load");
    host
}

#[test]
fn parse_json_keeps_scalar_types_and_nests() {
    let mut host = load(
        r##"
        fn status() {
            let root = parse_json(`{"status_code": 200, "geo": {"city_name": "Paris"}, "tags": ["a", "b"]}`);
            return root.status_code;
        }
        fn city() {
            let root = parse_json(`{"geo": {"city_name": "Paris"}}`);
            return root.geo.city_name;
        }
        fn first_tag() {
            let root = parse_json(`{"tags": ["a", "b"]}`);
            return root.tags[0];
        }
        fn broken() { return parse_json("{oops"); }
        "##,
    );

    assert_eq!(
        host.call("status", &[]).unwrap().ret,
        Some(ScriptValue::I64(200))
    );
    assert_eq!(
        host.call("city", &[]).unwrap().ret,
        Some(ScriptValue::Str("Paris".to_owned()))
    );
    assert_eq!(
        host.call("first_tag", &[]).unwrap().ret,
        Some(ScriptValue::Str("a".to_owned()))
    );
    assert_eq!(
        host.call("broken", &[]).unwrap().ret,
        Some(ScriptValue::Unit),
        "malformed JSON reads as unit"
    );
}

#[test]
fn parse_markdown_returns_block_records() {
    let mut host = load(
        r##"
        fn blocks_len() {
            let blocks = parse_markdown("# Heading\n\nBody text\n");
            return blocks.len();
        }
        fn first_kind() {
            let blocks = parse_markdown("# Heading\n\nBody text\n");
            return blocks[0].kind;
        }
        fn first_level() {
            let blocks = parse_markdown("# Heading\n\nBody text\n");
            return blocks[0].level;
        }
        "##,
    );

    assert_eq!(
        host.call("blocks_len", &[]).unwrap().ret,
        Some(ScriptValue::I64(2))
    );
    assert_eq!(
        host.call("first_kind", &[]).unwrap().ret,
        Some(ScriptValue::Str("h".to_owned()))
    );
    assert_eq!(
        host.call("first_level", &[]).unwrap().ret,
        Some(ScriptValue::I64(1))
    );
}
