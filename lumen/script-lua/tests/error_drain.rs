//! A Lua handler that queues commands and *then* errors must contribute
//! NO commands: the failed batch is discarded so it cannot leak into an
//! unrelated later event's outcome. Parity with the Rhai host's
//! `erroring_handler_leaks_no_commands` test; guards the error-path sink
//! drain in `LuaHost::call`.

use lumen_script_lua::LuaHost;

#[test]
fn erroring_handler_leaks_no_commands() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_ok() set_text("lbl", "kept") end
        function on_boom()
            set_text("lbl", "leaked")
            error("deliberate failure")
        end
        "#,
    )
    .expect("load inline script");

    // Positive control: a successful handler drains its one command.
    let cmds = host.call_event("on_ok", &[]).expect("ok handler runs");
    assert_eq!(cmds.len(), 1, "successful handler yields its command");

    // The erroring handler queued a SetText *before* raising.
    let res = host.call_event("on_boom", &[]);
    assert!(res.is_err(), "handler error surfaces as Err, not Ok");

    // End-to-end: the next unrelated event sees only its own command,
    // proving the failed handler's queued command did not leak across the
    // event boundary.
    let next = host.call_event("on_ok", &[]).expect("ok handler runs");
    assert_eq!(next.len(), 1, "next event only sees its own command");
}
