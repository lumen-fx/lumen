# `ScriptHost::call` returns a `CallFailure` when the function raises

This affects Rust code that implements `lumen_script::ScriptHost` for a script
language of its own, or calls `call` on a host directly. A failed call used to
return a bare `ScriptError` and drop the commands the function had queued; it
now returns `CallFailure { error, commands }`, and the runtime applies those
commands the way a browser keeps what a throwing listener did.

A host implementation drains its command sink on the error path too:

```rust
fn call(&mut self, name: &str, args: &[ScriptValue]) -> Result<CallOutcome, CallFailure> {
    let result = self.run(name, args);
    let commands = self.drain_commands();
    match result {
        Ok(ret) => Ok(CallOutcome { commands, found: ret.is_some(), ret }),
        Err(error) => Err(CallFailure { error, commands }),
    }
}
```

A caller that only wants the error reads `failure.error`.

Scripts need no change. A handler that raised used to lose the text it set, the
timers it armed, and the lines it printed before the error; those now apply.
