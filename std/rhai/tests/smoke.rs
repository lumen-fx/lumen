//! The Rhai host still works: it loads a program, runs a handler, and reads
//! and writes a signal, reached the way an app reaches it, through the
//! language the module registers.

use lumen_core::prelude::App;
use lumen_script::{ScriptProgram, ScriptValue, install_program};

const PROGRAM: &str = r#"
fn on_start() {
    signal_set_int("count", 1);
}

fn bump() {
    let n = signal_get_int("count");
    signal_set_int("count", n + 1);
    n + 1
}
"#;

#[test]
fn the_host_loads_runs_a_handler_and_reads_and_writes_a_signal() {
    let mut app = App::new();
    app.add_plugin(lumen_rhai::RhaiPlugin);
    let host = install_program(
        &mut app,
        "rhai",
        ScriptProgram {
            source: PROGRAM.to_owned(),
            uri: "smoke.rhai".to_owned(),
            ..ScriptProgram::default()
        },
        false,
    )
    .expect("the module registered the language");
    app.tick();

    assert_eq!(
        (host.signal)(&app.world, "count"),
        Some(ScriptValue::I64(1)),
        "on_start wrote the signal"
    );
    assert_eq!(
        (host.call)(&mut app.world, "bump").expect("bump runs"),
        Some(ScriptValue::I64(2)),
        "the handler read the signal and returned what it wrote"
    );
    assert_eq!(
        (host.signal)(&app.world, "count"),
        Some(ScriptValue::I64(2))
    );
}
