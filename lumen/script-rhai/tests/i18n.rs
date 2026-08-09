//! `t("key")` / `tr("key")` read the process-wide translator the runtime
//! installs from the app's `locale/*.ftl` catalogues. With no catalogue
//! loaded, both return the key, so an untranslated app still renders text.

use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_rhai::RhaiHost;

/// The translator hook is a process-global singleton.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn returns(src: &str) -> Option<ScriptValue> {
    let mut host = RhaiHost::new();
    host.load(src).expect("load");
    host.call("probe", &[]).expect("call").ret
}

#[test]
fn t_and_tr_resolve_through_the_translator() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_core::i18n::set_translator(|key| (key == "greet").then(|| "Hallo!".to_string()));

    assert_eq!(
        returns(r#"fn probe() { return t("greet"); }"#),
        Some(ScriptValue::Str("Hallo!".into()))
    );
    assert_eq!(
        returns(r#"fn probe() { return tr("greet"); }"#),
        Some(ScriptValue::Str("Hallo!".into()))
    );

    lumen_core::i18n::clear_translator();
}

#[test]
fn an_unknown_key_returns_itself() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lumen_core::i18n::clear_translator();
    assert_eq!(
        returns(r#"fn probe() { return t("app-title"); }"#),
        Some(ScriptValue::Str("app-title".into()))
    );
}
