//! `lumen::t("key")` / `lumen::tr("key")` read the process-wide translator
//! the runtime installs from the app's `locale/*.ftl` catalogues. With no
//! catalogue loaded, both return the key, so an untranslated app still
//! renders text.

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// The translator hook is a process-global singleton.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn returns(body: &str) -> Option<ScriptValue> {
    let src = format!("import \"lumen.cdl\";\nfn go() {{ return {body}; }}\nfn main() {{}}\n");
    let mut host = CandelaHost::new();
    host.load(&src, "i18n.cdl").expect("load");
    host.call("go", &[]).expect("call").ret
}

#[test]
fn t_and_tr_resolve_through_the_translator() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_core::i18n::set_translator(|key| (key == "greet").then(|| "Hallo!".to_string()));

    assert_eq!(
        returns("lumen::t(\"greet\")"),
        Some(ScriptValue::Str("Hallo!".into()))
    );
    assert_eq!(
        returns("lumen::tr(\"greet\")"),
        Some(ScriptValue::Str("Hallo!".into()))
    );

    lumen_core::i18n::clear_translator();
}

#[test]
fn an_unknown_key_returns_itself() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_core::i18n::clear_translator();
    assert_eq!(
        returns("lumen::t(\"app-title\")"),
        Some(ScriptValue::Str("app-title".into()))
    );
}
