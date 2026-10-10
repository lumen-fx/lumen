# `lumen_script::push_plugin_event` takes the event by value

This affects Rust runtime modules that deliver events to scripts through
`lumen_module::lumen_script::push_plugin_event`. The function now takes the
`PluginEvent` itself instead of a reference, so a call that passes `&event`
no longer compiles.

Before:

```rust
lumen_script::push_plugin_event(&event);
```

After:

```rust
lumen_script::push_plugin_event(event);
```

Pass a clone if you still need the event after the call.
