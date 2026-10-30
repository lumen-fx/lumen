# Window script commands name the window

This affects script hosts and runtime modules built outside this repository
against `lumen-script`. `ScriptCommand::WindowSetTitle`, `WindowSetSize`, and
`WindowClose` gained a `window` field, the packed handle of the window they act
on, `0` for the current window, and `SCRIPT_WIRE_VERSION` is now 5. Fill it
with `lumen_core::window_state::current()`, which is the window of the event
being handled, and rebuild: a module built against version 4 is refused at
load.

```rust
ScriptCommand::WindowClose
ScriptCommand::WindowClose { window: window_state::current() }
```

Scripts are unaffected.
