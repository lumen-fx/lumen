//! `derive(name, deps, f)` on the candela host. candela has no first-class closure
//! value, so the recompute body is referenced by the script function's NAME (a
//! string) and the dep list is a `string[]`. Proves the derivation is
//! registered (pending-initial + dep-matched) and that a recompute reads the
//! current dep values and reacts when a dep signal changes - the contract
//! `apply_derivations` drives.

use std::collections::HashSet;

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

const HEADER: &str = r#"
host "lumen" {
    derive(string, string[], string);
    signal_set_int(string, int);
    int signal_get_int(string);
}
"#;

fn load(host: &mut CandelaHost, body: &str) {
    let src = format!("{HEADER}\n{body}\n");
    host.load(&src, "derive.cdl")
        .unwrap_or_else(|e| panic!("script should compile: {e}"));
}

#[test]
fn derive_registers_and_recomputes_on_dep_change() {
    let mut host = CandelaHost::new();
    load(
        &mut host,
        r#"
fn on_start() {
    lumen::signal_set_int("count", 2);
    lumen::derive("doubled", ["count"], "double_it");
}
fn set_count(n) { lumen::signal_set_int("count", n); }
fn double_it(n) { return n * 2; }
fn main() {}
"#,
    );

    host.call("on_start", &[]).expect("on_start");

    // The derivation is registered: pending its initial run, and matched by
    // its dep `count`.
    assert!(host.pending_initial().contains("doubled"));
    let dirty: HashSet<&str> = ["count"].into_iter().collect();
    let matching = host.derivations_matching(&dirty, &HashSet::new());
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].0, "doubled");
    assert_eq!(matching[0].1, vec!["count".to_owned()]);
    assert_eq!(matching[0].2, "double_it");

    // A derivation whose deps did not change and is not pending does not match.
    let unrelated: HashSet<&str> = ["something_else"].into_iter().collect();
    assert!(
        host.derivations_matching(&unrelated, &HashSet::new())
            .is_empty()
    );

    // Recompute reads the current dep value (count = 2) -> double_it(2) = 4.
    let text = host
        .eval_derivation(&"double_it".to_owned(), &["count".to_owned()], "doubled")
        .expect("eval");
    assert_eq!(text, "4");
    assert_eq!(host.mirror_get("doubled"), Some(ScriptValue::I64(4)));

    // Change the dep signal, then recompute reacts to the new value.
    host.call("set_count", &[ScriptValue::I64(5)])
        .expect("set_count");
    let text = host
        .eval_derivation(&"double_it".to_owned(), &["count".to_owned()], "doubled")
        .expect("eval");
    assert_eq!(text, "10");
    assert_eq!(host.mirror_get("doubled"), Some(ScriptValue::I64(10)));
}
